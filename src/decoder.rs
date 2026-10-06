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
        // Farbfeld has one spelling and no compression, so its rows
        // go straight into the frame and there is no buffered shape left.
        Some(Format::Farbfeld) => formats::farbfeld::stream(info),
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
    /// The subimage the probe selected, in whatever namespace the format uses
    /// for one -- an icon's directory index, say. Kept for the same reason as
    /// [`Self::route`]: a decode of this `ImageInfo` reads the subimage the
    /// probe described rather than scoring the container again. It is `None`
    /// for an `ImageInfo` that no probe built, which selects one itself.
    pub subimage: Option<usize>,
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

    /// As [`Self::place_rgb8`], for a source whose fourth sample is alpha.
    ///
    /// The alpha sample goes to the alpha clip when the call hands one out and
    /// is dropped when it does not, which is what the planar writer does with a
    /// four channel layout too: a file that carries an alpha channel costs
    /// nothing extra for a graph that never asks for it.
    ///
    /// Answers `None` when the sink does not hold three colour planes, which a
    /// caller reports its own way.
    pub fn place_rgba8(&mut self, source: &[u8], row: usize) -> Option<()> {
        let (red, rest) = self.colour.split_first_mut()?;
        let (green, rest) = rest.split_first_mut()?;
        let (blue, _) = rest.split_first_mut()?;
        let alpha = self.alpha.as_mut().and_then(|planes| planes.first_mut());
        let red_row = red.row(row);
        let green_row = green.row(row);
        let blue_row = blue.row(row);
        let alpha_row = alpha.map(|plane| plane.row(row));
        // One row read once: a pixel's four samples are taken apart in the same
        // walk that writes them, and the alpha plane joins it only when there is
        // one to write into.
        let pixels = source.as_chunks::<4>().0.iter();
        match alpha_row {
            Some(alpha_row) => {
                for ((((red_byte, green_byte), blue_byte), alpha_byte), pixel) in red_row
                    .iter_mut()
                    .zip(green_row)
                    .zip(blue_row)
                    .zip(alpha_row)
                    .zip(pixels)
                {
                    *red_byte = pixel[0];
                    *green_byte = pixel[1];
                    *blue_byte = pixel[2];
                    *alpha_byte = pixel[3];
                }
            }
            None => {
                for (((red_byte, green_byte), blue_byte), pixel) in
                    red_row.iter_mut().zip(green_row).zip(blue_row).zip(pixels)
                {
                    *red_byte = pixel[0];
                    *green_byte = pixel[1];
                    *blue_byte = pixel[2];
                }
            }
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

    /// Writes one interleaved sixteen bit four channel row into the three
    /// colour planes and, when the call hands one out, the alpha plane.
    ///
    /// Farbfeld is the caller: four big-endian `u16` a pixel and nothing else, so
    /// the swap into the byte order a frame holds happens in the same walk that
    /// takes the row apart rather than as a pass over the whole picture first.
    ///
    /// The alpha channel is written only when the call asked for an alpha clip.
    /// A call that hands out none reads three channels and drops the fourth, and
    /// that is the format's own choice: every farbfeld has four channels and
    /// [`RowStream::has_alpha`] is what tells the caller the alpha clip is not
    /// left to an opaque fill.
    ///
    /// Answers `None` when the sink does not hold three colour planes, which a
    /// caller reports its own way.
    pub fn place_rgba16(&mut self, source: &[u8], row: usize) -> Option<()> {
        let (red, rest) = self.colour.split_first_mut()?;
        let (green, rest) = rest.split_first_mut()?;
        let (blue, _) = rest.split_first_mut()?;
        let alpha = self.alpha.as_mut().and_then(|planes| planes.first_mut());
        // One source row, read once. A pixel is four big-endian samples and each
        // target takes one of them in the byte order the frame holds, which is what
        // makes this the only pass over the row.
        let sample = |pixel: &[u8], channel: usize| -> [u8; 2] {
            u16::from_be_bytes([pixel[channel], pixel[channel + 1]]).to_ne_bytes()
        };
        let red_row = red.row(row);
        let green_row = green.row(row);
        let blue_row = blue.row(row);
        let alpha_row = alpha.map(|plane| plane.row(row));
        // A frame's row is a whole number of sixteen bit samples, so each plane
        // splits into samples and the leftovers are the frame's own padding.
        let (red_words, _) = red_row.as_chunks_mut::<2>();
        let (green_words, _) = green_row.as_chunks_mut::<2>();
        let (blue_words, _) = blue_row.as_chunks_mut::<2>();
        let colour = red_words.iter_mut().zip(green_words).zip(blue_words);
        let pixels = source.as_chunks::<8>().0.iter();
        match alpha_row {
            Some(alpha_row) => {
                let (alpha_words, _) = alpha_row.as_chunks_mut::<2>();
                for ((((r, g), b), a), pixel) in colour.zip(alpha_words.iter_mut()).zip(pixels) {
                    r.copy_from_slice(&sample(pixel, 0));
                    g.copy_from_slice(&sample(pixel, 2));
                    b.copy_from_slice(&sample(pixel, 4));
                    a.copy_from_slice(&sample(pixel, 6));
                }
            }
            None => {
                for (((r, g), b), pixel) in colour.zip(pixels) {
                    r.copy_from_slice(&sample(pixel, 0));
                    g.copy_from_slice(&sample(pixel, 2));
                    b.copy_from_slice(&sample(pixel, 4));
                }
            }
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
    /// One buffer holding every plane, laid out the way a separate-planar
    /// decoder laid it out: `plane_stride` bytes from one plane to the next,
    /// and `row_stride` bytes from one row of a plane to the next.
    ///
    /// This is [`Self::Planar`] without the copy. A planar page is already one
    /// plane per channel, so splitting it into a buffer a plane spends a whole
    /// picture of allocation and a whole picture of memory traffic on a picture
    /// that is already in the shape the frame wants. Handing the decoder's own
    /// buffer over with its stride attached is the same picture for one buffer;
    /// see `docs/improvements/34-input-routing-and-planar-decode.md`.
    ///
    /// `alpha` says whether the plane after the colour ones is the alpha clip
    /// rather than a fourth colour channel. It is the same question
    /// [`Self::Planar`] answers with an `Option`, asked of the buffer rather than
    /// of a value: the plane is still inside the one buffer, one stride after the
    /// last colour plane.
    Strided {
        /// How many planes the buffer holds, one per channel.
        planes: usize,
        /// Whether the plane after the colour ones is the alpha clip.
        alpha: bool,
        /// The decoder's own buffer, the planes laid end to end inside it.
        buffer: Vec<u8>,
        /// Bytes from one row of a plane to the next.
        row_stride: usize,
        /// Bytes from one plane to the next.
        plane_stride: usize,
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
            Self::Strided {
                planes,
                alpha,
                buffer,
                row_stride,
                plane_stride,
            } => Self::Strided {
                planes: *planes,
                alpha: *alpha,
                buffer: buffer.clone(),
                row_stride: *row_stride,
                plane_stride: *plane_stride,
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
            (
                Self::Strided {
                    planes: left_planes,
                    alpha: left_alpha,
                    buffer: left_buffer,
                    row_stride: left_row_stride,
                    plane_stride: left_plane_stride,
                },
                Self::Strided {
                    planes: right_planes,
                    alpha: right_alpha,
                    buffer: right_buffer,
                    row_stride: right_row_stride,
                    plane_stride: right_plane_stride,
                },
            ) => {
                left_planes == right_planes
                    && left_alpha == right_alpha
                    && left_buffer == right_buffer
                    && left_row_stride == right_row_stride
                    && left_plane_stride == right_plane_stride
            }
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
#[cfg(test)]
pub(crate) fn image_head(path: &Path) -> Result<Vec<u8>> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|error| image_error("open", path, error))?;
    let mut data = Vec::new();
    file.take(PROBE_HEAD_BYTES)
        .read_to_end(&mut data)
        .map_err(|error| image_error("open", path, error))?;
    Ok(data)
}

/// One file, opened once, and the leading bytes read from it at most once.
///
/// A probe asks two questions of a file -- which format owns it, and what that
/// format states -- and until now each asked by opening it. This is the one open
/// both ask through, with the leading bytes kept and grown on demand rather than
/// read twice.
///
/// The window grows because the two questions want different amounts: the route
/// needs sixteen bytes, and a module whose header is a bitmap's palette wants the
/// [`PROBE_HEAD_BYTES`]. A module that opens for itself still may; what this
/// removes is the read that answered the same question twice.
#[derive(Debug)]
pub struct Input {
    path: PathBuf,
    /// The open itself, held rather than reopened, so the window below and the
    /// container walk that follows it are one open of the file.
    reader: std::io::BufReader<std::fs::File>,
    /// What has been read so far, which is the whole file when it is shorter.
    head: Vec<u8>,
}

impl Input {
    /// Opens `path` without reading anything.
    ///
    /// # Errors
    ///
    /// Returns [`ImgSeqError`] when the file cannot be opened.
    pub fn open(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path).map_err(|error| image_error("open", path, error))?;
        #[cfg(test)]
        INPUT_OPENS.with(|count| count.set(count.get() + 1));
        Ok(Self {
            path: path.to_path_buf(),
            reader: std::io::BufReader::new(file),
            head: Vec::new(),
        })
    }

    /// The first `want` bytes of the file, read once through the one open this holds.
    ///
    /// Asking for fewer than are already held answers from what is there rather than
    /// re-reading, which is what makes the route's sixteen bytes free once a module has
    /// asked for the window. Asking for more extends what is held from where it stops,
    /// so the sixteen the route read first are not read a second time.
    ///
    /// # Errors
    ///
    /// Returns [`ImgSeqError`] when the file cannot be read.
    pub fn head(&mut self, want: usize) -> Result<&[u8]> {
        if (self.head.len() as u64) < want as u64 {
            use std::io::{Read, Seek, SeekFrom};
            #[cfg(test)]
            let first = self.head.is_empty();
            let held = self.head.len();
            // The window grows from where it stops, so the sixteen bytes the route
            // read are not read a second time. The seek is absolute, which is what
            // lets a caller have moved the reader before asking again.
            self.reader
                .seek(SeekFrom::Start(held as u64))
                .map_err(|error| image_error("open", &self.path, error))?;
            let mut more = Vec::new();
            self.reader
                .by_ref()
                .take((want - held) as u64)
                .read_to_end(&mut more)
                .map_err(|error| image_error("open", &self.path, error))?;
            self.head.extend_from_slice(&more);
            // One read of the front per open, however many times it is asked for.
            #[cfg(test)]
            if first {
                HEAD_READS.with(|count| count.set(count.get() + 1));
            }
        }
        Ok(&self.head)
    }

    /// The open this holds, rewound to the front of the file.
    ///
    /// A timeline reader wants the container rather than the window the route and
    /// the describing module asked for, so it reads the same open at its start
    /// instead of opening the file a second time. Where the reader is left is the
    /// caller's until it asks again, which is why [`Self::head`] seeks absolutely
    /// rather than from wherever the reader happens to be.
    ///
    /// # Errors
    ///
    /// Returns [`ImgSeqError`] when the file cannot be rewound.
    pub fn reader(&mut self) -> Result<&mut std::io::BufReader<std::fs::File>> {
        use std::io::{Seek, SeekFrom};
        self.reader
            .seek(SeekFrom::Start(0))
            .map_err(|error| image_error("open", &self.path, error))?;
        Ok(&mut self.reader)
    }
}

/// How many reads of a file's leading bytes a probe has made on this thread.
///
/// This is the measure [`crate::formats::identify::head_reads`] used to be, moved
/// here: the routing read no longer opens the file itself, so a probe's one head
/// read is made here, by the one input the whole probe asks through.
#[cfg(test)]
pub(crate) fn head_reads() -> usize {
    HEAD_READS.with(std::cell::Cell::get)
}

#[cfg(test)]
thread_local! {
static HEAD_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Starts [`head_reads`] from zero.
#[cfg(test)]
pub(crate) fn reset_head_reads() {
    HEAD_READS.with(|count| count.set(0));
}

/// How many times this thread has opened a file through [`Input`].
///
/// This is the measure the animation adapters used to have a number for: each of
/// the four that went looking for a timeline opened the file for itself, on top of
/// the open the probe had already made. They read this input now, so what is left
/// here is the one open a probe cannot do without. A module that still opens for
/// itself is invisible to this counter by construction; the plan under
/// `docs/improvements/34-input-routing-and-planar-decode.md` lists which.
#[cfg(test)]
pub(crate) fn input_opens() -> usize {
    INPUT_OPENS.with(std::cell::Cell::get)
}

#[cfg(test)]
thread_local! {
    static INPUT_OPENS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Starts [`input_opens`] from zero.
#[cfg(test)]
pub(crate) fn reset_input_opens() {
    INPUT_OPENS.with(|count| count.set(0));
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
    // One open answers the whole probe: which module owns the file, what that
    // module states about it, and what its container says about a timeline. Which
    // of them reads the file is still each one's own choice, but it is opened here
    // once, and the adapters that used to open it again read this instead.
    let mut input = Input::open(path)?;
    let mut info = describe(path, apply_rotation, &mut input)?;
    keep_profile(&mut info, export_icc_profile);
    // An animated file is described by its own container: the delay of every
    // picture and how to replay it. Whether a file is one is the container's
    // answer, and the probe has already asked it: this reads the route the
    // probe saved rather than the file's name. A format whose
    // adapter finds no timeline is a still, which is one frame and no decoder
    // at all.
    let animated = match info.route {
        Some(Format::Png) => {
            crate::animation::apng::segment_info(path, info.clone(), fps, input.reader()?)?
        }
        Some(Format::Gif) => {
            crate::animation::gif::segment_info(path, info.clone(), fps, input.reader()?)?
        }
        Some(Format::Avif | Format::Heif) => {
            crate::animation::heif::segment_info(path, info.clone(), fps, input.reader()?)?
        }
        // A jpeg xl's scan still opens for itself; the plan lists it with the rest
        // of the modules that do.
        Some(Format::Jxl) => {
            crate::animation::jxl::segment_info(path, info.clone(), fps, input.reader()?)?
        }
        // A webp whose bitstream is a still image stays on the libwebp path,
        // which is what hands a lossy file out as its own yuv planes. Only an
        // animated one is claimed here.
        Some(Format::Webp) => {
            crate::animation::webp::segment_info(path, info.clone(), fps, input.reader()?)?
        }
        _ => None,
    };
    match animated {
        Some(segment) => segment.into_segment(),
        None => Ok(Segment::still(info)),
    }
}

/// Describes one file and nothing else, which is the form a caller that wants a
/// description rather than a timeline uses.
///
/// The clip path does not come through here: [`probe_segment`] needs the open the
/// description was made through, because the container walk that follows reads it
/// too, and a probe that threw the open away would make the adapters reopen.
#[cfg(test)]
pub fn probe(path: &Path, apply_rotation: bool, export_icc_profile: bool) -> Result<ImageInfo> {
    let mut input = Input::open(path)?;
    let mut info = describe(path, apply_rotation, &mut input)?;
    keep_profile(&mut info, export_icc_profile);
    Ok(info)
}

/// Keeps an embedded ICC profile only for a caller that asked to export it.
///
/// Whether a file has one is a fact `ImgSeqHasICC` reports either way; what this
/// decides is whether the bytes are retained for the life of the clip.
fn keep_profile(info: &mut ImageInfo, export_icc_profile: bool) {
    if !export_icc_profile {
        info.icc_profile = None;
    }
}

/// Describes one file through the open the caller holds.
///
/// The route and the module that describes the file read one window out of it,
/// and a caller that also wants a timeline reads that from the same open; see
/// [`probe_segment`].
fn describe(path: &Path, apply_rotation: bool, input: &mut Input) -> Result<ImageInfo> {
    // One read of the head answers which module can describe this file, and it
    // is the same call the decode makes: see [`identify::route`]. A module that
    // declines a file its own bytes name -- an openexr whose every layer is deep
    // or not r,g,b, say -- is then followed by the generic decoder below, which
    // is what a chain of fifteen `owns` calls did before: with the content
    // deciding, no other module could have claimed it anyway.
    // One open answers both questions: the route reads sixteen bytes of it and
    // the module below reads the window from the same input rather than opening the
    // file a second time. A module that reads past the window still opens for itself,
    // and those are listed in the plan as what is left.
    let route = {
        let head = input.head(crate::formats::identify::HEAD_BYTES)?;
        identify::route_from(head, path)
    };
    // A module that reads the window asks for it here, and the read is the same one
    // the route would have made, so the two questions share it.
    fn window(input: &mut Input) -> Result<&[u8]> {
        input.head(PROBE_HEAD_BYTES as usize)
    }
    let described = match route {
        // The two containers whose own reader answers a size without decoding
        // the picture at all.
        Some(Format::Heif) => {
            formats::heif::image_info(path, apply_rotation, route, input.reader()?)
        }
        Some(Format::Avif) => formats::avif::image_info(path, apply_rotation, input.reader()?),
        // A jpeg xl is read here whether or not the `image` crate could reach a
        // decoder for one, which it cannot: it has no jpeg xl format of its own.
        // These two answer with a description rather than with an option: the
        // route above is the check that used to come before the call.
        Some(Format::Jxl) => Some(formats::jxl::image_info(
            path,
            apply_rotation,
            input.reader()?,
        )?),
        Some(Format::Jp2) => Some(formats::jp2::image_info(
            path,
            apply_rotation,
            input.reader()?,
        )?),
        // A quite ok image states its size and its channel count in fourteen
        // bytes, and a farbfeld's header is the same shape and just as cheap.
        Some(Format::Qoi) => {
            let data = window(input)?;
            formats::qoi::image_info_headed(path, apply_rotation, route, data)?
        }
        Some(Format::Farbfeld) => {
            let data = window(input)?;
            formats::farbfeld::image_info_headed(path, apply_rotation, route, data)?
        }
        // A bitmap states its depth, its compression and its masks in a header
        // this module reads without touching a sample, and the alpha decision is
        // part of that header rather than of the samples.
        Some(Format::Bmp) => {
            let data = window(input)?;
            formats::bmp::image_info_headed(path, apply_rotation, route, data)?
        }
        // An icon is a directory of payloads, and which one is read is decided
        // by the directory rather than by the frame.
        Some(Format::Ico) => {
            formats::ico::image_info(path, apply_rotation, route, input.reader()?)?
        }
        // A targa states its layout in an eighteen byte header, and the two
        // direction bits in that header decide where the pixels go rather than
        // an orientation property.
        Some(Format::Tga) => {
            let data = window(input)?;
            formats::tga::image_info_headed(path, apply_rotation, route, data)?
        }
        // A surface states its compression in a four character code or in a
        // DXGI format number, and its size has to be a whole number of four by
        // four blocks.
        Some(Format::Dds) => {
            let data = window(input)?;
            formats::dds::image_info_headed(path, apply_rotation, route, data)?
        }
        // A netpbm states its magic, its size and its `MAXVAL` in a text
        // preamble, and that preamble is also where a comment is legal.
        Some(Format::Pnm) => {
            formats::pnm::image_info(path, apply_rotation, route, input.reader()?)?
        }
        // A tagged format, whose directories this module walks without decoding
        // a sample.
        Some(Format::Tiff) => {
            formats::tiff::image_info(path, apply_rotation, route, input.reader()?)?
        }
        // A radiance picture states its layout in a resolution line and its
        // samples as four bytes a pixel.
        Some(Format::Hdr) => {
            let data = window(input)?;
            formats::hdr::image_info_headed(path, apply_rotation, route, data)?
        }
        // An openexr states its layers, their channels and their sample types in
        // its header, and the crate parses that header without decompressing a
        // block.
        Some(Format::Exr) => {
            formats::exr::image_info(path, apply_rotation, route, input.reader()?)?
        }
        // A png is read here for the same reason a jpeg is, and one thing more:
        // the `cICP` chunk is not something the `image` decoder exposes at all.
        Some(Format::Png) => {
            formats::png::image_info(path, apply_rotation, route, input.reader()?)?
        }
        // A jpeg is read here rather than through the generic decoder, whose
        // reader would parse the file's headers four times for one probe.
        Some(Format::Jpeg) => {
            formats::jpeg::image_info(path, apply_rotation, route, input.reader()?)?
        }
        // A gif is read here: the container states the logical screen and its one
        // frame draws a rectangle onto it, so a still gif has a reader of its own.
        Some(Format::Gif) => {
            formats::gif::image_info(path, apply_rotation, route, input.reader()?)?
        }
        // A webp is read here too: the container states the canvas, the alpha
        // flag, the orientation and the profile, so describing one no longer
        // costs a decode of the whole picture.
        Some(Format::Webp) => {
            formats::webp::image_info(path, apply_rotation, route, input.reader()?)?
        }
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

    /// An open of a fixture, for the few tests that hand a reader to a module
    /// directly rather than going through a probe.
    fn opened(path: &Path) -> std::io::BufReader<std::fs::File> {
        std::io::BufReader::new(std::fs::File::open(path).expect("the fixture opens"))
    }

    /// A probe reads a file's head once, and the timeline decision reads the
    /// route it saved rather than reading the head again.
    #[test]
    fn a_probe_reads_the_head_once_and_saves_the_route() {
        use crate::formats::identify::Format;

        let path = fixture("alpha-rgb8.png");
        super::reset_head_reads();
        let segment =
            probe_segment(&path, Rate::from_fps(24, 1), true, false).expect("the fixture probes");
        // The route is the only read: the module the router chose takes the saved
        // answer instead of opening the file to ask the ownership question again.
        assert_eq!(super::head_reads(), 1, "one probe reads the head once");
        assert_eq!(segment.info.route, Some(Format::Png));
    }

    /// The routing head read is one for an animated file too: the timeline
    /// decision reuses the route the probe saved, exactly as a still's does.
    ///
    /// What this does **not** count is the front a format reads for itself to
    /// find a timeline -- `apng`'s chunk walk, `webp`'s RIFF window -- because
    /// those do not go through `identify::head_of`. Those are what a shared
    /// probe reader would unify, so they need a measure of their own before
    /// that slice starts, and this test records the half that is already one.
    #[test]
    fn an_animation_probe_reads_the_routing_head_once() {
        for name in ["animation.png", "animation.gif", "animation.webp"] {
            let path = fixture(name);
            super::reset_head_reads();
            let segment = probe_segment(&path, Rate::from_fps(24, 1), true, false)
                .expect("the fixture probes");
            assert!(segment.animated, "{name}");
            assert_eq!(super::head_reads(), 1, "{name}: the routing head read");
        }
    }

    /// A probe asks the router and the module the same sixteen bytes of one file, and
    /// it asks them once: the routing read and the module's window are the same read.
    ///
    /// The six readers whose whole header is inside the window -- a bitmap's palette,
    /// a surface's magic, a farbfeld's sixteen bytes, a radiance preamble, a qoi
    /// header and a targa's colour map -- take it from the one input rather than
    /// opening the file again, which is what this asserts. A png and an exr still open
    /// for themselves; the counter does not see those opens either way, and the plan
    /// lists which are left.
    #[test]
    fn a_probe_reads_the_head_once_for_the_router_and_the_module() {
        for name in [
            "bmp-depth24.bmp",
            "alpha-dds.dds",
            "alpha-rgba16.ff",
            "hdr-flat.hdr",
            "qoi-rgb8.qoi",
            "tga-rgb24.tga",
        ] {
            let path = fixture(name);
            super::reset_head_reads();
            crate::formats::identify::reset_head_reads();
            let info = probe(&path, true, false).unwrap_or_else(|error| panic!("{name}: {error}"));
            assert!(info.width > 0, "{name} is described");
            assert_eq!(
                super::head_reads(),
                1,
                "{name}: one read of the head answers both questions"
            );
            assert_eq!(
                crate::formats::identify::head_reads(),
                0,
                "{name}: the router reads the same bytes rather than opening the file"
            );
        }
    }

    /// A probe of an animated file opens the file once, and its timeline is read
    /// through that same open.
    ///
    /// This is the number the four adapters that went looking for a timeline used
    /// to spend one each -- `apng`'s chunk walk, `gif`'s scan, `webp`'s RIFF
    /// window and the avif/heif sequence walk each opened the file for itself, on
    /// top of the open the probe had already made for the route and the describing
    /// module. They read the probe's input now, so what a probe leaves here is the
    /// one open it cannot do without, whatever the container is.
    ///
    /// A jpeg xl is on the same line as the other four for the first time. Its
    /// adapter always answered from the codestream header its own probe read, and
    /// this count no longer tells the two cases apart -- it counts opens the probe
    /// made, not reads the adapters made. What each adapter spends is on top of
    /// this, and the plan lists the modules that still open for themselves.
    ///
    /// The count was measured by running this loop rather than predicted.
    #[test]
    fn an_animation_probe_opens_the_file_once_for_its_timeline() {
        for name in [
            "animation.png",
            "animation-rgba16.png",
            "animation.gif",
            "animation.webp",
            "animation.avif",
            "animation.heic",
            "animation.jxl",
        ] {
            let path = fixture(name);
            super::reset_input_opens();
            let segment = probe_segment(&path, Rate::from_fps(24, 1), true, false)
                .expect("the fixture probes");
            assert!(segment.animated, "{name}: the fixture is an animation");
            assert!(
                !segment.presentations.is_empty(),
                "{name}: has presentations"
            );
            assert_eq!(
                super::input_opens(),
                1,
                "{name}: the timeline is read from the open the probe made"
            );
        }
    }

    /// A png probe answers its header and its `cICP` out of the one open the
    /// router made, and the second read starts at the front rather than where the
    /// first left off.
    ///
    /// The png reader stops after the header chunks, which is well past the
    /// signature a `cICP` walk starts from, so a walk that did not rewind would
    /// find nothing and hand back a file that states no colour at all. The open
    /// count is the other half of the claim: one, where this probe spent three --
    /// the input, the png reader and the walk.
    #[test]
    fn a_png_probe_reads_its_cicp_from_the_same_open_as_its_header() {
        let path = fixture("cicp-rgb8.png");
        super::reset_input_opens();
        let info = probe(&path, true, false).expect("the fixture probes");
        let cicp = info.cicp.expect("a stated colour");
        assert_eq!((cicp.primaries, cicp.transfer, cicp.matrix), (9, 18, 0));
        assert!(cicp.full_range);
        assert_eq!(
            super::input_opens(),
            1,
            "the header and the walk share the open the router made"
        );
    }

    /// A gif, an icon and a jpeg read their containers out of the open the router
    /// made, and each reads it from the front.
    ///
    /// `Input::reader` rewinds before it hands the handle out, so a probe never
    /// meets a moved reader -- which is exactly why nothing else in the suite
    /// would notice if one of these lost its rewind. Each is therefore asked
    /// directly with a reader left in the middle of the file, which is the state
    /// a change to `reader` would hand it.
    #[test]
    fn a_probe_that_reads_from_the_front_ignores_where_the_reader_was_left() {
        use std::io::{BufReader, Seek, SeekFrom};

        let moved = |path: &Path| {
            let file = std::fs::File::open(path).expect("the fixture opens");
            let mut reader = BufReader::new(file);
            reader
                .seek(SeekFrom::Start(4096))
                .expect("the reader moves");
            reader
        };

        let gif = fixture("gif-still.gif");
        let screen = crate::animation::gif::screen(&mut moved(&gif), &gif)
            .expect("the logical screen is read from the front");
        // Measured, not guessed: the fixture is a 4x4 logical screen.
        assert_eq!((screen.0, screen.1), (4, 4));

        let ico = fixture("ico-png.ico");
        let info = crate::formats::ico::image_info(&ico, true, None, &mut moved(&ico))
            .expect("the icon is described")
            .expect("the icon is taken over");
        assert!(
            info.width > 0 && info.height > 0,
            "the size comes from the payload"
        );
    }

    /// A tiff and a jpeg xl read their headers out of the open the router made,
    /// and each reads from the front of it.
    ///
    /// A tiff is the sharper of the two: its directory can sit anywhere in the
    /// file, so a signature read from the middle of one is a signature of
    /// whatever happens to be at that offset rather than the format's own.
    #[test]
    fn a_tiff_or_jpeg_xl_probe_reads_its_header_from_the_front() {
        use std::io::{Seek, SeekFrom};

        let moved = |path: &Path| {
            let mut reader = opened(path);
            reader
                .seek(SeekFrom::Start(4096))
                .expect("the reader moves");
            reader
        };

        let tiff = fixture("tiff-rgb8.tiff");
        let info = crate::formats::tiff::image_info(&tiff, true, None, &mut moved(&tiff))
            .expect("the tiff is described")
            .expect("the tiff is taken over");
        assert!(
            info.width > 0 && info.height > 0,
            "the tiff size comes from its directory"
        );

        let jxl = fixture("alpha-rgba8.jxl");
        let info = crate::formats::jxl::image_info(&jxl, true, &mut moved(&jxl))
            .expect("the codestream is described");
        assert!(
            info.width > 0 && info.height > 0,
            "the codestream size is read from its front"
        );
    }

    /// An avif, a heif and an openexr read what they can from the front of the
    /// open they are handed.
    ///
    /// These are the three whose readers take a path rather than a handle, so
    /// what is pinned here is the part each of them *could* give up: the avif's
    /// own box walk, the heif's `irot`/`imir` walk, and the openexr's magic.
    #[test]
    fn a_container_or_magic_read_from_the_front_ignores_where_the_reader_was() {
        use std::io::{Seek, SeekFrom};

        let moved = |path: &Path| {
            let mut reader = opened(path);
            reader
                .seek(SeekFrom::Start(4096))
                .expect("the reader moves");
            reader
        };

        let avif = fixture("avif-yuv420p.avif");
        let info = crate::formats::avif::image_info(&avif, true, &mut moved(&avif))
            .expect("the container describes it");
        assert!(
            info.width > 0 && info.height > 0,
            "the avif size comes from its boxes"
        );

        let heic = fixture("alpha-rgba8.heic");
        let info = crate::formats::heif::describe(&heic, true, &mut moved(&heic))
            .expect("libheif describes it");
        assert!(
            info.width > 0 && info.height > 0,
            "the heic size comes from libheif"
        );

        let exr = fixture("exr-none.exr");
        // `Some`, not merely `Ok`: an openexr whose magic was read from the
        // middle of the file is not one this reader declines, it is one this
        // reader has never seen, and only the second shows up as a missing file
        // in a clip.
        let info = crate::formats::exr::image_info(&exr, true, None, &mut moved(&exr))
            .expect("the probe reads the file")
            .expect("the magic is read from the front, so the probe still knows the format");
        assert!(
            info.width > 0 && info.height > 0,
            "the openexr size comes from its header"
        );
    }

    /// An avif, a heif and an openexr are described from the one open the router
    /// made.
    ///
    /// The open count is `input_opens`, which counts opens through `Input`. The
    /// two that remain are the ones that cannot be shared and are *not* counted
    /// here: `libheif` and the `exr` crate each open the path themselves, which
    /// is why this slice was worth less for a heif and an openexr than it was for
    /// an avif.
    #[test]
    fn an_avif_heif_or_exr_probe_starts_from_the_routers_open() {
        for name in [
            "avif-yuv420p.avif",
            "alpha-rgba8.heic",
            "animation.avif",
            "exr-none.exr",
        ] {
            let path = fixture(name);
            super::reset_input_opens();
            let info = probe(&path, true, false).expect("the fixture probes");
            assert!(info.width > 0 && info.height > 0, "{name} is described");
            assert_eq!(
                super::input_opens(),
                1,
                "{name}: the container starts from the open the router made"
            );
        }
    }

    /// A tiff and a jpeg xl are described from the one open the router made, and
    /// a jpeg xl's animation answer is read from the same one rather than a
    /// second.
    #[test]
    fn a_tiff_or_jpeg_xl_probe_reads_from_the_routers_open() {
        for name in [
            "tiff-rgb8.tiff",
            "tiff-planar.tiff",
            "alpha-rgba8.jxl",
            "animation.jxl",
        ] {
            let path = fixture(name);
            super::reset_input_opens();
            let info = probe(&path, true, false).expect("the fixture probes");
            assert!(info.width > 0 && info.height > 0, "{name} is described");
            assert_eq!(
                super::input_opens(),
                1,
                "{name}: the header comes out of the open the router made"
            );
        }
    }

    /// A gif, an icon and a jpeg are described from the one open the router made.
    #[test]
    fn a_gif_icon_or_jpeg_probe_reads_from_the_routers_open() {
        for name in ["gif-still.gif", "ico-png.ico", "animation.gif"] {
            let path = fixture(name);
            super::reset_input_opens();
            let info = probe(&path, true, false).expect("the fixture probes");
            assert!(info.width > 0 && info.height > 0, "{name} is described");
            assert_eq!(
                super::input_opens(),
                1,
                "{name}: the container comes out of the open the router made"
            );
        }
    }

    /// A jpeg 2000 and a webp read their containers out of the same open, and a
    /// webp's walk reads from the front of it rather than from wherever the
    /// routing head left the reader.
    #[test]
    fn a_jpeg_2000_or_webp_probe_reads_its_container_from_the_routers_open() {
        for name in ["alpha-jp2-rgb8.jp2", "lossy.webp", "orientation-6.webp"] {
            let path = fixture(name);
            super::reset_input_opens();
            let info = probe(&path, true, false).expect("the fixture probes");
            assert!(info.width > 0 && info.height > 0, "{name} is described");
            assert_eq!(
                super::input_opens(),
                1,
                "{name}: the container comes out of the open the router made"
            );
        }
    }

    /// A netpbm's preamble is a text window that grows while the parse needs
    /// more, and it grows out of the open the router made rather than one of its
    /// own.
    #[test]
    fn a_netpbm_probe_reads_its_preamble_from_the_open_the_router_made() {
        let path = fixture("pnm-comment.pgm");
        super::reset_input_opens();
        let info = probe(&path, true, false).expect("the fixture probes");
        assert_eq!(
            (info.width, info.height),
            (37, 23),
            "the size is the one the preamble states, comments and all"
        );
        assert_eq!(
            super::input_opens(),
            1,
            "the window that grows is grown from the open the router made"
        );
    }

    /// The decode reads the route the probe saved instead of the head again.
    #[test]
    fn a_decode_reads_the_head_none() {
        let path = fixture("alpha-rgb8.png");
        let info = probe(&path, true, false).expect("the fixture probes");
        super::reset_head_reads();
        let decoded = super::decode(&info, super::Demand::COLOR).expect("the fixture decodes");
        assert!(decoded.width > 0);
        assert_eq!(super::head_reads(), 0, "the decode reuses the saved route");
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
        let animated = crate::formats::jxl::states_animation(
            &mut opened(&fixture("animation.jxl")),
            &fixture("animation.jxl"),
        )
        .expect("the fixture reads");
        assert!(animated, "the fixture holds four pictures");
        for name in ["alpha-rgba8.jxl", "jxl-gray10.jxl"] {
            let still =
                crate::formats::jxl::states_animation(&mut opened(&fixture(name)), &fixture(name))
                    .expect("the fixture reads");
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
