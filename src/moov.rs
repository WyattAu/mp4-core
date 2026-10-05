//! Top-level file structure: `ftyp`, `moov`, and the track summary an audiobook
//! player needs.
//!
//! A file's shape is a flat box list, and the boxes that matter for audio are:
//!
//! ```text
//! ftyp   brand, compatibility
//! moov   the movie box — metadata lives here
//!   mvhd  timescale + duration
//!   trak  a track
//!     tkhd  track id, duration, flags (enabled | in movie | in preview)
//     mdia
//!       mdhd  timescale + duration
//!       hdlr  'soun' for audio, 'text' for a chapter track
//!       minf
//!         stbl
//!           stsd  sample descriptions — 'mp4a' for audio
//!           stsz  sample sizes
//!           stco  chunk offsets
//! mdat  the media data
//! ```
//!
//! # Duration is not the same in two boxes
//!
//! `mvhd` and `mdhd` each carry a duration, and each is in **its own** timescale
//! — which need not match. This is the single most common bug in an MP4 reader:
//! a duration read from `mvhd` and applied at the track's timescale is wrong
//! whenever the two differ. Every accessor here returns milliseconds computed
//! with the timescale that governs the field.

use alloc::vec::Vec;

use crate::container::{parse_boxes, Box_};
use crate::error::Mp4Error;
use crate::fields::{
    audio_sample_rate, f64_to_fixed_16_16, fixed_16_16_to_f64, push_u16, push_u32, u32_at, u64_at,
    version,
};
use crate::metadata::Metadata;

/// A major or compatible brand.
pub type Brand = [u8; 4];

/// `isom` — the ISO base media brand.
pub const BRAND_ISOM: Brand = *b"isom";
/// `M4A ` — Apple audio, single audio track.
pub const BRAND_M4A: Brand = *b"M4A ";
/// `M4B ` — Apple audiobook: an M4A with a chapter track and `stik` = 9.
pub const BRAND_M4B: Brand = *b"M4B ";
/// `mp42` — ISO/IEC 14496-12 base media.
pub const BRAND_MP42: Brand = *b"mp42";
/// `isom` minor version 512, the common "version 0" file.
pub const MINOR_VERSION_512: u32 = 512;

/// The `ftyp` box: what the file claims to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileType {
    /// The major brand.
    pub major_brand: Brand,
    /// The minor version.
    pub minor_version: u32,
    /// Compatible brands, in order.
    pub compatible_brands: Vec<Brand>,
}

impl FileType {
    /// An M4B file type: major brand `M4B `, compatible with M4A and isom.
    #[must_use]
    pub fn audiobook() -> Self {
        Self {
            major_brand: BRAND_M4B,
            minor_version: 0,
            compatible_brands: alloc::vec![BRAND_M4B, BRAND_M4A, BRAND_ISOM],
        }
    }

    /// Parse an `ftyp` payload.
    ///
    /// # Errors
    ///
    /// [`Mp4Error::TruncatedField`] when the payload is shorter than the
    /// 8-byte fixed header.
    pub fn parse(payload: &[u8]) -> Result<Self, Mp4Error> {
        if payload.len() < 8 {
            return Err(Mp4Error::TruncatedField {
                field: "ftyp",
                expected: 8,
                got: payload.len(),
            });
        }
        let major: Brand = payload
            .get(..4)
            .and_then(|s| <[u8; 4]>::try_from(s).ok())
            .unwrap_or([0; 4]);
        let minor_version = u32_at(payload, 4, "minor version")?;
        let mut compatible_brands = Vec::new();
        let mut i = 8;
        while let Some(chunk) = payload.get(i..i + 4) {
            let b: Brand = <[u8; 4]>::try_from(chunk).unwrap_or([0; 4]);
            if b == [0; 4] {
                break;
            }
            compatible_brands.push(b);
            i += 4;
        }
        Ok(Self {
            major_brand: major,
            minor_version,
            compatible_brands,
        })
    }

    /// Serialize to an `ftyp` payload.
    #[must_use]
    pub fn to_payload(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + self.compatible_brands.len() * 4);
        out.extend_from_slice(&self.major_brand);
        push_u32(&mut out, self.minor_version);
        for b in &self.compatible_brands {
            out.extend_from_slice(b);
        }
        out
    }

    /// Whether the file declares itself compatible with `brand`.
    #[must_use]
    pub fn is_compatible_with(&self, brand: Brand) -> bool {
        self.major_brand == brand || self.compatible_brands.contains(&brand)
    }

    /// Whether this looks like an audiobook.
    ///
    /// The brand is a hint, not proof: a file can carry `M4B ` without chapters
    /// and an `M4A ` with them. Check the media kind as well.
    #[must_use]
    pub fn claims_audiobook(&self) -> bool {
        self.major_brand == BRAND_M4B || self.is_compatible_with(BRAND_M4B)
    }
}

/// A track's header: its id, duration, and where it appears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackHeader {
    /// The track id, which `tref` uses to refer to this track.
    pub track_id: u32,
    /// Duration in the *movie's* timescale.
    pub duration_movie_timescale: u64,
    /// Duration in milliseconds, converted using the movie timescale.
    pub duration_ms: u64,
    /// Whether the track is enabled (bit 0 of `tkhd` flags).
    pub enabled: bool,
    /// Whether the track is in the movie presentation (bit 1).
    pub in_movie: bool,
}

impl TrackHeader {
    /// Parse a `tkhd` payload.
    ///
    /// # Errors
    ///
    /// [`Mp4Error::TruncatedField`] when the payload is too short for the
    /// version being read.
    pub fn parse(payload: &[u8], movie_timescale: u32) -> Result<Self, Mp4Error> {
        let ver = version(payload)?;
        let flags = crate::fields::flags(payload)?;
        // version 0: id(4) reserved(4) duration(4) ; version 1 uses 8 for each.
        let (track_id, dur_off) = if ver == 1 {
            (u32_at(payload, 20, "tkhd track id")?, 28usize)
        } else {
            (u32_at(payload, 12, "tkhd track id")?, 20usize)
        };
        let duration = if ver == 1 {
            u64_at(payload, dur_off, "tkhd duration")?
        } else {
            u64::from(u32_at(payload, dur_off, "tkhd duration")?)
        };
        let scale = if movie_timescale == 0 {
            1
        } else {
            movie_timescale
        };
        Ok(Self {
            track_id,
            duration_movie_timescale: duration,
            duration_ms: duration * 1000 / u64::from(scale),
            enabled: flags & 1 != 0,
            in_movie: flags & 2 != 0,
        })
    }
}

/// A track's media header: its timescale and duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaHeader {
    /// Samples per second — the unit every duration in this track is in.
    pub timescale: u32,
    /// Duration in `timescale` units.
    pub duration_units: u64,
    /// Duration in milliseconds.
    pub duration_ms: u64,
}

impl MediaHeader {
    /// Parse an `mdhd` payload.
    ///
    /// # Errors
    ///
    /// As [`TrackHeader::parse`].
    pub fn parse(payload: &[u8]) -> Result<Self, Mp4Error> {
        let ver = version(payload)?;
        let (timescale, duration) = if ver == 1 {
            (
                u32_at(payload, 20, "mdhd timescale")?,
                u64_at(payload, 24, "mdhd duration")?,
            )
        } else {
            (
                u32_at(payload, 12, "mdhd timescale")?,
                u64::from(u32_at(payload, 16, "mdhd duration")?),
            )
        };
        let scale = if timescale == 0 { 1 } else { timescale };
        Ok(Self {
            timescale,
            duration_units: duration,
            duration_ms: duration * 1000 / u64::from(scale),
        })
    }

    /// Build an `mdhd` payload for a duration in milliseconds.
    #[must_use]
    pub fn to_payload(timescale: u32, duration_ms: u64) -> Vec<u8> {
        let mut out = Vec::with_capacity(24);
        out.push(0); // version 0
        out.extend_from_slice(&[0, 0, 0]); // flags
        out.extend_from_slice(&0u32.to_be_bytes()); // creation_time
        out.extend_from_slice(&0u32.to_be_bytes()); // modification_time
        push_u32(&mut out, timescale);
        push_u32(
            &mut out,
            u32::try_from(duration_ms * u64::from(timescale) / 1000).unwrap_or(u32::MAX),
        );
        out.extend_from_slice(&0x55C4u16.to_be_bytes()); // language 'und'
        out.extend_from_slice(&0u16.to_be_bytes()); // pre_defined
        out
    }
}

/// The movie header: the file's timescale and duration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MovieHeader {
    /// The unit every `mvhd`/`tkhd` duration is expressed in.
    pub timescale: u32,
    /// Duration in `timescale` units.
    pub duration_units: u64,
    /// Duration in milliseconds.
    pub duration_ms: u64,
    /// Nominal playback rate, as 16.16 fixed point. 1.0 is normal speed.
    pub rate: f64,
}

impl MovieHeader {
    /// Parse an `mvhd` payload.
    ///
    /// # Errors
    ///
    /// [`Mp4Error::TruncatedField`] when the payload is too short.
    pub fn parse(payload: &[u8]) -> Result<Self, Mp4Error> {
        let ver = version(payload)?;
        let (timescale, dur_off, rate_off) = if ver == 1 {
            (u32_at(payload, 20, "mvhd timescale")?, 24usize, 28usize)
        } else {
            (u32_at(payload, 12, "mvhd timescale")?, 16usize, 20usize)
        };
        let duration = if ver == 1 {
            u64_at(payload, dur_off, "mvhd duration")?
        } else {
            u64::from(u32_at(payload, dur_off, "mvhd duration")?)
        };
        let scale = if timescale == 0 { 1 } else { timescale };
        Ok(Self {
            timescale,
            duration_units: duration,
            duration_ms: duration * 1000 / u64::from(scale),
            rate: fixed_16_16_to_f64(u32_at(payload, rate_off, "mvhd rate")?),
        })
    }

    /// Build an `mvhd` payload for a duration in milliseconds.
    #[must_use]
    pub fn to_payload(timescale: u32, duration_ms: u64) -> Vec<u8> {
        let mut out = Vec::with_capacity(100);
        out.push(0); // version 0
        out.extend_from_slice(&[0, 0, 0]); // flags
        out.extend_from_slice(&0u32.to_be_bytes()); // creation_time
        out.extend_from_slice(&0u32.to_be_bytes()); // modification_time
        push_u32(&mut out, timescale);
        push_u32(
            &mut out,
            u32::try_from(duration_ms * u64::from(timescale) / 1000).unwrap_or(u32::MAX),
        );
        push_u32(&mut out, f64_to_fixed_16_16(1.0)); // rate
        push_u16(&mut out, 0x0100); // volume 1.0
        out.extend_from_slice(&[0u8; 2]); // reserved
        out.extend_from_slice(&[0u8; 8]); // reserved
                                          // Unity matrix, as 16.16 fixed point with the usual [1 0 0 0 1 0 0 0 1].
        for v in [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
            push_u32(&mut out, v);
        }
        out.extend_from_slice(&[0u8; 24]); // pre_defined
        out.extend_from_slice(&2u32.to_be_bytes()); // next_track_id
        out
    }
}

/// The handler type of a track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandlerType {
    /// `soun` — audio.
    Audio,
    /// `text` — text, used for a chapter track.
    Text,
    /// `vide` — video.
    Video,
    /// A type this crate does not model.
    Other([u8; 4]),
}

impl HandlerType {
    /// Decode the four handler bytes.
    #[must_use]
    pub fn from_bytes(b: [u8; 4]) -> Self {
        match &b {
            b"soun" => HandlerType::Audio,
            b"text" => HandlerType::Text,
            b"vide" => HandlerType::Video,
            _ => HandlerType::Other(b),
        }
    }
}

/// A track, summarized.
#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    /// The track's header.
    pub header: TrackHeader,
    /// The track's media header.
    pub media: MediaHeader,
    /// What the track carries.
    pub handler: HandlerType,
}

impl Track {
    /// Parse a `trak` box.
    ///
    /// # Errors
    ///
    /// [`Mp4Error::MissingBox`] when `tkhd`, `mdhd`, or `hdlr` is absent.
    pub fn parse(trak: &Box_, movie_timescale: u32) -> Result<Self, Mp4Error> {
        let header = trak
            .child(*b"tkhd")
            .ok_or(Mp4Error::MissingBox { box_type: *b"tkhd" })?;
        let tkhd = TrackHeader::parse(&header.payload, movie_timescale)?;

        let mdia = trak
            .child(*b"mdia")
            .ok_or(Mp4Error::MissingBox { box_type: *b"mdia" })?;
        let mdhd_box = mdia
            .child(*b"mdhd")
            .ok_or(Mp4Error::MissingBox { box_type: *b"mdhd" })?;
        let media = MediaHeader::parse(&mdhd_box.payload)?;

        let hdlr_box = mdia
            .child(*b"hdlr")
            .ok_or(Mp4Error::MissingBox { box_type: *b"hdlr" })?;
        // hdlr: version+flags(4) pre_defined(4) handler_type(4).
        let handler_bytes: [u8; 4] = hdlr_box
            .payload
            .get(8..12)
            .and_then(|s| <[u8; 4]>::try_from(s).ok())
            .unwrap_or([0; 4]);

        Ok(Self {
            header: tkhd,
            media,
            handler: HandlerType::from_bytes(handler_bytes),
        })
    }
}

/// A parsed MP4 file: its structure, tracks, and metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct Mp4File {
    /// The `ftyp` contents, when present.
    pub file_type: Option<FileType>,
    /// The movie header, when present.
    pub movie: Option<MovieHeader>,
    /// Every track, in file order.
    pub tracks: Vec<Track>,
    /// The metadata from `moov` → `udta` → `meta` → `ilst`.
    pub metadata: Metadata,
    /// The parsed chapter list, from `chpl` when present.
    pub chapters: crate::chapter::ChapterList,
}

impl Mp4File {
    /// Parse a file's boxes and summarize it.
    ///
    /// # Errors
    ///
    /// [`Mp4Error::TruncatedBox`] when a box declares more bytes than the file
    /// holds.
    pub fn parse(data: &[u8]) -> Result<Self, Mp4Error> {
        let boxes = parse_boxes(data)?;

        let file_type = boxes
            .iter()
            .find(|b| b.box_type == *b"ftyp")
            .and_then(|b| FileType::parse(&b.payload).ok());

        let moov = boxes.iter().find(|b| b.box_type == *b"moov");

        let movie = moov
            .and_then(|m| m.child(*b"mvhd"))
            .and_then(|b| MovieHeader::parse(&b.payload).ok());

        let movie_timescale = movie.map_or(1, |m| if m.timescale == 0 { 1 } else { m.timescale });

        let mut tracks = Vec::new();
        if let Some(m) = moov {
            for trak in m.children_of(*b"trak") {
                if let Ok(t) = Track::parse(trak, movie_timescale) {
                    tracks.push(t);
                }
            }
        }

        let metadata = moov
            .and_then(|m| m.child(*b"udta"))
            .and_then(|u| u.child(*b"meta"))
            .and_then(crate::metadata::parse_meta)
            .unwrap_or_default();

        let chapters = crate::chapter::ChapterList::find_chpl(data).unwrap_or_default();

        Ok(Self {
            file_type,
            movie,
            tracks,
            metadata,
            chapters,
        })
    }

    /// The first audio track.
    #[must_use]
    pub fn audio_track(&self) -> Option<&Track> {
        self.tracks.iter().find(|t| t.handler == HandlerType::Audio)
    }

    /// The chapter track, when the file uses the text-track form.
    #[must_use]
    pub fn chapter_track(&self) -> Option<&Track> {
        self.tracks.iter().find(|t| t.handler == HandlerType::Text)
    }

    /// The file's duration in milliseconds, from the movie header.
    #[must_use]
    pub fn duration_ms(&self) -> Option<u64> {
        self.movie.map(|m| m.duration_ms)
    }

    /// Whether this file has an audio track.
    #[must_use]
    pub fn has_audio_track(&self) -> bool {
        self.audio_track().is_some()
    }

    /// Whether this file is an audiobook by either signal.
    ///
    /// The brand alone is not trusted: a file can carry `M4B ` with no chapters,
    /// and an `M4A ` with them. Both the media kind and the presence of chapters
    /// count.
    #[must_use]
    pub fn is_audiobook(&self) -> bool {
        self.metadata.is_audiobook()
            || !self.chapters.chapters.is_empty()
            || self.chapter_track().is_some()
            || self
                .file_type
                .as_ref()
                .is_some_and(FileType::claims_audiobook)
    }
}

/// Read the sample rate from the first audio sample entry in a file.
///
/// The rate sits at a fixed offset in the `AudioSampleEntry` — 6 reserved +
/// 2 `data_reference_index` + 8 reserved + 2 channels + 2 sample size +
/// 2 pre-defined + 2 reserved, then the 4-byte rate. That word is an integer in
/// its **high** half, not a 16.16 fraction; see
/// [`crate::fields::audio_sample_rate`] for why reading it as a fraction yields
/// 0 for every real file. A stored 0 means "same as the track's timescale", the
/// spec's convention rather than a zero-rate track, so it yields `None`.
///
/// Reaching it needs the `stsd` full-box header to have been handled: a reader
/// that skipped only `version`+`flags` would mistake the entry count for a box
/// size and find no entries at all.
#[must_use]
pub fn read_sample_rate(data: &[u8]) -> Option<u32> {
    let boxes = parse_boxes(data).ok()?;
    let moov = boxes.iter().find(|b| b.box_type == *b"moov")?;
    for trak in moov.children_of(*b"trak") {
        let Some(stsd) = trak
            .child(*b"mdia")
            .and_then(|m| m.child(*b"minf"))
            .and_then(|m| m.child(*b"stbl"))
            .and_then(|s| s.child(*b"stsd"))
        else {
            continue;
        };
        let Some(entry) = stsd.children.first() else {
            continue;
        };
        if entry.box_type != *b"mp4a" && entry.box_type != *b".mp3" {
            continue;
        }
        let raw = u32_at(&entry.payload, 24, "sample rate").ok()?;
        let rate = audio_sample_rate(raw);
        if rate == 0 {
            // The spec's "same as the track's timescale" convention.
            continue;
        }
        return Some(rate);
    }
    None
}

/// Read an `ftyp` from a file, if it has one.
#[must_use]
pub fn read_ftyp(data: &[u8]) -> Option<FileType> {
    parse_boxes(data)
        .ok()?
        .into_iter()
        .find(|b| b.box_type == *b"ftyp")
        .and_then(|b| FileType::parse(&b.payload).ok())
}

/// Read the `mvhd` from a file, if it has one.
#[must_use]
pub fn read_mvhd(data: &[u8]) -> Option<MovieHeader> {
    let moov = parse_boxes(data)
        .ok()?
        .into_iter()
        .find(|b| b.box_type == *b"moov")?;
    MovieHeader::parse(&moov.child(*b"mvhd")?.payload).ok()
}

/// Read every track's summary, without the metadata or chapters.
#[must_use]
pub fn read_tracks(data: &[u8]) -> Vec<Track> {
    let boxes = parse_boxes(data).unwrap_or_default();
    let moov = match boxes.iter().find(|b| b.box_type == *b"moov") {
        Some(m) => m,
        None => return Vec::new(),
    };
    let movie = moov
        .child(*b"mvhd")
        .and_then(|b| MovieHeader::parse(&b.payload).ok());
    let scale = movie.map_or(1, |m| if m.timescale == 0 { 1 } else { m.timescale });
    moov.children_of(*b"trak")
        .into_iter()
        .filter_map(|t| Track::parse(t, scale).ok())
        .collect()
}

/// A convenience for the common "how long is this file" question.
#[must_use]
pub fn duration_ms(data: &[u8]) -> Option<u64> {
    read_mvhd(data).map(|m| m.duration_ms)
}

/// A convenience re-export of the brand constants.
pub mod brands {
    pub use super::{BRAND_ISOM, BRAND_M4A, BRAND_M4B, BRAND_MP42, MINOR_VERSION_512};
}
