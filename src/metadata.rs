//! iTunes-style metadata (`udta` → `meta` → `ilst`).
//!
//! The format Apple established and everyone adopted: a `meta` box holding an
//! `ilst` of atoms, each named with a leading `©` for the standard fields
//! (`©nam` title, `©ART` artist, `©alb` album, `covr` artwork), each
//! containing one or more `data` boxes that carry a type flag and the payload.
//!
//! # The `data` type flag
//!
//! The interesting subtlety is that the same atom means different things by its
//! type flag: flag 1 is UTF-8 text, 0 is binary, 13 is JPEG, 14 is PNG, 21 is a
//! signed integer, 22 is an unsigned one. Getting this wrong yields a title
//! that reads as raw bytes, so the flag is modelled explicitly rather than
//! guessed from the payload.
//!
//! # The 4-byte version quirk
//!
//! `meta` is a *full box* and so begins with a version and flags — but Apple's
//! writer omitted them for years, and files in the wild carry a `meta` whose
//! payload starts directly with `hdlr`. Both are accepted here, detected by
//! whether the first four bytes look like a plausible version/flags pair.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::container::{container, leaf, parse_boxes, BoxType, Box_};
use crate::error::Mp4Error;
use crate::fields::{u32_at, version};

/// The `data` box's type flag: what the payload actually is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DataType {
    /// Reserved / binary, uninterpreted.
    Binary,
    /// UTF-8 text.
    Utf8,
    /// A UTF-16 string.
    Utf16,
    /// A JPEG image.
    Jpeg,
    /// A PNG image.
    Png,
    /// A big-endian signed integer.
    SignedInteger,
    /// A big-endian unsigned integer.
    UnsignedInteger,
    /// A type this crate does not model.
    Unknown(u32),
}

impl DataType {
    /// The wire flag.
    #[must_use]
    pub fn as_flag(self) -> u32 {
        match self {
            DataType::Binary => 0,
            DataType::Utf8 => 1,
            DataType::Utf16 => 2,
            DataType::Jpeg => 13,
            DataType::Png => 14,
            DataType::SignedInteger => 21,
            DataType::UnsignedInteger => 22,
            DataType::Unknown(f) => f,
        }
    }

    /// Decode a wire flag.
    #[must_use]
    pub fn from_flag(flag: u32) -> DataType {
        match flag {
            0 => DataType::Binary,
            1 => DataType::Utf8,
            2 => DataType::Utf16,
            13 => DataType::Jpeg,
            14 => DataType::Png,
            21 => DataType::SignedInteger,
            22 => DataType::UnsignedInteger,
            other => DataType::Unknown(other),
        }
    }

    /// Whether this type carries image bytes.
    #[must_use]
    pub fn is_image(self) -> bool {
        matches!(self, DataType::Jpeg | DataType::Png)
    }
}

/// One value inside a metadata atom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataValue {
    /// What the payload is.
    pub data_type: DataType,
    /// The raw payload, interpreted according to `data_type`.
    pub payload: Vec<u8>,
    /// The locale, when the atom carried one (`data` flag 0 with a language
    /// prefix). Empty for the common case.
    pub locale: u32,
}

impl MetadataValue {
    /// The payload as UTF-8 text, if that is what it is.
    ///
    /// Returns `None` for a non-text value rather than lossy-converting: a JPEG
    /// read as a string is a bug in the caller, not a decoding to paper over.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self.data_type {
            DataType::Utf8 => core::str::from_utf8(&self.payload).ok(),
            _ => None,
        }
    }

    /// The payload as image bytes, if that is what it is.
    #[must_use]
    pub fn as_image(&self) -> Option<&[u8]> {
        if self.data_type.is_image() {
            Some(&self.payload)
        } else {
            None
        }
    }

    /// The payload as an unsigned integer, if that is what it is.
    #[must_use]
    pub fn as_u32(&self) -> Option<u32> {
        match self.data_type {
            DataType::UnsignedInteger => self
                .payload
                .get(..4)
                .and_then(|s| <[u8; 4]>::try_from(s).ok())
                .map(u32::from_be_bytes),
            _ => None,
        }
    }

    /// Build a UTF-8 text value.
    #[must_use]
    pub fn text(value: &str) -> Self {
        Self {
            data_type: DataType::Utf8,
            payload: value.as_bytes().to_vec(),
            locale: 0,
        }
    }

    /// Build an image value, picking the flag from the leading bytes.
    #[must_use]
    pub fn image(data: Vec<u8>) -> Self {
        let data_type = if data.starts_with(&[0x89, b'P', b'N', b'G']) {
            DataType::Png
        } else {
            DataType::Jpeg
        };
        Self {
            data_type,
            payload: data,
            locale: 0,
        }
    }
}

/// A metadata atom: a name and its values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Atom {
    /// The atom's four-character name, e.g. `[0xA9, b'n', b'a', b'm']` for
    /// `©nam`.
    pub name: BoxType,
    /// Every `data` box under it, in order.
    pub values: Vec<MetadataValue>,
}

impl Atom {
    /// The first UTF-8 value, if any.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        self.values.iter().find_map(MetadataValue::as_text)
    }

    /// The first image value, if any.
    #[must_use]
    pub fn image(&self) -> Option<&[u8]> {
        self.values.iter().find_map(MetadataValue::as_image)
    }
}

/// The well-known atom names this crate names, with their copyright prefix
/// spelled out.
pub mod atoms {
    use crate::container::BoxType;

    /// `©nam` — title.
    pub const TITLE: BoxType = [0xA9, b'n', b'a', b'm'];
    /// `©ART` — artist.
    pub const ARTIST: BoxType = [0xA9, b'A', b'R', b'T'];
    /// `©alb` — album.
    pub const ALBUM: BoxType = [0xA9, b'a', b'l', b'b'];
    /// `©day` — year.
    pub const YEAR: BoxType = [0xA9, b'd', b'a', b'y'];
    /// `©gen` — genre.
    pub const GENRE: BoxType = [0xA9, b'g', b'e', b'n'];
    /// `©cmt` — comment.
    pub const COMMENT: BoxType = [0xA9, b'c', b'm', b't'];
    /// `©wrt` — composer.
    pub const COMPOSER: BoxType = [0xA9, b'w', b'r', b't'];
    /// `©too` — encoder.
    pub const ENCODER: BoxType = [0xA9, b't', b'o', b'o'];
    /// `trkn` — track number and total.
    pub const TRACK: BoxType = *b"trkn";
    /// `disk` — disc number and total.
    pub const DISC: BoxType = *b"disk";
    /// `covr` — cover art. No copyright prefix, and it carries image bytes.
    pub const COVER: BoxType = *b"covr";
    /// `cpil` — compilation flag.
    pub const COMPILATION: BoxType = *b"cpil";
    /// `pgap` — part of a gapless album.
    pub const GAPLESS: BoxType = *b"pgap";
    /// `stik` — media kind. 9 is "audiobook", which is what an M4B carries.
    pub const MEDIA_KIND: BoxType = *b"stik";
    /// `desc` — description, used for the audiobook's synopsis.
    pub const DESCRIPTION: BoxType = *b"desc";
}

/// The iTunes media kind for an audiobook.
///
/// `stik` = 9 is what marks a file as an audiobook rather than music, and it is
/// what a library keys on to decide whether to show chapter navigation.
pub const MEDIA_KIND_AUDIOBOOK: u32 = 9;

/// A parsed `ilst`: atom name to values.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Metadata {
    /// Every atom, in file order.
    pub atoms: Vec<Atom>,
}

impl Metadata {
    /// The first atom with the given name.
    #[must_use]
    pub fn atom(&self, name: BoxType) -> Option<&Atom> {
        self.atoms.iter().find(|a| a.name == name)
    }

    /// The first UTF-8 value of the named atom.
    #[must_use]
    pub fn text(&self, name: BoxType) -> Option<&str> {
        self.atom(name).and_then(Atom::text)
    }

    /// The cover art bytes, if present.
    #[must_use]
    pub fn cover(&self) -> Option<&[u8]> {
        self.atom(atoms::COVER).and_then(Atom::image)
    }

    /// The track number, from the `trkn` atom's 16-bit big-endian pair.
    ///
    /// `trkn` packs two big-endian `u16`s with a leading zero word: track in
    /// bytes 2..4 and total in bytes 4..6.
    #[must_use]
    pub fn track_number(&self) -> Option<u16> {
        let payload = self.atom(atoms::TRACK)?.values.first()?.payload.as_slice();
        let pair = payload
            .get(2..4)
            .and_then(|s| <[u8; 2]>::try_from(s).ok())?;
        let hi = u16::from_be_bytes(pair);
        if hi == 0 {
            None
        } else {
            Some(hi)
        }
    }

    /// The total track count, from the `trkn` atom.
    #[must_use]
    pub fn track_total(&self) -> Option<u16> {
        let payload = self.atom(atoms::TRACK)?.values.first()?.payload.as_slice();
        let pair = payload
            .get(4..6)
            .and_then(|s| <[u8; 2]>::try_from(s).ok())?;
        let total = u16::from_be_bytes(pair);
        if total == 0 {
            None
        } else {
            Some(total)
        }
    }

    /// Whether this file is marked as an audiobook (`stik` = 9).
    #[must_use]
    pub fn is_audiobook(&self) -> bool {
        self.atom(atoms::MEDIA_KIND)
            .and_then(|a| a.values.first())
            .and_then(MetadataValue::as_u32)
            .is_some_and(|k| k == MEDIA_KIND_AUDIOBOOK)
    }

    /// Locate an `ilst` anywhere in the tree and parse it.
    ///
    /// The usual path is `moov` → `udta` → `meta` → `ilst`, but a file may
    /// carry metadata at the file level too, so this searches rather than
    /// assuming a path.
    #[must_use]
    pub fn find_in(data: &[u8]) -> Option<Metadata> {
        let boxes = parse_boxes(data).ok()?;
        let mut found: Option<&Box_> = None;
        for b in &boxes {
            if b.box_type == *b"meta" {
                if let Some(il) = b.child(*b"ilst") {
                    found = Some(il);
                    break;
                }
            }
        }
        found.map(Metadata::from_ilst)
    }

    /// Parse an `ilst` box into atoms.
    #[must_use]
    pub fn from_ilst(ilst: &Box_) -> Metadata {
        let mut atoms = Vec::new();
        for child in &ilst.children {
            let mut values = Vec::new();
            for data in child.children_of(*b"data") {
                if let Some(v) = MetadataValue::parse(data) {
                    values.push(v);
                }
            }
            atoms.push(Atom {
                name: child.box_type,
                values,
            });
        }
        Metadata { atoms }
    }

    /// Build an `ilst` box from these atoms.
    #[must_use]
    pub fn to_ilst(&self) -> Box_ {
        let mut children = Vec::with_capacity(self.atoms.len());
        for atom in &self.atoms {
            let mut data_boxes = Vec::with_capacity(atom.values.len());
            for value in &atom.values {
                data_boxes.push(
                    leaf(*b"data", value.to_payload())
                        .unwrap_or_else(|_| crate::container::empty(*b"data")),
                );
            }
            children.push(container(atom.name, data_boxes));
        }
        container(*b"ilst", children)
    }

    /// Build the enclosing `udta` → `meta` → `ilst` chain.
    ///
    /// `meta` is a full box, so it carries a version and flags — but Apple's
    /// writer omitted them for years and readers accept both, so this emits the
    /// spec-correct form with the four-byte header.
    #[must_use]
    pub fn to_udta(&self) -> Box_ {
        // `meta` is a full box *and* a container: its payload is version+flags
        // followed by child boxes. Building it as an opaque leaf would make it
        // unreadable, because a parser cannot descend into bytes it treats as
        // data — which is exactly the bug this shape had.
        let mut hdlr_payload: Vec<u8> = vec![0u8, 0, 0, 0]; // version + flags
        hdlr_payload.extend_from_slice(&[0u8, 0, 0, 0]); // pre_defined
        hdlr_payload.extend_from_slice(b"mdirappl"); // handler_type
        hdlr_payload.extend(core::iter::repeat_n(0u8, 9)); // reserved
        let hdlr =
            leaf(*b"hdlr", hdlr_payload).unwrap_or_else(|_| crate::container::empty(*b"hdlr"));

        let meta = crate::container::full_box_container_box(*b"meta", vec![hdlr, self.to_ilst()]);
        container(*b"udta", vec![meta])
    }
}

impl MetadataValue {
    /// Parse a `data` box payload: version, flags, a 4-byte type, a 4-byte
    /// locale, then the value.
    #[must_use]
    pub fn parse(box_: &Box_) -> Option<MetadataValue> {
        let p = &box_.payload;
        // 1 version + 3 flags + 4 type + 4 locale = 12 bytes of header.
        if p.len() < 8 {
            return None;
        }
        let flag = u32_at(p, 4, "data type flag").ok()?;
        let locale = u32_at(p, 8, "data locale").unwrap_or(0);
        Some(MetadataValue {
            data_type: DataType::from_flag(flag),
            payload: p.get(12..).unwrap_or(&[]).to_vec(),
            locale,
        })
    }

    /// Serialize this value as a `data` box payload.
    #[must_use]
    pub fn to_payload(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(12 + self.payload.len());
        out.push(0); // version
        out.extend_from_slice(&[0, 0, 0]); // flags
        out.extend_from_slice(&self.data_type.as_flag().to_be_bytes());
        out.extend_from_slice(&self.locale.to_be_bytes());
        out.extend_from_slice(&self.payload);
        out
    }
}

/// Parse a `meta` box's payload into metadata.
///
/// # Two layouts, and why
///
/// `meta` is a *full box*, so its payload should begin with a version and flags
/// before the child boxes. Apple's writer omitted that header for years, so
/// files exist both ways. Telling them apart needs care: the header-less form
/// starts with a child's *size* field, so the box type sits four bytes in, not
/// at offset zero.
///
/// This crate's own reader treats `meta` as a full-box container (see
/// [`crate::container::full_box_container`]), so a `Box_` produced by the parser
/// has already had those four bytes removed from `children` — but a `meta`
/// assembled by hand, or one whose payload we read raw, may still carry them.
/// Both are handled here.
#[must_use]
pub fn parse_meta(meta: &Box_) -> Option<Metadata> {
    // The parser path: children were already populated past the full-box
    // header, so look there first.
    if let Some(ilst) = meta.child(*b"ilst") {
        return Some(Metadata::from_ilst(ilst));
    }

    let payload = &meta.payload;
    if payload.len() < 8 {
        return None;
    }

    // Header-less: bytes 4..8 name a child box directly.
    let type_at_four = payload.get(4..8)?;
    let headerless = type_at_four == b"hdlr" || type_at_four == b"ilst" || type_at_four == b"free";

    let body = if headerless {
        payload.as_slice()
    } else if version(payload).is_ok() {
        payload.get(4..).unwrap_or(&[])
    } else {
        // Not a version byte and not a child type at 4..8: treat the whole
        // payload as a box list rather than rejecting the metadata.
        payload.as_slice()
    };

    let inner = parse_boxes(body).ok()?;
    let ilst = inner.iter().find(|b| b.box_type == *b"ilst")?;
    Some(Metadata::from_ilst(ilst))
}

/// A minimal typed error for callers that want to know why an atom failed.
#[must_use]
pub fn atom_error(atom: &str, reason: &'static str) -> Mp4Error {
    Mp4Error::MalformedMetadataItem {
        atom: String::from(atom),
        reason,
    }
}
