//! The crate-wide error type.
//!
//! Every fallible parse returns [`Mp4Error`]. Parsing is total in the useful
//! sense: an unknown box type is not an error, and a malformed box yields a
//! typed error the caller can skip past rather than a panic.

use alloc::string::String;
use core::fmt;

use crate::container::BoxType;

/// Errors returned by box and metadata parsers.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Mp4Error {
    /// A box declared a size larger than the bytes available.
    TruncatedBox {
        /// The box's four-character type.
        box_type: BoxType,
        /// The size the box declared.
        declared: u64,
        /// The bytes actually available.
        available: usize,
    },
    /// A box declared a size that cannot be valid — below its own header, or
    /// beyond the format's ceiling.
    InvalidBoxSize {
        /// The box's type.
        box_type: BoxType,
        /// The offending size.
        size: u64,
    },
    /// A box's declared size did not agree with its contents, which is how
    /// truncation of a parent is detected.
    SizeMismatch {
        /// The box's type.
        box_type: BoxType,
        /// The size the box declared.
        declared: u64,
        /// The size its content occupied.
        actual: u64,
    },
    /// A `uuid` box carried a malformed or unrecognised extended type.
    InvalidExtendedType,
    /// An `ilst` metadata item used a type this crate does not model, or a
    /// value whose `data` box was malformed.
    MalformedMetadataItem {
        /// The atom name, e.g. `©nam`.
        atom: String,
        /// What was wrong.
        reason: &'static str,
    },
    /// An iTunes metadata string was not valid UTF-8.
    InvalidUtf8,
    /// A `chpl` (Nero chapter list) box was structurally invalid.
    MalformedChapters {
        /// What was wrong.
        reason: &'static str,
    },
    /// A required box was absent from a structure that cannot be built without
    /// it.
    MissingBox {
        /// The box type that was required.
        box_type: BoxType,
    },
    /// A fixed-width field ran past the end of its parent box.
    TruncatedField {
        /// The field name, for the error message.
        field: &'static str,
        /// Bytes needed.
        expected: usize,
        /// Bytes present.
        got: usize,
    },
    /// A numeric field could not be represented in the requested width.
    ValueOutOfRange {
        /// The field name.
        field: &'static str,
        /// The value that did not fit.
        value: i64,
    },
}

impl fmt::Display for Mp4Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Mp4Error::TruncatedBox {
                box_type,
                declared,
                available,
            } => write!(
                f,
                "box `{}` declares {declared} byte(s) but only {available} remain",
                crate::container::display_type(box_type)
            ),
            Mp4Error::InvalidBoxSize { box_type, size } => {
                write!(
                    f,
                    "box `{}` has an unusable size of {size}",
                    crate::container::display_type(box_type)
                )
            }
            Mp4Error::SizeMismatch {
                box_type,
                declared,
                actual,
            } => write!(
                f,
                "box `{}` declares {declared} byte(s) but its content is {actual}",
                crate::container::display_type(box_type)
            ),
            Mp4Error::InvalidExtendedType => {
                write!(f, "`uuid` box has a malformed extended type")
            }
            Mp4Error::MalformedMetadataItem { atom, reason } => {
                write!(f, "metadata item `{atom}` is malformed: {reason}")
            }
            Mp4Error::InvalidUtf8 => write!(f, "string is not valid UTF-8"),
            Mp4Error::MalformedChapters { reason } => {
                write!(f, "chapter list is malformed: {reason}")
            }
            Mp4Error::MissingBox { box_type } => {
                write!(
                    f,
                    "required box `{}` is missing",
                    crate::container::display_type(box_type)
                )
            }
            Mp4Error::TruncatedField {
                field,
                expected,
                got,
            } => write!(
                f,
                "field `{field}` needs {expected} byte(s) but only {got} remain"
            ),
            Mp4Error::ValueOutOfRange { field, value } => {
                write!(f, "value {value} does not fit field `{field}`")
            }
        }
    }
}

impl core::error::Error for Mp4Error {}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic
    )]
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn display_is_stable() {
        let e = Mp4Error::TruncatedBox {
            box_type: *b"moov",
            declared: 1000,
            available: 12,
        };
        assert_eq!(
            e.to_string(),
            "box `moov` declares 1000 byte(s) but only 12 remain"
        );
        assert_eq!(
            Mp4Error::MissingBox { box_type: *b"trak" }.to_string(),
            "required box `trak` is missing"
        );
        assert_eq!(
            Mp4Error::TruncatedField {
                field: "timescale",
                expected: 4,
                got: 2
            }
            .to_string(),
            "field `timescale` needs 4 byte(s) but only 2 remain"
        );
    }

    #[test]
    fn is_std_error() {
        fn assert_error<E: core::error::Error>(_: &E) {}
        assert_error(&Mp4Error::InvalidUtf8);
    }
}
