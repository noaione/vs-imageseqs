//! Container reads for a png that the `image` decoder has no accessor for.
//!
//! `image` hands out the pixels of a png, its size and its ICC profile, but not
//! the `cICP` chunk, which is where a png states its own colour in the same
//! H.273 code points the other containers use. The chunk is a handful of bytes
//! in the metadata at the front of the file, so it is read from the file itself
//! rather than by decoding the picture; see
//! `docs/improvements/08-color-metadata.md`.
//!
//! This module decodes nothing, so it has no `handles` and no `decode`; the
//! `image` png decoder stays the one that produces the samples.

use std::{fs::File, io::Read, path::Path};

use crate::color::Cicp;

/// The bytes every png starts with, which is also how a file that is not one is
/// declined without reading further.
const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Bytes of the checksum that follows every chunk payload.
const CRC_LENGTH: usize = 4;

/// How far into a file the chunks are read. Every writer that writes a `cICP`
/// writes it among the chunks that come before the image data, and this is far
/// past the metadata any real file holds.
const CHUNK_LIMIT: usize = 1024 * 1024;

/// The colour description a png states with a `cICP` chunk, or `None` when it
/// has none and when it is not a png at all.
pub fn cicp(path: &Path) -> Option<Cicp> {
    if !has_png_extension(path) {
        return None;
    }
    cicp_from(File::open(path).ok()?)
}

/// Reads chunks until the image data starts, and answers what a `cICP` among
/// them states.
///
/// A `cICP` written after the image data is not what a png states about its
/// samples, so the walk stops there rather than reading the whole file. A
/// truncated or malformed chunk ends it too, which leaves that file with the
/// properties its decoded color type alone describes.
fn cicp_from(mut file: impl Read) -> Option<Cicp> {
    let mut signature = [0; 8];
    file.read_exact(&mut signature).ok()?;
    if signature != SIGNATURE {
        return None;
    }
    let mut read = signature.len();
    loop {
        let mut header = [0; 8];
        file.read_exact(&mut header).ok()?;
        let length = usize::try_from(u32::from_be_bytes(header[..4].try_into().ok()?)).ok()?;
        let kind = &header[4..];
        if kind == b"IDAT" || kind == b"IEND" {
            return None;
        }
        read = read.checked_add(header.len() + length + CRC_LENGTH)?;
        if read > CHUNK_LIMIT {
            return None;
        }
        let mut payload = vec![0; length];
        file.read_exact(&mut payload).ok()?;
        file.read_exact(&mut [0; CRC_LENGTH]).ok()?;
        if kind == b"cICP" {
            return cicp_chunk(&payload);
        }
    }
}

/// The colour description a `cICP` chunk payload states.
///
/// The payload is four bytes: the primaries, transfer and matrix code points,
/// then the video full range flag, each of them the parameter of the same name
/// in h.273. The code points are single bytes here, unlike the 16 bit fields an
/// `nclx` box holds, so a file that states one this module has no property for
/// states it as it is: the mapping in [`Cicp`] is what turns a code point into a
/// property, or leaves it unset.
///
/// The flag is a whole byte that conforming files write as `0` or `1` (the png
/// specification's own examples are `09 12 00 01` and `01 01 00 00`), so it is
/// read as a flag rather than as the top bit of a bit field the way an `nclx`
/// box stores it. Any nonzero byte is the full range, which reads a file that
/// carried the `nclx` convention into this chunk as the writer meant it.
fn cicp_chunk(payload: &[u8]) -> Option<Cicp> {
    let [primaries, transfer, matrix, full_range, ..] = payload else {
        return None;
    };
    Some(Cicp {
        primaries: *primaries,
        transfer: *transfer,
        matrix: *matrix,
        full_range: *full_range != 0,
    })
}

fn has_png_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A png signature, then `chunks`, each `(kind, payload)`.
    fn file(chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut file = SIGNATURE.to_vec();
        for (kind, payload) in chunks {
            file.extend_from_slice(
                &u32::try_from(payload.len())
                    .expect("chunk size")
                    .to_be_bytes(),
            );
            file.extend_from_slice(*kind);
            file.extend_from_slice(payload);
            file.extend_from_slice(&[0; CRC_LENGTH]);
        }
        file
    }

    #[test]
    fn a_cicp_chunk_states_the_colour() {
        // The png specification's own example of a bt.2100 hlg full range
        // image, whose matrix is 0 because rgb is the only colour model a png
        // has.
        let bytes = file(&[(b"cICP", &[9, 18, 0, 1])]);
        let cicp = cicp_from(&bytes[..]).expect("a stated colour");
        assert_eq!(cicp.primaries, 9);
        assert_eq!(cicp.transfer, 18);
        assert_eq!(cicp.matrix, 0);
        assert!(cicp.full_range);
    }

    #[test]
    fn the_full_range_flag_is_a_byte_not_a_bit() {
        for (flags, full_range) in [(0u8, false), (1, true)] {
            let bytes = file(&[(b"cICP", &[1, 1, 0, flags])]);
            assert_eq!(
                cicp_from(&bytes[..]).expect("a stated colour").full_range,
                full_range,
                "flag {flags}"
            );
        }
        // A writer that carried the `nclx` convention into this chunk set the
        // top bit instead, and meant the same thing by it.
        let bytes = file(&[(b"cICP", &[9, 10, 0, 0x80])]);
        assert!(cicp_from(&bytes[..]).expect("a stated colour").full_range);
    }

    #[test]
    fn chunks_before_the_cicp_chunk_are_skipped() {
        let bytes = file(&[
            (b"IHDR", &[0; 13]),
            (b"sRGB", &[0]),
            (b"cICP", &[1, 13, 0, 1]),
        ]);
        assert!(cicp_from(&bytes[..]).is_some());
    }

    #[test]
    fn a_png_without_a_cicp_chunk_states_nothing() {
        let bytes = file(&[(b"IHDR", &[0; 13]), (b"IDAT", &[0; 4])]);
        assert!(cicp_from(&bytes[..]).is_none());
    }

    #[test]
    fn a_cicp_chunk_after_the_image_data_states_nothing() {
        let bytes = file(&[(b"IHDR", &[0; 13]), (b"IDAT", &[0; 4])]);
        let mut with_trailing = bytes.clone();
        with_trailing.extend_from_slice(&file(&[(b"cICP", &[1, 13, 0, 1])])[8..]);
        assert!(cicp_from(&with_trailing[..]).is_none());
    }

    #[test]
    fn a_file_that_is_not_a_png_states_nothing() {
        let mut bytes = b"GIF89a".to_vec();
        bytes.extend_from_slice(&[0; 64]);
        assert!(cicp_from(&bytes[..]).is_none());
    }

    #[test]
    fn a_short_cicp_chunk_states_nothing() {
        let bytes = file(&[(b"cICP", &[1, 13, 0])]);
        assert!(cicp_from(&bytes[..]).is_none());
    }

    #[test]
    fn a_truncated_chunk_states_nothing() {
        let bytes = file(&[(b"IHDR", &[0; 13])]);
        let truncated = &bytes[..bytes.len() - CRC_LENGTH];
        assert!(cicp_from(truncated).is_none());
    }

    #[test]
    fn only_png_extensions_are_read() {
        for path in ["a.png", "b.PNG"] {
            assert!(has_png_extension(Path::new(path)), "{path}");
        }
        for path in ["a.jpg", "b.apng", "c", "d.pngx"] {
            assert!(!has_png_extension(Path::new(path)), "{path}");
        }
    }
}
