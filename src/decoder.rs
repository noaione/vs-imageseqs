use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
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
    still,
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
    // One read of the head answers which module owns this file, and it is the
    // same call the probe makes -- see [`identify::route`]. This replaces a
    // chain of sixteen `owns` calls, one open each, that had to agree with
    // `describe` and could not be kept in agreement: a module answering from the
    // name rather than from the bytes probed as one format and decoded as
    // another.
    Some(match identify::route(&info.path) {
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
        Some(Format::Png | Format::Gif) | None => return None,
    })
}

/// Format a module decodes this file into, when it is not the one the probed
/// color type suggests; see [`crate::formats`].
fn format_override(path: &Path, color_type: ColorType) -> Option<PixelFormat> {
    formats::webp::output_format(path, color_type)
        .or_else(|| formats::avif::output_format(path, color_type))
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

fn open_decoder(path: &Path) -> Result<still::Decoder> {
    // No hooks are registered here any more. Every container this plugin reads
    // is read by the module that owns it, and the libheif integration that
    // taught `image` to read an avif or a heif is gone with them.
    still::Decoder::open(path).map_err(|error| image_error(error.action, path, error.detail))
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
    // picture and how to replay it. A file no adapter claims is a still, which
    // is one frame and no decoder at all, and that is also what an adapter
    // answers for a file of its format that turns out not to be animated.
    let animated = if crate::animation::apng::owns(path) {
        crate::animation::apng::segment_info(path, info.clone(), fps)?
    } else if crate::animation::gif::owns(path) {
        crate::animation::gif::segment_info(path, info.clone(), fps)?
    } else if crate::animation::heif::owns_avif(path) || crate::animation::heif::owns_heif(path) {
        crate::animation::heif::segment_info(path, info.clone(), fps)?
    } else if crate::animation::jxl::owns(path) {
        crate::animation::jxl::segment_info(path, info.clone(), fps)?
    } else if crate::animation::webp::owns(path) {
        // A webp whose bitstream is a still image stays on the libwebp path,
        // which is what hands a lossy file out as its own yuv planes. Only an
        // animated one is claimed here.
        crate::animation::webp::segment_info(path, info.clone(), fps)?
    } else {
        None
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
    let described = match identify::route(path) {
        // The two containers whose own reader answers a size without decoding
        // the picture at all.
        Some(Format::Heif) => formats::heif::image_info(path, apply_rotation),
        Some(Format::Avif) => formats::avif::image_info(path, apply_rotation),
        // A jpeg xl is read here whether or not the `image` crate could reach a
        // decoder for one, which it cannot: it has no jpeg xl format of its own.
        // These two answer with a description rather than with an option: the
        // route above is the check that used to come before the call.
        Some(Format::Jxl) => Some(formats::jxl::image_info(path, apply_rotation)?),
        Some(Format::Jp2) => Some(formats::jp2::image_info(path, apply_rotation)?),
        // A quite ok image states its size and its channel count in fourteen
        // bytes, and a farbfeld's header is the same shape and just as cheap.
        Some(Format::Qoi) => formats::qoi::image_info(path, apply_rotation)?,
        Some(Format::Farbfeld) => formats::farbfeld::image_info(path, apply_rotation)?,
        // A bitmap states its depth, its compression and its masks in a header
        // this module reads without touching a sample, and the alpha decision is
        // part of that header rather than of the samples.
        Some(Format::Bmp) => formats::bmp::image_info(path, apply_rotation)?,
        // An icon is a directory of payloads, and which one is read is decided
        // by the directory rather than by the frame.
        Some(Format::Ico) => formats::ico::image_info(path, apply_rotation)?,
        // A targa states its layout in an eighteen byte header, and the two
        // direction bits in that header decide where the pixels go rather than
        // an orientation property.
        Some(Format::Tga) => formats::tga::image_info(path, apply_rotation)?,
        // A surface states its compression in a four character code or in a
        // DXGI format number, and its size has to be a whole number of four by
        // four blocks.
        Some(Format::Dds) => formats::dds::image_info(path, apply_rotation)?,
        // A netpbm states its magic, its size and its `MAXVAL` in a text
        // preamble, and that preamble is also where a comment is legal.
        Some(Format::Pnm) => formats::pnm::image_info(path, apply_rotation)?,
        // A tagged format, whose directories this module walks without decoding
        // a sample.
        Some(Format::Tiff) => formats::tiff::image_info(path, apply_rotation)?,
        // A radiance picture states its layout in a resolution line and its
        // samples as four bytes a pixel.
        Some(Format::Hdr) => formats::hdr::image_info(path, apply_rotation)?,
        // An openexr states its layers, their channels and their sample types in
        // its header, and the crate parses that header without decompressing a
        // block.
        Some(Format::Exr) => formats::exr::image_info(path, apply_rotation)?,
        // A png is read here for the same reason a jpeg is, and one thing more:
        // the `cICP` chunk is not something the `image` decoder exposes at all.
        Some(Format::Png) => formats::png::image_info(path, apply_rotation)?,
        // A jpeg is read here rather than through the generic decoder, whose
        // reader would parse the file's headers four times for one probe.
        Some(Format::Jpeg) => formats::jpeg::image_info(path, apply_rotation)?,
        // A gif and a webp are the generic decoder's, which is where they have
        // always been: the animation module composes a gif's timeline, but a one
        // frame gif is a still and this reader has no gif module of its own, and
        // a webp states its lossy yuv arrangement somewhere only `output_format`
        // reads, which the path below already asks for.
        Some(Format::Gif | Format::Webp) | None => None,
    };
    if let Some(info) = described {
        return Ok(info);
    }

    let decoder = open_decoder(path)?;
    let metadata = decoder.metadata();
    let (width, height) = (metadata.width, metadata.height);
    let color_type = metadata.color_type;
    let original_color_type = metadata.original_color_type;
    let icc_profile = decoder.icc_profile().map(Arc::<[u8]>::from);
    // The containers that state a colour description do it somewhere the `image`
    // decoder has no accessor for: a heif item property inside a `libheif`
    // handle, or a chunk beside the data of a png. Both are read from the file
    // rather than from the decoder, and both decline a file of another kind
    // without opening it.
    let cicp = formats::heif::cicp(path).or_else(|| formats::png::cicp(path));
    let orientation = metadata.orientation;
    let format = format_override(path, color_type)
        .or_else(|| PixelFormat::from_color_type(color_type))
        .ok_or_else(|| {
            ImgSeqError::new(format!(
                "unsupported color type {color_type:?} in image '{}'",
                path.display()
            ))
        })?;

    Ok(ImageInfo {
        path: path.to_path_buf(),
        width,
        height,
        color_type,
        original_color_type,
        has_icc_profile: icc_profile.is_some(),
        icc_profile,
        cicp,
        // The `image` decoders have no accessor for a chroma sample position,
        // and the containers that state one are read by the modules that know
        // how to read it out of their own boxes.
        chroma_location: None,
        orientation,
        transform: if apply_rotation {
            Transform::from_orientation(orientation)
        } else {
            Transform::IDENTITY
        },
        format,
    })
}

/// Decodes `info` with the `image` crate.
///
/// This is the adapter every format module hands a file to when it is not one
/// that module reads itself. It is a function rather than the tail of [`decode`]
/// because a module can be the one that *knows* a file is not its own -- an
/// avif whose container the walk refuses is libheif's, and one whose samples
/// have no planar yuv format is this path's -- and the alternative is for every
/// such module to re-walk the container to find that out a second time.
/// Decodes `info`.
///
/// The format modules go first, then the png row walk, then the `image` crate;
/// see [`decode_through_image`] for the last of those.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when no path produces the frame.
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
    decode_through_image(info)
}

/// Decodes `info` with the `image` crate.
///
/// This is the adapter every format module hands a file to when it is not one
/// that module reads itself. It is a function rather than the tail of [`decode`]
/// because a module can be the one that *knows* a file is not its own -- an
/// avif whose container the walk refuses is libheif's, and one whose samples
/// have no planar yuv format is this path's -- and the alternative is for every
/// such module to re-walk the container to find that out a second time.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file cannot be read, or when it changed
/// between the probe and this read.
pub(crate) fn decode_through_image(info: &ImageInfo) -> Result<DecodedImage> {
    let open_started = Instant::now();
    let decoder = open_decoder(&info.path)?;
    let open = open_started.elapsed();

    let metadata_started = Instant::now();
    let probed = decoder.metadata();
    let (width, height, color_type) = (probed.width, probed.height, probed.color_type);
    let metadata = metadata_started.elapsed();
    if (width, height, color_type) != (info.width, info.height, info.color_type) {
        return Err(ImgSeqError::new(format!(
            "image '{}' changed after probing (was {old_width}x{old_height} {old:?}, now {new_width}x{new_height} {new:?})",
            info.path.display(),
            old_width = info.width,
            old_height = info.height,
            old = info.color_type,
            new_width = width,
            new_height = height,
            new = color_type,
        )));
    }

    let size = usize::try_from(probed.total_bytes).map_err(|_| {
        ImgSeqError::new(format!(
            "decoded image '{}' is too large for this platform",
            info.path.display()
        ))
    })?;
    let buffer_started = Instant::now();
    let mut pixels = vec![0; size];
    let buffer = buffer_started.elapsed();
    let read_started = Instant::now();
    decoder
        .read(&mut pixels)
        .map_err(|error| image_error(error.action, &info.path, error.detail))?;
    let read = read_started.elapsed();

    Ok(DecodedImage {
        width,
        height,
        format: info.format,
        transform: info.transform,
        pixels: Pixels::Interleaved {
            color_type,
            buffer: pixels,
        },
        timings: DecodeTimings {
            open,
            metadata,
            buffer,
            read,
        },
    })
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

    /// The fixture animations all state the same 600 ms of four pictures, so
    /// they sample onto the same output timeline whatever their container is.
    #[test]
    fn every_animation_format_samples_onto_the_same_timeline() {
        for name in ["animation.png", "animation.gif", "animation.webp"] {
            let segment = probe_segment(&fixture(name), Rate::from_fps(24, 1), true, false)
                .expect("the fixture probes");
            assert!(segment.animated, "{name}");
            assert_eq!(segment.frame_count(), 14, "{name}");
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
            // samples; both are 600 ms, which is 14.4 ticks at 24 fps.
            assert_eq!(segment.frame_count(), 14, "{name}");
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
        assert_eq!(segment.frame_count(), 14);
        assert_eq!(segment.output_size(), (16, 12));
        assert_eq!(segment.info.format, crate::pixel::PixelFormat::Rgb8);
        // The codestream states its timeline as ticks of a 1000 Hz timescale,
        // which is the rate the segment keeps.
        assert_eq!(segment.rate, Rate::new(1000, 1));
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
        // 7.2 output ticks at 24 fps, so seven ticks start before it ends.
        assert_eq!(segment.frame_count(), 7);
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
