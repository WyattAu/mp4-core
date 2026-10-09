//! Random access to a file's bytes, and the box headers it takes to walk one.
//!
//! # Why this exists
//!
//! [`Mp4File::parse`](crate::Mp4File::parse) takes a `&[u8]`, which is the right shape
//! for a caller holding the bytes and the wrong one for a library with a 2 GB audiobook
//! on disk. An MP4 states its duration in `mvhd`, its chapter list in `udta`, and its
//! chapter track's sample table in `stbl` — all inside `moov`, and `moov` sits at one end
//! of the file or the other, chosen by the muxer. Everything a caller wants is a few tens
//! of kilobytes at one end; the gigabytes in the middle are audio.
//!
//! So: read box *headers* by seeking, find `moov`, read only that, and read chapter
//! samples one at a time where the sample table says they are. A file larger than memory
//! becomes readable, which is the actual point.
//!
//! # The contract
//!
//! A short read is normal, not a failure: it means the file ended. A box that claims more
//! bytes than the file holds is reported, not clamped — a clamped size reads as a valid
//! box of the wrong length, and every field derived from it would be a plausible wrong
//! answer rather than an error.

use alloc::vec::Vec;

use crate::container::BoxType;
use crate::error::Mp4Error;

/// Random access to a file's bytes.
pub trait ByteReader {
    /// The file's total length, when the reader knows it.
    ///
    /// Named `byte_len` rather than `len` for the ordinary reason: a `len` with no
    /// `is_empty` beside it is a clippy lint, and a `len` returning `Option` is a second
    /// question nobody asked.
    fn byte_len(&self) -> Option<u64>;

    /// Fill `buf` from `offset`, returning how many bytes were actually read.
    ///
    /// A short read means the file ended. `None` means the read could not be attempted.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Option<usize>;
}

impl ByteReader for &[u8] {
    fn byte_len(&self) -> Option<u64> {
        let data: &[u8] = self;
        Some(u64::try_from(data.len()).unwrap_or(u64::MAX))
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Option<usize> {
        let data: &[u8] = self;
        let start = usize::try_from(offset).ok()?;
        let available = data.get(start..)?;
        let take = available.len().min(buf.len());
        buf.get_mut(..take)?.copy_from_slice(available.get(..take)?);
        Some(take)
    }
}

/// One top-level box, located but not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TopLevelBox {
    /// Where the box's header starts.
    pub offset: u64,
    /// The box's size as declared, header included.
    pub size: u64,
    /// Its four-character type.
    pub box_type: BoxType,
    /// How wide its header was, which `offset + size` does not account for on its own.
    pub header_len: u64,
}

impl TopLevelBox {
    /// The first byte after this box.
    #[must_use]
    pub fn end(&self) -> u64 {
        self.offset.saturating_add(self.size)
    }
}

/// The widest a box header can be: 8 bytes, or 16 with a 64-bit `largesize`.
const MAX_HEADER_LEN: usize = 16;

/// Locate every top-level box in a file, reading only headers.
///
/// This is the whole trick behind probing a large file cheaply: sixteen bytes per box,
/// however large the boxes are. A 2 GB M4B has three or four of them.
///
/// # Errors
///
/// [`Mp4Error::TruncatedBox`] when a header claims more bytes than the file holds, and
/// [`Mp4Error::InvalidBoxSize`] for a size below its own header width. Both are refused
/// rather than clamped: a clamped size is a box of a plausible wrong length, and
/// everything derived from it is then a plausible wrong answer.
pub fn read_top_level(
    reader: &mut impl ByteReader,
    total_len: u64,
) -> Result<Vec<TopLevelBox>, Mp4Error> {
    let mut out = Vec::new();
    let mut offset = 0u64;

    while offset.saturating_add(8) <= total_len {
        let mut header = [0u8; MAX_HEADER_LEN];
        let want = usize::try_from((total_len - offset).min(MAX_HEADER_LEN as u64)).unwrap_or(0);
        let Some(window) = header.get_mut(..want) else {
            break;
        };
        let read = reader.read_at(offset, window).unwrap_or(0);
        if read < 8 {
            break;
        }
        let size32 = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
        let box_type: BoxType = [header[4], header[5], header[6], header[7]];
        let (size, header_len) = match size32 {
            0 => (total_len - offset, 8u64),
            1 => {
                let Some(wide) = header.get(8..16) else {
                    return Err(Mp4Error::TruncatedBox {
                        box_type,
                        declared: 16,
                        available: read,
                    });
                };
                if read < 16 {
                    return Err(Mp4Error::TruncatedBox {
                        box_type,
                        declared: 16,
                        available: read,
                    });
                }
                let wide = <[u8; 8]>::try_from(wide).map_err(|_| Mp4Error::TruncatedBox {
                    box_type,
                    declared: 16,
                    available: read,
                })?;
                (u64::from_be_bytes(wide), 16u64)
            }
            n => (u64::from(n), 8u64),
        };

        if size < header_len {
            return Err(Mp4Error::InvalidBoxSize { box_type, size });
        }
        if offset.saturating_add(size) > total_len {
            return Err(Mp4Error::TruncatedBox {
                box_type,
                declared: size,
                available: usize::try_from(total_len - offset).unwrap_or(usize::MAX),
            });
        }

        out.push(TopLevelBox {
            offset,
            size,
            box_type,
            header_len,
        });
        // A `size == 0` box runs to the end, so this terminates rather than looping.
        offset = offset.saturating_add(size);
        if size32 == 0 {
            break;
        }
    }
    Ok(out)
}

/// Read one box in full, given where [`read_top_level`] said it was.
///
/// # Errors
///
/// [`Mp4Error::TruncatedBox`] when the box runs past the end of the file, and
/// [`Mp4Error::InvalidBoxSize`] for a size below its header width.
pub fn read_box(reader: &mut impl ByteReader, top: &TopLevelBox) -> Result<Vec<u8>, Mp4Error> {
    if top.size < top.header_len {
        return Err(Mp4Error::InvalidBoxSize {
            box_type: top.box_type,
            size: top.size,
        });
    }
    let mut buf = alloc::vec![0u8; usize::try_from(top.size).unwrap_or(usize::MAX)];
    let want = buf.len();
    let read = reader.read_at(top.offset, &mut buf).unwrap_or(0);
    if u64::try_from(read).unwrap_or(0) < top.size {
        return Err(Mp4Error::TruncatedBox {
            box_type: top.box_type,
            declared: top.size,
            available: read,
        });
    }
    buf.truncate(want);
    Ok(buf)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::*;
    use alloc::vec;

    /// A reader over a buffer that records the *volume* of what was read, so a test can
    /// assert that finding a box did not require holding the file.
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

    /// A box of `size` bytes total, the last `size - 8` of them payload.
    ///
    /// The size field is 32 bits on the wire even when the walk works in `u64`, which is
    /// exactly the kind of narrowing that makes a hand-built fixture disagree with the
    /// reader it is meant to check.
    fn box_of(size: u32, box_type: &[u8; 4]) -> Vec<u8> {
        let mut b = size.to_be_bytes().to_vec();
        b.extend_from_slice(box_type);
        b.resize(size as usize, 0);
        b
    }

    #[test]
    fn locating_a_moov_at_the_end_costs_sixteen_bytes_a_box() {
        // The shape ffmpeg writes: ftyp, a very large mdat, then moov at the end.
        let mut data = box_of(32, b"ftyp");
        data.extend_from_slice(&box_of(64_000_000u32, b"mdat"));
        data.extend_from_slice(&box_of(1024, b"moov"));
        let total = u64::try_from(data.len()).unwrap_or(0);
        assert_eq!(total, 64_001_056, "the file really is that big");

        let mut reader = Counting::new(data);
        let top = read_top_level(&mut reader, total).expect("walks");
        assert_eq!(top.len(), 3);
        let moov = *top.iter().find(|b| b.box_type == *b"moov").expect("a moov");
        assert_eq!(moov.size, 1024);
        assert_eq!(moov.offset, 64_000_032);
        assert_eq!(moov.header_len, 8);
        // Three headers of at most sixteen bytes each, and nothing else.
        assert!(
            reader.read <= 3 * MAX_HEADER_LEN,
            "read {} bytes to find boxes in a {} byte file",
            reader.read,
            total
        );
    }

    #[test]
    fn a_moov_can_be_read_on_its_own() {
        let mut data = box_of(32, b"ftyp");
        data.extend_from_slice(&box_of(4096u32, b"mdat"));
        let mut moov = box_of(64, b"moov");
        moov[24..28].copy_from_slice(b"mvhd");
        data.extend_from_slice(&moov);
        let total = u64::try_from(data.len()).unwrap_or(0);

        let mut reader = Counting::new(data);
        let top = read_top_level(&mut reader, total).expect("walks");
        let found = *top.iter().find(|b| b.box_type == *b"moov").expect("a moov");
        let bytes = read_box(&mut reader, &found).expect("reads");
        assert_eq!(bytes.len(), 64);
        assert_eq!(&bytes[4..8], b"moov");
    }

    #[test]
    fn a_box_claiming_past_the_end_is_refused_rather_than_clamped() {
        // The header says four kilobytes; forty bytes are there. Clamping to forty would
        // produce a box of a plausible wrong length, and every field read out of it would
        // be a plausible wrong answer.
        let mut data = box_of(32, b"ftyp");
        data.extend_from_slice(&4096u32.to_be_bytes());
        data.extend_from_slice(b"mdat");
        data.resize(72, 0);
        let total = u64::try_from(data.len()).unwrap_or(0);
        let mut reader = Counting::new(data);
        let err = read_top_level(&mut reader, total).expect_err("refused");
        match err {
            Mp4Error::TruncatedBox {
                box_type, declared, ..
            } => {
                assert_eq!(&box_type, b"mdat");
                assert_eq!(declared, 4096);
            }
            other => panic!("expected TruncatedBox, got {other:?}"),
        }
    }

    #[test]
    fn a_size_below_the_header_is_refused() {
        // A file long enough to hold a header, whose first box declares four bytes: fewer
        // than the eight its own header occupies.
        let mut data = 4u32.to_be_bytes().to_vec();
        data.extend_from_slice(b"free");
        data.resize(32, 0);
        let total = u64::try_from(data.len()).unwrap_or(0);
        let mut reader = Counting::new(data);
        let err = read_top_level(&mut reader, total).expect_err("refused");
        assert!(matches!(err, Mp4Error::InvalidBoxSize { .. }), "{err:?}");
    }

    #[test]
    fn a_64_bit_size_is_read_from_the_largesize_field() {
        let mut data = 1u32.to_be_bytes().to_vec();
        data.extend_from_slice(b"mdat");
        data.extend_from_slice(&(1_000_000u64 + 16).to_be_bytes());
        data.resize(1_000_016, 0);
        let total = u64::try_from(data.len()).unwrap_or(0);
        let mut reader = Counting::new(data);
        let top = read_top_level(&mut reader, total).expect("walks");
        let mdat = *top.first().expect("one box");
        assert_eq!(top.len(), 1);
        assert_eq!(mdat.size, 1_000_016);
        assert_eq!(mdat.header_len, 16);
    }

    #[test]
    fn a_size_of_zero_ends_the_walk_rather_than_looping() {
        let mut data = box_of(32, b"ftyp");
        let mut tail = vec![0u8; 40];
        tail[..4].copy_from_slice(&0u32.to_be_bytes()); // size 0: to the end
        tail[4..8].copy_from_slice(b"mdat");
        data.extend_from_slice(&tail);
        let total = u64::try_from(data.len()).unwrap_or(0);
        let mut reader = Counting::new(data);
        let top = read_top_level(&mut reader, total).expect("walks");
        assert_eq!(top.len(), 2);
        assert_eq!(top.last().expect("a last box").size, 40);
    }
}
