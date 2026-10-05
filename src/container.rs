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
        let children = if is_container_type(&box_type) {
            // A full-box container's children start after its own header, which
            // for `stsd` includes the entry count.
            let skip = full_box_header_len(&box_type);
            let inner = if skip == 0 {
                payload
            } else {
                payload.get(skip..).unwrap_or(&[])
            };
            parse_boxes(inner)?
        } else {
            Vec::new()
        };

        // A container's payload must not be duplicated into `payload`.
        let stored_payload: Vec<u8> = if children.is_empty() {
            payload.to_vec()
        } else {
            Vec::new()
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
    // `container` writes only children; a full box needs the header inside the
    // payload, so rebuild the payload here rather than in `to_bytes`.
    let mut payload: Vec<u8> = vec![0u8, 0, 0, 0];
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
