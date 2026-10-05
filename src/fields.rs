//! Big-endian field readers.
//!
//! ISO-BMFF is big-endian throughout, and every field is at a fixed offset
//! inside a parent. Getting an offset wrong is the most common defect in a
//! hand-written parser, so every read here is bounds-checked and returns
//! [`Mp4Error::TruncatedField`] rather than panicking.

use alloc::vec::Vec;

use crate::error::Mp4Error;

/// Read a big-endian `u8`.
pub fn u8_at(data: &[u8], offset: usize, field: &'static str) -> Result<u8, Mp4Error> {
    data.get(offset).copied().ok_or(Mp4Error::TruncatedField {
        field,
        expected: offset + 1,
        got: data.len(),
    })
}

/// Read a big-endian `u16`.
pub fn u16_at(data: &[u8], offset: usize, field: &'static str) -> Result<u16, Mp4Error> {
    let s = data
        .get(offset..offset + 2)
        .and_then(|s| <[u8; 2]>::try_from(s).ok())
        .ok_or(Mp4Error::TruncatedField {
            field,
            expected: offset + 2,
            got: data.len(),
        })?;
    Ok(u16::from_be_bytes(s))
}

/// Read a big-endian `u32`.
pub fn u32_at(data: &[u8], offset: usize, field: &'static str) -> Result<u32, Mp4Error> {
    let s = data
        .get(offset..offset + 4)
        .and_then(|s| <[u8; 4]>::try_from(s).ok())
        .ok_or(Mp4Error::TruncatedField {
            field,
            expected: offset + 4,
            got: data.len(),
        })?;
    Ok(u32::from_be_bytes(s))
}

/// Read a big-endian `i32`.
pub fn i32_at(data: &[u8], offset: usize, field: &'static str) -> Result<i32, Mp4Error> {
    Ok(u32_at(data, offset, field)? as i32)
}

/// Read a big-endian `u64`.
pub fn u64_at(data: &[u8], offset: usize, field: &'static str) -> Result<u64, Mp4Error> {
    let s = data
        .get(offset..offset + 8)
        .and_then(|s| <[u8; 8]>::try_from(s).ok())
        .ok_or(Mp4Error::TruncatedField {
            field,
            expected: offset + 8,
            got: data.len(),
        })?;
    Ok(u64::from_be_bytes(s))
}

/// Read the 4-byte version at the head of a full box.
///
/// A "full box" prefixes its payload with a version and flags, which is why
/// every field in such a box is offset by four from where you might expect.
pub fn version(data: &[u8]) -> Result<u8, Mp4Error> {
    u8_at(data, 0, "version")
}

/// Read the 3-byte flags that follow a full box's version.
pub fn flags(data: &[u8]) -> Result<u32, Mp4Error> {
    let s = data
        .get(1..4)
        .and_then(|s| <[u8; 3]>::try_from(s).ok())
        .ok_or(Mp4Error::TruncatedField {
            field: "flags",
            expected: 4,
            got: data.len(),
        })?;
    Ok(u32::from_be_bytes([0, s[0], s[1], s[2]]))
}

/// Append a big-endian `u32`.
pub fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

/// Append a big-endian `i32`.
pub fn push_i32(out: &mut Vec<u8>, v: i32) {
    out.extend_from_slice(&v.to_be_bytes());
}

/// Append a big-endian `u16`.
pub fn push_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}

/// Append a big-endian `u64`.
pub fn push_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_be_bytes());
}

/// The 16.16 fixed-point format `mvhd` uses for its rate and volume.
///
/// MP4 stores duration and volume as 16.16 (or 8.8 for the volume) fixed point.
/// Converting through `f64` is exact for the ranges involved — a 32-bit integer
/// part over 16 fractional bits fits an f64 mantissa — so no precision is lost.
#[must_use]
pub fn fixed_16_16_to_f64(raw: u32) -> f64 {
    f64::from(raw) / 65536.0
}

/// The 8.8 fixed-point format `mvhd` uses for its volume.
#[must_use]
pub fn fixed_8_8_to_f64(raw: u16) -> f64 {
    f64::from(raw) / 256.0
}

/// Encode an `f64` as 16.16 fixed point, saturating rather than wrapping.
///
/// Saturating matters: a NaN or infinity from a bad rate must not become a
/// wrapped small number that looks valid.
#[must_use]
pub fn f64_to_fixed_16_16(v: f64) -> u32 {
    if !v.is_finite() {
        return 0;
    }
    let scaled = v * 65536.0;
    if scaled <= 0.0 {
        return 0;
    }
    if scaled >= f64::from(u32::MAX) {
        return u32::MAX;
    }
    scaled as u32
}

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

    #[test]
    fn reads_are_bounds_checked_not_panicking() {
        let data = [0x00, 0x01, 0x02];
        assert_eq!(u16_at(&data, 0, "x"), Ok(1));
        assert!(matches!(
            u32_at(&data, 0, "x"),
            Err(Mp4Error::TruncatedField { .. })
        ));
        assert!(matches!(
            u64_at(&data, 0, "x"),
            Err(Mp4Error::TruncatedField { .. })
        ));
        assert!(matches!(
            u8_at(&data, 99, "x"),
            Err(Mp4Error::TruncatedField { .. })
        ));
    }

    #[test]
    fn full_box_header_is_version_then_flags() {
        let data = [0x01, 0x00, 0x00, 0x03, 0xAA];
        assert_eq!(version(&data), Ok(1));
        assert_eq!(flags(&data), Ok(3));
    }

    #[test]
    fn fixed_point_round_trips() {
        assert_eq!(fixed_16_16_to_f64(65536), 1.0);
        assert_eq!(fixed_16_16_to_f64(0), 0.0);
        assert!((fixed_16_16_to_f64(1_000_000) - 15.2587).abs() < 1e-3);
        assert_eq!(fixed_8_8_to_f64(256), 1.0);

        assert_eq!(f64_to_fixed_16_16(1.0), 65536);
        assert_eq!(f64_to_fixed_16_16(0.0), 0);
    }

    #[test]
    fn fixed_point_saturates_rather_than_wrapping() {
        assert_eq!(f64_to_fixed_16_16(-5.0), 0, "negative clamps to zero");
        assert_eq!(f64_to_fixed_16_16(1e30), u32::MAX);
        assert_eq!(f64_to_fixed_16_16(f64::NAN), 0);
        assert_eq!(f64_to_fixed_16_16(f64::INFINITY), 0);
    }

    #[test]
    fn push_helpers_are_big_endian() {
        let mut out = Vec::new();
        push_u32(&mut out, 0x0102_0304);
        assert_eq!(out, vec![1, 2, 3, 4]);
        out.clear();
        push_i32(&mut out, -1);
        assert_eq!(out, vec![0xFF; 4]);
    }
}
