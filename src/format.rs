//! What kind of picture a path names, decided here rather than by a codec crate.
//!
//! Every container this plugin can read is named here, including the ones whose
//! decode is owned by a format module rather than by the generic still path:
//! jpeg xl, jpeg 2000 and both heif families are identified so that the route a
//! file takes can be decided in one place. [`Format::Other`] is left for a file
//! that is genuinely none of them, and it is the only answer that is not a
//! statement about the file.
//!
//! Two questions are answered, and they are deliberately separate:
//!
//! - [`Format::of_path`] names the format a path claims through its extension,
//!   which is the *preferred* format and not a promise about the bytes.
//! - [`Format::sniff`] names the format the first bytes of a file are, which is
//!   the *content* format.
//!
//! The `image` crate used to answer both with one call that consulted its own
//! extension table and signature table. Those tables are reproduced here, and
//! two things beyond them are recognized:
//!
//! - The `ftyp` box, which is what tells a heif family member from a jpeg 2000
//!   or a jpeg xl. Every brand this plugin decodes is mapped, so an `avif`, a
//!   `heic`, a `heif` and a still `avis` are told apart from each other and from
//!   an unrelated ISO base media file such as an `mp4`.
//! - The jpeg 2000 signature and the jpeg xl container, both of which the crate
//!   had no entry for, because the crate could not decode them.
//!
//! A file whose extension lies about it is decoded as what its bytes are, which
//! is what the old guess did and what a corpus that renamed a jpeg to `.png`
//! depends on. Where the two disagree the content wins, except for a format with
//! no signature at all: a targa is only ever named by its extension.

// The enum below is the complete list of what this plugin can be handed, so a
// variant and a helper a given migration has not reached yet are part of that
// description rather than leftovers.
#![allow(dead_code, reason = "the format inventory is deliberately complete")]

use std::path::Path;

/// The bytes read to decide what a file's signature is.
///
/// The longest signature in the table below is ten bytes, and the `ftyp` brand
/// sits at offset eight, so twelve bytes are read where sixteen were.
pub const SIGNATURE_BYTES: usize = 16;

/// A picture format this plugin decodes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Format {
    /// A png, which is also the container an apng is.
    Png,
    /// A jpeg.
    Jpeg,
    /// A gif, still or animated.
    Gif,
    /// A webp, still or animated.
    Webp,
    /// A tiff.
    Tiff,
    /// A directdraw surface, which holds block-compressed data.
    Dds,
    /// A windows bitmap.
    Bmp,
    /// A windows icon or cursor.
    Ico,
    /// A radiance hdr.
    Hdr,
    /// An openexr.
    Exr,
    /// A quite ok image.
    Qoi,
    /// A netpbm, in any of its seven variants.
    Pnm,
    /// A farbfeld.
    Farbfeld,
    /// A truevision targa, which no signature names.
    Tga,
    /// An avif, still or animated.
    Avif,
    /// A heif or heic, still or animated, since the two share a container.
    Heif,
    /// A jpeg xl, still or animated.
    Jxl,
    /// A jpeg 2000, in its own codestream or in a jp2 container.
    Jpeg2000,
    /// A file no table and no box names.
    Other,
}

impl Format {
    /// The format a path's extension claims.
    ///
    /// The match is on the extension alone and is case-insensitive, which is
    /// what the `image` table did: a `.JPG` is a jpeg and an `apng` extension
    /// claims png, because that is the container.
    #[must_use]
    pub fn of_path(path: &Path) -> Self {
        let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
            return Self::Other;
        };
        // The heif family and its five spellings, and the two containers the
        // crate had no case for, are the entries this table adds.
        match extension.to_ascii_lowercase().as_str() {
            "png" | "apng" => Self::Png,
            "jpg" | "jpeg" | "jfif" => Self::Jpeg,
            "gif" => Self::Gif,
            "webp" => Self::Webp,
            "tif" | "tiff" => Self::Tiff,
            "tga" => Self::Tga,
            "dds" => Self::Dds,
            "bmp" | "dib" => Self::Bmp,
            "ico" | "cur" => Self::Ico,
            "hdr" => Self::Hdr,
            "exr" => Self::Exr,
            "pbm" | "pam" | "ppm" | "pgm" | "pnm" => Self::Pnm,
            "ff" => Self::Farbfeld,
            "qoi" => Self::Qoi,
            "avif" => Self::Avif,
            "heic" | "heics" | "heif" | "hif" => Self::Heif,
            "jxl" => Self::Jxl,
            "jp2" | "j2k" | "jpc" | "jpf" | "jpx" | "j2c" => Self::Jpeg2000,
            _ => Self::Other,
        }
    }

    /// The format the start of a file is, for a signature that names one.
    ///
    /// The order matters for the masked entries: a heif's brand has to be read
    /// before the windows icon signature, because a short `ftyp` box begins with
    /// the same `00 00 01 00` an icon does, and the RIFF signature has to be
    /// tested before anything that could also match `RIFF????WEBP`.
    #[must_use]
    pub fn sniff(bytes: &[u8]) -> Self {
        // (signature, mask, format). An empty mask is an exact prefix.
        const TABLE: [(&[u8], &[u8], Format); 22] = [
            (b"\x89PNG\r\n\x1a\n", b"", Format::Png),
            (&[0xff, 0xd8, 0xff], b"", Format::Jpeg),
            (b"GIF89a", b"", Format::Gif),
            (b"GIF87a", b"", Format::Gif),
            (
                b"RIFF\0\0\0\0WEBP",
                b"\xFF\xFF\xFF\xFF\0\0\0\0",
                Format::Webp,
            ),
            (b"MM\x00*", b"", Format::Tiff),
            (b"II*\x00", b"", Format::Tiff),
            (b"DDS ", b"", Format::Dds),
            (b"BM", b"", Format::Bmp),
            (&[0, 0, 1, 0], b"", Format::Ico),
            (b"#?RADIANCE", b"", Format::Hdr),
            (b"\0\0\0\0ftypavif", b"\xFF\xFF\0\0", Format::Avif),
            (&[0x76, 0x2f, 0x31, 0x01], b"", Format::Exr),
            (b"qoif", b"", Format::Qoi),
            (b"P1", b"", Format::Pnm),
            (b"P2", b"", Format::Pnm),
            (b"P3", b"", Format::Pnm),
            (b"P4", b"", Format::Pnm),
            (b"P5", b"", Format::Pnm),
            (b"P6", b"", Format::Pnm),
            (b"P7", b"", Format::Pnm),
            (b"farbfeld", b"", Format::Farbfeld),
        ];

        if let Some(format) = sniff_iso_brand(bytes) {
            return format;
        }
        if bytes.starts_with(&[0x00, 0x00, 0x00, 0x0c, b'j', b'P', b' ', b' ']) {
            return Self::Jpeg2000;
        }
        if bytes.starts_with(&[0xff, 0x4f, 0xff, 0x51]) {
            return Self::Jpeg2000;
        }
        // A bare jpeg xl codestream, `FF0A`, and the container box, `JXL `.
        if bytes.starts_with(&[0xff, 0x0a])
            || bytes.starts_with(&[0x00, 0x00, 0x00, 0x0c, b'J', b'X', b'L', b' '])
        {
            return Self::Jxl;
        }
        for (signature, mask, format) in TABLE {
            if matches(bytes, signature, mask) {
                return format;
            }
        }
        Self::Other
    }
}

/// The family member an ISO base media file's brand names, when it is one this
/// plugin decodes.
///
/// The `ftyp` box is a four byte size, the four byte type, and then the major
/// brand: the major brand is read here, and the compatible brands that follow it
/// are not, because a file that lists `avif` as compatible while stating another
/// major brand is a file whose primary item is of that other brand.
fn sniff_iso_brand(bytes: &[u8]) -> Option<Format> {
    if bytes.len() < 12 || &bytes[4..8] != b"ftyp" {
        return None;
    }
    Some(match &bytes[8..12] {
        b"avif" | b"avis" => Format::Avif,
        b"heic" | b"heix" | b"heim" | b"heis" | b"hevc" | b"hevx" | b"hevm" | b"hevs" | b"mif1"
        | b"msf1" => Format::Heif,
        _ => return None,
    })
}

/// Whether `bytes` starts with `signature`, ignoring the bits `mask` clears.
fn matches(bytes: &[u8], signature: &[u8], mask: &[u8]) -> bool {
    if bytes.len() < signature.len() {
        return false;
    }
    bytes
        .iter()
        .zip(signature)
        .zip(mask.iter().chain(std::iter::repeat(&0xFF)))
        .all(|((&byte, &expected), &masked)| byte & masked == expected)
}

#[cfg(test)]
mod tests {
    use super::{Format, SIGNATURE_BYTES};

    fn path(name: &str) -> &std::path::Path {
        std::path::Path::new(name)
    }

    #[test]
    fn an_extension_names_the_format_it_always_did() {
        for (name, format) in [
            ("a.avif", Format::Avif),
            ("a.AVIF", Format::Avif),
            ("a.jpg", Format::Jpeg),
            ("a.jpeg", Format::Jpeg),
            ("a.jfif", Format::Jpeg),
            ("a.png", Format::Png),
            ("a.apng", Format::Png),
            ("a.gif", Format::Gif),
            ("a.webp", Format::Webp),
            ("a.tif", Format::Tiff),
            ("a.tiff", Format::Tiff),
            ("a.tga", Format::Tga),
            ("a.dds", Format::Dds),
            ("a.bmp", Format::Bmp),
            ("a.dib", Format::Bmp),
            ("a.ico", Format::Ico),
            ("a.cur", Format::Ico),
            ("a.hdr", Format::Hdr),
            ("a.exr", Format::Exr),
            ("a.pbm", Format::Pnm),
            ("a.pam", Format::Pnm),
            ("a.ppm", Format::Pnm),
            ("a.pgm", Format::Pnm),
            ("a.pnm", Format::Pnm),
            ("a.ff", Format::Farbfeld),
            ("a.qoi", Format::Qoi),
            ("a.heic", Format::Heif),
            ("a.heics", Format::Heif),
            ("a.heif", Format::Heif),
            ("a.hif", Format::Heif),
            ("a.jxl", Format::Jxl),
            ("a.jp2", Format::Jpeg2000),
            ("a.j2k", Format::Jpeg2000),
            ("a.j2c", Format::Jpeg2000),
            ("a.jpx", Format::Jpeg2000),
        ] {
            assert_eq!(Format::of_path(path(name)), format, "{name}");
        }
        // An extension nothing names, and no extension at all, both state
        // nothing rather than guessing.
        assert_eq!(Format::of_path(path("a")), Format::Other);
        assert_eq!(Format::of_path(path("a.psd")), Format::Other);
        assert_eq!(Format::of_path(path("a.png ")), Format::Other);
    }

    #[test]
    fn a_signature_names_the_format_its_bytes_are() {
        for (bytes, format) in [
            (&b"\x89PNG\r\n\x1a\n"[..], Format::Png),
            (&[0xff, 0xd8, 0xff][..], Format::Jpeg),
            (&b"GIF89a"[..], Format::Gif),
            (&b"GIF87a"[..], Format::Gif),
            (&b"RIFF\x00\x00\x00\x00WEBPVP8 "[..], Format::Webp),
            (&b"MM\x00*"[..], Format::Tiff),
            (&b"II*\x00"[..], Format::Tiff),
            (&b"DDS "[..], Format::Dds),
            (&b"BM"[..], Format::Bmp),
            (&[0x76, 0x2f, 0x31, 0x01][..], Format::Exr),
            (&b"qoif"[..], Format::Qoi),
            (&b"P6\n"[..], Format::Pnm),
            (&b"farbfeld"[..], Format::Farbfeld),
        ] {
            assert_eq!(Format::sniff(bytes), format);
        }
    }

    /// The two containers the crate had no signature for are named, so a file
    /// that reaches the generic path with one of them is refused as what it is
    /// rather than as an unknown.
    #[test]
    fn the_containers_with_no_crate_entry_are_named() {
        assert_eq!(
            Format::sniff(&b"\x00\x00\x00\x0cjP  \r\n\x87\n"[..]),
            Format::Jpeg2000
        );
        assert_eq!(
            Format::sniff(&[0xff, 0x4f, 0xff, 0x51, 0x00]),
            Format::Jpeg2000
        );
        assert_eq!(Format::sniff(&[0xff, 0x0a, 0x00]), Format::Jxl);
        assert_eq!(
            Format::sniff(&b"\x00\x00\x00\x0cJXL \r\n\x87\n"[..]),
            Format::Jxl
        );
    }

    /// The `ftyp` brand tells the heif family apart, and a brand this plugin
    /// does not decode is not mistaken for one it does.
    #[test]
    fn the_iso_brand_tells_the_heif_family_apart() {
        for (brand, format) in [
            (&b"avif"[..], Format::Avif),
            (&b"avis"[..], Format::Avif),
            (&b"heic"[..], Format::Heif),
            (&b"heix"[..], Format::Heif),
            (&b"hevc"[..], Format::Heif),
            (&b"heim"[..], Format::Heif),
            (&b"mif1"[..], Format::Heif),
            (&b"msf1"[..], Format::Heif),
        ] {
            let mut bytes = b"\x00\x00\x00\x18ftyp".to_vec();
            bytes.extend_from_slice(brand);
            assert_eq!(Format::sniff(&bytes), format, "{brand:?}");
        }
        // An mp4, a mov and a plain ISO base media file are none of ours, and
        // neither is a file too short to hold a brand.
        for brand in [&b"isom"[..], &b"mp41"[..], &b"qt  "[..], &b"crx "[..]] {
            let mut bytes = b"\x00\x00\x00\x18ftyp".to_vec();
            bytes.extend_from_slice(brand);
            assert_eq!(Format::sniff(&bytes), Format::Other, "{brand:?}");
        }
        assert_eq!(Format::sniff(b"ftyp"), Format::Other);
        assert_eq!(Format::sniff(b""), Format::Other);
    }

    /// A short `ftyp` box starts with the bytes an icon does, so the brand is
    /// read before the icon signature rather than after it.
    #[test]
    fn a_short_ftyp_box_is_a_brand_not_an_icon() {
        assert_eq!(
            Format::sniff(&b"\x00\x00\x01\x00ftypavif"[..]),
            Format::Avif
        );
        assert_eq!(
            Format::sniff(&b"\x00\x00\x01\x00ftypheic"[..]),
            Format::Heif
        );
        // A real icon is still an icon.
        assert_eq!(Format::sniff(&[0, 0, 1, 0, 1, 0][..]), Format::Ico);
    }

    /// A content format wins over an extension that lies.
    #[test]
    fn a_lied_about_extension_is_read_as_what_the_bytes_are() {
        assert_eq!(Format::sniff(&b"\xff\xd8\xff\xe0"[..]), Format::Jpeg);
        assert_eq!(Format::of_path(path("x.png")), Format::Png);
        // The RIFF mask clears the four size bytes before matching WEBP.
        assert_eq!(
            Format::sniff(&b"RIFF\x12\x34\x56\x78WEBPVP8L"[..]),
            Format::Webp
        );
        assert_eq!(Format::sniff(b"RIFF\x00\x00\x00\x00WAVE"), Format::Other);
        assert_eq!(Format::sniff(&[0u8; SIGNATURE_BYTES]), Format::Other);
        assert_eq!(Format::sniff(b"\x89PNG"), Format::Other);
    }
}
