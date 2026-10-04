//! What a file's own bytes say it is.
//!
//! Plan 34's phase 1 asks for "one content identification and one saved decoder
//! plan". Today every format module's `owns` answers from the file's *extension*,
//! so a correct file under a wrong name is handed to a decoder that cannot read
//! it: `target/bench/routing.py` measures 59 of 84 renamed copies reading
//! differently from their source.
//!
//! This module is the identification half and nothing else. It reads a prefix and
//! answers which format the bytes are, or `None` when they say nothing -- and for
//! the formats that have no reliable leading signature it answers from the
//! extension, which is what the plan says to do for them:
//!
//! > Strong signatures win over extensions. Extend the table for BigTIFF, CUR and
//! > RGBE, then validate the selected format's header. TGA needs the extension as a
//! > hint because it lacks a reliable leading signature; known conflicting magic
//! > still wins.
//!
//! [`identify`] is therefore the strong half and [`from_extension`] the weak one,
//! kept apart so that a caller can tell which answered. Wiring them into `describe`
//! and `format_decoder` is the next slice; this one exists to be tested against
//! every fixture on its own.

#![allow(dead_code)]

use std::path::Path;

/// A leading-bytes length that covers every signature here.
///
/// The longest is the 12 byte ISO base media and JPEG 2000 brand, so a prefix of
/// sixteen is enough for all of them and keeps the read to one small chunk.
pub const HEAD_BYTES: usize = 16;

/// The formats this tree routes stills to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Avif,
    Bmp,
    Dds,
    Exr,
    Farbfeld,
    Gif,
    Heif,
    Hdr,
    Ico,
    Jp2,
    Jpeg,
    Jxl,
    Png,
    Pnm,
    Qoi,
    Tga,
    Tiff,
    Webp,
}

impl Format {
    /// The extensions this format is named with, without their dot.
    ///
    /// A file is only ever recognised by its extension as a *hint*, so an entry
    /// here that no signature can confirm is a format the extension alone can
    /// choose.
    #[must_use]
    pub const fn extensions(self) -> &'static [&'static str] {
        match self {
            Self::Avif => &["avif"],
            Self::Bmp => &["bmp", "dib"],
            Self::Dds => &["dds"],
            Self::Exr => &["exr"],
            Self::Farbfeld => &["ff", "farbfeld"],
            Self::Gif => &["gif"],
            Self::Heif => &["heic", "heif", "hif", "avci"],
            Self::Hdr => &["hdr", "rgbe", "pic"],
            Self::Ico => &["ico", "cur"],
            Self::Jp2 => &["jp2", "j2k", "jpc", "jpf", "jpx"],
            Self::Jpeg => &["jpg", "jpeg", "jpe", "jfif"],
            Self::Jxl => &["jxl"],
            Self::Png => &["png", "apng"],
            Self::Pnm => &["pbm", "pgm", "ppm", "pam", "pnm"],
            Self::Qoi => &["qoi"],
            Self::Tga => &["tga", "targa", "icb", "vda", "vst"],
            Self::Tiff => &["tif", "tiff"],
            Self::Webp => &["webp"],
        }
    }

    /// Whether a leading signature can name this format at all.
    ///
    /// Targa is one: it has no leading magic, only an optional footer, so its
    /// extension is not a hint but the whole answer. The icon family is the other,
    /// and not for want of magic -- `\0\0\1\0` and `\0\0\2\0` are a Targa type 1
    /// or type 2 header as much as they are an icon, so the two collide and the
    /// extension is what separates them. A bare DIB is the third, and the plan
    /// gives it "a distinct extension-assisted structural probe" rather than a
    /// signature.
    #[must_use]
    pub const fn has_signature(self) -> bool {
        !matches!(self, Self::Tga | Self::Ico)
    }
}

/// The format a file's extension names, when it names one.
///
/// This is the weak half: it is what a caller falls back to when the bytes say
/// nothing, and it is what plan 34 wants confined to formats that have no
/// signature to win with.
#[must_use]
pub fn from_extension(path: &Path) -> Option<Format> {
    let extension = path.extension()?.to_str()?;
    const ALL: [Format; 18] = [
        Format::Avif,
        Format::Bmp,
        Format::Dds,
        Format::Exr,
        Format::Farbfeld,
        Format::Gif,
        Format::Heif,
        Format::Hdr,
        Format::Ico,
        Format::Jp2,
        Format::Jpeg,
        Format::Jxl,
        Format::Png,
        Format::Pnm,
        Format::Qoi,
        Format::Tga,
        Format::Tiff,
        Format::Webp,
    ];
    ALL.into_iter().find(|format| {
        format
            .extensions()
            .iter()
            .any(|known| extension.eq_ignore_ascii_case(known))
    })
}

/// The format a file's leading bytes are, or `None` when they say nothing.
///
/// A signature is a claim about content, so it is checked before any extension is
/// consulted -- that ordering is the whole of what plan 34 asks for, and it is why
/// this answers `None` rather than guessing when the bytes are inconclusive.
#[must_use]
pub fn identify(head: &[u8]) -> Option<Format> {
    let starts = |magic: &[u8]| head.starts_with(magic);
    let at = |offset: usize, magic: &[u8]| {
        head.get(offset..offset + magic.len())
            .is_some_and(|found| found == magic)
    };

    // The ISO base media format puts its brand after a box length and `ftyp`, and
    // avif and heif share the container, so the brand is what separates them.
    if at(4, b"ftyp") {
        let brand = head.get(8..12).unwrap_or_default();
        return match brand {
            b"avif" | b"avis" => Some(Format::Avif),
            b"heic" | b"heix" | b"hevc" | b"hevx" | b"mif1" | b"msf1" | b"heim" | b"heis"
            | b"hevm" | b"hevs" => Some(Format::Heif),
            _ => None,
        };
    }
    // The JPEG 2000 family, whose signature box names the codestream's kind.
    if starts(b"\x00\x00\x00\x0cjP  \r\n\x87\n") {
        return Some(Format::Jp2);
    }
    if starts(b"\xff\x4f\xff\x51") {
        return Some(Format::Jp2);
    }
    // A bare JPEG 2000 codestream starts with the SOC marker and a SIZ marker.
    if starts(b"\xff\x4f") {
        return Some(Format::Jp2);
    }
    if starts(b"\x89PNG\r\n\x1a\n") {
        return Some(Format::Png);
    }
    if starts(b"\xff\xd8\xff") {
        return Some(Format::Jpeg);
    }
    if starts(b"GIF87a") || starts(b"GIF89a") {
        return Some(Format::Gif);
    }
    if starts(b"RIFF") && at(8, b"WEBP") {
        return Some(Format::Webp);
    }
    if starts(b"farbfeld") {
        return Some(Format::Farbfeld);
    }
    if starts(b"qoif") {
        return Some(Format::Qoi);
    }
    if starts(b"DDS ") {
        return Some(Format::Dds);
    }
    if starts(b"BM") {
        return Some(Format::Bmp);
    }
    // The icon and cursor magic is deliberately **not** identified here. Both
    // `\0\0\1\0` and `\0\0\2\0` are also the first four bytes of a Targa whose
    // image type is 1 or 2, which is most of them, so a content-first rule that
    // claimed them would take every such Targa away from the reader that owns it.
    // Those two bytes are the whole of the leading signature there is, so the
    // extension has to arbitrate; see `Format::has_signature`.
    // EXR's magic is a version-bearing word, and every version starts the same.
    if starts(b"\x76\x2f\x31\x01") {
        return Some(Format::Exr);
    }
    // Classic TIFF and BigTIFF differ in the version word, and the plan asks for
    // both: the pinned crate decodes BigTIFF, so gating it out was the bug.
    if starts(b"II\x2a\x00") || starts(b"MM\x00\x2a") {
        return Some(Format::Tiff);
    }
    if starts(b"II\x2b\x00") || starts(b"MM\x00\x2b") {
        return Some(Format::Tiff);
    }
    // Radiance, both the long spelling and the four byte one the plan flags as
    // having been compared by the wrong prefix length.
    if starts(b"#?RADIANCE") || starts(b"#?RGBE") {
        return Some(Format::Hdr);
    }
    // The netpbm magic is two bytes and then whitespace, so `P6 ` is a picture
    // where `P6x` is not.
    if let [b'P', kind @ b'1'..=b'7', rest @ ..] = head
        && kind.is_ascii()
        && rest.first().is_none_or(u8::is_ascii_whitespace)
    {
        return Some(Format::Pnm);
    }
    // The JPEG XL container, and the bare codestream beside it.
    if starts(b"\x00\x00\x00\x0cJXL \r\n\x87\n") {
        return Some(Format::Jxl);
    }
    if starts(b"\xff\x0a") {
        return Some(Format::Jxl);
    }
    None
}

/// Reads the leading bytes of a file, or nothing when it cannot be read.
///
/// A file that cannot be opened reads as no content rather than as an error: a
/// caller asking whether a format owns a path is asking a question that a missing
/// file answers, and the format that ends up declining it reports the reason.
fn head_of(path: &Path) -> Vec<u8> {
    use std::io::Read;
    #[cfg(test)]
    HEAD_READS.with(|count| count.set(count.get() + 1));
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let mut head = Vec::with_capacity(HEAD_BYTES);
    let _ = file.take(HEAD_BYTES as u64).read_to_end(&mut head);
    head
}

/// How many times this thread has read a file's head, which is how a probe and
/// a decode are checked to read it once between them.
#[cfg(test)]
pub(crate) fn head_reads() -> usize {
    HEAD_READS.with(std::cell::Cell::get)
}

/// Starts [`head_reads`] from zero.
#[cfg(test)]
pub(crate) fn reset_head_reads() {
    HEAD_READS.with(|count| count.set(0));
}

#[cfg(test)]
thread_local! {
    static HEAD_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The one format that owns a path, from one read of its leading bytes.
///
/// This is [`identify`] with [`from_extension`] as the fallback, which is the
/// whole of the routing rule in one place. A caller that asks this asks the same
/// question every other caller asks, so a probe and a decode cannot settle on
/// different formats -- which a chain of `owns` calls could, because each module
/// answered for itself and each answer cost an open.
///
/// `None` means no format here owns the file, which is what sends it to the
/// generic decoder.
#[must_use]
pub fn route(path: &Path) -> Option<Format> {
    route_from(&head_of(path), path)
}

/// [`route`] for a caller that has already read a file's leading bytes.
#[must_use]
pub fn route_from(head: &[u8], path: &Path) -> Option<Format> {
    identify(head).or_else(|| from_extension(path))
}

/// Whether a format owns a file: the content decides, the extension is the hint.
///
/// This is the rule plan 34 states in one place -- "strong signatures win over
/// extensions" -- and it is what every still module's `owns` delegates to. A
/// format with a signature of its own claims a file only when that signature is
/// there, however the file is named; a format without one falls back on the name,
/// which is Targa and a bare DIB and nothing else.
///
/// It is [`route_from`] against one format, so the question has one answer here
/// rather than two.
#[must_use]
pub fn owns(format: Format, path: &Path) -> bool {
    route_from(&head_of(path), path) == Some(format)
}

/// Whether `format` owns a file whose route a probe already settled.
///
/// A saved route answers without a read, which is what stops a module the
/// router already chose from opening the file to ask the question again. `None`
/// is a caller that built its own description, and that falls back to the head.
#[must_use]
pub fn route_agrees(route: Option<Format>, format: Format, path: &Path) -> bool {
    match route {
        Some(saved) => saved == format,
        None => owns(format, path),
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn head_of(path: &Path) -> Vec<u8> {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        bytes[..bytes.len().min(HEAD_BYTES)].to_vec()
    }

    #[test]
    fn a_signature_never_contradicts_the_extension_that_names_the_same_format() {
        // Every fixture whose format has a signature must be identified as that
        // format from its bytes alone. A contradiction here means the table names
        // a format one way and the file states another, which is the class of bug
        // plan 34 is about -- so it is an assertion rather than a comparison.
        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures");
        let mut checked = 0;
        let mut skipped = Vec::new();
        for entry in std::fs::read_dir(&directory).expect("the fixtures are readable") {
            let path = entry.expect("a fixture").path();
            if !path.is_file() {
                continue;
            }
            let Some(named) = from_extension(&path) else {
                continue;
            };
            if !named.has_signature() {
                skipped.push(
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                );
                continue;
            }
            let identified = identify(&head_of(&path));
            assert_eq!(
                identified,
                Some(named),
                "{}: the bytes say {identified:?} and the name says {named:?}",
                path.display()
            );
            checked += 1;
        }
        assert!(checked > 100, "only {checked} fixtures were identified");
        assert!(
            skipped.iter().all(|name| [".tga", ".ico", ".cur", ".dib"]
                .iter()
                .any(|known| name.to_lowercase().ends_with(known))),
            "a format without a signature turned up that was not expected: {skipped:?}"
        );
    }

    #[test]
    fn the_signatures_that_are_not_leading_work() {
        assert_eq!(
            identify(b"RIFF\x00\x00\x00\x00WEBPVP8 "),
            Some(Format::Webp)
        );
        assert_eq!(identify(b"\x00\x00\x00\x20ftypavif"), Some(Format::Avif));
        assert_eq!(identify(b"\x00\x00\x00\x20ftypheic"), Some(Format::Heif));
        assert_eq!(identify(b"\x00\x00\x00\x20ftypmif1"), Some(Format::Heif));
        // A brand neither container claims is not guessed at.
        assert_eq!(identify(b"\x00\x00\x00\x20ftypisom"), None);
    }

    #[test]
    fn both_tiff_versions_and_both_radiance_spellings_are_named() {
        assert_eq!(identify(b"II\x2a\x00\x08\x00\x00\x00"), Some(Format::Tiff));
        assert_eq!(identify(b"MM\x00\x2a\x00\x00\x00\x08"), Some(Format::Tiff));
        assert_eq!(identify(b"II\x2b\x00\x08\x00"), Some(Format::Tiff));
        assert_eq!(identify(b"MM\x00\x2b\x00\x00"), Some(Format::Tiff));
        assert_eq!(identify(b"#?RADIANCE\n"), Some(Format::Hdr));
        assert_eq!(identify(b"#?RGBE\n"), Some(Format::Hdr));
    }

    #[test]
    fn the_netpbm_magic_needs_the_whitespace_after_it() {
        assert_eq!(identify(b"P6\n1200 900\n"), Some(Format::Pnm));
        assert_eq!(identify(b"P1 "), Some(Format::Pnm));
        assert_eq!(identify(b"P7\n"), Some(Format::Pnm));
        // The magic with nothing after it is still the magic: identifying it as
        // netpbm and letting the header check refuse a file that stops there is
        // the layering plan 34 asks for -- "validate the selected format's
        // header" -- and is better than routing it away on its extension.
        assert_eq!(identify(b"P6"), Some(Format::Pnm));
        assert_eq!(identify(b"P6x"), None);
        assert_eq!(identify(b"P9\n"), None);
    }

    #[test]
    fn a_file_that_states_nothing_is_not_guessed_at() {
        assert_eq!(identify(b""), None);
        assert_eq!(identify(&[0u8; HEAD_BYTES]), None);
        assert_eq!(identify(b"not an image at all"), None);
    }

    #[test]
    fn the_extension_hint_ignores_case_and_unknown_names() {
        assert_eq!(from_extension(Path::new("a.PNG")), Some(Format::Png));
        assert_eq!(from_extension(Path::new("a.JpEg")), Some(Format::Jpeg));
        assert_eq!(from_extension(Path::new("a.dat")), None);
        assert_eq!(from_extension(Path::new("noextension")), None);
    }
}
