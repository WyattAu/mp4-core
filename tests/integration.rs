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
fn text_trak(track_id: u32) -> mp4_core::Box_ {
    let mut tkhd_payload = vec![0u8, 0, 0, 3];
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

    assert_eq!(file.chapters.chapters.len(), 3);
    assert_eq!(file.chapters.chapters[1].title, "Chapter One");
    assert_eq!(file.chapters.chapters[1].start_ms, 900_000);
    assert!(file.is_audiobook());
}

#[test]
fn a_chapter_track_marks_a_file_as_an_audiobook() {
    // No stik, no M4B brand, no chpl — only a text track. That is still an
    // audiobook as far as a player is concerned.
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
    assert_eq!(file.tracks.len(), 2);
    assert!(file.chapter_track().is_some());
    assert!(file.is_audiobook(), "a text track alone marks an audiobook");
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
fn a_chpl_box_with_a_wrong_version_is_rejected() {
    let mut payload = vec![0u8, 0, 0, 2]; // version 2, not 1
    payload.extend_from_slice(&[0u8; 4]);
    payload.extend_from_slice(&0u32.to_be_bytes());
    assert!(matches!(
        ChapterList::parse_chpl(&payload),
        Err(mp4_core::Mp4Error::MalformedChapters { .. })
    ));
}

#[test]
fn a_chpl_box_with_a_nonzero_reserved_field_is_rejected() {
    let mut payload = vec![0u8, 0, 0, 1];
    payload.extend_from_slice(&[0u8, 0, 0, 1]); // reserved must be zero
    payload.extend_from_slice(&0u32.to_be_bytes());
    assert!(matches!(
        ChapterList::parse_chpl(&payload),
        Err(mp4_core::Mp4Error::MalformedChapters { .. })
    ));
}

#[test]
fn a_chpl_title_running_past_the_box_is_rejected() {
    let mut payload = vec![0u8, 0, 0, 1];
    payload.extend_from_slice(&[0u8; 4]);
    payload.extend_from_slice(&1u32.to_be_bytes()); // one entry
    payload.extend_from_slice(&0u64.to_be_bytes()); // timestamp
    payload.push(200); // claims a 200-byte title
    payload.extend_from_slice(b"short"); // only 5 bytes follow
    assert!(matches!(
        ChapterList::parse_chpl(&payload),
        Err(mp4_core::Mp4Error::MalformedChapters { .. })
    ));
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
