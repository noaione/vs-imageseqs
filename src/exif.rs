//! The exif orientation a file states, out of the TIFF header it stores it in.
//!
//! Three formats state an orientation in this shape and each carries it slightly
//! differently: a jpeg holds the TIFF header inside the `Exif\0\0` payload of an
//! `APP1` segment, a png holds a bare TIFF header in an `eXIf` chunk with no
//! such heading, and a webp holds the same payload a jpeg does in an `EXIF`
//! chunk. The payload itself is the same in all three, which is why the reader
//! lives here rather than in each of them.
//!
//! What this deliberately does **not** do is decide an orientation for a
//! container that states one normatively. An avif or a heif states its rotation
//! as `irot` and `imir` item properties, and those win over any exif tag beside
//! them; `src/formats/avif.rs` owns that reading and this module is not called
//! for those two containers.
//!
//! A tag whose value is not one of the eight codes, a payload that is truncated,
//! and an endianness marker that is neither `II` nor `MM` all state no
//! orientation rather than a wrong one, which is what the `image` reader did
//! with them too.

use crate::layout::Orientation;

/// The orientation an exif payload states, for a payload that states one.
///
/// The payload starts at the TIFF header, which is what every container above
/// hands over: a jpeg's `APP1` payload has its heading removed by the decoder
/// that reads it, and a png's `eXIf` chunk never had one.
#[must_use]
pub fn orientation_of(exif: &[u8]) -> Option<Orientation> {
    let reader = Tiff::new(exif)?;
    let ifd = usize::try_from(reader.u32(4)?).ok()?;
    let count = reader.u16(ifd)?;
    for index in 0..count {
        // The count is the first two bytes of the IFD and the entries follow it,
        // so entry `index` starts at `ifd + 2 + index * 12`. Each entry is the
        // tag, the type, the count, and then the value or its offset; the
        // orientation is a `SHORT` of one element, so its value is in the first
        // two bytes of the value field.
        let entry = ifd
            .checked_add(2)?
            .checked_add(usize::from(index).checked_mul(12)?)?;
        if reader.u16(entry) != Some(0x0112) {
            continue;
        }
        let value = reader.u16(entry.checked_add(8)?)?;
        return Orientation::from_exif(u8::try_from(value).ok()?);
    }
    None
}

/// A TIFF header, read with the endianness it states.
///
/// Both byte orders are read, because an exif payload states which one it uses
/// and a file from either kind of camera has to be read the same way. Every read
/// is bounds-checked against the payload, so a truncated one answers `None`
/// instead of reading past its own end — a release build aborts on a panic.
struct Tiff<'a> {
    bytes: &'a [u8],
    big_endian: bool,
}

impl<'a> Tiff<'a> {
    fn new(bytes: &'a [u8]) -> Option<Self> {
        let order = bytes.get(..2)?;
        let big_endian = match order {
            b"MM" => true,
            b"II" => false,
            _ => return None,
        };
        // The magic number is in the order the file states, so it is checked
        // one way or the other rather than literally.
        let magic = bytes.get(2..4)?;
        let expected = if big_endian {
            [0x00, 0x2a]
        } else {
            [0x2a, 0x00]
        };
        if magic != expected {
            return None;
        }
        Some(Self { bytes, big_endian })
    }

    fn u16(&self, at: usize) -> Option<u16> {
        let pair = self.bytes.get(at..at.checked_add(2)?)?;
        let pair: [u8; 2] = pair.try_into().ok()?;
        Some(if self.big_endian {
            u16::from_be_bytes(pair)
        } else {
            u16::from_le_bytes(pair)
        })
    }

    fn u32(&self, at: usize) -> Option<u32> {
        let quad = self.bytes.get(at..at.checked_add(4)?)?;
        let quad: [u8; 4] = quad.try_into().ok()?;
        Some(if self.big_endian {
            u32::from_be_bytes(quad)
        } else {
            u32::from_le_bytes(quad)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Tiff, orientation_of};
    use crate::layout::Orientation;

    /// A tiff header and one IFD entry stating `value`, in `order`.
    fn exif(order: &[u8; 2], value: u16) -> Vec<u8> {
        let big = order == b"MM";
        let mut bytes = order.to_vec();
        bytes.extend_from_slice(if big { &[0x00, 0x2a] } else { &[0x2a, 0x00] });
        // Offset of the first IFD, and its single entry.
        bytes.extend_from_slice(if big { &[0, 0, 0, 8] } else { &[8, 0, 0, 0] });
        bytes.extend_from_slice(if big { &[0, 1] } else { &[1, 0] });
        // Tag 0x0112, type SHORT, count 1, then the value in the first two bytes.
        bytes.extend_from_slice(if big { &[0x01, 0x12] } else { &[0x12, 0x01] });
        bytes.extend_from_slice(if big { &[0, 3] } else { &[3, 0] });
        bytes.extend_from_slice(if big { &[0, 0, 0, 1] } else { &[1, 0, 0, 0] });
        let encoded = if big {
            value.to_be_bytes()
        } else {
            value.to_le_bytes()
        };
        bytes.extend_from_slice(&encoded);
        bytes.extend_from_slice(&[0, 0]);
        // No next IFD. Zero is zero in either order.
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes
    }

    /// Both byte orders are read, and every exif code round trips.
    #[test]
    fn an_exif_tag_reads_in_either_byte_order() {
        for order in [b"II", b"MM"] {
            for code in 1..=8u8 {
                let bytes = exif(order, u16::from(code));
                assert_eq!(
                    orientation_of(&bytes).expect("a known code"),
                    Orientation::from_exif(code).expect("a known code"),
                    "{order:?} code {code}"
                );
            }
        }
    }

    /// A payload that states no orientation, or states one this build does not
    /// name, is not a wrong orientation.
    #[test]
    fn an_unusable_exif_payload_states_no_orientation() {
        assert_eq!(orientation_of(&[]), None);
        assert_eq!(orientation_of(b"XX\x2a\x00"), None);
        assert_eq!(orientation_of(b"II\x00\x00"), None);
        // A code outside the eight, and a code of zero.
        assert_eq!(orientation_of(&exif(b"II", 0)), None);
        assert_eq!(orientation_of(&exif(b"II", 9)), None);
        // A truncated entry: the value field is cut off.
        let mut short = exif(b"II", 6);
        short.truncate(18);
        assert_eq!(orientation_of(&short), None);
    }

    /// A payload whose first entry is another tag still finds the orientation
    /// when it is the second one, which is what a real `APP1` looks like: the
    /// orientation tag is written wherever the camera put it, not first.
    #[test]
    fn a_later_entry_states_the_orientation() {
        // Little endian, two entries, the second one the orientation.
        let mut bytes = vec![b'I', b'I', 0x2a, 0x00, 8, 0, 0, 0];
        bytes.extend_from_slice(&[2, 0]);
        // Entry one: an unrelated tag whose value is an offset, not a code.
        bytes.extend_from_slice(&[0x0f, 0x01, 3, 0, 1, 0, 0, 0, 0, 0, 0, 0]);
        // Entry two: tag 0x0112, SHORT, one element, value 6.
        bytes.extend_from_slice(&[0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0]);
        // No next IFD.
        bytes.extend_from_slice(&[0, 0, 0, 0]);

        let found = orientation_of(&bytes).expect("the second entry states it");
        assert_eq!(found, Orientation::Rotate90);
    }

    /// A tiff header this build cannot read states nothing.
    #[test]
    fn a_header_that_is_not_tiff_is_refused() {
        assert!(Tiff::new(b"").is_none());
        assert!(Tiff::new(b"II").is_none());
        assert!(Tiff::new(b"II\x2b\x00\x08\x00\x00\x00").is_none());
        assert!(Tiff::new(b"II\x2a\x00\x08\x00\x00\x00").is_some());
    }
}
