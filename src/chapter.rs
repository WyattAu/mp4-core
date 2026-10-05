//! Chapters: the `chpl` box and the QuickTime text-track form.
//!
//! Two incompatible chapter formats exist in the wild, and an M4B needs both
//! understood because different muxers emit different ones:
//!
//! - **`chpl`** (Nero) — a single box holding a version byte and a list of
//!   100-nanosecond timestamp/title pairs. Compact, and what most audiobook
//!   muxers write.
//! - **A QuickTime text track** — a real `trak` with a `text` sample entry and
//!   chapter names as samples, referenced by a `tref`/`chap` track reference.
//!   More work to write, better supported by players.
//!
//! This crate reads and writes `chpl`, and reads the text-track form's chapter
//! names. Producing a conformant text track is left to the caller, which owns
//! the sample tables.
//!
//! # Units
//!
//! `chpl` timestamps are in 100-nanosecond units, which is **not** what MP4
//! durations use (those are in timescale units, usually samples per second).
//! Every conversion goes through this crate explicitly so the factor cannot be
//! applied twice.

use alloc::string::String;
use alloc::vec::Vec;

use crate::container::{leaf, Box_};
use crate::error::Mp4Error;

/// Nanoseconds per `chpl` tick.
pub const CHPL_TICKS_PER_SECOND: u64 = 10_000_000;

/// The most chapters a `chpl` box can hold, because its count field is a `u8`.
///
/// A format ceiling, not a policy choice. Players have been observed to fail above
/// it — see the note in [`ChapterList::parse_chpl`].
pub const MAX_CHPL_CHAPTERS: usize = 255;

/// The most bytes a `chpl` chapter title can hold, for the same reason.
pub const MAX_CHPL_TITLE_BYTES: usize = 255;

/// One chapter: a start time and a title.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Chapter {
    /// Start offset in milliseconds from the beginning of the audio.
    pub start_ms: u64,
    /// The chapter's title. Empty is legal — a chapter with no name still marks
    /// a boundary.
    pub title: String,
}

impl Chapter {
    /// A chapter at `start_ms` with `title`.
    #[must_use]
    pub fn new(start_ms: u64, title: &str) -> Self {
        Self {
            start_ms,
            title: String::from(title),
        }
    }
}

/// A parsed chapter list.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChapterList {
    /// The chapters, in file order. Not sorted: the file's order is preserved so
    /// a caller can detect a file written out of order.
    pub chapters: Vec<Chapter>,
}

impl ChapterList {
    /// An empty list.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The chapters sorted by start time, the order a player wants.
    #[must_use]
    pub fn sorted(&self) -> Vec<&Chapter> {
        let mut v: Vec<&Chapter> = self.chapters.iter().collect();
        v.sort_by(|a, b| {
            a.start_ms
                .cmp(&b.start_ms)
                .then_with(|| a.title.cmp(&b.title))
        });
        v
    }

    /// The chapter covering `at_ms`, if any.
    ///
    /// Chapters are half-open `[start, end)` with the last chapter running to
    /// infinity — the same convention `CHAP` uses in ID3, so a player can use
    /// one rule for both.
    #[must_use]
    pub fn chapter_at(&self, at_ms: u64) -> Option<&Chapter> {
        let sorted = self.sorted();
        let mut found: Option<&Chapter> = None;
        for c in sorted {
            if c.start_ms <= at_ms {
                found = Some(c);
            } else {
                break;
            }
        }
        found
    }

    /// The total duration implied by the last chapter, if any.
    #[must_use]
    pub fn span_ms(&self) -> Option<u64> {
        self.sorted().last().map(|c| c.start_ms)
    }

    /// Parse a `chpl` box payload.
    ///
    /// # The layout, which is not what the Adobe FLV specification says
    ///
    /// The FLV specification the format derives from describes
    /// `version(1) flags(3) reserved(4) chapter_count(4)`. TagLib — the reference
    /// implementation for this box — writes and reads it as:
    ///
    /// ```text
    /// version(1) flags(3) [reserved(4) if version >= 1] chapter_count(1)
    /// ```
    ///
    /// Two differences, both of which make a reader built from the specification
    /// fail on every real file:
    ///
    /// 1. **The count is one byte, not four.** It is not a quirk but the format's
    ///    limit: 255 chapters, because the count is a `u8`. A reader that reads four
    ///    bytes gets `0x03000000` where the answer is `3`, then walks off the end of
    ///    the box and reports the chapter list as malformed.
    /// 2. **The reserved bytes are absent when the version is 0**, making that
    ///    header five bytes rather than nine.
    ///
    /// This crate had exactly that specification-derived bug: it read a `u32` count
    /// at a fixed offset, so it could not read a single real `chpl` box — only ones it
    /// had written itself. It was found by generating a chapter list with ffmpeg and
    /// reading it back, which no amount of self-consistent testing would have caught.
    ///
    /// Both versions are accepted. A version above 1 is refused, because its layout is
    /// unknown rather than merely uncommon.
    ///
    /// # The 255-chapter ceiling
    ///
    /// Because the count is a `u8`, a book with more chapters than that cannot be
    /// written in this format at all — not by this crate, not by any other. Players
    /// have been observed to fail above that ceiling. [`ChapterList::to_chpl_payload`]
    /// refuses to silently drop chapters for this reason; a caller needing more
    /// should use the QuickTime chapter-track form instead.
    ///
    /// # Errors
    ///
    /// [`Mp4Error::MalformedChapters`] when the header is too short, the count
    /// exceeds what the box can hold, or a title runs past the end.
    pub fn parse_chpl(payload: &[u8]) -> Result<ChapterList, Mp4Error> {
        // version(1) flags(3) count(1) = 5 bytes minimum; version 1 adds 4 reserved.
        const MIN_HEADER: usize = 5;
        if payload.len() < MIN_HEADER {
            return Err(Mp4Error::MalformedChapters {
                reason: "header is shorter than 5 bytes",
            });
        }
        let ver = payload.first().copied().unwrap_or(u8::MAX);
        if ver > 1 {
            return Err(Mp4Error::MalformedChapters {
                reason: "chpl version is newer than the layout this crate knows",
            });
        }
        let mut offset = 4usize;
        if ver >= 1 {
            // Version 1 carries four reserved bytes between the flags and the count.
            let Some(reserved) = payload.get(offset..offset + 4) else {
                return Err(Mp4Error::MalformedChapters {
                    reason: "version 1 header is truncated before its reserved field",
                });
            };
            if reserved.iter().any(|b| *b != 0) {
                return Err(Mp4Error::MalformedChapters {
                    reason: "reserved field is not zero",
                });
            }
            offset += 4;
        }
        let count = payload.get(offset).copied().unwrap_or(0) as usize;
        offset += 1;

        // A count that cannot fit is a malformed box rather than a chapter list to
        // truncate: every entry needs at least 9 bytes, so a count beyond that is
        // certainly wrong and silently clamping it would drop chapters.
        let minimum_entry = 9usize;
        if count.saturating_mul(minimum_entry) > payload.len().saturating_sub(offset) {
            return Err(Mp4Error::MalformedChapters {
                reason: "chapter count cannot fit in the box",
            });
        }

        let mut chapters = Vec::with_capacity(count);
        for _ in 0..count {
            let Some(raw) = payload
                .get(offset..offset + 8)
                .and_then(|b| <[u8; 8]>::try_from(b).ok())
            else {
                return Err(Mp4Error::MalformedChapters {
                    reason: "chapter timestamp runs past the end of the box",
                });
            };
            let ticks = u64::from_be_bytes(raw);
            offset += 8;
            let len = payload.get(offset).copied().unwrap_or(0) as usize;
            offset += 1;
            let title_bytes =
                payload
                    .get(offset..offset + len)
                    .ok_or(Mp4Error::MalformedChapters {
                        reason: "chapter title runs past the end of the box",
                    })?;
            offset += len;

            // Nero writes titles as UTF-8; some muxers wrote Latin-1. Decode as UTF-8
            // and fall back to Latin-1 rather than failing the whole list, because a
            // book with one mis-encoded title is still a readable book.
            let title = match core::str::from_utf8(title_bytes) {
                Ok(s) => String::from(s),
                Err(_) => title_bytes.iter().map(|b| char::from(*b)).collect(),
            };
            chapters.push(Chapter {
                start_ms: ticks / (CHPL_TICKS_PER_SECOND / 1000),
                title,
            });
        }

        Ok(ChapterList { chapters })
    }

    /// Serialize to a `chpl` payload, as version 1.
    ///
    /// Version 1 is what ffmpeg writes and what TagLib reads, and it is the layout
    /// with the documented reserved field. Version 0 — a five-byte header with no
    /// reserved bytes — is still accepted on the way in, because real files carry
    /// it.
    ///
    /// # Two format limits, both silent unless you look for them
    ///
    /// - **At most 255 chapters.** The count is a `u8`, so this is the format's
    ///   ceiling rather than this crate's. A longer list is *dropped* on the way out,
    ///   so [`ChapterList::to_chpl_box_checked`] exists to refuse instead.
    /// - **Titles are truncated at 255 bytes**, because a title's length field is
    ///   also a `u8`.
    ///
    /// Both are unavoidable in this format; a caller who needs more should use the
    /// QuickTime chapter-track form, which has no such ceilings.
    #[must_use]
    pub fn to_chpl_payload(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(16 + self.chapters.len() * 24);
        out.push(1); // version 1
        out.extend_from_slice(&[0u8, 0, 0]); // flags
        out.extend_from_slice(&[0u8; 4]); // reserved
        out.push(u8::try_from(self.chapters.len()).unwrap_or(u8::MAX)); // count
        for c in &self.chapters {
            let ticks = c.start_ms.saturating_mul(CHPL_TICKS_PER_SECOND / 1000);
            out.extend_from_slice(&ticks.to_be_bytes());
            let bytes = c.title.as_bytes();
            let len = bytes.len().min(255);
            out.push(len as u8);
            out.extend_from_slice(bytes.get(..len).unwrap_or(&[]));
        }
        out
    }

    /// Serialize to a `chpl` box, refusing anything the format cannot hold.
    ///
    /// # Errors
    ///
    /// [`Mp4Error::MalformedChapters`] when there are more than 255 chapters or a
    /// title is longer than 255 bytes. This is the form to prefer for a *write* path:
    /// `to_chpl_box` truncates silently, and a library that quietly drops the last
    /// forty chapters of a long book is worse than one that reports it cannot store
    /// them.
    pub fn to_chpl_box_checked(&self) -> Result<Box_, Mp4Error> {
        if self.chapters.len() > MAX_CHPL_CHAPTERS {
            return Err(Mp4Error::MalformedChapters {
                reason: "more chapters than the chpl count field can express",
            });
        }
        if self
            .chapters
            .iter()
            .any(|c| c.title.len() > MAX_CHPL_TITLE_BYTES)
        {
            return Err(Mp4Error::MalformedChapters {
                reason: "a chapter title is longer than the chpl length field can express",
            });
        }
        Ok(self.to_chpl_box())
    }

    /// Build a complete `chpl` box.
    #[must_use]
    pub fn to_chpl_box(&self) -> Box_ {
        leaf(*b"chpl", self.to_chpl_payload()).unwrap_or_else(|_| {
            leaf(*b"chpl", Vec::new()).unwrap_or_else(|_| crate::container::empty(*b"chpl"))
        })
    }

    /// Locate and parse a `chpl` box anywhere in the tree.
    ///
    /// In a real M4B the box lives under `moov` → `udta` → `chpl`, but it is
    /// also written at the file level by some tools, so this searches rather
    /// than assuming.
    #[must_use]
    pub fn find_chpl(data: &[u8]) -> Option<ChapterList> {
        let boxes = crate::container::parse_boxes(data).ok()?;
        let mut found: Option<Box_> = None;
        crate::container::walk(
            &Box_ {
                box_type: *b"root",
                size: crate::container::Size::Fixed(0),
                size_form: crate::container::SizeForm::Normal,
                extended_type: None,
                payload: Vec::new(),
                children: boxes,
            },
            &mut |b| {
                if b.box_type == *b"chpl" {
                    found = Some(b.clone());
                }
                Ok(())
            },
        )
        .ok()?;
        let b = found?;
        ChapterList::parse_chpl(&b.payload).ok()
    }
}

/// Whether a track is a chapter list rather than some other text track.
///
/// # `tref`/`chap` is what makes the difference
///
/// A QuickTime chapter list *is* a text track — one sample per chapter — plus a
/// track reference of type `chap` **from an enabled track** to it. Apple's
/// QuickTime File Format documentation is explicit: "It is the 'chap' track
/// reference that makes the text track into a chapter list."
///
/// So an M4B with subtitles, closed captions, or lyrics has a text track that is
/// not a chapter list. Treating any text track as chapters puts a subtitle list in
/// a player's chapter menu, which is the visible symptom of this crate's earlier
/// behaviour.
///
/// # Which track's flag matters
///
/// The asymmetry is easy to get backwards. The **referencing** track must be
/// enabled; the chapter track itself "does not need to be enabled, and normally is
/// not". So the test is not "is this track enabled" but "does some *enabled*
/// track point at this one's id" — which is why this takes the whole `moov`
/// rather than just the candidate.
///
/// A bare `chap` directly under `udta` is also accepted, because some muxers place
/// it there instead of inside a `tref`.
#[must_use]
pub fn is_chapter_list(track: &Box_, movie: &Box_) -> bool {
    let Some(id) = track_id(track) else {
        return false;
    };
    if movie
        .child(*b"udta")
        .is_some_and(|u| u.child(*b"chap").is_some())
    {
        return true;
    }
    movie.children_of(*b"trak").iter().any(|other| {
        // Only an *enabled* track's reference counts.
        track_enabled(other)
            && other.child(*b"tref").is_some_and(|t| {
                t.children_of(*b"chap")
                    .iter()
                    .any(|c| reference_id(c) == Some(id))
            })
    })
}

/// A track's id from its `tkhd`, for both header versions.
fn track_id(track: &Box_) -> Option<u32> {
    let tkhd = track.child(*b"tkhd")?;
    let ver = tkhd.payload.first().copied()?;
    let offset = if ver == 1 { 20 } else { 12 };
    crate::fields::u32_at(&tkhd.payload, offset, "tkhd track id").ok()
}

/// Whether a track's `tkhd` has the enabled flag (bit 0 of flags).
fn track_enabled(track: &Box_) -> bool {
    track
        .child(*b"tkhd")
        .and_then(|t| crate::fields::flags(&t.payload).ok())
        .is_some_and(|f| f & 1 != 0)
}

/// The track id a `chap` reference box points at.
fn reference_id(chap: &Box_) -> Option<u32> {
    crate::fields::u32_at(&chap.payload, 0, "chap reference").ok()
}

/// Read a QuickTime chapter track's chapters, with titles and start times.
///
/// Each sample's **display time** is the chapter's start — Apple's wording is
/// "the display time for each sample corresponds to the point in the movie that marks
/// the beginning of that chapter" — so the times come from `stts` accumulated through
/// the track's own timescale, and the titles from the samples' bytes.
///
/// Returns an empty list when the box chain is absent, which is the normal case for a
/// file that uses `chpl` instead. A caller that wants to tell "absent" from "present
/// but unreadable" should check
/// [`crate::Mp4File::chapter_track_id`] first, because this returns `Ok` either way.
///
/// # Errors
///
/// [`Mp4Error::MissingBox`] when the sample table is absent, and
/// [`Mp4Error::TruncatedBox`] when a sample lies outside `file`.
pub fn read_text_track_chapters(track: &Box_, file: &[u8]) -> Result<ChapterList, Mp4Error> {
    let stbl = track
        .child(*b"mdia")
        .and_then(|m| m.child(*b"minf"))
        .and_then(|m| m.child(*b"stbl"))
        .ok_or(Mp4Error::MissingBox { box_type: *b"stbl" })?;
    let table = crate::sample::read_sample_table(stbl)?;

    // The timescale governs every time in this track. Reading it from `mvhd` instead
    // is the classic mistake, and the two need not match.
    let timescale = track
        .child(*b"mdia")
        .and_then(|m| m.child(*b"mdhd"))
        .and_then(|b| crate::moov::MediaHeader::parse(&b.payload).ok())
        .map_or(0, |m| m.timescale);

    let mut chapters = Vec::with_capacity(table.sample_count as usize);
    for i in 0..table.sample_count as usize {
        // An incomplete table cannot place the sample. Stopping is much better than
        // reading a plausible-looking offset: a wrong offset yields bytes that look
        // like text, so the failure would be garbage titles rather than an error.
        let (Some(offset), Some(size)) = (table.offsets.get(i), table.sizes.get(i)) else {
            break;
        };
        let bytes = table.sample_bytes(i, file).ok_or(Mp4Error::TruncatedBox {
            box_type: *b"mdat",
            declared: offset.saturating_add(u64::from(*size)),
            available: file.len(),
        })?;
        chapters.push(Chapter {
            start_ms: table.start_time(i, timescale).unwrap_or(0),
            title: decode_text_sample(bytes),
        });
    }
    Ok(ChapterList { chapters })
}

/// Read just the titles of a QuickTime chapter track, without their times.
///
/// Kept because a caller sometimes has its own timing — a sidecar file, say — and only
/// wants the names. Prefer [`read_text_track_chapters`] when times are wanted.
pub fn read_text_track_titles(track: &Box_, file: &[u8]) -> Result<Vec<String>, Mp4Error> {
    Ok(read_text_track_chapters(track, file)?
        .chapters
        .into_iter()
        .map(|c| c.title)
        .collect())
}

/// Decode a QuickTime chapter text sample.
///
/// # The two-byte length prefix is not optional
///
/// Apple's QuickTime File Format and Technical Note TN2174 both specify the
/// sample as a 16-bit big-endian length followed by the text, with **no**
/// terminating null. A chapter title of "Chapter 1" is therefore ten bytes:
///
/// ```text
/// 00 09 43 68 61 70 74 65 72 20 31
/// ^^^^^ ^-------------------------
/// len=9   "Chapter 1"
/// ```
///
/// A reader that skips the prefix returns a title starting with two NUL
/// characters, which every UI renders as `��Chapter 1` — a corruption that is
/// easy to miss because the name is still recognisable.
///
/// The prefix is honoured when it is consistent with the sample, and ignored when
/// it is not, because some encoders write bare text. Requiring the prefix would
/// reject those; trusting it blindly would mangle them.
#[must_use]
pub fn decode_text_sample(bytes: &[u8]) -> String {
    // Strip the length prefix when it agrees with what is present. A prefix that
    // points past the sample is a bare-text sample misread as prefixed, so the
    // bytes are used as they are.
    let declared = bytes
        .get(..2)
        .and_then(|p| <[u8; 2]>::try_from(p).ok())
        .map(u16::from_be_bytes);
    let body = match declared {
        Some(len) if usize::from(len) <= bytes.len().saturating_sub(2) => {
            bytes.get(2..2 + usize::from(len)).unwrap_or(&[])
        }
        _ => bytes,
    };
    decode_text_body(body)
}

/// Decode the text of a sample, honouring a UTF-16 BOM if present.
fn decode_text_body(bytes: &[u8]) -> String {
    match (bytes.first(), bytes.get(1)) {
        (Some(0xFF), Some(0xFE)) | (Some(0xFE), Some(0xFF)) => {
            let le = bytes.first().copied().unwrap_or(0) == 0xFF;
            let units: Vec<u16> = bytes
                .get(2..)
                .unwrap_or(&[])
                .chunks_exact(2)
                .map(|c| {
                    let [a, b] = <[u8; 2]>::try_from(c).unwrap_or([0, 0]);
                    if le {
                        u16::from_le_bytes([a, b])
                    } else {
                        u16::from_be_bytes([a, b])
                    }
                })
                .collect();
            String::from_utf16_lossy(&units)
        }
        _ => match core::str::from_utf8(bytes) {
            Ok(s) => String::from(s),
            Err(_) => bytes.iter().map(|b| char::from(*b)).collect(),
        },
    }
}

/// Convert a chapter list into millisecond pairs, the shape a UI wants.
#[must_use]
pub fn to_ms_pairs(list: &ChapterList) -> Vec<(u64, String)> {
    list.sorted()
        .iter()
        .map(|c| (c.start_ms, c.title.clone()))
        .collect()
}

/// Build a chapter list from millisecond pairs.
#[must_use]
pub fn from_ms_pairs(pairs: &[(u64, String)]) -> ChapterList {
    ChapterList {
        chapters: pairs.iter().map(|(ms, t)| Chapter::new(*ms, t)).collect(),
    }
}
