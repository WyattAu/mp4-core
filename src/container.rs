//! Box headers and the box tree.
//!
//! ISO-BMFF is a flat list of length-prefixed boxes; a "container" is just a
//! box whose payload is more boxes. That makes the whole format two types: a
//! [`BoxType`] and a recursive [`Box`].
//!
//! # Size forms
//!
//! A box header is 8 bytes minimum, but there are three size encodings and the
//! difference matters:
//!
//! | Form | Header | When |
//! |---|---|---|
//! | 32-bit | 8 | `size != 1` |
//! | 64-bit `largesize` | 16 | `size == 1` |
//! | to end of file | 8 | `size == 0` |
//!
//! `size == 0` means "extends to the end of the containing file", which is legal
//! only for the last box — and is a common way for a truncated file to look
//! valid. This crate resolves it against the parent's extent and records which
//! form was used, so a caller can tell a deliberately open-ended box from one
//! that merely ran off the end.

use alloc::borrow::Cow;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::error::Mp4Error;

/// A four-character box type, stored as the four ASCII bytes in wire order.
///
/// Wire order, not the order the characters read: a box header stores the type
/// big-endian, so `moov` is `['m', 'o', 'o', 'v']` and `©nam` is
/// `[0xA9, b'n', b'a', b'm']`.
pub type BoxType = [u8; 4];

/// The extended type of a `uuid` box: a 16-byte UUID following the header.
pub type ExtendedType = [u8; 16];

/// Render a box type for a human-facing message.
///
/// A box type is four bytes that are almost always ASCII but are not required
/// to be, so unprintable bytes become `.` rather than producing mojibake in an
/// error message.
#[must_use]
pub fn display_type(box_type: &BoxType) -> String {
    let mut s = String::with_capacity(4);
    for b in box_type {
        if b.is_ascii_graphic() || *b == b' ' {
            s.push(char::from(*b));
        } else {
            s.push('.');
        }
    }
    s
}

/// How a box declared its size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeForm {
    /// A 32-bit size in the standard 8-byte header.
    Normal,
    /// `size == 1` followed by a 64-bit `largesize`, a 16-byte header.
    Large,
    /// `size == 0`: the box runs to the end of its container.
    ToEnd,
}

/// Which size encoding a box used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    /// An explicit length.
    Fixed(u64),
    /// The box extends to the end of its container.
    ToEnd,
}

impl Size {
    /// The header width this size implies.
    #[must_use]
    pub const fn header_len(self) -> usize {
        match self {
            Size::ToEnd => 8,
            Size::Fixed(_) => 8,
        }
    }
}

/// A parsed box: its header, and either a sub-tree or opaque payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Box_ {
    /// The four-character type.
    pub box_type: BoxType,
    /// The size, already resolved against the parent.
    pub size: Size,
    /// The encoding the header used.
    pub size_form: SizeForm,
    /// The `uuid` extended type, when this is a `uuid` box.
    pub extended_type: Option<ExtendedType>,
    /// The payload after the header.
    ///
    /// For a container this is empty and `children` is populated instead.
    pub payload: Vec<u8>,
    /// Nested boxes, when this box is a container.
    pub children: Vec<Box_>,
}

impl Box_ {
    /// The box's type as a string, for display and matching.
    ///
    /// `©` (0xA9) is the iTunes copyright-sign prefix used by metadata atoms
    /// and is not valid UTF-8 in isolation, so it is transliterated.
    #[must_use]
    pub fn type_str(&self) -> Cow<'_, str> {
        type_str(&self.box_type)
    }

    /// Whether this box contains nested boxes.
    #[must_use]
    pub fn is_container(&self) -> bool {
        !self.children.is_empty() || is_container_type(&self.box_type)
    }

    /// The first child of the given type.
    #[must_use]
    pub fn child(&self, box_type: BoxType) -> Option<&Box_> {
        self.children.iter().find(|c| c.box_type == box_type)
    }

    /// Every child of the given type, in order.
    #[must_use]
    pub fn children_of(&self, box_type: BoxType) -> Vec<&Box_> {
        self.children
            .iter()
            .filter(|c| c.box_type == box_type)
            .collect()
    }

    /// The total serialized length: header plus payload.
    #[must_use]
    pub fn encoded_len(&self) -> u64 {
        let header = match self.size_form {
            SizeForm::Normal | SizeForm::ToEnd => 8,
            SizeForm::Large => 16,
        };
        u64::try_from(header)
            .unwrap_or(u64::MAX)
            .saturating_add(u64::try_from(self.payload.len()).unwrap_or(u64::MAX))
            .saturating_add(self.children.iter().map(|c| c.encoded_len()).sum::<u64>())
    }

    /// Serialize this box, recursing into children.
    ///
    /// The size written is always computed from the content, so a tree built by
    /// hand cannot carry a stale length.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut body: Vec<u8> = Vec::new();
        if let Some(us) = self.extended_type {
            body.extend_from_slice(&us);
        }
        body.extend_from_slice(&self.payload);
        for child in &self.children {
            body.extend_from_slice(&child.to_bytes());
        }

        // A container's payload lives in its children, so the length must cover
        // both.
        let total = body.len() as u64 + 8;
        let mut out = Vec::with_capacity(body.len() + 8);
        out.extend_from_slice(&u32::try_from(total).unwrap_or(u32::MAX).to_be_bytes());
        out.extend_from_slice(&self.box_type);
        out.extend_from_slice(&body);
        out
    }
}

/// Render a box type, transliterating the iTunes `©` prefix.
///
/// `©nam` is not valid UTF-8 on its own because 0xA9 is a Latin-1 byte, and
/// lossy decoding would turn it into U+FFFD — so it is mapped to the actual
/// copyright sign.
#[must_use]
pub fn type_str(box_type: &BoxType) -> Cow<'_, str> {
    let bytes = *box_type;
    if bytes[0] == 0xA9 {
        // The remaining three bytes are ASCII per the iTunes convention.
        let rest: Vec<u8> = bytes[1..].to_vec();
        return match core::str::from_utf8(&rest) {
            Ok(s) => {
                let mut owned = String::from("\u{a9}");
                owned.push_str(s);
                Cow::Owned(owned)
            }
            Err(_) => Cow::Borrowed("?"),
        };
    }
    // Borrow from the caller's slice; only the non-UTF-8 case allocates.
    match core::str::from_utf8(box_type) {
        Ok(s) => Cow::Borrowed(s),
        Err(_) => {
            let mut owned = String::with_capacity(4);
            for b in box_type {
                owned.push(char::from(*b));
            }
            Cow::Owned(owned)
        }
    }
}

/// The container box types this crate descends into.
///
/// The set is deliberately explicit rather than "everything": a box is only
/// descended into if its payload is known to be boxes. Treating an unknown box
/// as a container is the classic source of a parser that reports nonsense
/// children inside `mdat`.
/// Whether a box is an `ilst` metadata atom — `©nam`, `covr`, `trkn`, and the
/// rest.
///
/// These are containers too, but their names are arbitrary four-byte codes
/// rather than a fixed set, so they cannot appear in the `matches!` above. A
/// metadata atom that is not descended into keeps its `data` children sealed
/// inside an opaque payload, and every atom then reads as empty — which is why
/// this predicate exists.
#[must_use]
pub fn is_ilst_atom(box_type: &BoxType) -> bool {
    // `©`-prefixed atoms are Apple's reserved namespace; `data`, `mean`, `name`
    // and `namei` are the free-form (mean/name) namespace.
    box_type[0] == 0xA9
        || matches!(
            box_type,
            b"covr" | b"trkn" | b"disk" | b"cpil" | b"pgap" | b"stik" | b"desc" | b"ldes"
        )
}

/// Bytes of header to skip before descending into a container.
///
/// A "full box" prefixes its payload with `version(1) flags(3)`; some also carry
/// a count. Getting this wrong is the classic descent bug: reading the first four
/// bytes as a box header sees `size == 0` — the "runs to end of file" form — and
/// swallows every sibling as one opaque blob, so the box silently parses as
/// empty.
///
/// | Container | Skipped | Why |
/// |---|---|---|
/// | `meta` | 4 | version + flags, then `hdlr`/`ilst` |
/// | `stsd` | 8 | version + flags + `entry_count`, then sample entries |
///
/// `stsd`'s extra four bytes are the reason this is a table and not a flag: a
/// descent that skips only the version would read the entry count as a box size
/// and lose every sample entry, which is where the codec's sample rate lives.
#[must_use]
pub fn full_box_header_len(box_type: &BoxType) -> usize {
    match box_type {
        b"meta" => 4,
        b"stsd" => 8,
        _ => 0,
    }
}

/// Whether this container's payload is a child box list behind a full-box header.
#[must_use]
pub fn full_box_container(box_type: &BoxType) -> bool {
    full_box_header_len(box_type) != 0
}

/// Whether this box's payload is a list of child boxes.
///
/// The set is deliberately explicit rather than "everything": a box is only
/// descended into if its payload is known to be boxes. Treating an unknown box
/// as a container is the classic source of a parser that reports nonsense
/// children inside `mdat`.
#[must_use]
pub fn is_container_type(box_type: &BoxType) -> bool {
    matches!(
        box_type,
        b"moov"
            | b"trak"
            | b"mdia"
            | b"minf"
            | b"stbl"
            | b"stsd"
            | b"edts"
            | b"dinf"
            | b"tref"
            | b"udta"
            | b"meta"
            | b"ilst"
            | b"moof"
            | b"traf"
            | b"mvex"
            | b"mfra"
            | b"skip"
            | b"wave"
    ) || is_ilst_atom(box_type)
}

/// Parse a sequence of sibling boxes from `data`.
///
/// Stops cleanly at the first all-zero region (padding), and stops rather than
/// erroring when fewer than eight bytes remain — a file may legitimately end
/// mid-padding.
///
/// # Errors
///
/// [`Mp4Error::InvalidBoxSize`] for a size below the header width, and
/// [`Mp4Error::TruncatedBox`] when a box claims more bytes than remain.
pub fn parse_boxes(data: &[u8]) -> Result<Vec<Box_>, Mp4Error> {
    let mut out = Vec::new();
    let mut offset = 0usize;

    while let Some(rest) = data.get(offset..) {
        if rest.len() < 8 {
            // Trailing bytes too short to be a header: padding or truncation.
            break;
        }
        // All-zero region: padding to the end of the container.
        if rest.iter().all(|b| *b == 0) {
            break;
        }

        // Both the size and the type are known-present: the loop above checked
        // `rest.len() >= 8`.
        let size32 = u32::from_be_bytes(
            rest.get(..4)
                .and_then(|s| <[u8; 4]>::try_from(s).ok())
                .unwrap_or([0; 4]),
        );
        let box_type: BoxType = rest
            .get(4..8)
            .and_then(|s| <[u8; 4]>::try_from(s).ok())
            .unwrap_or([0; 4]);

        let (size, size_form, header_len) = match size32 {
            0 => (Size::ToEnd, SizeForm::ToEnd, 8usize),
            1 => {
                // A 64-bit `largesize` spans bytes 8..16.
                let Some(wide) = rest.get(8..16).and_then(|s| <[u8; 8]>::try_from(s).ok()) else {
                    return Err(Mp4Error::TruncatedBox {
                        box_type,
                        declared: 16,
                        available: rest.len(),
                    });
                };
                (
                    Size::Fixed(u64::from_be_bytes(wide)),
                    SizeForm::Large,
                    16usize,
                )
            }
            n => (Size::Fixed(u64::from(n)), SizeForm::Normal, 8usize),
        };

        // `uuid` carries a 16-byte extended type immediately after the header.
        let mut extended_type = None;
        let mut body_start = header_len;
        if &box_type == b"uuid" {
            let Some(ext) = rest.get(header_len..header_len + 16) else {
                return Err(Mp4Error::InvalidExtendedType);
            };
            let mut e: ExtendedType = [0; 16];
            e.copy_from_slice(ext);
            extended_type = Some(e);
            body_start = header_len + 16;
        }

        let available = rest.len().saturating_sub(body_start);
        let payload_len: usize = match size {
            Size::ToEnd => available,
            Size::Fixed(n) => {
                let n_usize = usize::try_from(n)
                    .map_err(|_| Mp4Error::InvalidBoxSize { box_type, size: n })?;
                if n_usize < body_start {
                    return Err(Mp4Error::InvalidBoxSize { box_type, size: n });
                }
                if n_usize - body_start > available {
                    return Err(Mp4Error::TruncatedBox {
                        box_type,
                        declared: n,
                        available,
                    });
                }
                n_usize - body_start
            }
        };

        let payload = rest
            .get(body_start..body_start + payload_len)
            .unwrap_or(&[]);
        let skip = full_box_header_len(&box_type);
        let children = if is_container_type(&box_type) {
            // A full-box container's children start after its own header, which
            // for `stsd` includes the entry count.
            let inner = if skip == 0 {
                payload
            } else {
                payload.get(skip..).unwrap_or(&[])
            };
            parse_boxes(inner)?
        } else {
            Vec::new()
        };

        // A container's payload must not be duplicated into `payload` — but a full-box
        // container's prefix must be kept, because it *is* part of the box: the version,
        // the flags, and for `stsd` the entry count. Dropping it serializes a box eight
        // bytes short (`stsd`) or four (`meta`), and the file a muxer writes from that
        // tree is corrupt in a way no reader of the original would have predicted.
        //
        // Reading never noticed, because reading parses the original bytes once and never
        // re-serializes them. Writing did: a `moov` round-tripped through this module was
        // short by exactly the sum of the prefixes its full-box containers had dropped.
        let stored_payload: Vec<u8> = if children.is_empty() {
            payload.to_vec()
        } else if skip == 0 {
            Vec::new()
        } else {
            payload.get(..skip).unwrap_or(&[]).to_vec()
        };

        out.push(Box_ {
            box_type,
            size,
            size_form,
            extended_type,
            payload: stored_payload,
            children,
        });

        // Advance past this box. `Size::ToEnd` consumed the rest, so the loop
        // ends naturally.
        let consumed = match size {
            Size::ToEnd => rest.len(),
            Size::Fixed(n) => usize::try_from(n).unwrap_or(usize::MAX),
        };
        offset = offset.saturating_add(consumed);
        if consumed == 0 {
            break;
        }
    }

    Ok(out)
}

/// Find the top-level box of the given type in a file.
#[must_use]
pub fn find_top_level(data: &[u8], box_type: BoxType) -> Option<Box_> {
    parse_boxes(data)
        .ok()?
        .into_iter()
        .find(|b| b.box_type == box_type)
}

/// Parse a tree of boxes, returning the first top-level box.
#[must_use]
pub fn first_box(data: &[u8]) -> Option<Box_> {
    parse_boxes(data).ok()?.into_iter().next()
}

/// Build a leaf box with a payload.
///
/// # Errors
///
/// Never in practice; the signature is fallable so a future size-limited box
/// type can reject an oversized payload.
pub fn leaf(box_type: BoxType, payload: Vec<u8>) -> Result<Box_, Mp4Error> {
    Ok(Box_ {
        box_type,
        size: Size::Fixed(8 + payload.len() as u64),
        size_form: SizeForm::Normal,
        extended_type: None,
        payload,
        children: Vec::new(),
    })
}

/// An empty box of the given type.
///
/// `leaf` is fallable so a future size-limited box type can reject an oversized
/// payload; an empty payload never trips that, so callers that build a tree
/// incrementally can fall back to this without an `expect`.
#[must_use]
pub fn empty(box_type: BoxType) -> Box_ {
    Box_ {
        box_type,
        size: Size::Fixed(8),
        size_form: SizeForm::Normal,
        extended_type: None,
        payload: Vec::new(),
        children: Vec::new(),
    }
}

/// Build a full-box container: `version(1) flags(3)` followed by child boxes.
///
/// `meta` is the case this exists for. Emitting the four-byte header is what
/// makes the box readable, since a parser strips those bytes before descending.
#[must_use]
pub fn full_box_container_box(box_type: BoxType, children: Vec<Box_>) -> Box_ {
    let mut c = container(box_type, children);
    // `container` writes only children; a full box needs its header (version,
    // flags, and for `stsd` the entry count) inside the payload, so rebuild
    // the payload here rather than in `to_bytes`. The prefix width comes from
    // the same table parsing uses — a hand-written four baked in a `meta`
    // bias and made every `stsd` built this way unparseable.
    let mut payload = vec![0u8; full_box_header_len(&box_type)];
    for child in &c.children {
        payload.extend_from_slice(&child.to_bytes());
    }
    c.children = Vec::new();
    c.payload = payload;
    c
}

/// Build a container box from children.
#[must_use]
pub fn container(box_type: BoxType, children: Vec<Box_>) -> Box_ {
    Box_ {
        box_type,
        size: Size::Fixed(8),
        size_form: SizeForm::Normal,
        extended_type: None,
        payload: Vec::new(),
        children,
    }
}

/// Recursively walk a box tree, calling `f` on each box in document order.
///
/// This is how a caller reaches a deeply nested box without knowing the
/// structure — an `ilst` atom inside `udta` inside `moov`, say.
///
/// # Errors
///
/// Propagates whatever `f` returns, so a walk that hits a malformed subtree can
/// abort. A `f` that always succeeds walks the whole tree.
pub fn walk<F>(b: &Box_, f: &mut F) -> Result<(), Mp4Error>
where
    F: FnMut(&Box_) -> Result<(), Mp4Error>,
{
    f(b)?;
    for child in &b.children {
        walk(child, f)?;
    }
    Ok(())
}

/// The total serialized length of a box list.
#[must_use]
pub fn encoded_len(boxes: &[Box_]) -> u64 {
    boxes.iter().map(|b| b.encoded_len()).sum()
}

/// A boxed value, so recursion through `Box_` is one pointer wide.
pub type BoxRef = Box<Box_>;

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic
    )]

    use super::*;
    use alloc::vec;

    // Fixtures are allowed to assert: an `unwrap` here is the test stating
    // what it assumes, the honest form, and the crate lint denies it
    // everywhere a caller would write it.

    const FOURCC_MOOV: BoxType = *b"moov";
    const FOURCC_FREE: BoxType = *b"free";
    const FOURCC_TINY: BoxType = *b"tiny";
    const FOURCC_BIGR: BoxType = *b"bigr";

    fn leaf_bytes(box_type: BoxType, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let total = u32::try_from(8 + payload.len()).unwrap_or(u32::MAX);
        out.extend_from_slice(&total.to_be_bytes());
        out.extend_from_slice(&box_type);
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn display_type_renders_printable_and_dots_the_rest() {
        assert_eq!(display_type(&FOURCC_MOOV), "moov");
        assert_eq!(display_type(b"m v "), "m v ", "space is kept");
        assert_eq!(display_type(&[0x00, b'a', 0x01, 0x7f]), ".a..");
    }

    #[test]
    fn size_header_len_is_eight_for_both_basic_forms() {
        assert_eq!(Size::Fixed(10).header_len(), 8);
        assert_eq!(Size::ToEnd.header_len(), 8);
    }

    #[test]
    fn type_str_transliterates_the_copyright_prefix() {
        assert_eq!(type_str(b"moov"), "moov");
        assert_eq!(type_str(&[0xA9, b'n', b'a', b'm']), "\u{a9}nam");
        // A non-UTF-8 tail after the prefix falls back to '?'.
        assert_eq!(type_str(&[0xA9, 0xFF, 0xFE, 0xFD]), "?");
        // The non-UTF-8 fallback maps each byte through char::from
        // (Latin-1), not through display_type's dot rule.
        assert_eq!(type_str(&[0x00, 0xFF, 0x01, 0x02]), "\0\u{ff}\u{1}\u{2}");
    }

    #[test]
    fn box_type_str_uses_the_free_function() {
        let leaf_box = leaf(FOURCC_MOOV, vec![1]).expect("leaf");
        assert_eq!(leaf_box.type_str(), "moov");
    }

    #[test]
    fn ilst_atoms_are_the_copyright_namespace_plus_the_named_set() {
        assert!(is_ilst_atom(&[0xA9, b'n', b'a', b'm']));
        assert!(is_ilst_atom(b"covr"));
        assert!(is_ilst_atom(b"trkn"));
        assert!(!is_ilst_atom(b"moov"));
    }

    #[test]
    fn full_box_headers_are_meta_four_and_stsd_eight() {
        assert_eq!(full_box_header_len(b"meta"), 4);
        assert_eq!(full_box_header_len(b"stsd"), 8);
        assert_eq!(full_box_header_len(b"moov"), 0);
        assert!(full_box_container(b"meta"));
        assert!(!full_box_container(b"mdat"));
    }

    #[test]
    fn container_types_are_the_explicit_set() {
        assert!(is_container_type(b"moov"));
        assert!(is_container_type(b"trak"));
        assert!(is_container_type(b"stsd"));
        assert!(!is_container_type(b"mdat"));
        assert!(!is_container_type(b"free"));
    }

    #[test]
    fn parse_boxes_reads_a_simple_leaf() {
        let data = leaf_bytes(*b"free", b"hello");
        let boxes = parse_boxes(&data).expect("parses");
        assert_eq!(boxes.len(), 1);
        assert_eq!(boxes[0].box_type, FOURCC_FREE);
        assert_eq!(boxes[0].payload, b"hello");
        assert!(boxes[0].children.is_empty());
    }

    #[test]
    fn parse_boxes_stops_at_trailing_padding() {
        // The stop rule is "the remainder is all zero" — padding written to
        // the end of a container, the common real-world shape.
        let mut data = leaf_bytes(*b"free", b"x");
        data.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let boxes = parse_boxes(&data).expect("parses");
        assert_eq!(boxes.len(), 1);
        assert_eq!(boxes[0].payload, b"x");
    }

    #[test]
    fn a_short_tail_below_a_header_is_ignored() {
        // Fewer than eight trailing bytes: padding or truncation, not a box.
        let mut data = leaf_bytes(*b"free", b"x");
        data.extend_from_slice(&[1, 2, 3]);
        let boxes = parse_boxes(&data).expect("parses");
        assert_eq!(boxes.len(), 1);
    }

    #[test]
    fn largesize_form_parses_a_sixteen_byte_header() {
        let mut data = Vec::new();
        data.extend_from_slice(&1u32.to_be_bytes()); // size == 1: largesize form
        data.extend_from_slice(&b"big1"[..]);
        let body = b"the payload";
        data.extend_from_slice(&u64::try_from(16 + body.len()).unwrap().to_be_bytes());
        data.extend_from_slice(body);

        let boxes = parse_boxes(&data).expect("parses");
        assert_eq!(boxes.len(), 1);
        assert_eq!(boxes[0].size_form, SizeForm::Large);
        assert_eq!(
            boxes[0].size,
            Size::Fixed(u64::try_from(16 + body.len()).unwrap())
        );
        assert_eq!(boxes[0].payload, body);
    }

    #[test]
    fn a_largesize_box_with_a_truncated_wide_field_is_an_error() {
        let mut data = Vec::new();
        data.extend_from_slice(&1u32.to_be_bytes());
        data.extend_from_slice(&b"big1"[..]);
        data.extend_from_slice(&[0, 0, 0]); // three bytes: not a u64
        let err = parse_boxes(&data).expect_err("truncated largesize");
        assert!(matches!(err, Mp4Error::TruncatedBox { declared: 16, .. }));
    }

    #[test]
    fn to_end_form_consumes_the_rest() {
        let mut data = leaf_bytes(*b"free", b"kept");
        data.extend_from_slice(&0u32.to_be_bytes()); // size 0: to end
        data.extend_from_slice(&b"tail"[..]);
        let boxes = parse_boxes(&data).expect("parses");
        assert_eq!(boxes.len(), 2, "the to-end box and the preceding leaf");
        assert_eq!(boxes[1].size_form, SizeForm::ToEnd);
        assert_eq!(boxes[1].size, Size::ToEnd);
        assert_eq!(boxes[1].box_type, *b"tail");
        assert_eq!(boxes[1].payload, b"", "the header was the last eight bytes");
    }

    #[test]
    fn a_size_below_the_header_is_rejected() {
        let mut data = Vec::new();
        data.extend_from_slice(&3u32.to_be_bytes()); // size 3 < header 8
        data.extend_from_slice(&b"tiny"[..]);
        let err = parse_boxes(&data).expect_err("size 3 < 8");
        assert!(matches!(
            err,
            Mp4Error::InvalidBoxSize {
                box_type: FOURCC_TINY,
                size: 3
            }
        ));
    }

    #[test]
    fn a_size_past_the_available_bytes_is_truncation() {
        let mut data = leaf_bytes(*b"free", b"abc");
        data.extend_from_slice(&u32::try_from(8 + 100).unwrap().to_be_bytes());
        data.extend_from_slice(&b"bigr"[..]);
        // The trailing box claims 108 bytes but only 8 remain.
        let err = parse_boxes(&data).expect_err("claims past the end");
        assert!(matches!(
            err,
            Mp4Error::TruncatedBox {
                box_type: FOURCC_BIGR,
                ..
            }
        ));
    }

    #[test]
    fn uuid_boxes_carry_their_extended_type() {
        let ext: [u8; 16] = core::array::from_fn(|i| u8::try_from(i).unwrap());
        let mut data = Vec::new();
        data.extend_from_slice(&u32::try_from(8 + 16 + 2).unwrap().to_be_bytes());
        data.extend_from_slice(&b"uuid"[..]);
        data.extend_from_slice(&ext);
        data.extend_from_slice(b"zz");

        let boxes = parse_boxes(&data).expect("parses");
        assert_eq!(boxes.len(), 1);
        assert_eq!(boxes[0].extended_type, Some(ext));
        assert_eq!(boxes[0].payload, b"zz");
    }

    #[test]
    fn a_uuid_box_without_room_for_the_extended_type_is_an_error() {
        let mut data = Vec::new();
        data.extend_from_slice(&u32::try_from(8 + 4).unwrap().to_be_bytes());
        data.extend_from_slice(&b"uuid"[..]);
        data.extend_from_slice(&[0, 1, 2, 3]); // four bytes: not sixteen
        let err = parse_boxes(&data).expect_err("truncated extended type");
        assert!(matches!(err, Mp4Error::InvalidExtendedType));
    }

    #[test]
    fn full_box_containers_keep_their_prefix_and_descend_past_it() {
        // meta: version+flags (4 bytes) then a free child — the child is
        // part of meta's payload, because that is how the wire lays it out.
        let data = full_box_container_box(
            *b"meta",
            vec![leaf(FOURCC_FREE, b"kid".to_vec()).expect("leaf")],
        )
        .to_bytes();

        let boxes = parse_boxes(&data).expect("parses");
        assert_eq!(boxes.len(), 1);
        let meta = &boxes[0];
        assert!(meta.is_container());
        assert_eq!(meta.payload, &[0, 0, 0, 0], "the full-box prefix is kept");
        assert_eq!(meta.children.len(), 1);
        assert_eq!(meta.children[0].box_type, FOURCC_FREE);
        // Round trip: the serialized meta is as long as the original.
        assert_eq!(meta.to_bytes().len(), data.len());
    }

    #[test]
    fn stsd_skips_eight_bytes_of_header_and_keeps_them() {
        // stsd: version+flags (4) + entry_count (4), then one free child —
        // inside stsd's payload, per the wire.
        let data = full_box_container_box(
            *b"stsd",
            vec![leaf(FOURCC_FREE, b"s".to_vec()).expect("leaf")],
        )
        .to_bytes();
        let boxes = parse_boxes(&data).expect("parses");
        let stsd = &boxes[0];
        assert_eq!(stsd.children.len(), 1);
        assert_eq!(stsd.payload.len(), 8, "both header bytes are kept");
        assert_eq!(stsd.to_bytes().len(), data.len(), "round trips exact");
    }

    #[test]
    fn containers_nest_and_round_trip() {
        let inner = container(
            *b"trak",
            vec![leaf(FOURCC_FREE, b"t".to_vec()).expect("leaf")],
        );
        let outer = container(
            FOURCC_MOOV,
            vec![inner, leaf(*b"udta", b"u".to_vec()).expect("leaf")],
        );
        let bytes = outer.to_bytes();

        let parsed = parse_boxes(&bytes).expect("parses");
        assert_eq!(parsed.len(), 1);
        let moov = &parsed[0];
        assert!(moov.is_container());
        assert_eq!(moov.child(*b"trak").map(|c| c.box_type), Some(*b"trak"));
        assert_eq!(moov.children_of(*b"udta").len(), 1);
        assert_eq!(
            moov.child(*b"trak")
                .and_then(|t| t.child(*b"free"))
                .map(|f| f.payload.as_slice()),
            Some(b"t".as_slice())
        );
        assert_eq!(moov.encoded_len(), u64::try_from(bytes.len()).unwrap());
        assert_eq!(moov.to_bytes(), bytes, "round trip is exact");
    }

    #[test]
    fn encoded_len_covers_children_and_the_large_header() {
        let child = leaf(FOURCC_FREE, b"payload".to_vec()).expect("leaf");
        let parent = full_box_container_box(*b"meta", vec![child]);
        // meta keeps its 4-byte prefix as payload: 8 header + 4 prefix + child.
        let child_len = 8 + 7;
        assert_eq!(
            parent.encoded_len(),
            u64::try_from(8 + 4 + child_len).unwrap()
        );
    }

    #[test]
    fn find_top_level_and_first_box_pick_from_the_head_of_the_file() {
        let mut data = leaf_bytes(*b"free", b"first");
        data.extend_from_slice(&leaf_bytes(*b"moov", b"second"));
        let moov = find_top_level(&data, *b"moov").expect("finds the moov");
        assert_eq!(moov.payload, b"second");
        assert!(find_top_level(&data, *b"mdat").is_none());
        let first = first_box(&data).expect("first");
        assert_eq!(first.box_type, FOURCC_FREE);
    }

    #[test]
    fn an_ilst_atom_descends_so_its_data_children_are_visible() {
        // The ilst namespace is descended into because its names are
        // arbitrary codes — a metadata atom that stayed sealed would read
        // as empty, which is why is_ilst_atom exists.
        let data = container(
            *b"ilst",
            vec![leaf(*b"data", b"opaque-value".to_vec()).expect("leaf")],
        )
        .to_bytes();
        let boxes = parse_boxes(&data).expect("parses");
        assert_eq!(boxes.len(), 1);
        let ilst = &boxes[0];
        assert!(ilst.is_container());
        assert_eq!(ilst.children.len(), 1);
        assert_eq!(ilst.children[0].box_type, *b"data");
    }

    #[test]
    fn a_meta_box_descent_survives_a_childless_payload() {
        // meta with a version+flags prefix and nothing after it: the skip
        // must not read past the payload.
        let data = full_box_container_box(*b"meta", vec![]).to_bytes();
        let boxes = parse_boxes(&data).expect("parses");
        assert_eq!(boxes.len(), 1);
        assert!(boxes[0].children.is_empty());
    }

    #[test]
    fn empty_box_has_no_payload_and_containers_accept_children() {
        let e = empty(FOURCC_FREE);
        assert!(e.payload.is_empty());
        assert_eq!(e.encoded_len(), 8);
        let c = container(FOURCC_MOOV, vec![]);
        assert!(c.children.is_empty());
        let fc = full_box_container_box(*b"meta", vec![]);
        assert_eq!(fc.payload.len(), 4, "the full-box prefix is retained");
    }
}
