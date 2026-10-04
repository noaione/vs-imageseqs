use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use vapoursynth4_rs::ffi;

use crate::{
    animation::{Rate, Segment, SegmentTable},
    color::Cicp,
    error::{ImgSeqError, Result},
    formats,
    formats::identify::{self, Format},
    layout::{ColorType, Orientation, SourceColorType},
    pixel::{PixelFormat, Transform},
};

/// What one decode has to produce.
///
/// A call that hands out only the colour clip never asks for the alpha plane,
/// and an avif alpha item is a coded item of its own: skipping it is a whole
/// decoder that is not created. The demand belongs to the call rather than to
/// whichever clip asks for a frame first, because one decode serves every clip
/// of it; see [`crate::prefetch::Prepare`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Demand {
    /// Whether the alpha plane is needed.
    pub alpha: bool,
}

impl Demand {
    /// Everything a decode can produce, which is what a call that hands out the
    /// alpha clip needs.
    pub const ALL: Self = Self { alpha: true };
    /// The colour planes alone, for a call that hands out no alpha clip.
    pub const COLOR: Self = Self { alpha: false };
}

/// Format modules that decode what the registered hooks cannot; see
/// [`crate::formats`].
///
/// Only the two whose alpha plane is work of its own are told what the call
/// needs: an avif alpha item is a second coded item, and a heif alpha plane is
/// allocated and packed here. A webp or a jxl arrives with its alpha channel
/// already in the buffer the decoder wrote, so there is nothing to skip.
fn format_decoder(info: &ImageInfo, demand: Demand) -> Option<Result<DecodedImage>> {
    // The probe already read the head to answer which module owns this file,
    // and this is that answer; an `ImageInfo` built by hand has none and
    // identifies from its path, which is what every decode did before. The
    // saved route replaces a chain of sixteen `owns` calls, one open each, that
    // had to agree with `describe` and could not be kept in agreement: a module
    // answering from the name rather than from the bytes probed as one format
    // and decoded as another.
    let route = info.route.or_else(|| identify::route(&info.path));
    Some(match route {
        Some(Format::Avif) => formats::avif::decode(info, demand),
        Some(Format::Heif) => formats::heif::decode(info, demand),
        Some(Format::Webp) => formats::webp::decode(info),
        Some(Format::Jxl) => formats::jxl::decode(info),
        Some(Format::Jp2) => formats::jp2::decode(info),
        Some(Format::Jpeg) => formats::jpeg::decode(info),
        Some(Format::Qoi) => formats::qoi::decode(info),
        Some(Format::Farbfeld) => formats::farbfeld::decode(info),
        // Three of these walk their own rows straight into the frame when they
        // can, and fall back to the whole buffer when they cannot.
        Some(Format::Bmp) => formats::bmp::stream(info).and_then(|streamed| match streamed {
            Some(ready) => Ok(ready),
            None => formats::bmp::decode(info),
        }),
        Some(Format::Ico) => formats::ico::decode(info),
        Some(Format::Tga) => formats::tga::stream(info).and_then(|streamed| match streamed {
            Some(ready) => Ok(ready),
            None => formats::tga::decode(info),
        }),
        Some(Format::Dds) => formats::dds::decode(info),
        Some(Format::Pnm) => formats::pnm::stream(info).and_then(|streamed| match streamed {
            Some(ready) => Ok(ready),
            None => formats::pnm::decode(info),
        }),
        Some(Format::Tiff) => formats::tiff::decode(info),
        Some(Format::Hdr) => formats::hdr::decode(info),
        Some(Format::Exr) => formats::exr::decode(info),
        // A png and a gif are the two the tail of [`decode`] owns: the png row
        // walk, and the generic decoder for both. No format here means the same
        // thing, so a file no module claims keeps the path it had.
        // A gif is read here now: its container states the logical screen and its
        // one frame draws a rectangle onto it, so `formats/gif.rs` composes the
        // picture rather than the crate handing a sub-rectangle back as the whole
        // image.
        Some(Format::Gif) => formats::gif::decode(info),
        // A png is [`decode`]'s tail rather than one decode here: the row walk,
        // and then the whole-frame read for the shapes the walk will not take.
        // A file no module here claims is one no reader in this tree knows.
        Some(Format::Png) | None => return None,
    })
}

#[derive(Clone, Debug)]
pub struct ImageInfo {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub color_type: ColorType,
    pub original_color_type: SourceColorType,
    pub has_icc_profile: bool,
    /// The source's embedded ICC profile, when it has one.
    pub icc_profile: Option<Arc<[u8]>>,
    /// The colour description the container states about its own samples, which
    /// is `None` for a file that states none and for one whose codes name no
    /// property; see [`crate::color::Cicp`].
    pub cicp: Option<Cicp>,
    /// The position of the chroma samples the file states, which is written as
    /// `_ChromaLocation` for a subsampled frame and is `None` for a file that
    /// names no position; see [`crate::color::chroma_location`].
    pub chroma_location: Option<ffi::VSChromaLocation>,
    /// The exif orientation the file states, which is reported as a property
    /// whatever the caller asked for.
    pub orientation: Orientation,
    /// The rearrangement the pixels are written with, which is the identity
    /// when the file states no orientation or the caller turned rotation off.
    pub transform: Transform,
    pub format: PixelFormat,
    /// The container [`identify::route`] named for this file, kept so that the
    /// timeline decision and the decode reuse it instead of reading the head
    /// again. It is `None` for an `ImageInfo` that no probe built.
    pub route: Option<Format>,
}

impl ImageInfo {
    /// Width this image is handed out as, which is its height for an
    /// orientation that transposes the picture.
    #[must_use]
    pub const fn output_width(&self) -> u32 {
        orientation_size(self.transform, self.width, self.height).0
    }

    /// Height this image is handed out as.
    #[must_use]
    pub const fn output_height(&self) -> u32 {
        orientation_size(self.transform, self.width, self.height).1
    }
}

#[derive(Clone, Debug, Default)]
pub struct DecodeTimings {
    pub open: Duration,
    pub metadata: Duration,
    pub buffer: Duration,
    pub read: Duration,
}

/// One plane of a frame a streaming decode writes into.
///
/// The rows are written at `stride` rather than into a buffer of their own,
/// which is what makes a streaming decode one pass over the picture: the bytes
/// the decoder produces are the bytes the frame holds.
#[derive(Debug)]
pub struct PlaneRows<'a> {
    /// The whole plane, the row padding a VapourSynth frame carries included.
    pub bytes: &'a mut [u8],
    /// Bytes between the starts of two rows.
    pub stride: usize,
    /// Bytes of each row that are written.
    pub row_bytes: usize,
}

impl PlaneRows<'_> {
    /// The writable bytes of one active row.
    ///
    /// # Panics
    ///
    /// Panics when `row` is outside the plane, which a decoder that walked its
    /// own height cannot ask for.
    #[must_use]
    pub fn row(&mut self, row: usize) -> &mut [u8] {
        let start = row * self.stride;
        &mut self.bytes[start..start + self.row_bytes]
    }
}

/// The frames one streaming decode fills.
///
/// A colour frame is one plane for a gray format and three for an rgb one, in
/// VapourSynth's own plane order, and `alpha` is the gray frame of the alpha
/// clip when the call hands one out.
pub struct RowSink<'a> {
    /// Planes of the colour clip's frame, in the order the frame indexes them.
    pub colour: Vec<PlaneRows<'a>>,
    /// Planes of the alpha clip's frame, for a call that hands one out.
    pub alpha: Option<Vec<PlaneRows<'a>>>,
}

impl RowSink<'_> {
    /// Writes one interleaved eight bit three channel row into three planes.
    ///
    /// The three plane rows are taken apart with `split_first_mut`, so they can
    /// be held together, and the walk is a `zip` over the source's pixels and
    /// the three rows at once. That is what makes it one pass: reading the row
    /// once per plane reads every byte three times, and indexing a plane per
    /// pixel carries a bound check per byte. `png.rs` learned this for palette
    /// pages and it is here rather than there so a second format does not learn
    /// it again.
    ///
    /// Answers `None` when the sink does not hold three colour planes, which a
    /// caller reports its own way.
    pub fn place_rgb8(&mut self, source: &[u8], row: usize) -> Option<()> {
        let (red, rest) = self.colour.split_first_mut()?;
        let (green, rest) = rest.split_first_mut()?;
        let (blue, _) = rest.split_first_mut()?;
        let planes = red
            .row(row)
            .iter_mut()
            .zip(green.row(row).iter_mut())
            .zip(blue.row(row).iter_mut());
        for (pixel, ((red_byte, green_byte), blue_byte)) in
            source.as_chunks::<3>().0.iter().zip(planes)
        {
            *red_byte = pixel[0];
            *green_byte = pixel[1];
            *blue_byte = pixel[2];
        }
        Some(())
    }

    /// As [`Self::place_rgb8`], for a source that stores blue first.
    ///
    /// Targa does. The swap is the one thing this does that the other does not,
    /// and it is folded into the same walk rather than run as a pass of its own
    /// over the picture afterwards.
    pub fn place_bgr8(&mut self, source: &[u8], row: usize) -> Option<()> {
        let (red, rest) = self.colour.split_first_mut()?;
        let (green, rest) = rest.split_first_mut()?;
        let (blue, _) = rest.split_first_mut()?;
        let planes = red
            .row(row)
            .iter_mut()
            .zip(green.row(row).iter_mut())
            .zip(blue.row(row).iter_mut());
        for (pixel, ((red_byte, green_byte), blue_byte)) in
            source.as_chunks::<3>().0.iter().zip(planes)
        {
            *red_byte = pixel[2];
            *green_byte = pixel[1];
            *blue_byte = pixel[0];
        }
        Some(())
    }
}
/// A decode that has not read its picture yet.
///
/// A format answers with one of these when it can hand every decoded row to
/// the frame it belongs in. That is one pass over the picture where a buffered
/// decode is two, because the pixels never exist anywhere but the frame; see
/// `docs/improvements/22-png-decode-path.md`.
///
/// It is `Send + Sync` because a decoded image is handed between the
/// lookahead workers and the requesting thread, and `Debug` because the pixels
/// a decode produced are printed beside the timings they cost.
pub trait RowStream: Send + Sync + std::fmt::Debug {
    /// Whether the rows this stream writes include an alpha channel.
    ///
    /// A file that states no alpha leaves the alpha clip's plane untouched, and
    /// a VapourSynth frame arrives holding whatever the allocator held, so the
    /// caller fills that plane with the opaque value first when this is false.
    fn has_alpha(&self) -> bool;

    /// Reads the picture into `sink`, and answers what the read cost.
    ///
    /// # Errors
    ///
    /// Returns [`ImgSeqError`] when the file cannot be read or decoded.
    fn fill(&mut self, sink: RowSink<'_>) -> Result<DecodeTimings>;

    /// A second reader over the same file.
    ///
    /// Cloning a decoded image clones its pixels, and a stream is a still
    /// that has not been read, so this is another reader over the same file
    /// rather than a reader continued from where this one is.
    fn duplicate(&self) -> Box<dyn RowStream>;
}

/// Pixels of one decoded image, in whichever layout its format decodes to.
#[derive(Debug)]
pub enum Pixels {
    /// One buffer holding every channel of `color_type`, interleaved.
    Interleaved {
        color_type: ColorType,
        buffer: Vec<u8>,
    },
    /// One tightly packed buffer per plane of the frame format.
    ///
    /// `alpha` is the alpha item of the file, tightly packed and the size of
    /// the image, which is `None` for a file without one. It is a plane of its
    /// own rather than a channel of the last plane because the alpha clip is a
    /// gray frame of the same depth.
    Planar {
        planes: Vec<Vec<u8>>,
        alpha: Option<Vec<u8>>,
    },
    /// A decode that hands each row to the frame it belongs in, and therefore
    /// has no buffer of its own.
    Stream(Box<dyn RowStream>),
}

impl Clone for Pixels {
    fn clone(&self) -> Self {
        match self {
            Self::Interleaved { color_type, buffer } => Self::Interleaved {
                color_type: *color_type,
                buffer: buffer.clone(),
            },
            Self::Planar { planes, alpha } => Self::Planar {
                planes: planes.clone(),
                alpha: alpha.clone(),
            },
            Self::Stream(stream) => Self::Stream(stream.duplicate()),
        }
    }
}

impl PartialEq for Pixels {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Interleaved {
                    color_type: left_type,
                    buffer: left_buffer,
                },
                Self::Interleaved {
                    color_type: right_type,
                    buffer: right_buffer,
                },
            ) => left_type == right_type && left_buffer == right_buffer,
            (
                Self::Planar {
                    planes: left_planes,
                    alpha: left_alpha,
                },
                Self::Planar {
                    planes: right_planes,
                    alpha: right_alpha,
                },
            ) => left_planes == right_planes && left_alpha == right_alpha,
            // A stream is a picture that has not been read, so there is
            // nothing to compare and two of them are never equal.
            _ => false,
        }
    }
}

impl Eq for Pixels {}
#[derive(Clone, Debug)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    /// The format the probe recorded for this image, which is the format of the
    /// frame it is written into.
    pub format: PixelFormat,
    /// How the samples are rearranged as they are written, from the probe.
    pub transform: Transform,
    pub pixels: Pixels,
    pub timings: DecodeTimings,
}

impl DecodedImage {
    /// Width of the frame these pixels are written into, which is the stored
    /// height for an orientation that transposes the picture.
    #[must_use]
    pub const fn output_width(&self) -> u32 {
        orientation_size(self.transform, self.width, self.height).0
    }

    /// Height of the frame these pixels are written into.
    #[must_use]
    pub const fn output_height(&self) -> u32 {
        orientation_size(self.transform, self.width, self.height).1
    }
}

/// Size `width`x`height` is handed out as under `transform`.
///
/// The probe and the decode result answer the same question, and the frame the
/// pixels are written into is this size rather than the size the file holds.
#[must_use]
pub const fn orientation_size(transform: Transform, width: u32, height: u32) -> (u32, u32) {
    if transform.transposes() {
        (height, width)
    } else {
        (width, height)
    }
}

/// Builds the error every decoder path reports, so the format modules in
/// [`crate::formats`] word theirs the same way.
pub(crate) fn image_error(action: &str, path: &Path, error: impl std::fmt::Display) -> ImgSeqError {
    ImgSeqError::new(format!(
        "failed to {action} image '{}': {error}",
        path.display()
    ))
}

/// How much of a file a probe may read. A header is never close to this: the
/// largest one here is a bitmap's palette or a netpbm's comments, both measured
/// in kilobytes, and this is deliberately far above either so a format that
/// grew a longer preamble could not start failing as a truncated file.
pub(crate) const PROBE_HEAD_BYTES: u64 = 64 * 1024;

/// Reads the beginning of a file, which is all a probe needs.
///
/// Reading the whole file to parse a header was measured as the one regression
/// in this tree's own readers: the probe went from 1.5 to 5 ms to 8 to 24 ms on
/// an eight file set, because every module was pulling in megabytes to look at a
/// few hundred bytes. The decode still reads the file it is given; this is for
/// the pass that only describes it. See [`crate::formats`].
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file cannot be opened or read.
pub(crate) fn image_head(path: &Path) -> Result<Vec<u8>> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|error| image_error("open", path, error))?;
    let mut data = Vec::new();
    file.take(PROBE_HEAD_BYTES)
        .read_to_end(&mut data)
        .map_err(|error| image_error("open", path, error))?;
    Ok(data)
}

/// Describes one file without decoding it, for a call that may or may not want
/// its embedded ICC bytes.
///
/// A profile is read either way, because whether the file has one is a fact
/// `ImgSeqHasICC` reports whatever the caller asked for. What the flag decides is
/// whether the bytes are *kept*: every reader here hands over a profile it has
/// already copied out of the file, so a sequence whose files each carry a large
/// one retains a copy per file for the life of the clip if nothing drops them.
/// The measurement is in `docs/improvements/18-demand-aware-decoding.md`.
/// Describes what every listed path contributes to the output timeline.
///
/// A still file contributes one frame; an animated one contributes its
/// displayed timeline sampled onto `fps`. The dispatch lives in
/// [`crate::animation`], which is where the per-format adapters are, so this
/// stays a thin wrapper over [`probe`] for the files no adapter claims.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when a file cannot be probed or its timeline cannot
/// be represented.
pub fn probe_segments(
    files: &[PathBuf],
    fps: Rate,
    apply_rotation: bool,
    export_icc_profile: bool,
) -> Result<SegmentTable> {
    let mut segments = Vec::with_capacity(files.len());
    for path in files {
        segments.push(probe_segment(
            path,
            fps,
            apply_rotation,
            export_icc_profile,
        )?);
    }
    SegmentTable::new(segments)
}

/// Describes what one listed path contributes.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file cannot be probed or its timeline
/// cannot be represented.
pub fn probe_segment(
    path: &Path,
    fps: Rate,
    apply_rotation: bool,
    export_icc_profile: bool,
) -> Result<Segment> {
    let info = probe(path, apply_rotation, export_icc_profile)?;
    // An animated file is described by its own container: the delay of every
    // picture and how to replay it. Whether a file is one is the container's
    // answer, and the probe has already asked it: this reads the route the
    // probe saved rather than the file's name. A format whose
    // adapter finds no timeline is a still, which is one frame and no decoder
    // at all.
    let animated = match info.route {
        Some(Format::Png) => crate::animation::apng::segment_info(path, info.clone(), fps)?,
        Some(Format::Gif) => crate::animation::gif::segment_info(path, info.clone(), fps)?,
        Some(Format::Avif | Format::Heif) => {
            crate::animation::heif::segment_info(path, info.clone(), fps)?
        }
        Some(Format::Jxl) => crate::animation::jxl::segment_info(path, info.clone(), fps)?,
        // A webp whose bitstream is a still image stays on the libwebp path,
        // which is what hands a lossy file out as its own yuv planes. Only an
        // animated one is claimed here.
        Some(Format::Webp) => crate::animation::webp::segment_info(path, info.clone(), fps)?,
        _ => None,
    };
    match animated {
        Some(segment) => segment.into_segment(),
        None => Ok(Segment::still(info)),
    }
}

pub fn probe(path: &Path, apply_rotation: bool, export_icc_profile: bool) -> Result<ImageInfo> {
    let mut info = describe(path, apply_rotation)?;
    if !export_icc_profile {
        info.icc_profile = None;
    }
    Ok(info)
}

fn describe(path: &Path, apply_rotation: bool) -> Result<ImageInfo> {
    // One read of the head answers which module can describe this file, and it
    // is the same call the decode makes: see [`identify::route`]. A module that
    // declines a file its own bytes name -- an openexr whose every layer is deep
    // or not r,g,b, say -- is then followed by the generic decoder below, which
    // is what a chain of fifteen `owns` calls did before: with the content
    // deciding, no other module could have claimed it anyway.
    let route = identify::route(path);
    let described = match route {
        // The two containers whose own reader answers a size without decoding
        // the picture at all.
        Some(Format::Heif) => formats::heif::image_info(path, apply_rotation, route),
        Some(Format::Avif) => formats::avif::image_info(path, apply_rotation),
        // A jpeg xl is read here whether or not the `image` crate could reach a
        // decoder for one, which it cannot: it has no jpeg xl format of its own.
        // These two answer with a description rather than with an option: the
        // route above is the check that used to come before the call.
        Some(Format::Jxl) => Some(formats::jxl::image_info(path, apply_rotation)?),
        Some(Format::Jp2) => Some(formats::jp2::image_info(path, apply_rotation)?),
        // A quite ok image states its size and its channel count in fourteen
        // bytes, and a farbfeld's header is the same shape and just as cheap.
        Some(Format::Qoi) => formats::qoi::image_info(path, apply_rotation, route)?,
        Some(Format::Farbfeld) => formats::farbfeld::image_info(path, apply_rotation, route)?,
        // A bitmap states its depth, its compression and its masks in a header
        // this module reads without touching a sample, and the alpha decision is
        // part of that header rather than of the samples.
        Some(Format::Bmp) => formats::bmp::image_info(path, apply_rotation, route)?,
        // An icon is a directory of payloads, and which one is read is decided
        // by the directory rather than by the frame.
        Some(Format::Ico) => formats::ico::image_info(path, apply_rotation, route)?,
        // A targa states its layout in an eighteen byte header, and the two
        // direction bits in that header decide where the pixels go rather than
        // an orientation property.
        Some(Format::Tga) => formats::tga::image_info(path, apply_rotation, route)?,
        // A surface states its compression in a four character code or in a
        // DXGI format number, and its size has to be a whole number of four by
        // four blocks.
        Some(Format::Dds) => formats::dds::image_info(path, apply_rotation, route)?,
        // A netpbm states its magic, its size and its `MAXVAL` in a text
        // preamble, and that preamble is also where a comment is legal.
        Some(Format::Pnm) => formats::pnm::image_info(path, apply_rotation, route)?,
        // A tagged format, whose directories this module walks without decoding
        // a sample.
        Some(Format::Tiff) => formats::tiff::image_info(path, apply_rotation, route)?,
        // A radiance picture states its layout in a resolution line and its
        // samples as four bytes a pixel.
        Some(Format::Hdr) => formats::hdr::image_info(path, apply_rotation, route)?,
        // An openexr states its layers, their channels and their sample types in
        // its header, and the crate parses that header without decompressing a
        // block.
        Some(Format::Exr) => formats::exr::image_info(path, apply_rotation, route)?,
        // A png is read here for the same reason a jpeg is, and one thing more:
        // the `cICP` chunk is not something the `image` decoder exposes at all.
        Some(Format::Png) => formats::png::image_info(path, apply_rotation, route)?,
        // A jpeg is read here rather than through the generic decoder, whose
        // reader would parse the file's headers four times for one probe.
        Some(Format::Jpeg) => formats::jpeg::image_info(path, apply_rotation, route)?,
        // A gif is read here: the container states the logical screen and its one
        // frame draws a rectangle onto it, so a still gif has a reader of its own.
        Some(Format::Gif) => formats::gif::image_info(path, apply_rotation, route)?,
        // A webp is read here too: the container states the canvas, the alpha
        // flag, the orientation and the profile, so describing one no longer
        // costs a decode of the whole picture.
        Some(Format::Webp) => formats::webp::image_info(path, apply_rotation, route)?,
        // A file no format here names is a file this plugin does not read. It
        // used to be the generic decoder's, which was the last thing `image`
        // was linked for; there is no generic decoder any more.
        None => None,
    };
    let mut info = described.ok_or_else(|| {
        ImgSeqError::new(format!(
            "failed to identify image '{}': no reader here knows its format",
            path.display()
        ))
    })?;
    // The route is saved on the description so that the timeline decision and
    // the decode do not read the head again to reach the same answer.
    info.route = route;
    Ok(info)
}

/// Decodes `info`.
///
/// The format modules go first, and then the two png paths the row walk cannot
/// take. A file none of them answers is one no reader here knows, which is an
/// error rather than a last resort: every format this plugin supports has a
/// reader of its own.
pub fn decode(info: &ImageInfo, demand: Demand) -> Result<DecodedImage> {
    if let Some(decoded) = format_decoder(info, demand) {
        return decoded;
    }
    // A png this module can walk a row at a time is answered with a decode
    // rather than with pixels: the rows go straight into the frame, which is
    // one pass over the picture instead of the two a whole buffer costs. The
    // timings come from the walk, which is where the read happens, so the ones
    // below are a placeholder for a file this branch does not take.
    if let Some(pixels) = formats::png::stream(info) {
        return Ok(DecodedImage {
            width: info.width,
            height: info.height,
            format: info.format,
            transform: info.transform,
            pixels,
            timings: DecodeTimings::default(),
        });
    }
    // An interlaced png is the one shape the row walk refuses: Adam7 hands the file
    // over one pass at a time rather than one picture row at a time. It is decoded
    // whole here, with the same crate call the generic decoder would have made, so
    // the picture is the same and the dependency is not asked for it.
    if let Some(decoded) = formats::png::decode(info)? {
        return Ok(decoded);
    }
    Err(ImgSeqError::new(format!(
        "failed to decode image '{}': no reader here knows its format",
        info.path.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::probe;
    use std::path::Path;

    use super::probe_segment;
    use crate::animation::Rate;
    use crate::layout::{ColorType, SourceColorType};
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    /// A probe reads a file's head once, and the timeline decision reads the
    /// route it saved rather than reading the head again.
    #[test]
    fn a_probe_reads_the_head_once_and_saves_the_route() {
        use crate::formats::identify::{self, Format};

        let path = fixture("alpha-rgb8.png");
        identify::reset_head_reads();
        let segment =
            probe_segment(&path, Rate::from_fps(24, 1), true, false).expect("the fixture probes");
        // The route is the only read: the module the router chose takes the saved
        // answer instead of opening the file to ask the ownership question again.
        assert_eq!(identify::head_reads(), 1, "one probe reads the head once");
        assert_eq!(segment.info.route, Some(Format::Png));
    }

    /// The decode reads the route the probe saved instead of the head again.
    #[test]
    fn a_decode_reads_the_head_none() {
        use crate::formats::identify;

        let path = fixture("alpha-rgb8.png");
        let info = probe(&path, true, false).expect("the fixture probes");
        identify::reset_head_reads();
        let decoded = super::decode(&info, super::Demand::COLOR).expect("the fixture decodes");
        assert!(decoded.width > 0);
        assert_eq!(
            identify::head_reads(),
            0,
            "the decode reuses the saved route"
        );
    }

    /// The fixture animations all state the same 600 ms of four pictures, so
    /// they sample onto the same output timeline whatever their container is.
    #[test]
    fn every_animation_format_samples_onto_the_same_timeline() {
        for name in ["animation.png", "animation.gif", "animation.webp"] {
            let segment = probe_segment(&fixture(name), Rate::from_fps(24, 1), true, false)
                .expect("the fixture probes");
            assert!(segment.animated, "{name}");
            assert_eq!(segment.frame_count(), 15, "{name}");
            assert_eq!(segment.output_size(), (16, 12), "{name}");
            assert_eq!(
                segment.info.format,
                crate::pixel::PixelFormat::Rgb8,
                "{name}"
            );
            // Every output frame shows the picture the file displays at that
            // instant, which is the documented sampling rule.
            let starts = [0i64, 80, 250, 360];
            let durations = [80i64, 170, 110, 240];
            for frame in 0..segment.frame_count() {
                // Output frame `frame` is at `frame * 1000 / 24` ms.
                let instant = frame as i64 * 1000 / 24;
                let expected = starts
                    .iter()
                    .enumerate()
                    .filter(|(index, start)| {
                        **start <= instant
                            && instant < starts.get(index + 1).copied().unwrap_or(i64::MAX)
                    })
                    .map(|(index, _)| index)
                    .next()
                    .unwrap_or_else(|| {
                        starts
                            .iter()
                            .rposition(|start| *start <= instant)
                            .unwrap_or(0)
                    });
                let _ = durations;
                assert_eq!(
                    segment.presentation(frame).unwrap(),
                    expected,
                    "{name} at frame {frame} ({instant} ms)"
                );
            }
        }
    }

    /// An avif and a heif sequence both state their timing in the container's
    /// sample tables, which is what the frames are placed by.
    #[test]
    fn a_sequence_is_placed_by_its_own_sample_tables() {
        for name in ["animation.avif", "animation.heic"] {
            let segment = probe_segment(&fixture(name), Rate::from_fps(24, 1), true, false)
                .expect("the fixture probes");
            assert!(segment.animated, "{name}");
            assert_eq!(segment.output_size(), (16, 12), "{name}");
            // The avif states 80/170/110/240 ms and the heic four 150 ms
            // samples; both are 600 ms, which ends 14.4 ticks in at 24 fps.
            assert_eq!(segment.frame_count(), 15, "{name}");
            assert_eq!(
                segment.info.format,
                crate::pixel::PixelFormat::Rgb8,
                "{name}"
            );
        }
    }

    /// An animated jpeg xl is scanned for its timing rather than replayed, and
    /// its frames keep the depth the codestream states.
    #[test]
    fn an_animated_jpeg_xl_is_scanned_for_its_timeline() {
        let segment = probe_segment(
            &fixture("animation.jxl"),
            Rate::from_fps(24, 1),
            true,
            false,
        )
        .expect("the fixture probes");
        assert!(segment.animated);
        assert_eq!(segment.frame_count(), 15);
        assert_eq!(segment.output_size(), (16, 12));
        assert_eq!(segment.info.format, crate::pixel::PixelFormat::Rgb8);
        // The codestream states its timeline as ticks of a 1000 Hz timescale,
        // which is the rate the segment keeps.
        assert_eq!(segment.rate, Rate::new(1000, 1));
    }

    /// The animation adapter declines a still from the codestream header, which
    /// is what keeps a still jpeg xl off the frame scan.
    #[test]
    fn only_an_animated_jpeg_xl_states_a_timeline() {
        let animated = crate::formats::jxl::states_animation(&fixture("animation.jxl"))
            .expect("the fixture reads");
        assert!(animated, "the fixture holds four pictures");
        for name in ["alpha-rgba8.jxl", "jxl-gray10.jxl"] {
            let still =
                crate::formats::jxl::states_animation(&fixture(name)).expect("the fixture reads");
            assert!(!still, "{name}");
        }
    }

    /// A still jpeg xl stays one frame.
    #[test]
    fn a_still_jpeg_xl_is_one_frame() {
        for name in ["alpha-rgba8.jxl", "jxl-gray10.jxl"] {
            let segment = probe_segment(&fixture(name), Rate::from_fps(24, 1), true, false)
                .expect("the fixture probes");
            assert!(!segment.animated, "{name}");
            assert_eq!(segment.frame_count(), 1, "{name}");
        }
    }

    /// A description reads an animated png's delays from its own chunks: it
    /// renders no frame to do it.
    #[test]
    fn describing_an_apng_renders_no_frame() {
        crate::animation::apng::reset_frames_decoded();
        let segment = probe_segment(
            &fixture("animation.png"),
            Rate::from_fps(24, 1),
            true,
            false,
        )
        .expect("the fixture probes");
        assert!(segment.animated);
        assert_eq!(
            crate::animation::apng::frames_decoded(),
            0,
            "a description renders no frame"
        );
    }

    /// A 16-bit APNG keeps its depth, which the `image` compositor cannot do.
    #[test]
    fn a_sixteen_bit_apng_keeps_its_depth() {
        let segment = probe_segment(
            &fixture("animation-rgba16.png"),
            Rate::from_fps(24, 1),
            true,
            false,
        )
        .expect("the fixture probes");
        assert!(segment.animated);
        assert_eq!(segment.info.format, crate::pixel::PixelFormat::Rgb16);
        assert_eq!(segment.info.original_color_type, SourceColorType::Rgba16);
        assert_eq!(segment.output_size(), (4, 3));
        // The fixture's two pictures hold for 100 ms and 200 ms, which is
        // 7.2 output ticks at 24 fps, so eight instants fall before it ends.
        assert_eq!(segment.frame_count(), 8);
        // The first picture is shown at tick 0 and the second at `ceil(2.4)`,
        // which is tick 3.
        assert_eq!(segment.presentation(0).unwrap(), 0);
        assert_eq!(segment.presentation(2).unwrap(), 0);
        assert_eq!(segment.presentation(3).unwrap(), 1);
        assert_eq!(segment.presentation(6).unwrap(), 1);
    }

    /// A still of a format that has an animation path stays one frame.
    #[test]
    fn a_still_file_is_one_frame_however_it_is_stored() {
        for name in ["alpha-rgba8.png", "alpha-rgb8.png"] {
            let segment = probe_segment(&fixture(name), Rate::from_fps(24, 1), true, false)
                .expect("the fixture probes");
            assert!(!segment.animated, "{name}");
            assert_eq!(segment.frame_count(), 1, "{name}");
        }
    }

    /// An animated file whose pictures are all smaller than one output tick is
    /// still shown once rather than dropped.
    #[test]
    fn slides_that_fall_between_ticks_are_omitted() {
        let segment = probe_segment(
            &fixture("animation.gif"),
            Rate::from_fps(1000, 1),
            true,
            false,
        )
        .expect("the fixture probes");
        // One tick is a millisecond at 1000 fps, so every one of the 600 ms of
        // pictures gets its own output frame.
        assert_eq!(segment.frame_count(), 600);
    }

    /// The orientation a file states is applied to animated frames too.
    #[test]
    fn an_animation_reports_its_own_format_and_transform() {
        let segment = probe_segment(
            &fixture("animation-rgba16.png"),
            Rate::from_fps(24, 1),
            false,
            false,
        )
        .expect("the fixture probes");
        assert_eq!(segment.info.color_type, ColorType::Rgba16);
        assert_eq!(
            segment.output_size(),
            (segment.info.output_width(), segment.info.output_height())
        );
    }

    /// A profile's bytes are kept only when the caller asked for them, and
    /// whether the file has one is reported either way.
    #[test]
    fn an_icc_profile_is_kept_only_when_it_is_exported() {
        let path = Path::new("tests/fixtures/icc-rgb8.png");
        let kept = probe(path, true, true).expect("the fixture probes");
        assert!(kept.has_icc_profile);
        assert!(kept.icc_profile.is_some());

        let dropped = probe(path, true, false).expect("the fixture probes");
        assert!(
            dropped.has_icc_profile,
            "the file still states that it carries a profile"
        );
        assert!(dropped.icc_profile.is_none(), "but its bytes are not kept");

        // A file without a profile states that too, whatever was asked for.
        let plain = Path::new("tests/fixtures/alpha-rgb8.png");
        for export in [true, false] {
            let info = probe(plain, true, export).expect("the fixture probes");
            assert!(!info.has_icc_profile, "export={export}");
            assert!(info.icc_profile.is_none(), "export={export}");
        }
    }
}
