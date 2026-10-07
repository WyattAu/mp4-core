//! `mp4-core`'s `chpl` writer, checked against ffmpeg's own bytes.
//!
//! # Why this test exists
//!
//! The rest of the suite checks the writer against this crate's own reader, which is the
//! arrangement that can hide a defect forever: a crate writes a box, reads it back, and
//! agrees with itself. The summary of how this crate's `chpl` layout was originally got
//! wrong is that every `chpl` test ran against a box the crate had written.
//!
//! ffmpeg is the independent implementation. Its bytes were captured from a real mux
//! (`ffmpeg -i audio -i chapters.ffmeta -map_metadata 1 -c:a aac out.m4b`, then the `chpl`
//! box lifted out of the file), so the fixture here is what ffmpeg actually emits and not
//! a reconstruction of what the specification says it should.
//!
//! # What is checked
//!
//! * ffmpeg's box parses to the expected chapters — the reader agrees with a foreign
//!   writer;
//! * the writer's output for those same chapters is **byte-identical** to ffmpeg's.
//!
//! Byte-identity is a stronger claim than "parses the same", and it is the useful one: if
//! the bytes match, then everything downstream of the box — checksums, byte offsets, a
//! muxer copying the box verbatim — behaves identically too.
//!
//! The lints the library runs under are relaxed here because this file is test data and
//! test oracle; the fixtures are byte constants and there is nothing to recover from.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use mp4_core::{Chapter, ChapterList, Mp4File};

/// The `chpl` box exactly as ffmpeg wrote it, captured from a real mux.
///
/// Layout, for whoever is reading this against the specification: size (4), `chpl` (4),
/// version 1 (1), flags (3), reserved (4), count (1), then per chapter a start time in
/// ticks (**8** bytes, not 4), a title length (1) and the title.
///
/// Both of those were only settled against these bytes. The start time being eight bytes
/// wide is invisible in the first chapter, which is zero, and the ticks are the *movie*
/// timescale rather than a fixed one — ffmpeg emits 100 000 000 for ten seconds here,
/// which no fixed-rate assumption explains. Transcribing the fixture by hand dropped the
/// width, and the box then declared 41 bytes while carrying 25; generating the array from
/// the captured hex is why it cannot drift again.
const FFMPEG_CHPL: &[u8] = &[
    0x00, 0x00, 0x00, b')', b'c', b'h', b'p', b'l', 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, b'O', b'n', b'e', 0x00, 0x00, 0x00,
    0x00, 0x05, 0xF5, 0xE1, 0x00, 0x03, b'T', b'w', b'o',
];

#[test]
fn ffmpegs_chpl_box_parses_to_the_chapters_it_describes() {
    let data = minimal_mp4_with(FFMPEG_CHPL);
    let file = Mp4File::parse(&data).expect("a real ffmpeg file parses");
    let chapters = &file.chpl_chapters.chapters;
    assert_eq!(chapters.len(), 2, "both chapters: {chapters:?}");
    assert_eq!(chapters[0].title, "One");
    assert_eq!(chapters[0].start_ms, 0);
    assert_eq!(chapters[1].title, "Two");
    assert_eq!(chapters[1].start_ms, 10_000, "at a 10_000 timescale");
}

#[test]
fn our_writer_is_byte_identical_to_ffmpeg() {
    let chapters = ChapterList {
        chapters: vec![Chapter::new(0, "One"), Chapter::new(10_000, "Two")],
    };
    let ours = chapters.to_chpl_box().to_bytes();
    assert_eq!(
        ours.len(),
        FFMPEG_CHPL.len(),
        "same length: ours {} bytes, ffmpeg {}",
        ours.len(),
        FFMPEG_CHPL.len()
    );
    assert_eq!(&ours, FFMPEG_CHPL, "byte for byte");
}

#[test]
fn a_round_trip_through_our_own_writer_still_parses() {
    let chapters = ChapterList {
        chapters: vec![Chapter::new(0, "One"), Chapter::new(10_000, "Two")],
    };
    let bytes = chapters.to_chpl_box().to_bytes();
    let data = minimal_mp4_with(&bytes);
    let file = Mp4File::parse(&data).expect("parses");
    assert_eq!(file.chpl_chapters.chapters.len(), 2);
    assert_eq!(file.chpl_chapters.chapters[1].title, "Two");
    assert_eq!(file.chpl_chapters.chapters[1].start_ms, 10_000);
}

/// Wrap a box in the smallest MP4 that `Mp4File::parse` accepts.
///
/// The parser needs a `moov` to anchor the box tree, and `find_chpl` walks the tree, so a
/// bare box on its own is not enough to exercise the read path.
fn minimal_mp4_with(box_bytes: &[u8]) -> Vec<u8> {
    let chpl = mp4_core::parse_boxes(box_bytes)
        .expect("a well-formed box")
        .remove(0);
    let moov = mp4_core::container(*b"moov", vec![chpl]);
    let moov_bytes = moov.to_bytes();

    // `ftyp`: size (4) + type (4) + major brand (4) + minor version (4) + brands (12)
    // = 28. The debug assert below is what catches the arithmetic, which is why it is
    // there rather than being trusted.
    //
    // Written by hand with the size asserted rather than through `Box_`, because the size
    // field has to match the bytes actually emitted. A mismatch reads as a truncated box
    // and the error names whatever follows, which is how this first appeared as a
    // `TruncatedBox` for `isom`: the parser had walked into the brand bytes.
    let ftyp_len: u32 = 28;
    let mut out = Vec::with_capacity(ftyp_len as usize + moov_bytes.len());
    out.extend_from_slice(&ftyp_len.to_be_bytes());
    out.extend_from_slice(b"ftyp");
    out.extend_from_slice(b"M4A "); // major brand
    out.extend_from_slice(&[0x00, 0x00, 0x02, 0x00]); // minor version
    out.extend_from_slice(b"M4A mp42isom"); // compatible brands
    debug_assert_eq!(
        out.len(),
        ftyp_len as usize,
        "size must match the bytes written"
    );
    out.extend_from_slice(&moov_bytes);
    out
}
