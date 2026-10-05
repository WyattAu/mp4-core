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
use crate::fields::{u32_at, u64_at, u8_at};

/// Nanoseconds per `chpl` tick.
pub const CHPL_TICKS_PER_SECOND: u64 = 10_000_000;

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
    /// # Errors
    ///
    /// [`Mp4Error::MalformedChapters`] when the reserved bytes are wrong, the
    /// entry count does not match the payload, or a title is unterminated.
    pub fn parse_chpl(payload: &[u8]) -> Result<ChapterList, Mp4Error> {
        // Layout: 4-byte version (must be 1) | 4-byte reserved (must be 0) |
        // 4-byte entry count | per entry: 8-byte timestamp | 1-byte title
        // length | title bytes.
        if payload.len() < 12 {
            return Err(Mp4Error::MalformedChapters {
                reason: "header is shorter than 12 bytes",
            });
        }
        let ver = u32_at(payload, 0, "chpl version")?;
        if ver != 1 {
            return Err(Mp4Error::MalformedChapters {
                reason: "unsupported chpl version",
            });
        }
        // Bytes 4..8 are reserved and must be zero.
        if payload.get(4..8).is_some_and(|r| r.iter().any(|b| *b != 0)) {
            return Err(Mp4Error::MalformedChapters {
                reason: "reserved field is not zero",
            });
        }
        let count = u32_at(payload, 8, "chpl entry count")?;
        let mut chapters = Vec::with_capacity(count.min(1024) as usize);
        let mut offset = 12usize;

        for _ in 0..count {
            let ticks = u64_at(payload, offset, "chapter timestamp")?;
            offset += 8;
            let len = u8_at(payload, offset, "chapter title length")? as usize;
            offset += 1;
            let title_bytes =
                payload
                    .get(offset..offset + len)
                    .ok_or(Mp4Error::MalformedChapters {
                        reason: "chapter title runs past the end of the box",
                    })?;
            // Nero writes titles as UTF-8; some muxers wrote Latin-1. Decode as
            // UTF-8 and fall back to Latin-1 rather than failing the whole list.
            let title = match core::str::from_utf8(title_bytes) {
                Ok(s) => String::from(s),
                Err(_) => title_bytes.iter().map(|b| char::from(*b)).collect(),
            };
            chapters.push(Chapter {
                start_ms: ticks / (CHPL_TICKS_PER_SECOND / 1000),
                title,
            });
            offset += len;
        }

        Ok(ChapterList { chapters })
    }

    /// Serialize to a `chpl` payload.
    ///
    /// Titles longer than 255 bytes are truncated, because the format's title
    /// length is a single byte. This is a format limit, not a policy choice.
    #[must_use]
    pub fn to_chpl_payload(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(16 + self.chapters.len() * 24);
        out.extend_from_slice(&1u32.to_be_bytes()); // version 1
        out.extend_from_slice(&[0u8; 4]); // reserved
        out.extend_from_slice(&(self.chapters.len() as u32).to_be_bytes());
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

/// Read chapter names out of a QuickTime text track's samples.
///
/// A chapter track stores each chapter's title as one text sample. This reads
/// the `stsz` sample sizes and pulls that many bytes per sample out of
/// `mdat`, which is enough to recover names without decoding the sample table
/// fully. Returns an empty list when the box chain is absent, which is the
/// normal case for a file that uses `chpl` instead.
///
/// # Errors
///
/// [`Mp4Error::MissingBox`] when a required box is absent, and
/// [`Mp4Error::TruncatedBox`] when the samples do not fit the `mdat`.
pub fn read_text_track_titles(track: &Box_, mdat: &[u8]) -> Result<Vec<String>, Mp4Error> {
    let stbl = track
        .child(*b"mdia")
        .and_then(|m| m.child(*b"minf"))
        .and_then(|m| m.child(*b"stbl"))
        .ok_or(Mp4Error::MissingBox { box_type: *b"stbl" })?;

    let stsz = stbl
        .child(*b"stsz")
        .ok_or(Mp4Error::MissingBox { box_type: *b"stsz" })?;
    let stco = stbl.child(*b"stco").or_else(|| stbl.child(*b"co64"));

    let sizes = stsz.payload.as_slice();
    // Layout: version+flags (4) | sample_size (4) | sample_count (4) | sizes.
    let uniform = u32_at(sizes, 4, "stsz sample size")?;
    let count = u32_at(sizes, 8, "stsz sample count")?;
    let mut per_sample: Vec<u32> = Vec::new();
    if uniform == 0 {
        for i in 0..count {
            let off = 12 + (i as usize) * 4;
            per_sample.push(u32_at(sizes, off, "stsz entry")?);
        }
    }

    // Chunk offsets: the first offset is where the track's data begins.
    let base = match stco {
        Some(c) => {
            let p = c.payload.as_slice();
            if c.box_type == *b"co64" {
                u64_at(p, 8, "co64 first offset")? as usize
            } else {
                u32_at(p, 8, "stco first offset")? as usize
            }
        }
        None => 0,
    };

    let mut titles = Vec::new();
    let mut cursor = base;
    for size in per_sample {
        let bytes = mdat
            .get(cursor..cursor + size as usize)
            .ok_or(Mp4Error::TruncatedBox {
                box_type: *b"mdat",
                declared: cursor as u64 + u64::from(size),
                available: mdat.len(),
            })?;
        // QuickTime text samples are usually UTF-8, sometimes UTF-16 with a BOM.
        titles.push(decode_text_sample(bytes));
        cursor += size as usize;
    }
    Ok(titles)
}

/// Decode a QuickTime text sample, honouring a UTF-16 BOM if present.
#[must_use]
pub fn decode_text_sample(bytes: &[u8]) -> String {
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
