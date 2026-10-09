//! The windowed reader must agree with the whole-file reader, field for field.
//!
//! These are the gates that make [`mp4_core::io`] worth having. A box walker that found
//! the wrong `moov`, or a sample read at a shifted offset, would produce a *plausible*
//! answer — a duration a few seconds out, a chapter title missing — so agreement with
//! [`mp4_core::Mp4File::parse`] is the only check that means anything. The two paths are
//! compared on every field the summary has, and the volume of I/O is asserted too,
//! because an unbounded "windowed" reader would pass every field check while being the
//! thing it exists to not be.

use mp4_core::chapter::{Chapter, ChapterList};
use mp4_core::container::{container, leaf, Box_};
use mp4_core::io::{read_box, read_top_level, ByteReader, TopLevelBox};
use mp4_core::metadata::{atoms, Metadata};
use mp4_core::metadata::{Atom, MetadataValue};
use mp4_core::moov::{FileType, MovieHeader};
use mp4_core::Mp4File;

/// An M4B with metadata, a `chpl` list, and a large `mdat` between them.
fn sample_file() -> Vec<u8> {
    let mut metadata = Metadata::default();
    metadata.atoms.push(Atom {
        name: atoms::TITLE,
        values: vec![MetadataValue::text("Windowed")],
    });
    metadata.atoms.push(Atom {
        name: atoms::ARTIST,
        values: vec![MetadataValue::text("A. Reader")],
    });
    let chapters = ChapterList {
        chapters: vec![
            Chapter::new(0, "Opening"),
            Chapter::new(900_000, "Chapter One"),
        ],
    };

    let mvhd = leaf(*b"mvhd", MovieHeader::to_payload(1000, 3_600_000)).expect("infallible");
    let moov = container(
        *b"moov",
        vec![
            mvhd,
            audio_trak(1, 44_100, 3_600_000),
            metadata.to_udta(),
            chapters.to_chpl_box(),
        ],
    );

    // `ftyp | mdat | moov` — the shape ffmpeg writes, with the metadata at the *end*,
    // which is why a head-only window cannot reach it.
    let mut data = leaf(*b"ftyp", FileType::audiobook().to_payload())
        .expect("infallible")
        .to_bytes();
    data.extend_from_slice(
        &leaf(*b"mdat", vec![0u8; 4_000_000])
            .expect("infallible")
            .to_bytes(),
    );
    data.extend_from_slice(&moov.to_bytes());
    data
}

/// An audio track, the same shape `tests/integration.rs` builds: `tkhd` + `mdia` with
/// an `soun` handler.
fn audio_trak(track_id: u32, timescale: u32, duration_ms: u64) -> Box_ {
    let mut tkhd_payload = vec![0u8, 0, 0, 3]; // version 0, flags: enabled|in-movie
    tkhd_payload.extend_from_slice(&0u32.to_be_bytes());
    tkhd_payload.extend_from_slice(&0u32.to_be_bytes());
    tkhd_payload.extend_from_slice(&track_id.to_be_bytes());
    tkhd_payload.extend_from_slice(&0u32.to_be_bytes());
    tkhd_payload.extend_from_slice(
        &u32::try_from(duration_ms * u64::from(timescale) / 1000)
            .expect("fits")
            .to_be_bytes(),
    );
    tkhd_payload.extend_from_slice(&[0u8; 8]);
    let tkhd = leaf(*b"tkhd", tkhd_payload).expect("infallible");

    let mdhd = leaf(
        *b"mdhd",
        mp4_core::moov::MediaHeader::to_payload(timescale, duration_ms),
    )
    .expect("infallible");

    let mut hdlr_payload = vec![0u8, 0, 0, 0];
    hdlr_payload.extend_from_slice(&[0u8, 0, 0, 0]);
    hdlr_payload.extend_from_slice(b"soun");
    hdlr_payload.extend_from_slice(&[0u8; 12]);
    let hdlr = leaf(*b"hdlr", hdlr_payload).expect("infallible");

    let stbl = container(
        *b"stbl",
        vec![leaf(*b"stsd", vec![0u8, 0, 0, 0, 0, 0, 0, 1]).expect("infallible")],
    );
    let minf = container(*b"minf", vec![stbl]);
    let mdia = container(*b"mdia", vec![mdhd, hdlr, minf]);

    container(*b"trak", vec![tkhd, mdia])
}

/// A reader that counts bytes, so a test can assert the *volume* of I/O.
struct Counting {
    data: Vec<u8>,
    read: usize,
}

impl Counting {
    fn new(data: Vec<u8>) -> Self {
        Self { data, read: 0 }
    }
}

impl ByteReader for Counting {
    fn byte_len(&self) -> Option<u64> {
        Some(u64::try_from(self.data.len()).unwrap_or(u64::MAX))
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Option<usize> {
        self.read += buf.len();
        let mut data: &[u8] = &self.data;
        data.read_at(offset, buf)
    }
}

fn find_moov(top: &[TopLevelBox]) -> TopLevelBox {
    *top.iter()
        .find(|b| b.box_type == *b"moov")
        .expect("a moov at top level")
}

#[test]
fn the_windowed_summary_agrees_with_the_whole_file_summary() {
    let data = sample_file();

    let whole = Mp4File::parse(&data).expect("parses");

    let total = u64::try_from(data.len()).unwrap_or(0);
    let mut reader = Counting::new(data.clone());
    let top = read_top_level(&mut reader, total).expect("walks");
    let moov_box = find_moov(&top);
    let moov_bytes = read_box(&mut reader, &moov_box).expect("reads");
    let parsed = mp4_core::container::parse_boxes(&moov_bytes).expect("moov parses");
    let moov = parsed.first().expect("one moov box");
    let ftyp_payload: Option<&[u8]> = None;
    let windowed = Mp4File::parse_from(ftyp_payload, Some(moov), &mut reader);

    assert_eq!(
        whole.duration_ms(),
        windowed.duration_ms(),
        "the duration is the field a library shows"
    );
    assert_eq!(
        whole.metadata.text(atoms::TITLE),
        windowed.metadata.text(atoms::TITLE)
    );
    assert_eq!(
        whole.metadata.text(atoms::ARTIST),
        windowed.metadata.text(atoms::ARTIST)
    );
    assert_eq!(
        whole.chpl_chapters.chapters, windowed.chpl_chapters.chapters,
        "the chpl list lives inside the moov and must be found there"
    );
    assert_eq!(
        whole.chapter_track_chapters.chapters, windowed.chapter_track_chapters.chapters,
        "the chapter track is read from mdat through the reader, not from the moov"
    );
    assert_eq!(whole.chapter_track_id, windowed.chapter_track_id);
    assert_eq!(whole.tracks.len(), windowed.tracks.len());
}

#[test]
fn reading_a_four_megabyte_file_to_its_moov_costs_kilobytes() {
    let data = sample_file();
    let total = u64::try_from(data.len()).unwrap_or(0);
    let mut reader = Counting::new(data);
    let top = read_top_level(&mut reader, total).expect("walks");
    let moov_box = find_moov(&top);
    let _ = read_box(&mut reader, &moov_box).expect("reads");

    // Two headers (16 bytes each) to find the moov, then the moov itself.
    assert!(
        reader.read < 8_000,
        "read {} bytes of a {} byte file",
        reader.read,
        total
    );
}

#[test]
fn a_file_whose_moov_is_last_is_found_at_the_end_not_scanned_for() {
    // The shape ffmpeg writes, and the reason a head-only window is not enough.
    let data = sample_file();
    let total = u64::try_from(data.len()).unwrap_or(0);
    let mut reader: &[u8] = &data;
    let top = read_top_level(&mut reader, total).expect("walks");
    let moov_box = find_moov(&top);
    assert!(
        moov_box.offset > total / 2,
        "the moov sits at {} of {} bytes, after the mdat",
        moov_box.offset,
        total
    );
}
