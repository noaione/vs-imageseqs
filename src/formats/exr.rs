//! OpenEXR stills, read straight from the format's own crate.
//!
//! The plan's row for this format is the one that says the candidate is already
//! in the lock file behind `image`, and that this crate's channel selection is
//! what the adapter was reaching for; see
//! `docs/improvements/27-direct-still-decoders.md`. What replaces the adapter is
//! this file: the `exr` crate parses the header and decompresses the blocks, and
//! the picture is laid out here.
//!
//! Three behaviours are the rule rather than an incidental detail:
//!
//! - **A frame is always float and native-endian.** Every sample type the format
//!   defines -- half, float and unsigned integer -- is handed over as `f32`, so
//!   the same picture stored as half and as float holds the same numbers.
//! - **The channels are chosen by name, not by position.** `R`, `G` and `B` are
//!   read wherever the file keeps them, so a file that stores them in another
//!   order still decodes to the picture it holds, and `A` becomes the alpha clip
//!   only when the file states one.
//! - **The probe reads the header alone.** `MetaData::read_from_file` parses the
//!   attributes and the block table and stops there, so describing a file never
//!   decompresses a block.
//!
//! A layer is handed out at its own data window, which is the pixels the file
//! stores. A file may place that layer inside a larger display window; the
//! canvas around it, which the reader this replaces filled with transparent
//! black, is not reconstructed here.

use std::{io::Read, path::Path};

use exr::{
    meta::MetaData,
    prelude::{ReadChannels, ReadLayers, ReadSpecificChannel, Vec2, read},
};

use crate::{
    decoder::{DecodeTimings, DecodedImage, ImageInfo, Pixels, image_error},
    error::{ImgSeqError, Result},
    layout::{ColorType, Orientation, SourceColorType},
    pixel::{PixelFormat, Transform},
};

/// The four bytes every openexr picture starts with, which is the magic number
/// 20000630 written little-endian.
const MAGIC: [u8; 4] = [0x76, 0x2f, 0x31, 0x01];

/// The colour channels a frame is built from, in the order a frame holds them.
const COLOR_CHANNELS: [&[u8]; 3] = [b"R", b"G", b"B"];

/// The channel the alpha clip is built from, when the file states one.
const ALPHA_CHANNEL: &[u8] = b"A";

/// What one layer's header states, and nothing else.
///
/// This is the whole of what a probe needs: the size of the layer, and whether
/// it carries an alpha channel. Every other attribute the format defines is the
/// file owner's and is not reported.
#[derive(Clone, Copy, Debug)]
struct Header {
    width: u32,
    height: u32,
    /// Whether the layer states an alpha channel.
    alpha: bool,
}

impl Header {
    /// The layout a frame is written from.
    #[must_use]
    const fn color_type(&self) -> ColorType {
        if self.alpha {
            ColorType::Rgba32F
        } else {
            ColorType::Rgb32F
        }
    }

    /// The label property's value.
    #[must_use]
    const fn source(&self) -> SourceColorType {
        if self.alpha {
            SourceColorType::Rgba32F
        } else {
            SourceColorType::Rgb32F
        }
    }

    /// The format a frame is written from.
    ///
    /// An alpha channel is not part of the colour frame even when the file
    /// holds one: it is the gray clip `ReadAlpha` hands out, so the colour
    /// frame is three floats wide either way.
    #[must_use]
    const fn format(&self) -> PixelFormat {
        PixelFormat::Rgb32F
    }
}

/// Whether this module reads `path`.
#[must_use]
pub fn owns(path: &Path) -> bool {
    // Content first: a file whose bytes say it is something else is that
    // something else however it is named, and the extension is the hint a
    // format with no signature of its own has to fall back on. See
    // [`identify::owns`](crate::formats::identify::owns).
    crate::formats::identify::owns(crate::formats::identify::Format::Exr, path)
}

/// Whether the file starts with the format's magic number.
///
/// A file too short to hold the four bytes is not one either, so it is declined
/// rather than refused at this step: the magic is what decides whether this
/// module owns a `.exr`, and the extension only decides whether it is asked.
fn is_exr(path: &Path) -> Result<bool> {
    let mut file = std::fs::File::open(path).map_err(|error| image_error("open", path, error))?;
    let mut magic = [0u8; MAGIC.len()];
    match file.read_exact(&mut magic) {
        Ok(()) => Ok(magic == MAGIC),
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(image_error("open", path, error)),
    }
}

/// Whether a layer is one this reader can hand out: a flat r,g,b one.
///
/// A deep layer has no flat samples at all, and a layer without all three of
/// `R`, `G` and `B` is a picture in a colour space this reader does not convert.
/// Both are declined rather than described wrongly.
fn flat_rgb(header: &exr::meta::header::Header) -> bool {
    !header.deep
        && COLOR_CHANNELS.iter().all(|name| {
            header
                .channels
                .list
                .iter()
                .any(|channel| channel.name.bytes() == *name)
        })
}

/// Whether the layer's channel list holds the alpha channel.
fn states_alpha(header: &exr::meta::header::Header) -> bool {
    header
        .channels
        .list
        .iter()
        .any(|channel| channel.name.bytes() == ALPHA_CHANNEL)
}

/// A size, checked against the type a frame's dimensions are held in.
///
/// The format states its windows in pointer-width numbers and a frame states
/// its dimensions in thirty-two bit ones, so this is where the two meet.
fn frame_size(width: usize, height: usize) -> Result<(u32, u32)> {
    if width == 0 || height == 0 {
        return Err(ImgSeqError::new("the header states no pixels"));
    }
    let width = u32::try_from(width)
        .map_err(|_| ImgSeqError::new("the header states a width a frame cannot hold"))?;
    let height = u32::try_from(height)
        .map_err(|_| ImgSeqError::new("the header states a height a frame cannot hold"))?;
    Ok((width, height))
}

/// Reads the header of a file this module owns.
///
/// Returns `Ok(None)` for a file that is not an openexr picture, and for one
/// whose layers are every one of them either deep or not r,g,b, so that another
/// reader may take it.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file cannot be read or its header cannot be
/// parsed.
fn header(path: &Path) -> Result<Option<Header>> {
    if !is_exr(path)? {
        return Ok(None);
    }
    let metadata = MetaData::read_from_file(path, false)
        .map_err(|error| image_error("identify", path, error))?;
    let Some(layer) = metadata.headers.iter().find(|header| flat_rgb(header)) else {
        return Ok(None);
    };
    // The layer's own data window: see the note at the head of this file.
    let (width, height) = frame_size(layer.layer_size.width(), layer.layer_size.height())
        .map_err(|error| image_error("identify", path, error))?;
    Ok(Some(Header {
        width,
        height,
        alpha: states_alpha(layer),
    }))
}

/// What a picture states, when this module reads the file.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file is one of ours and cannot be read.
pub fn image_info(
    path: &Path,
    _apply_rotation: bool,
    route: Option<crate::formats::identify::Format>,
) -> Result<Option<ImageInfo>> {
    if !route.map_or_else(
        || owns(path),
        |saved| saved == crate::formats::identify::Format::Exr,
    ) {
        return Ok(None);
    }
    let Some(header) = header(path)? else {
        return Ok(None);
    };
    Ok(Some(ImageInfo {
        route: None,
        subimage: None,
        path: path.to_path_buf(),
        width: header.width,
        height: header.height,
        color_type: header.color_type(),
        original_color_type: header.source(),
        has_icc_profile: false,
        icc_profile: None,
        cicp: None,
        chroma_location: None,
        // An openexr states no orientation: a layer is placed inside a canvas
        // rather than turned by a tag, and what is handed out is the layer's own
        // pixels.
        orientation: Orientation::NoTransforms,
        transform: Transform::IDENTITY,
        format: header.format(),
    }))
}

/// Decodes a picture into one interleaved buffer of native-endian floats.
///
/// The layer read is the one the probe chose: the first that holds `R`, `G` and
/// `B`. The crate's own `first_valid_layer` answers a different question -- "the
/// first layer I can read at all" -- and a file whose first part is a depth pass
/// has a first layer with no colour channel in it, so a probe that asked for a
/// colour layer and a decode that asked for any layer read different parts of
/// the same file. The requirement is stated here in the same terms the probe
/// uses, so the two cannot disagree about which part holds the picture.
///
/// The channels are asked for by name rather than by position, so a file
/// that stores them in another order still reads as the picture it holds, and
/// each sample is written straight into the buffer the frame is written from.
/// That buffer is one plane per channel laid end to end rather than an
/// interleaved picture, so the frame writer takes each plane out of it where it
/// already is and the picture is never interleaved on the way and separated
/// again on the way out; `pixels::PlaneSource::Strided` is the shape that says
/// so. An alpha channel is the last plane of that buffer, which is where the
/// alpha clip reads it from.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file cannot be read or is malformed.
#[inline(never)]
pub fn decode(info: &ImageInfo) -> Result<DecodedImage> {
    let alpha = info.color_type.has_alpha();
    let slots = if alpha { 4 } else { 3 };
    let stride = usize::try_from(info.width)
        .map_err(|_| ImgSeqError::new("the layer states a width a frame cannot hold"))?;
    let plane_bytes = usize::try_from(info.width)
        .ok()
        .and_then(|width| {
            usize::try_from(info.height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(size_of::<f32>()))
        .ok_or_else(|| ImgSeqError::new("the layer is too large for a frame"))?;
    // One plane per channel, laid end to end: the frame writer takes each one
    // out of the buffer where it already is, so the picture is never interleaved
    // on the way and separated again on the way out.
    let bytes = plane_bytes
        .checked_mul(slots)
        .ok_or_else(|| ImgSeqError::new("the layer is too large for a frame"))?;

    let open_started = std::time::Instant::now();
    let image = read()
        .no_deep_data()
        .largest_resolution_level()
        .specific_channels()
        // The three the probe insists on, which is what makes this skip a
        // layer the probe skipped: `flat_rgb` states the same predicate, and
        // the two part fixture is what fails if the two ever drift apart.
        .required::<f32>("R")
        .required::<f32>("G")
        .required::<f32>("B")
        // Alpha is asked for either way so that one read answers a call that
        // wants it and one that does not: a file without an `A` leaves the
        // default in the fourth slot, which the stride below never writes.
        .optional::<f32>("A", 1.0)
        .collect_pixels(
            // The one buffer the frame is written from, one plane a channel laid
            // end to end. This closure cannot report an error, so a layer too large to
            // lay out is refused by the size check above rather than aborting a release
            // build here.
            move |_: Vec2<usize>, _: &_| vec![0u8; bytes],
            move |buffer: &mut Vec<u8>, at: Vec2<usize>, (r, g, b, a): (f32, f32, f32, f32)| {
                // Channel `slot` of the picture is `slot` planes into the buffer, and a
                // position that does not fit is skipped rather than indexed, because the
                // same size check is what refuses the file.
                for (slot, value) in [r, g, b, a].into_iter().take(slots).enumerate() {
                    let from = at
                        .y()
                        .checked_mul(stride)
                        .and_then(|row| row.checked_add(at.x()))
                        .and_then(|pixel| {
                            plane_bytes.checked_mul(slot).and_then(|plane| {
                                pixel.checked_mul(size_of::<f32>()).map(|at| plane + at)
                            })
                        });
                    let Some(from) = from else {
                        return;
                    };
                    if let Some(target) = buffer.get_mut(from..from + size_of::<f32>()) {
                        target.copy_from_slice(&value.to_ne_bytes());
                    }
                }
            },
        )
        .first_valid_layer()
        .all_attributes()
        .from_file(&info.path)
        .map_err(|error| image_error("decode", &info.path, error))?;
    let open = open_started.elapsed();

    let layer = image.layer_data;
    let (width, height) = frame_size(layer.size.width(), layer.size.height())
        .map_err(|error| image_error("decode", &info.path, error))?;
    if (width, height) != (info.width, info.height) {
        return Err(ImgSeqError::new(format!(
            "image '{}' changed after probing (was {}x{}, now {width}x{height})",
            info.path.display(),
            info.width,
            info.height,
        )));
    }
    if layer.channel_data.channels.3.is_some() != alpha {
        return Err(ImgSeqError::new(format!(
            "image '{}' changed after probing: its alpha channel is {}",
            info.path.display(),
            if alpha { "gone" } else { "there" },
        )));
    }

    let buffer = layer.channel_data.pixels;
    if buffer.len() != bytes {
        return Err(ImgSeqError::new(format!(
            "the layer lays out {} bytes where the frame holds {bytes}",
            buffer.len(),
        )));
    }

    Ok(DecodedImage {
        width,
        height,
        format: info.format,
        transform: info.transform,
        pixels: Pixels::Strided {
            planes: slots,
            alpha,
            buffer,
            row_stride: stride * size_of::<f32>(),
            plane_stride: plane_bytes,
        },
        timings: DecodeTimings {
            open,
            metadata: std::time::Duration::ZERO,
            buffer: std::time::Duration::ZERO,
            // Laying the picture out is the crate's own walk over the layer,
            // so it lands in `open` with the rest of the read.
            read: std::time::Duration::ZERO,
        },
    })
}
#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    /// One fixture's planes, from the probe to the decoded pixels.
    ///
    /// The decode hands over one buffer with a plane per channel, so the helper
    /// reads it back by stride and copies the colour planes out the way the frame
    /// writer takes them. The tests below compare pictures rather than the shape
    /// the reader chose; [`one_buffer_holds_a_plane_a_channel`] asserts the shape.
    fn read(name: &str) -> (ImageInfo, Vec<Vec<u8>>) {
        let info = image_info(&fixture(name), true, None)
            .unwrap_or_else(|error| panic!("{name}: {error}"))
            .unwrap_or_else(|| panic!("{name} is taken over"));
        let decoded = decode(&info).unwrap_or_else(|error| panic!("{name}: {error}"));
        let Pixels::Strided {
            planes,
            alpha,
            buffer,
            row_stride,
            plane_stride,
        } = decoded.pixels
        else {
            panic!("{name} hands out one buffer with a plane per channel");
        };
        assert_eq!(
            buffer.len(),
            plane_stride * planes,
            "{name}: the buffer is exactly its planes"
        );
        assert_eq!(
            plane_stride % row_stride,
            0,
            "{name}: a plane is a whole number of rows"
        );
        // The alpha plane is the last one; the colour clip reads the rest.
        let colour = planes - usize::from(alpha);
        let planes_out = (0..colour)
            .map(|plane| {
                let start = plane * plane_stride;
                buffer[start..start + plane_stride].to_vec()
            })
            .collect();
        (info, planes_out)
    }

    /// Every committed fixture, and what its header states. A file without an
    /// alpha channel is described as `Rgb32F` however its samples are stored,
    /// and a file with one is not made wider than the frame it is written into.
    #[test]
    fn the_fixtures_state_the_layout_they_hold() {
        for (name, color_type, source) in [
            (
                "exr-half-rgb.exr",
                ColorType::Rgb32F,
                SourceColorType::Rgb32F,
            ),
            (
                "exr-half-rgba.exr",
                ColorType::Rgba32F,
                SourceColorType::Rgba32F,
            ),
            (
                "exr-float-rgba.exr",
                ColorType::Rgba32F,
                SourceColorType::Rgba32F,
            ),
            ("exr-none.exr", ColorType::Rgb32F, SourceColorType::Rgb32F),
            ("exr-rle.exr", ColorType::Rgb32F, SourceColorType::Rgb32F),
            ("exr-zip.exr", ColorType::Rgb32F, SourceColorType::Rgb32F),
            ("exr-zips.exr", ColorType::Rgb32F, SourceColorType::Rgb32F),
            ("exr-piz.exr", ColorType::Rgb32F, SourceColorType::Rgb32F),
        ] {
            let info = image_info(&fixture(name), true, None)
                .unwrap_or_else(|error| panic!("{name}: {error}"))
                .unwrap_or_else(|| panic!("{name} is taken over"));
            assert_eq!((info.width, info.height), (37, 23), "{name}");
            assert_eq!(info.color_type, color_type, "{name}");
            assert_eq!(info.original_color_type, source, "{name}");
            assert_eq!(info.format, PixelFormat::Rgb32F, "{name}");
            assert_eq!(info.orientation, Orientation::NoTransforms, "{name}");
            assert_eq!(info.transform, Transform::IDENTITY, "{name}");
            assert!(!info.has_icc_profile, "{name}");
            assert!(info.icc_profile.is_none(), "{name}");
            assert!(info.cicp.is_none(), "{name}");
            assert!(info.chroma_location.is_none(), "{name}");

            let (_, planes) = read(name);
            // The colour clip takes the planes before an alpha one.
            let channels = color_type.channels() - usize::from(color_type.has_alpha());
            assert_eq!(planes.len(), channels, "{name}: one plane a colour channel");
            for plane in &planes {
                assert_eq!(
                    plane.len(),
                    37 * 23 * size_of::<f32>(),
                    "{name}: a plane is a whole picture"
                );
            }
        }
    }

    /// A file whose first part holds no colour channel at all.
    ///
    /// The probe picks the first part that has `R`, `G` and `B`, which here is
    /// the second one, and the decode has to read **that** part rather than the
    /// first one the crate happens to accept. A decode that answers a different
    /// question than the probe did promises a frame it cannot produce.
    #[test]
    fn the_part_the_probe_chose_is_the_part_that_is_decoded() {
        let (info, planes) = read("exr-multipart-z-rgb.exr");
        assert_eq!((info.width, info.height), (2, 2));
        assert_eq!(info.color_type, ColorType::Rgb32F);
        assert_eq!(info.format, PixelFormat::Rgb32F);

        // The colour part holds `B`, `G` and `R` plane by plane, and the frame
        // is r,g,b. A decode that read the depth part would either refuse the
        // file -- there is no `R` in it -- or hand back the `Z` samples.
        assert_eq!(planes.len(), 3, "three colour planes");
        let samples = |plane: &[u8]| -> Vec<f32> {
            plane
                .as_chunks::<4>()
                .0
                .iter()
                .map(|bytes| f32::from_ne_bytes(*bytes))
                .collect()
        };
        assert_eq!(samples(&planes[0]), vec![1.25, 1.5, 2.75, 3.0]);
        assert_eq!(samples(&planes[1]), vec![0.75, 1.0, 2.25, 2.5]);
        assert_eq!(samples(&planes[2]), vec![0.25, 0.5, 1.75, 2.0]);
    }

    /// The five compressions the fixtures cover, which are the whole of what a
    /// header can ask for short of the lossy ones: a reader that takes one and
    /// not the others reads most real files wrongly. They hold the same picture,
    /// so they decode to the same bytes.
    #[test]
    fn every_compression_decodes_to_the_same_picture() {
        let (_, expected) = read("exr-none.exr");
        for name in ["exr-rle.exr", "exr-zip.exr", "exr-zips.exr", "exr-piz.exr"] {
            let (_, got) = read(name);
            assert_eq!(got, expected, "{name} is the same picture as exr-none.exr");
        }
    }

    /// Half is the format's native sample and float is the other, and the same
    /// picture stored either way holds the same numbers. The alpha file's colour
    /// planes are the colour file's, pixel for pixel.
    #[test]
    fn a_half_picture_and_a_float_one_agree_on_colour() {
        let (_, rgb) = read("exr-half-rgb.exr");
        assert_eq!(rgb.len(), 3, "three colour planes");
        for name in ["exr-half-rgba.exr", "exr-float-rgba.exr"] {
            let (_, rgba) = read(name);
            assert_eq!(rgba.len(), 3, "{name}: three colour planes");
            for (channel, colour) in rgba.iter().zip(rgb.iter()) {
                assert_eq!(channel, colour, "{name}: channel by channel");
            }
        }
    }

    /// The decode hands over one buffer with a plane per channel rather than an
    /// interleaved picture, and an alpha channel is the last of those planes.
    ///
    /// This asserts the shape and not just the picture: the interleaved layout
    /// is the same samples with one more pass over them, so a decode that went
    /// back to it would pass every other test in this module.
    #[test]
    fn one_buffer_holds_a_plane_a_channel() {
        for (name, alpha) in [
            ("exr-none.exr", false),
            ("exr-half-rgba.exr", true),
            ("exr-multipart-z-rgb.exr", false),
        ] {
            let info = image_info(&fixture(name), true, None)
                .unwrap_or_else(|error| panic!("{name}: {error}"))
                .unwrap_or_else(|| panic!("{name} is taken over"));
            let decoded = decode(&info).unwrap_or_else(|error| panic!("{name}: {error}"));
            let Pixels::Strided {
                planes,
                alpha: says_alpha,
                buffer,
                row_stride,
                plane_stride,
            } = decoded.pixels
            else {
                panic!("{name} hands out one buffer with a plane per channel");
            };
            assert_eq!(says_alpha, alpha, "{name}: whether it has an alpha plane");
            let colour = planes - usize::from(alpha);
            assert_eq!(colour, 3, "{name}: the colour clip is three planes");
            assert_eq!(
                buffer.len(),
                plane_stride * planes,
                "{name}: the buffer is exactly its planes"
            );
            assert_eq!(
                row_stride,
                usize::try_from(info.width).expect("width") * size_of::<f32>(),
                "{name}: a row of a plane is a row of the picture"
            );
            assert_eq!(
                plane_stride,
                row_stride * usize::try_from(info.height).expect("height"),
                "{name}: a plane is a whole picture"
            );
        }
    }

    /// A file of another format is declined however it is named, and the magic
    /// is what decides: the extension only decides whether this module is asked.
    #[test]
    fn a_file_of_another_format_is_declined() {
        assert!(!owns(Path::new("a.png")));
        assert!(owns(Path::new("a.exr")));
        assert!(owns(Path::new("a.EXR")));
        assert!(
            image_info(&fixture("cicp-rgb8.png"), true, None)
                .expect("a png is not ours")
                .is_none()
        );
        assert!(
            !is_exr(&fixture("cicp-rgb8.png")).expect("the png's first bytes are read"),
            "a png does not start with the openexr magic"
        );
    }
}
