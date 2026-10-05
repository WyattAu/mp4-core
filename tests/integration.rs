#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

//! Integration: the shapes a real M4B has.
//!
//! The unit tests inside each module prove each box parses alone. These prove
//! the combinations hold — a file with metadata and chapters, a fragmented file,
//! the three size encodings, and the malformed inputs a real library meets.

use mp4_core::chapter::{Chapter, ChapterList};
use mp4_core::container::{container, leaf, parse_boxes, Size, SizeForm};
use mp4_core::metadata::{atoms, DataType, Metadata, MetadataValue};
use mp4_core::moov::{FileType, HandlerType, MovieHeader, Mp4File};
use std::vec;
use std::vec::Vec;

/// A minimal but structurally valid `moov` with one audio track.
fn audio_moov(timescale: u32, duration_ms: u64) -> Vec<u8> {
    audio_moov_with(timescale, timescale, duration_ms)
}

/// A `moov` whose movie and track timescales differ — the case that catches a
/// reader conflating them.
fn audio_moov_with(movie_ts: u32, track_ts: u32, duration_ms: u64) -> Vec<u8> {
    let mvhd = leaf(*b"mvhd", MovieHeader::to_payload(movie_ts, duration_ms))
        .expect("a leaf is infallible");
    let trak = audio_trak(1, track_ts, duration_ms);
    container(*b"moov", vec![mvhd, trak]).to_bytes()
}

/// An `M4B`-shaped file carrying only a `chpl` chapter list.
fn file_with_chpl(chapters: &[(&str, u64)]) -> Vec<u8> {
    let list = mp4_core::ChapterList {
        chapters: chapters
            .iter()
            .map(|(t, ms)| mp4_core::Chapter::new(*ms, t))
            .collect(),
    };
    let mvhd = leaf(*b"mvhd", MovieHeader::to_payload(1000, 10_000)).expect("infallible");
    let moov = container(
        *b"moov",
        vec![mvhd, audio_trak(1, 44_100, 10_000), list.to_chpl_box()],
    );
    let mut data = leaf(*b"ftyp", FileType::audiobook().to_payload())
        .expect("infallible")
        .to_bytes();
    data.extend_from_slice(&moov.to_bytes());
    data.extend_from_slice(
        &leaf(*b"mdat", vec![0u8; 32])
            .expect("infallible")
            .to_bytes(),
    );
    data
}

/// A `trak` whose `tkhd` enabled flag can be toggled.
///
/// Apple's chapter-list rule turns on which track carries the `tref`, so the tests
/// need both states of the same track.
trait ToggleEnabled {
    fn with_enabled(self, enabled: bool) -> Self;
}

impl ToggleEnabled for mp4_core::Box_ {
    fn with_enabled(mut self, enabled: bool) -> Self {
        if let Some(tkhd) = self.child(*b"tkhd") {
            let mut payload = tkhd.payload.clone();
            // Byte 0 is the version; flags occupy bytes 1..4, so bit 0 of the
            // flags word is the low bit of byte **3**. Clearing byte 2 would look
            // plausible and change nothing.
            if let Some(byte) = payload.get_mut(3) {
                *byte = if enabled { *byte | 0b1 } else { *byte & 0xFE };
            }
            let rebuilt = leaf(*b"tkhd", payload).expect("infallible");
            if let Some(slot) = self.children.iter_mut().find(|c| c.box_type == *b"tkhd") {
                *slot = rebuilt;
            }
        }
        self
    }
}

/// One audio `trak`: `tkhd`, `mdia`/`mdhd`, `mdia`/`hdlr`, `mdia`/`minf`/`stbl`.
fn audio_trak(track_id: u32, timescale: u32, duration_ms: u64) -> mp4_core::Box_ {
    let mut tkhd_payload = vec![0u8, 0, 0, 3]; // version 0, flags: enabled|in-movie
    tkhd_payload.extend_from_slice(&0u32.to_be_bytes()); // creation
    tkhd_payload.extend_from_slice(&0u32.to_be_bytes()); // modification
    tkhd_payload.extend_from_slice(&track_id.to_be_bytes());
    tkhd_payload.extend_from_slice(&0u32.to_be_bytes()); // reserved
    tkhd_payload.extend_from_slice(
        &u32::try_from(duration_ms * u64::from(timescale) / 1000)
            .expect("fits")
            .to_be_bytes(),
    );
    tkhd_payload.extend_from_slice(&[0u8; 8]); // reserved
    let tkhd = leaf(*b"tkhd", tkhd_payload).expect("infallible");

    let mdhd = leaf(
        *b"mdhd",
        mp4_core::moov::MediaHeader::to_payload(timescale, duration_ms),
    )
    .expect("infallible");

    let mut hdlr_payload = vec![0u8, 0, 0, 0];
    hdlr_payload.extend_from_slice(&[0u8, 0, 0, 0]); // pre_defined
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

/// A text `trak`, which is how the QuickTime chapter form stores titles.
///
/// **Disabled**, per Apple: the chapter track "does not need to be enabled, and
/// normally is not". The *referencing* media track is the one whose flag matters.
fn text_trak(track_id: u32) -> mp4_core::Box_ {
    let mut tkhd_payload = vec![0u8, 0, 0, 0]; // flags 0: not enabled
    tkhd_payload.extend_from_slice(&0u32.to_be_bytes());
    tkhd_payload.extend_from_slice(&0u32.to_be_bytes());
    tkhd_payload.extend_from_slice(&track_id.to_be_bytes());
    tkhd_payload.extend_from_slice(&0u32.to_be_bytes());
    tkhd_payload.extend_from_slice(&0u32.to_be_bytes());
    tkhd_payload.extend_from_slice(&[0u8; 8]);
    let tkhd = leaf(*b"tkhd", tkhd_payload).expect("infallible");

    let mdhd = leaf(
        *b"mdhd",
        mp4_core::moov::MediaHeader::to_payload(1000, 60_000),
    )
    .expect("infallible");
    let mut hdlr_payload = vec![0u8, 0, 0, 0];
    hdlr_payload.extend_from_slice(&[0u8, 0, 0, 0]);
    hdlr_payload.extend_from_slice(b"text");
    hdlr_payload.extend_from_slice(&[0u8; 12]);
    let hdlr = leaf(*b"hdlr", hdlr_payload).expect("infallible");

    let minf = container(*b"minf", vec![container(*b"stbl", vec![])]);
    let mdia = container(*b"mdia", vec![mdhd, hdlr, minf]);
    container(*b"trak", vec![tkhd, mdia])
}

/// An audio track carrying a `tref`/`chap` reference to `target`.
///
/// The reference belongs on the **referencing** track, not on the chapter track:
/// Apple puts it on the enabled media track, pointing at the text track. That
/// direction is easy to get backwards, and the first version of this test did.
fn audio_trak_referencing(target: u32, timescale: u32, duration_ms: u64) -> mp4_core::Box_ {
    let mut track = audio_trak(1, timescale, duration_ms);
    track.children.push(container(
        *b"tref",
        vec![leaf(*b"chap", target.to_be_bytes().to_vec()).expect("infallible")],
    ));
    track
}

#[test]
fn a_minimal_file_parses_with_the_expected_shape() {
    let mut data = leaf(*b"ftyp", FileType::audiobook().to_payload())
        .expect("infallible")
        .to_bytes();
    data.extend_from_slice(&audio_moov(44_100, 3_600_000));
    data.extend_from_slice(
        &leaf(*b"mdat", vec![0u8; 64])
            .expect("infallible")
            .to_bytes(),
    );

    let file = Mp4File::parse(&data).expect("parses");
    assert!(file.file_type.is_some());
    assert!(file.file_type.as_ref().expect("ftyp").claims_audiobook());
    assert_eq!(file.duration_ms(), Some(3_600_000));
    assert_eq!(file.tracks.len(), 1);
    assert_eq!(
        file.audio_track().map(|t| t.handler),
        Some(HandlerType::Audio)
    );
    assert_eq!(file.audio_track().expect("audio").media.timescale, 44_100);
    assert!(file.audio_track().expect("audio").header.enabled);
}

#[test]
fn movie_and_track_timescales_are_kept_distinct() {
    // The classic MP4 reader bug: mvhd says 1000 and the track says 44100, so a
    // duration read from one and applied at the other is off by 44x. Both
    // conversions must use their own timescale.
    let mut data = leaf(*b"ftyp", FileType::audiobook().to_payload())
        .expect("infallible")
        .to_bytes();
    // Both boxes are handed the same *millisecond* duration, so they encode
    // different unit counts. A reader that divided the track's units by the
    // movie's timescale would be off by 44.1x.
    data.extend_from_slice(&audio_moov_with(1_000, 44_100, 60_000));

    let file = Mp4File::parse(&data).expect("parses");
    let movie = file.movie.expect("mvhd");
    assert_eq!(movie.timescale, 1_000);
    assert_eq!(
        movie.duration_ms, 60_000,
        "mvhd holds 60_000 units at 1000/s = 60 s"
    );

    let track = file.audio_track().expect("audio");
    assert_eq!(track.media.timescale, 44_100);
    assert_eq!(
        track.media.duration_ms, 60_000,
        "mdhd holds 2_646_000 units at 44100/s = 60 s; reading those units at the \
         movie's 1000/s timescale would wrongly give 2_646_000 ms"
    );
    // The unit *counts* differ by 2646x while the durations agree — which is
    // exactly the property a conflated reader loses.
    assert_eq!(movie.duration_units, 60_000);
    assert_eq!(track.media.duration_units, 2_646_000);
    assert_ne!(movie.duration_units, track.media.duration_units);
    assert_eq!(movie.duration_ms, track.media.duration_ms);
}

#[test]
fn a_file_with_metadata_and_chapters_round_trips() {
    let mut metadata = Metadata::default();
    metadata.atoms.push(mp4_core::metadata::Atom {
        name: atoms::TITLE,
        values: vec![MetadataValue::text("The Long Now")],
    });
    metadata.atoms.push(mp4_core::metadata::Atom {
        name: atoms::ARTIST,
        values: vec![MetadataValue::text("A. Writer")],
    });
    metadata.atoms.push(mp4_core::metadata::Atom {
        name: atoms::MEDIA_KIND,
        values: vec![MetadataValue {
            data_type: DataType::UnsignedInteger,
            payload: 9u32.to_be_bytes().to_vec(),
            locale: 0,
        }],
    });
    metadata.atoms.push(mp4_core::metadata::Atom {
        name: atoms::TRACK,
        // trkn packs two big-endian u16s after a zero word.
        values: vec![MetadataValue {
            data_type: DataType::Binary,
            payload: vec![0, 0, 0, 3, 0, 12],
            locale: 0,
        }],
    });

    let chapters = ChapterList {
        chapters: vec![
            Chapter::new(0, "Opening"),
            Chapter::new(900_000, "Chapter One"),
            Chapter::new(1_800_000, "Chapter Two"),
        ],
    };

    // Assemble: ftyp | moov(mvhd, trak, udta, chpl) | mdat
    let mvhd = leaf(*b"mvhd", MovieHeader::to_payload(1000, 3_600_000)).expect("infallible");
    let udta = metadata.to_udta();
    let moov = container(
        *b"moov",
        vec![
            mvhd,
            audio_trak(1, 44_100, 3_600_000),
            udta,
            chapters.to_chpl_box(),
        ],
    );

    let mut data = leaf(*b"ftyp", FileType::audiobook().to_payload())
        .expect("infallible")
        .to_bytes();
    data.extend_from_slice(&moov.to_bytes());
    data.extend_from_slice(
        &leaf(*b"mdat", vec![0u8; 32])
            .expect("infallible")
            .to_bytes(),
    );

    let file = Mp4File::parse(&data).expect("parses");
    assert_eq!(file.metadata.text(atoms::TITLE), Some("The Long Now"));
    assert_eq!(file.metadata.text(atoms::ARTIST), Some("A. Writer"));
    assert!(file.metadata.is_audiobook(), "stik=9 marks an audiobook");
    assert_eq!(file.metadata.track_number(), Some(3));
    assert_eq!(file.metadata.track_total(), Some(12));

    assert_eq!(file.effective_chapters().chapters.len(), 3);
    assert_eq!(file.effective_chapters().chapters[1].title, "Chapter One");
    assert_eq!(file.effective_chapters().chapters[1].start_ms, 900_000);
    assert!(file.is_audiobook());
}

#[test]
fn a_referenced_chapter_track_marks_a_file_as_an_audiobook() {
    // No stik, no M4B brand, no chpl — only a QuickTime chapter track. That is
    // still an audiobook as far as a player is concerned.
    let mvhd = leaf(*b"mvhd", MovieHeader::to_payload(1000, 60_000)).expect("infallible");
    let moov = container(
        *b"moov",
        vec![
            mvhd,
            audio_trak_referencing(2, 44_100, 60_000),
            text_trak(2),
        ],
    );
    let mut data = leaf(*b"ftyp", FileType::audiobook().to_payload())
        .expect("infallible")
        .to_bytes();
    data.extend_from_slice(&moov.to_bytes());

    let file = Mp4File::parse(&data).expect("parses");
    assert_eq!(file.tracks.len(), 2);
    assert_eq!(file.chapter_track_id, Some(2));
    assert_eq!(
        file.chapter_track().map(|t| t.header.track_id),
        Some(2),
        "the referenced track is the chapter track"
    );
    assert!(file.is_audiobook(), "a chapter track marks an audiobook");
}

#[test]
fn a_subtitle_track_is_not_a_chapter_track() {
    // The same file without the `chap` reference: a text track, but not chapters.
    // This test previously asserted the opposite, which is the bug — a reader that
    // treats any text track as chapters puts a subtitle list in the chapter menu.
    let mvhd = leaf(*b"mvhd", MovieHeader::to_payload(1000, 60_000)).expect("infallible");
    let moov = container(
        *b"moov",
        vec![mvhd, audio_trak(1, 44_100, 60_000), text_trak(2)],
    );
    let mut data = leaf(*b"ftyp", FileType::audiobook().to_payload())
        .expect("infallible")
        .to_bytes();
    data.extend_from_slice(&moov.to_bytes());

    let file = Mp4File::parse(&data).expect("parses");
    assert_eq!(file.tracks.len(), 2, "the text track is still parsed");
    assert_eq!(
        file.chapter_track_id, None,
        "but nothing says it carries chapters"
    );
    assert!(
        file.chapter_track().is_none(),
        "a subtitle track is not a chapter track"
    );
}

#[test]
fn the_two_chapter_mechanisms_are_independent() {
    // Real files carry `chpl`, a QuickTime chapter track, or both — and a report on
    // M4B chapters in the wild notes some files have "only one or the other, some
    // have both". So neither mechanism may be inferred from the other's presence.
    let with_chpl_only = file_with_chpl(&[("A", 0u64), ("B", 1_000)]);
    let file = Mp4File::parse(&with_chpl_only).expect("parses");
    assert_eq!(file.effective_chapters().chapters.len(), 2);
    assert_eq!(file.chapter_track_id, None, "no text track at all");
    assert!(file.is_audiobook(), "`chpl` alone is enough");

    let neither = file_with_chpl(&[]);
    let file = Mp4File::parse(&neither).expect("parses");
    assert!(file.effective_chapters().chapters.is_empty());
    assert_eq!(file.chapter_track_id, None);
    // Still an audiobook by brand alone — which is why `is_audiobook` ORs its
    // signals rather than chaining them. A file can carry `M4B ` with no chapters,
    // and reporting "not an audiobook" would be as wrong as the reverse.
    assert!(file.is_audiobook(), "the M4B brand alone still marks it");
}

#[test]
fn an_empty_chapter_title_is_legal() {
    // A chapter with no name still marks a boundary; a library must not drop it.
    let list = ChapterList {
        chapters: vec![Chapter::new(0, ""), Chapter::new(1_000, "Named")],
    };
    let payload = list.to_chpl_payload();
    let back = ChapterList::parse_chpl(&payload).expect("reparses");
    assert_eq!(back.chapters.len(), 2);
    assert_eq!(back.chapters[0].title, "");
    assert_eq!(back.chapters[1].title, "Named");
}

#[test]
fn a_chapter_lookup_covers_the_gap_between_marks() {
    let list = ChapterList {
        chapters: vec![
            Chapter::new(0, "First"),
            Chapter::new(1_000, "Second"),
            Chapter::new(2_000, "Third"),
        ],
    };
    assert_eq!(list.chapter_at(0).map(|c| c.title.as_str()), Some("First"));
    assert_eq!(
        list.chapter_at(999).map(|c| c.title.as_str()),
        Some("First")
    );
    assert_eq!(
        list.chapter_at(1_500).map(|c| c.title.as_str()),
        Some("Second")
    );
    assert_eq!(list.span_ms(), Some(2_000));
}

#[test]
fn the_three_size_encodings_all_round_trip() {
    let payload = vec![1u8, 2, 3, 4];

    // 32-bit.
    let normal = leaf(*b"free", payload.clone())
        .expect("infallible")
        .to_bytes();
    assert_eq!(normal.len(), 12);
    let parsed = parse_boxes(&normal).expect("parses");
    assert_eq!(parsed[0].size_form, SizeForm::Normal);
    assert_eq!(parsed[0].payload, payload);

    // 64-bit largesize: header is size=1, then the real size.
    let mut large = vec![0u8, 0, 0, 1];
    large.extend_from_slice(b"free");
    large.extend_from_slice(&20u64.to_be_bytes());
    large.extend_from_slice(&payload);
    let parsed = parse_boxes(&large).expect("parses");
    assert_eq!(parsed[0].size_form, SizeForm::Large);
    assert_eq!(parsed[0].size, Size::Fixed(20));
    assert_eq!(parsed[0].payload, payload);

    // size == 0: runs to the end of the container.
    let mut to_end = vec![0u8, 0, 0, 0];
    to_end.extend_from_slice(b"free");
    to_end.extend_from_slice(&payload);
    let parsed = parse_boxes(&to_end).expect("parses");
    assert_eq!(parsed[0].size_form, SizeForm::ToEnd);
    assert_eq!(parsed[0].payload, payload);
}

#[test]
fn a_truncated_box_is_an_error_not_a_panic() {
    // A box claiming 1000 bytes with only 12 present.
    let mut bad = vec![0u8, 0, 3, 0xE8];
    bad.extend_from_slice(b"moov");
    bad.extend_from_slice(&[0u8; 4]);
    assert!(matches!(
        parse_boxes(&bad),
        Err(mp4_core::Mp4Error::TruncatedBox { .. })
    ));
}

#[test]
fn a_box_claiming_less_than_its_header_is_rejected() {
    let mut bad = vec![0u8, 0, 0, 4]; // size 4, below the 8-byte header
    bad.extend_from_slice(b"free");
    assert!(matches!(
        parse_boxes(&bad),
        Err(mp4_core::Mp4Error::InvalidBoxSize { .. })
    ));
}

#[test]
fn padding_ends_the_box_list_without_erroring() {
    let mut data = leaf(*b"free", vec![9u8]).expect("infallible").to_bytes();
    data.extend_from_slice(&[0u8; 16]); // trailing padding
    let parsed = parse_boxes(&data).expect("parses");
    assert_eq!(parsed.len(), 1, "padding must not become boxes");
}

#[test]
fn a_meta_box_without_a_full_box_header_is_still_read() {
    // Apple's writer omitted meta's version/flags for years. A meta whose
    // payload starts directly with `ilst` must parse.
    let metadata = Metadata {
        atoms: vec![mp4_core::metadata::Atom {
            name: atoms::TITLE,
            values: vec![MetadataValue::text("Headerless")],
        }],
    };
    let ilst = metadata.to_ilst();
    let headerless = leaf(*b"meta", ilst.to_bytes()).expect("infallible");

    let read = mp4_core::metadata::parse_meta(&headerless).expect("parses");
    assert_eq!(read.text(atoms::TITLE), Some("Headerless"));
}

#[test]
fn an_unknown_box_is_preserved_not_rejected() {
    // A file written by a newer muxer carries boxes this crate has never heard
    // of. They must survive a read so the media data is not lost.
    let mut data = leaf(*b"ftyp", FileType::audiobook().to_payload())
        .expect("infallible")
        .to_bytes();
    data.extend_from_slice(
        &leaf(*b"pict", vec![1, 2, 3])
            .expect("infallible")
            .to_bytes(),
    );
    data.extend_from_slice(&leaf(*b"moov", vec![0u8; 4]).expect("infallible").to_bytes());

    let parsed = parse_boxes(&data).expect("parses");
    assert_eq!(parsed.len(), 3);
    assert_eq!(parsed[1].box_type, *b"pict");
    assert_eq!(parsed[1].payload, vec![1, 2, 3]);
}

#[test]
fn a_conformant_stsd_entry_yields_the_sample_rate() {
    // `stsd` is a full box *and* carries an entry count: version(1) flags(3)
    // entry_count(4) before the entries. A descent that skips only four bytes
    // reads the entry count as a box size, loses every sample entry, and reports
    // no sample rate at all — so the header length has to be eight.
    let entry: Vec<u8> = [
        vec![0u8; 6],  // reserved
        vec![0u8, 1],  // data_reference_index
        vec![0u8; 8],  // version + revision + vendor
        vec![0u8, 2],  // channel count: stereo
        vec![0u8, 16], // sample size
        vec![0u8, 0],  // pre_defined
        vec![0u8, 0],  // reserved
        // The spec declares this field as `samplerate << 16`, so 44 100 Hz is
        // 0xAC44_0000 — an integer part in the high half, not 44 100 itself.
        (44_100u32 << 16).to_be_bytes().to_vec(),
    ]
    .concat();
    let mp4a = leaf(*b"mp4a", entry.clone()).expect("infallible");

    // The conformant stsd: a full-box header then the entry.
    let mut stsd_payload = vec![0u8, 0, 0, 0]; // version + flags
    stsd_payload.extend_from_slice(&1u32.to_be_bytes()); // entry_count
    stsd_payload.extend_from_slice(&mp4a.to_bytes());
    // One track, built by hand, so the stsd under test is the only one present.
    let mut tkhd_payload = vec![0u8, 0, 0, 3];
    tkhd_payload.extend_from_slice(&0u32.to_be_bytes());
    tkhd_payload.extend_from_slice(&0u32.to_be_bytes());
    tkhd_payload.extend_from_slice(&1u32.to_be_bytes());
    tkhd_payload.extend_from_slice(&0u32.to_be_bytes());
    tkhd_payload.extend_from_slice(&1_000u32.to_be_bytes());
    tkhd_payload.extend_from_slice(&[0u8; 8]);
    let tkhd = leaf(*b"tkhd", tkhd_payload).expect("infallible");
    let mdhd = leaf(
        *b"mdhd",
        mp4_core::moov::MediaHeader::to_payload(44_100, 1_000),
    )
    .expect("infallible");
    let mut hdlr_payload = vec![0u8, 0, 0, 0];
    hdlr_payload.extend_from_slice(&[0u8, 0, 0, 0]);
    hdlr_payload.extend_from_slice(b"soun");
    hdlr_payload.extend_from_slice(&[0u8; 12]);
    let hdlr = leaf(*b"hdlr", hdlr_payload).expect("infallible");

    let stbl = container(
        *b"stbl",
        vec![leaf(*b"stsd", stsd_payload).expect("infallible")],
    );
    let minf = container(*b"minf", vec![stbl]);
    let mdia = container(*b"mdia", vec![mdhd, hdlr, minf]);
    let trak = container(*b"trak", vec![tkhd, mdia]);
    let moov = container(
        *b"moov",
        vec![
            leaf(*b"mvhd", MovieHeader::to_payload(1_000, 1_000)).expect("infallible"),
            trak,
        ],
    );

    let mut data = leaf(*b"ftyp", FileType::audiobook().to_payload())
        .expect("infallible")
        .to_bytes();
    data.extend_from_slice(&moov.to_bytes());

    // The entry is reachable at all.
    let boxes = parse_boxes(&data).expect("parses");
    let moov_b = boxes.iter().find(|b| b.box_type == *b"moov").expect("moov");
    let stsd = moov_b
        .children_of(*b"trak")
        .into_iter()
        .flat_map(|t| t.children_of(*b"mdia"))
        .flat_map(|m| m.children_of(*b"minf"))
        .filter_map(|m| m.child(*b"stbl"))
        .filter_map(|s| s.child(*b"stsd"))
        .next()
        .expect("an stsd");
    assert_eq!(
        stsd.children.len(),
        1,
        "the entry count must not be read as a box size"
    );
    assert_eq!(stsd.children[0].payload, entry);

    // And the rate reads back through the public accessor.
    assert_eq!(
        mp4_core::moov::read_sample_rate(&data),
        Some(44_100),
        "the integer part of the 16.16 word, not the whole word read as a fraction"
    );

    // Storing the rate unshifted is the mistake a hand-written muxer makes, and it
    // must read as "no rate" rather than as 44 100 by accident.
    let mut wrong = entry.clone();
    wrong[24..28].copy_from_slice(&44_100u32.to_be_bytes());
    assert_eq!(mp4_core::fields::audio_sample_rate(44_100), 0);
}

/// The chapter-track sample tables exactly as ffmpeg writes them, captured from a
/// real `book.m4b`.
///
/// One chunk at file offset 44 (the first byte of `mdat`), three samples of 25, 25 and
/// 27 bytes, and one `stts` entry declaring all three as 2 646 000 ticks at a
/// 44 100 Hz timescale — 60 s each.
fn ffmpeg_chapter_stbl() -> mp4_core::Box_ {
    let mut stts = vec![0u8, 0, 0, 0];
    stts.extend_from_slice(&1u32.to_be_bytes()); // entry count
    stts.extend_from_slice(&3u32.to_be_bytes()); // covering 3 samples
    stts.extend_from_slice(&2_646_000u32.to_be_bytes()); // 60 s at 44.1 kHz

    let mut stsz = vec![0u8, 0, 0, 0];
    stsz.extend_from_slice(&0u32.to_be_bytes()); // not uniform
    stsz.extend_from_slice(&3u32.to_be_bytes());
    stsz.extend_from_slice(&25u32.to_be_bytes());
    stsz.extend_from_slice(&25u32.to_be_bytes());
    stsz.extend_from_slice(&27u32.to_be_bytes());

    let mut stsc = vec![0u8, 0, 0, 0];
    stsc.extend_from_slice(&1u32.to_be_bytes());
    stsc.extend_from_slice(&1u32.to_be_bytes()); // first chunk
    stsc.extend_from_slice(&3u32.to_be_bytes()); // samples per chunk
    stsc.extend_from_slice(&1u32.to_be_bytes()); // description index

    let mut stco = vec![0u8, 0, 0, 0];
    stco.extend_from_slice(&1u32.to_be_bytes());
    stco.extend_from_slice(&44u32.to_be_bytes()); // the first byte of mdat

    container(
        *b"stbl",
        vec![
            leaf(*b"stts", stts).expect("infallible"),
            leaf(*b"stsz", stsz).expect("infallible"),
            leaf(*b"stsc", stsc).expect("infallible"),
            leaf(*b"stco", stco).expect("infallible"),
        ],
    )
}

/// A `text` track carrying that sample table, with the 44.1 kHz timescale ffmpeg uses.
fn ffmpeg_chapter_trak() -> mp4_core::Box_ {
    let mut tkhd_payload = vec![0u8, 0, 0, 0]; // flags 0: a chapter track is disabled
    tkhd_payload.extend_from_slice(&0u32.to_be_bytes());
    tkhd_payload.extend_from_slice(&0u32.to_be_bytes());
    tkhd_payload.extend_from_slice(&2u32.to_be_bytes()); // track id 2
    tkhd_payload.extend_from_slice(&0u32.to_be_bytes());
    tkhd_payload.extend_from_slice(&7_935_952u32.to_be_bytes()); // duration
    tkhd_payload.extend_from_slice(&[0u8; 8]);
    let tkhd = leaf(*b"tkhd", tkhd_payload).expect("infallible");

    let mdhd = leaf(
        *b"mdhd",
        mp4_core::moov::MediaHeader::to_payload(44_100, 180_000),
    )
    .expect("infallible");

    let mut hdlr = vec![0u8, 0, 0, 0];
    hdlr.extend_from_slice(&[0u8, 0, 0, 0]);
    hdlr.extend_from_slice(b"text");
    hdlr.extend_from_slice(&[0u8; 12]);
    let hdlr = leaf(*b"hdlr", hdlr).expect("infallible");

    // `gmhd`, which is what a QuickTime text track uses. TN2174's chapter marker track
    // uses `nmhd`; both are accepted because the handler type is what distinguishes
    // the track and nothing else depends on the difference.
    let gmhd = leaf(
        *b"gmhd",
        [
            vec![0u8, 0, 0, 0],
            vec![0u8, 0, 0, 0],
            vec![0u8, 0, 0, 0, 0, 0, 0, 0x12, 0, 0, 0, 0],
        ]
        .concat(),
    )
    .expect("infallible");
    let stsd = {
        let mut p = vec![0u8, 0, 0, 0];
        p.extend_from_slice(&1u32.to_be_bytes());
        p.extend_from_slice(&[0u8; 6]); // reserved
        p.extend_from_slice(&[0u8, 1]); // data reference index
        p.extend_from_slice(&[0u8; 6]); // display flags, justification, bg colour
        p.extend_from_slice(&[0u8; 6]); // box, border, style
        p.extend_from_slice(&[0u8; 8]); // font number, font face, reserved
        p.extend_from_slice(&[0u8; 1]); // foreground colour count
        p.extend_from_slice(&[0u8; 3]); // foreground colour
        let entry = leaf(*b"text", p).expect("infallible");
        leaf(*b"stsd", {
            let mut q = vec![0u8, 0, 0, 0];
            q.extend_from_slice(&1u32.to_be_bytes());
            q.extend_from_slice(&entry.to_bytes());
            q
        })
        .expect("infallible")
    };

    let minf = container(*b"minf", vec![gmhd, stsd, ffmpeg_chapter_stbl()]);
    let mdia = container(*b"mdia", vec![mdhd, hdlr, minf]);
    container(*b"trak", vec![tkhd, mdia])
}

/// One QuickTime text sample: a 2-byte length prefix, the text with no null
/// terminator, then a trailing modifier box.
///
/// The modifier is not decoration — it is what makes the real samples 25, 25 and 27
/// bytes rather than 13, 13 and 15, and `stsz` records those larger sizes. A decoder
/// that appended everything after the prefix would return
/// `"Chapter One\u{0}\u{0}\u{0}\u{c}encd..."`, so its presence here is the test.
fn qt_text_sample(title: &str) -> Vec<u8> {
    let mut out = (title.len() as u16).to_be_bytes().to_vec();
    out.extend_from_slice(title.as_bytes());
    // A 12-byte `encd` modifier box: 4-byte size, 4-byte type, 4-byte payload.
    out.extend_from_slice(&12u32.to_be_bytes());
    out.extend_from_slice(b"encd");
    out.extend_from_slice(&[0u8; 4]);
    out
}

/// The three chapter samples ffmpeg writes, at the sizes `stsz` declares.
fn ffmpeg_chapter_samples() -> Vec<u8> {
    let mut out = Vec::new();
    for title in ["Chapter One", "Chapter Two", "Chapter Three"] {
        out.extend_from_slice(&qt_text_sample(title));
    }
    out
}

/// A file laid out like a real one: `ftyp`, a `free` box, then the sample data at
/// offset 44, with `moov` last.
///
/// The chapter track's `stco` says offset 44, so the samples must genuinely *be* at
/// 44 in whatever file the track is embedded in — hence assembling the whole file
/// rather than appending a `trak` to a buffer that already had headers in front of it.
fn ffmpeg_assembled(children: &[mp4_core::Box_]) -> Vec<u8> {
    use std::vec::Vec as StdVec;
    let mut file = leaf(*b"ftyp", mp4_core::FileType::audiobook().to_payload())
        .expect("infallible")
        .to_bytes();
    // A bare 8-byte `free` box, then an `mdat` whose header spans 36..44 so that its
    // payload — the chapter samples — starts at exactly 44. This is the real layout:
    // writing the samples bare would make the top-level box walker read their length
    // prefix as a box size, which is a fixture bug that looks exactly like a parser bug.
    file.extend_from_slice(&8u32.to_be_bytes());
    file.extend_from_slice(b"free");
    assert_eq!(file.len(), 36, "the mdat header must span 36..44");

    let samples = ffmpeg_chapter_samples();
    file.extend_from_slice(&((samples.len() + 8) as u32).to_be_bytes());
    file.extend_from_slice(b"mdat");
    assert_eq!(file.len(), 44, "the sample data must start where stco says");
    file.extend_from_slice(&samples);

    let mut moov_children: StdVec<mp4_core::Box_> =
        vec![leaf(*b"mvhd", MovieHeader::to_payload(1_000, 180_000)).expect("infallible")];
    moov_children.extend_from_slice(children);
    file.extend_from_slice(&container(*b"moov", moov_children).to_bytes());
    file
}

/// Just the sample data at offset 44, for the tests that call the reader directly.
fn ffmpeg_chapter_file() -> Vec<u8> {
    let mut file = vec![0u8; 44];
    file.extend_from_slice(&ffmpeg_chapter_samples());
    file
}

#[test]
fn a_quicktime_chapter_track_reads_titles_and_times() {
    // The whole point: a chapter track's titles need four boxes to recover — `stts` for
    // when each sample appears, `stsz` for how long it is, and `stco` with `stsc` for
    // where it lives. A reader that confuses chunk offsets with sample offsets reads
    // *garbage* rather than failing, which is why `stsc` is consulted at all.
    let trak = ffmpeg_chapter_trak();
    let file = ffmpeg_chapter_file();
    let list = mp4_core::chapter::read_text_track_chapters(&trak, &file).expect("reads");

    assert_eq!(list.chapters.len(), 3);
    assert_eq!(list.chapters[0].start_ms, 0);
    assert_eq!(list.chapters[0].title, "Chapter One");
    assert_eq!(list.chapters[1].start_ms, 60_000, "from stts, not a guess");
    assert_eq!(list.chapters[1].title, "Chapter Two");
    assert_eq!(list.chapters[2].start_ms, 120_000);
    assert_eq!(list.chapters[2].title, "Chapter Three");
}

#[test]
fn the_length_prefix_does_not_reach_the_title() {
    // Without stripping it, every title begins with two NULs.
    let trak = ffmpeg_chapter_trak();
    let file = ffmpeg_chapter_file();
    for chapter in mp4_core::chapter::read_text_track_chapters(&trak, &file)
        .expect("reads")
        .chapters
    {
        assert!(
            !chapter.title.starts_with('\u{0}'),
            "`{}` carries the length prefix",
            chapter.title
        );
    }
}

#[test]
fn titles_only_needs_the_same_boxes() {
    let trak = ffmpeg_chapter_trak();
    let file = ffmpeg_chapter_file();
    let titles = mp4_core::chapter::read_text_track_titles(&trak, &file).expect("reads");
    assert_eq!(titles, vec!["Chapter One", "Chapter Two", "Chapter Three"]);
}

#[test]
fn a_missing_sample_table_is_an_error_rather_than_no_chapters() {
    // "Absent" and "present but unreadable" are different problems, and a caller has
    // to be able to tell them apart.
    let mdia = container(*b"mdia", vec![]);
    let trak = container(*b"trak", vec![container(*b"mdia", vec![])]);
    let _ = mdia;
    assert!(matches!(
        mp4_core::chapter::read_text_track_chapters(&trak, &[0u8; 64]),
        Err(mp4_core::Mp4Error::MissingBox { .. })
    ));
}

#[test]
fn a_sample_pointing_outside_the_file_is_an_error() {
    let trak = ffmpeg_chapter_trak();
    // A file too short to hold the samples.
    let short = vec![0u8; 50];
    assert!(matches!(
        mp4_core::chapter::read_text_track_chapters(&trak, &short),
        Err(mp4_core::Mp4Error::TruncatedBox { .. })
    ));
}

#[test]
fn the_track_is_preferred_over_chpl_because_apple_players_ignore_chpl() {
    // QuickTime, iTunes and the built-in iOS audiobook player read the chapter track and
    // ignore Nero chapters entirely. So when a file carries both, the track is the list
    // a player shows.
    let data = ffmpeg_assembled(&[
        audio_trak_referencing(2, 44_100, 180_000),
        ffmpeg_chapter_trak(),
        // A `chpl` that deliberately disagrees, to prove the preference is real.
        mp4_core::ChapterList {
            chapters: vec![mp4_core::Chapter::new(0, "Nero Only")],
        }
        .to_chpl_box(),
    ]);

    let file = Mp4File::parse(&data).expect("parses");
    assert_eq!(file.chapter_track_id, Some(2));
    assert_eq!(file.chpl_chapters.chapters.len(), 1);
    assert_eq!(
        file.effective_chapters().chapters[0].title,
        "Chapter One",
        "the track wins"
    );
    assert!(
        file.chapter_disagreement().is_some(),
        "and the disagreement is reportable rather than hidden"
    );
}

#[test]
fn agreeing_mechanisms_report_no_disagreement() {
    let data = ffmpeg_assembled(&[
        audio_trak_referencing(2, 44_100, 180_000),
        ffmpeg_chapter_trak(),
        mp4_core::ChapterList {
            chapters: vec![
                mp4_core::Chapter::new(0, "Chapter One"),
                mp4_core::Chapter::new(60_000, "Chapter Two"),
                mp4_core::Chapter::new(120_000, "Chapter Three"),
            ],
        }
        .to_chpl_box(),
    ]);

    let file = Mp4File::parse(&data).expect("parses");
    assert_eq!(file.chapter_track_chapters.chapters.len(), 3);
    assert!(
        file.chapter_disagreement().is_none(),
        "both mechanisms carrying the same list is not a finding"
    );
}

#[test]
fn with_only_chpl_the_effective_list_falls_back_to_it() {
    let data = file_with_chpl(&[("One", 0u64), ("Two", 60_000)]);
    let file = Mp4File::parse(&data).expect("parses");
    assert_eq!(file.chapter_track_id, None);
    assert_eq!(file.effective_chapters().chapters.len(), 2);
    assert!(
        file.chapter_disagreement().is_none(),
        "nothing to disagree about"
    );
}

#[test]
fn a_chapter_sample_length_prefix_is_stripped() {
    // Apple's QuickTime File Format and TN2174 both specify the sample as a 16-bit
    // big-endian length followed by the text, with no null terminator. Reading the
    // whole sample as text yields a title starting with two NULs, which renders as
    // "..Chapter 1" in every UI — a corruption that is easy to miss because the
    // name is still recognisable.
    let mut sample = 9u16.to_be_bytes().to_vec();
    sample.extend_from_slice(b"Chapter 1");
    assert_eq!(
        mp4_core::chapter::decode_text_sample(&sample),
        "Chapter 1",
        "the length prefix must not reach the caller"
    );

    // A longer title, where the prefix genuinely differs from the sample length.
    let title = "The Inimitable Aardvark";
    let mut sample = (title.len() as u16).to_be_bytes().to_vec();
    sample.extend_from_slice(title.as_bytes());
    assert_eq!(mp4_core::chapter::decode_text_sample(&sample), title);
}

#[test]
fn a_bare_text_sample_without_a_prefix_still_reads() {
    // Some encoders write the text with no length prefix. Trusting the first two
    // bytes blindly would turn the title's own first character into a length, so
    // an inconsistent prefix is ignored rather than obeyed.
    assert_eq!(
        mp4_core::chapter::decode_text_sample(b"Chapter 1"),
        "Chapter 1",
        "a prefix claiming more than the sample holds is not a prefix"
    );
    // A prefix that claims *less* than the sample holds is honoured, since the
    // rest would be padding.
    let mut sample = 4u16.to_be_bytes().to_vec();
    sample.extend_from_slice(b"Open   ");
    assert_eq!(mp4_core::chapter::decode_text_sample(&sample), "Open");
}

#[test]
fn a_chapter_sample_in_utf16_is_decoded_with_its_prefix() {
    let title = "Chapitre";
    let mut text = vec![0xFFu8, 0xFE]; // little-endian BOM
    for unit in title.encode_utf16() {
        text.extend_from_slice(&unit.to_le_bytes());
    }
    let mut sample = (text.len() as u16).to_be_bytes().to_vec();
    sample.extend_from_slice(&text);

    let decoded = mp4_core::chapter::decode_text_sample(&sample);
    assert_eq!(decoded, title);
    assert!(!decoded.starts_with('\u{0}'), "no stray prefix characters");
}

#[test]
fn a_text_track_is_only_chapters_when_a_chap_reference_says_so() {
    // A QuickTime chapter list *is* a text track, plus a `chap` track reference
    // from an *enabled* track. Without the reference the text track is subtitles,
    // lyrics, or captions — and a reader that treats it as chapters puts a
    // subtitle list in the chapter menu.
    let moov = container(*b"moov", vec![text_trak(2)]);
    assert!(
        !mp4_core::chapter::is_chapter_list(&moov.children[0], &moov),
        "an unreferenced text track is not a chapter list"
    );

    // With an enabled audio track referencing it, it is. Note the chapter track
    // itself is disabled, per Apple.
    let moov = container(
        *b"moov",
        vec![audio_trak_referencing(2, 44_100, 1_000), text_trak(2)],
    );
    assert!(
        mp4_core::chapter::is_chapter_list(&moov.children[1], &moov),
        "a `chap` reference from an enabled track makes it a chapter list"
    );

    // A reference naming a track that does not exist: no.
    let moov = container(
        *b"moov",
        vec![audio_trak_referencing(99, 44_100, 1_000), text_trak(2)],
    );
    assert!(
        !mp4_core::chapter::is_chapter_list(&moov.children[1], &moov),
        "a reference to a different track id does not count"
    );
}

#[test]
fn a_chap_reference_from_a_disabled_track_is_not_honoured() {
    // The asymmetry that is easy to get backwards: the *referencing* track must be
    // enabled, not the chapter track. A disabled media track's reference is one a
    // player ignores.
    let moov = container(
        *b"moov",
        vec![
            audio_trak_referencing(2, 44_100, 1_000).with_enabled(false),
            text_trak(2),
        ],
    );
    assert!(
        !mp4_core::chapter::is_chapter_list(&moov.children[1], &moov),
        "a disabled track's reference is not honoured"
    );
}

#[test]
fn a_bare_chap_under_udta_also_counts() {
    // Some muxers put the reference under `udta` rather than inside a `tref`.
    let udta = container(
        *b"udta",
        vec![leaf(*b"chap", 2u32.to_be_bytes().to_vec()).expect("infallible")],
    );
    let moov = container(
        *b"moov",
        vec![audio_trak(1, 44_100, 1_000), text_trak(2), udta],
    );
    assert!(mp4_core::chapter::is_chapter_list(&moov.children[1], &moov));
}

#[test]
fn a_track_without_a_tkhd_is_not_a_chapter_list() {
    // No id, so nothing can reference it.
    let moov = container(*b"moov", vec![container(*b"trak", vec![])]);
    assert!(!mp4_core::chapter::is_chapter_list(
        &moov.children[0],
        &moov
    ));
}

/// A `chpl` payload exactly as ffmpeg writes it, captured from a real file.
///
/// This is ground truth, not a fixture invented to match the code: the bytes came
/// out of `ffmpeg -i src.wav -i chapters.ffmeta -c:a aac book.m4b`, and they are what
/// TagLib documents. Building a fixture by hand from the Adobe FLV specification
/// instead is how this crate came to read a `u32` count at offset 8 and fail on every
/// real file.
const FFMPEG_CHPL: &[u8] = &[
    0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x0b, b'C', b'h', b'a', b'p', b't', b'e', b'r', 0x20, b'O', b'n', b'e', 0x00, 0x00, 0x00,
    0x00, 0x23, 0xc3, b'F', 0x00, 0x0b, b'C', b'h', b'a', b'p', b't', b'e', b'r', 0x20, b'T', b'w',
    b'o', 0x00, 0x00, 0x00, 0x00, b'G', 0x86, 0x8c, 0x00, 0x0d, b'C', b'h', b'a', b'p', b't', b'e',
    b'r', 0x20, b'T', b'h', b'r', b'e', b'e',
];

#[test]
fn a_real_ffmpeg_chpl_parses() {
    // The bug this pins: the count is a `u8` at offset 8, and version 1 puts four
    // reserved bytes before it. A reader built from the Adobe FLV specification — which
    // describes a `u32` count — reads 0x03000000 as the count, fails to walk the
    // entries, and reports every real audiobook as having no chapters.
    let list = ChapterList::parse_chpl(FFMPEG_CHPL).expect("real ffmpeg output parses");
    assert_eq!(list.chapters.len(), 3);
    assert_eq!(list.chapters[0].start_ms, 0);
    assert_eq!(list.chapters[0].title, "Chapter One");
    assert_eq!(list.chapters[1].start_ms, 60_000);
    assert_eq!(list.chapters[1].title, "Chapter Two");
    assert_eq!(list.chapters[2].start_ms, 120_000);
    assert_eq!(list.chapters[2].title, "Chapter Three");
}

#[test]
fn the_ffmpeg_payload_and_our_writer_agree() {
    // Round-tripping through our own writer must produce the same bytes ffmpeg
    // produces, or we are writing a file real players may not read.
    let list = ChapterList::parse_chpl(FFMPEG_CHPL).expect("parses");
    let written = list.to_chpl_payload();
    assert_eq!(
        written, FFMPEG_CHPL,
        "our writer must produce byte-identical output to ffmpeg"
    );
    let bytes = list.to_chpl_box().to_bytes();
    assert_eq!(
        ChapterList::parse_chpl(&bytes[8..])
            .expect("reparses")
            .chapters,
        list.chapters
    );
}

#[test]
fn a_version_zero_chpl_has_no_reserved_bytes() {
    // Version 0's header is five bytes, not nine — the reserved field is absent, so a
    // reader that always skips four bytes reads the count from the wrong place.
    let mut payload: Vec<u8> = vec![0u8, 0, 0, 0]; // version 0, flags 0
    payload.push(1); // count at offset 4 — no reserved field before it
    payload.extend_from_slice(&0u64.to_be_bytes()); // start 0
    payload.push(5);
    payload.extend_from_slice(b"First");

    let list = ChapterList::parse_chpl(&payload).expect("version 0 parses");
    assert_eq!(list.chapters.len(), 1);
    assert_eq!(list.chapters[0].title, "First");
}

#[test]
fn a_specification_shaped_count_yields_an_empty_list_not_a_wrong_one() {
    // The Adobe FLV layout: a four-byte big-endian count of 3, so the bytes are
    // `00 00 00 03` where the real format puts `03 00 00 00`.
    //
    // Read by the real rules the count byte is `0x00`, so the list is empty — and an
    // empty list is *valid*. This test exists to pin that failure mode rather than
    // pretend it is detectable: a specification-following writer produces a box that
    // reads as "no chapters" with nothing to distinguish it from a book that truly has
    // none. That is precisely why this crate reads the layout ffmpeg and TagLib use
    // instead of the one the specification describes.
    let mut payload: Vec<u8> = vec![1u8, 0, 0, 0];
    payload.extend_from_slice(&[0u8; 4]);
    payload.extend_from_slice(&3u32.to_be_bytes());
    payload.extend_from_slice(&[0u8; 30]); // the chapters the count would have described

    let list = ChapterList::parse_chpl(&payload).expect("a zero count is not malformed");
    assert!(
        list.chapters.is_empty(),
        "read as empty rather than as three nonsense chapters"
    );
}

#[test]
fn a_count_larger_than_the_box_is_rejected_not_truncated() {
    // Clamping would silently drop chapters, which is the worst outcome available.
    let mut payload: Vec<u8> = vec![1u8, 0, 0, 0];
    payload.extend_from_slice(&[0u8; 4]);
    payload.push(200); // claims 200 chapters
    payload.extend_from_slice(&0u64.to_be_bytes());
    payload.push(1);
    payload.extend_from_slice(b"One");
    assert!(matches!(
        ChapterList::parse_chpl(&payload),
        Err(mp4_core::Mp4Error::MalformedChapters { .. })
    ));
}

#[test]
fn a_nonzero_reserved_field_is_rejected_for_version_one() {
    let mut payload: Vec<u8> = vec![1u8, 0, 0, 0];
    payload.extend_from_slice(&[0u8, 0, 0, 1]); // reserved must be zero
    payload.push(0); // count 0 — no entries, so only the reserved field is at issue
    assert!(matches!(
        ChapterList::parse_chpl(&payload),
        Err(mp4_core::Mp4Error::MalformedChapters { .. })
    ));
}

#[test]
fn a_title_running_past_the_box_is_rejected() {
    let mut payload: Vec<u8> = vec![1u8, 0, 0, 0];
    payload.extend_from_slice(&[0u8; 4]);
    payload.push(1); // one chapter
    payload.extend_from_slice(&0u64.to_be_bytes());
    payload.push(200); // claims a 200-byte title
    payload.extend_from_slice(b"short"); // only 5 bytes follow
    assert!(matches!(
        ChapterList::parse_chpl(&payload),
        Err(mp4_core::Mp4Error::MalformedChapters { .. })
    ));
}

#[test]
fn a_chpl_version_newer_than_known_is_rejected() {
    let mut payload: Vec<u8> = vec![2u8, 0, 0, 0];
    payload.extend_from_slice(&[0u8; 4]);
    payload.push(0);
    assert!(matches!(
        ChapterList::parse_chpl(&payload),
        Err(mp4_core::Mp4Error::MalformedChapters { .. })
    ));
}

#[test]
fn a_truncated_header_is_rejected_rather_than_read() {
    assert!(ChapterList::parse_chpl(&[]).is_err());
    assert!(ChapterList::parse_chpl(&[1, 0, 0]).is_err());
    // Version 1 with no room for its reserved field.
    assert!(ChapterList::parse_chpl(&[1, 0, 0, 0, 0, 0]).is_err());
}

#[test]
fn an_empty_chapter_list_is_legal() {
    let mut payload: Vec<u8> = vec![1u8, 0, 0, 0];
    payload.extend_from_slice(&[0u8; 4]);
    payload.push(0);
    let list = ChapterList::parse_chpl(&payload).expect("a zero count is valid");
    assert!(list.chapters.is_empty());
}

#[test]
fn the_255_chapter_ceiling_is_reported_rather_than_silently_applied() {
    use mp4_core::{MAX_CHPL_CHAPTERS, MAX_CHPL_TITLE_BYTES};

    let many: Vec<Chapter> = (0..MAX_CHPL_CHAPTERS)
        .map(|i| Chapter::new(i as u64 * 1_000, "x"))
        .collect();
    let ok = ChapterList {
        chapters: many.clone(),
    }
    .to_chpl_box_checked()
    .expect("255 chapters fit");
    assert!(ok.box_type == *b"chpl");

    // One more than the count field can express.
    let mut too_many = many;
    too_many.push(Chapter::new(999_000, "x"));
    assert!(matches!(
        ChapterList { chapters: too_many }.to_chpl_box_checked(),
        Err(mp4_core::Mp4Error::MalformedChapters { .. })
    ));

    // A title longer than the length field can express.
    let long = "a".repeat(MAX_CHPL_TITLE_BYTES + 1);
    assert!(matches!(
        ChapterList {
            chapters: vec![Chapter::new(0, &long)]
        }
        .to_chpl_box_checked(),
        Err(mp4_core::Mp4Error::MalformedChapters { .. })
    ));
}

#[test]
fn a_title_at_the_length_ceiling_is_kept_whole() {
    use mp4_core::MAX_CHPL_TITLE_BYTES;
    let title = "b".repeat(MAX_CHPL_TITLE_BYTES);
    let box_ = ChapterList {
        chapters: vec![Chapter::new(0, &title)],
    }
    .to_chpl_box_checked()
    .expect("a title at the ceiling fits");
    let back = ChapterList::parse_chpl(&box_.payload).expect("reparses");
    assert_eq!(back.chapters[0].title, title, "not truncated");
}

#[test]
fn a_metadata_value_is_only_text_when_its_flag_says_so() {
    // A JPEG's bytes read as UTF-8 would be nonsense; the type flag is what
    // decides, and asking for the wrong interpretation returns None.
    let jpeg = MetadataValue::image(vec![0xFF, 0xD8, 0xFF, 0xE0]);
    assert_eq!(jpeg.data_type, DataType::Jpeg);
    assert_eq!(jpeg.as_text(), None, "image bytes must not read as text");
    assert_eq!(jpeg.as_image(), Some(&[0xFF, 0xD8, 0xFF, 0xE0][..]));

    let text = MetadataValue::text("plain");
    assert_eq!(text.as_text(), Some("plain"));
    assert_eq!(text.as_image(), None);
}

#[test]
fn png_and_jpeg_art_get_distinct_type_flags() {
    let png = MetadataValue::image(vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    assert_eq!(png.data_type, DataType::Png, "PNG magic must set flag 14");
    let jpeg = MetadataValue::image(vec![0xFF, 0xD8, 0xFF]);
    assert_eq!(
        jpeg.data_type,
        DataType::Jpeg,
        "JPEG magic must set flag 13"
    );
}
