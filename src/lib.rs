//! ISO-BMFF (MP4/M4A/M4B) box reader and writer.
//!
//! `mp4-core` is the estate's L0 MP4 container layer. `id3-core` reads the tags
//! in an MP3; this reads the box structure of an MP4 — and an audiobook's M4B
//! is an MP4 with a chapter track, so this is the container half of the same
//! problem.
//!
//! It is `#![no_std]` + `alloc` with one runtime dependency (`thiserror`, the
//! same typed-error choice as every other L0 kit).
//!
//! # What it covers
//!
//! - **The box tree** — all three size encodings (32-bit, 64-bit `largesize`,
//!   and `size == 0` to end of container), the `uuid` extended type, container
//!   descent through the known container set, and recursive serialization that
//!   recomputes sizes.
//! - **Top-level structure** — `ftyp`, `moov`/`mvhd`, `trak`/`tkhd`,
//!   `mdia`/`mdhd`, `hdlr`, and the track summary an audiobook player needs.
//! - **iTunes metadata** — `udta` → `meta` → `ilst`, with the `data` type flag
//!   modelled explicitly (UTF-8 vs JPEG vs PNG vs integer are not
//!   interchangeable) and the `meta` full-box-header quirk tolerated.
//! - **Chapters** — the Nero `chpl` box, and reading chapter titles out of a
//!   QuickTime text track's samples.
//!
//! # What it deliberately does not do
//!
//! - **No codec work.** A sample entry's `mp4a` box is parsed for its sample
//!   rate; the audio inside is `dsp-core`'s and a decoder's business.
//! - **No muxing.** Boxes can be built and assembled, but this crate does not
//!   interleave samples into `mdat` or produce a conformant chapter *text
//!   track*; it reads one if given.
//! - **No fragmented-MP4 streaming logic.** `moof`/`traf` are parsed as
//!   containers, but the segment-sequence rules are left to the caller.
//!
//! # Example
//!
//! Read an M4B and ask what it holds:
//!
//! ```
//! use mp4_core::chapter::{Chapter, ChapterList};
//! use mp4_core::{Mp4File};
//!
//! // A chapter list is the part of an M4B a library cares about most.
//! let chapters = ChapterList {
//!     chapters: vec![
//!         Chapter::new(0, "Opening"),
//!         Chapter::new(900_000, "Chapter One"),
//!     ],
//! };
//!
//! // `chpl` round-trips through its own wire form.
//! let payload = chapters.to_chpl_payload();
//! let back = ChapterList::parse_chpl(&payload).expect("reparses");
//! assert_eq!(back.chapters[1].title, "Chapter One");
//! assert_eq!(back.chapter_at(1_000_000).map(|c| c.title.as_str()), Some("Chapter One"));
//!
//! // An empty buffer is not a file.
//! assert!(Mp4File::parse(&[]).is_ok());
//! ```

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

pub mod chapter;
pub mod container;
pub mod error;
pub mod fields;
pub mod metadata;
pub mod moov;

pub use chapter::{Chapter, ChapterList, MAX_CHPL_CHAPTERS, MAX_CHPL_TITLE_BYTES};
pub use container::{parse_boxes, BoxType, Box_, Size, SizeForm};
pub use error::Mp4Error;
pub use metadata::{atoms, DataType, Metadata, MetadataValue};
pub use moov::{
    duration_ms, read_ftyp, read_mvhd, read_tracks, FileType, HandlerType, MediaHeader,
    MovieHeader, Mp4File, Track, TrackHeader,
};
