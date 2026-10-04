//! Farbfeld, read here rather than through a crate.
//!
//! Farbfeld is the smallest image format there is: the eight bytes `farbfeld`,
//! the width and the height as big-endian `u32`, then one big-endian `u16` per
//! channel per pixel, red first and alpha last. There is no compression, no
//! palette and no metadata, which is why the whole reader is [`stream`] and a
//! header reader, and why a crate for it was rejected: the one candidate's
//! `decode_into` is not usable, and the format is less code than the wrapper
//! around it would be.
//!
//! Two things are worth naming, because they are the only places this can go
//! wrong:
//!
//! - **The samples are big-endian on disk and native in memory.** A frame is
//!   written from a `u16` cast, so the bytes have to be swapped as they are
//!   read, and [`RowSink::place_rgba16`] swaps each one as it takes it apart
//!   rather than in a pass over the whole picture first.
//!   `tests/fixtures/alpha-rgba16.ff` holds `1000, 2000, 3000, 100` followed by
//!   `4000, 5000, 6000, 200`, which no byte order could read back as anything
//!   else by accident.
//! - **Every farbfeld has four channels.** The format has no three-channel
//!   spelling and no way to say "no alpha", so every file is `Rgba16` and every
//!   file has an alpha plane, whatever its samples contain. A call that hands out
//!   no alpha clip still reads the fourth channel of every pixel and drops it.
//!
//! The provenance is [27](../../../docs/improvements/27-direct-still-decoders.md)'s
//! row, which decided this one would be written here.

use std::{fs::File, io::Read, path::Path};

use crate::{
    decoder::{
        DecodeTimings, DecodedImage, ImageInfo, Pixels, RowSink, RowStream, image_error, image_head,
    },
    error::{ImgSeqError, Result},
    layout::{ColorType, Orientation, SourceColorType},
    pixel::{PixelFormat, Transform},
};

/// The eight bytes every farbfeld starts with.
const MAGIC: &[u8; 8] = b"farbfeld";

/// Bytes of the magic and the two dimensions.
const HEADER: usize = 16;

/// Channels per pixel, which the format fixes at four.
const CHANNELS: usize = 4;

/// Whether this module reads `path`.
#[must_use]
pub fn owns(path: &Path) -> bool {
    // Content first: a file whose bytes say it is something else is that
    // something else however it is named, and the extension is the hint a
    // format with no signature of its own has to fall back on. See
    // [`identify::owns`](crate::formats::identify::owns).
    crate::formats::identify::owns(crate::formats::identify::Format::Farbfeld, path)
}

/// The size a farbfeld states, when it states one.
///
/// `None` means "this is not a farbfeld", which is a different answer from
/// "this is a broken one": a file that does not start with the magic is left to
/// whatever else can read it, and one that does is this module's to refuse.
fn dimensions(data: &[u8]) -> Option<(u32, u32)> {
    if data.len() < HEADER || &data[..8] != MAGIC {
        return None;
    }
    let width = u32::from_be_bytes(data[8..12].try_into().ok()?);
    let height = u32::from_be_bytes(data[12..16].try_into().ok()?);
    Some((width, height))
}

/// How many bytes the pixels of a `width` by `height` farbfeld occupy.
fn pixel_bytes(width: u32, height: u32) -> Result<usize> {
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(CHANNELS * 2))
        .ok_or_else(|| ImgSeqError::new("the farbfeld is too large to read"))
}

/// What a farbfeld states, when this module reads the file.
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
        |saved| saved == crate::formats::identify::Format::Farbfeld,
    ) {
        return Ok(None);
    }
    let data = image_head(path).map_err(|error| image_error("open", path, error))?;
    let Some((width, height)) = dimensions(&data) else {
        return Ok(None);
    };
    if width == 0 || height == 0 {
        return Err(image_error("identify", path, "the header states no pixels"));
    }
    Ok(Some(ImageInfo {
        route: None,
        subimage: None,
        path: path.to_path_buf(),
        width,
        height,
        // Every farbfeld is four channels of sixteen bits. The format has no
        // other spelling, so the decoded layout and the label are the same.
        color_type: ColorType::Rgba16,
        original_color_type: SourceColorType::Rgba16,
        // A farbfeld states no profile, no colour description and no
        // orientation: its header is a magic and a size and nothing else.
        has_icc_profile: false,
        icc_profile: None,
        cicp: None,
        chroma_location: None,
        orientation: Orientation::NoTransforms,
        transform: Transform::IDENTITY,
        format: PixelFormat::Rgb16,
    }))
}

/// What a farbfeld's header states.
#[derive(Clone, Copy, Debug)]
struct Header {
    width: u32,
    height: u32,
}

/// Opens `path`, checks it against its own header and leaves the reader on the
/// first byte of the raster.
///
/// Every farbfeld is eligible: the format has one spelling, one depth and no
/// compression, so there is nothing a buffered fallback would still be needed
/// for. The header is read again here rather than trusted, because a stream that
/// opens the file at fill time has to answer the same questions a probe did.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file cannot be read, is not a farbfeld, or
/// is too short to hold the pixels its header states.
fn prepare(path: &Path) -> Result<(Header, File)> {
    let mut file = File::open(path).map_err(|error| image_error("open", path, error))?;
    let mut head = [0u8; HEADER];
    file.read_exact(&mut head)
        .map_err(|error| image_error("open", path, error))?;
    let (width, height) =
        dimensions(&head).ok_or_else(|| image_error("decode", path, "not a farbfeld"))?;
    if width == 0 || height == 0 {
        return Err(image_error("decode", path, "the header states no pixels"));
    }
    // The header is a claim about a length, and the claim is what has to hold
    // before a row is read into a frame.
    let expected = pixel_bytes(width, height)?;
    let held = file
        .metadata()
        .map_err(|error| image_error("open", path, error))?
        .len();
    if held < HEADER as u64 + expected as u64 {
        return Err(image_error(
            "decode",
            path,
            format!(
                "the file holds {} bytes of pixels where the header states {expected}",
                held.saturating_sub(HEADER as u64)
            ),
        ));
    }
    Ok((Header { width, height }, file))
}

/// A raster this reader walks a row at a time.
///
/// A row of a farbfeld is a row of the picture once its samples are swapped, so
/// each one is read into a buffer one row wide, taken apart by
/// [`RowSink::place_rgba16`] and written straight into the frame. Nothing holds the
/// whole raster, where [`decode`] holds it twice: once as the bytes on disk and
/// once as the native words the frame is written from.
#[derive(Debug)]
pub struct Rows {
    path: std::path::PathBuf,
    /// What the header states, kept so a read does not parse it again.
    header: Option<Header>,
    /// The reader positioned at the first byte of the raster. `None` is a stream
    /// that came from [`RowStream::duplicate`], which prepares itself on its way into
    /// a fill: duplicating cannot report the failure that opening the file can.
    raster: Option<File>,
}

impl Rows {
    /// The header and the reader positioned at the raster, prepared here when this
    /// stream is a duplicate that has not read anything yet.
    fn raster(&mut self) -> Result<(Header, &mut File)> {
        if self.raster.is_none() {
            let (header, file) = prepare(&self.path)?;
            self.header = Some(header);
            self.raster = Some(file);
        }
        let header = self
            .header
            .ok_or_else(|| ImgSeqError::new("the raster header is missing"))?;
        let file = self
            .raster
            .as_mut()
            .ok_or_else(|| ImgSeqError::new("the raster reader is missing"))?;
        Ok((header, file))
    }
}

impl RowStream for Rows {
    fn has_alpha(&self) -> bool {
        // Every farbfeld is four channels and the fourth is the alpha clip, so a
        // call that asks for no alpha clip still reads the channel and drops it.
        true
    }

    fn fill(&mut self, mut sink: RowSink<'_>) -> Result<DecodeTimings> {
        let read_started = std::time::Instant::now();
        let (header, file) = self.raster()?;
        let width = header.width as usize;
        let height = header.height as usize;
        let row_bytes = width
            .checked_mul(CHANNELS * 2)
            .ok_or_else(|| ImgSeqError::new("a farbfeld row does not fit in memory"))?;
        let mut row = vec![0u8; row_bytes];
        for line in 0..height {
            file.read_exact(&mut row)
                .map_err(|_| ImgSeqError::new(format!("the raster ends in row {line}")))?;
            sink.place_rgba16(&row, line)
                .ok_or_else(|| ImgSeqError::new("the frame holds no three colour planes"))?;
        }
        // A stream is read once. Letting the reader go here means a second fill
        // starts from the raster again rather than from where this one stopped.
        self.raster = None;
        Ok(DecodeTimings {
            open: std::time::Duration::ZERO,
            metadata: std::time::Duration::ZERO,
            buffer: std::time::Duration::ZERO,
            read: read_started.elapsed(),
        })
    }

    fn duplicate(&self) -> Box<dyn RowStream> {
        Box::new(Self {
            path: self.path.clone(),
            header: None,
            raster: None,
        })
    }
}

/// A farbfeld that hands its rows to the frame as it reads them.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file cannot be opened or read.
pub fn stream(info: &ImageInfo) -> Result<DecodedImage> {
    let (header, file) = prepare(&info.path)?;
    if (header.width, header.height) != (info.width, info.height) {
        return Err(ImgSeqError::new(format!(
            "image '{}' changed after probing (was {}x{}, now {}x{})",
            info.path.display(),
            info.width,
            info.height,
            header.width,
            header.height,
        )));
    }
    Ok(DecodedImage {
        width: info.width,
        height: info.height,
        format: info.format,
        transform: info.transform,
        pixels: Pixels::Stream(Box::new(Rows {
            path: info.path.clone(),
            header: Some(header),
            raster: Some(file),
        })),
        timings: DecodeTimings {
            open: std::time::Duration::ZERO,
            metadata: std::time::Duration::ZERO,
            buffer: std::time::Duration::ZERO,
            read: std::time::Duration::ZERO,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::decoder::PlaneRows;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    /// Writes bytes to a temp file named for this test process.
    fn write_temp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("imgseqs-farbfeld-{}-{name}", std::process::id()));
        std::fs::write(&path, bytes).expect("a writable image");
        path
    }

    /// The frame a stream writes into: three colour planes and, for a call that
    /// asks for one, an alpha plane.
    ///
    /// The planes are a row's samples with no padding between them, which is
    /// the tightest row a VapourSynth frame can hand out; a frame with padding is
    /// what the validator reads.
    struct Frame {
        colour: Vec<Vec<u8>>,
        alpha: Option<Vec<u8>>,
        row_bytes: usize,
    }

    impl Frame {
        fn new(width: usize, height: usize, with_alpha: bool) -> Self {
            let row_bytes = width * 2;
            let plane = || vec![0u8; row_bytes * height];
            Self {
                colour: vec![plane(), plane(), plane()],
                alpha: with_alpha.then(plane),
                row_bytes,
            }
        }

        /// This frame's planes, borrowed for one fill.
        fn sink(&mut self) -> RowSink<'_> {
            let row_bytes = self.row_bytes;
            let colour = self
                .colour
                .iter_mut()
                .map(|plane| PlaneRows {
                    bytes: plane.as_mut_slice(),
                    stride: row_bytes,
                    row_bytes,
                })
                .collect();
            let alpha = self.alpha.as_mut().map(|plane| {
                vec![PlaneRows {
                    bytes: plane.as_mut_slice(),
                    stride: row_bytes,
                    row_bytes,
                }]
            });
            RowSink { colour, alpha }
        }
    }

    /// The samples of one plane, in the byte order a frame holds them.
    fn samples(plane: &[u8]) -> Vec<u16> {
        plane
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_ne_bytes(*pair))
            .collect()
    }

    /// The committed fixture, streamed into a frame of the given shape.
    fn fill(width: usize, height: usize, with_alpha: bool) -> Frame {
        let info = image_info(&fixture("alpha-rgba16.ff"), true, None)
            .expect("the header is read")
            .expect("a farbfeld is taken over");
        assert_eq!((info.width as usize, info.height as usize), (width, height));
        let decoded = stream(&info).expect("the file streams");
        let Pixels::Stream(mut rows) = decoded.pixels else {
            panic!("a farbfeld hands its rows to the frame");
        };
        assert!(rows.has_alpha(), "the fourth channel is the alpha clip");
        let mut frame = Frame::new(width, height, with_alpha);
        rows.fill(frame.sink()).expect("the rows are written");
        frame
    }

    /// The committed fixture: two by two, four channels of sixteen bits.
    #[test]
    fn the_header_is_read_as_the_file_states_it() {
        let info = image_info(&fixture("alpha-rgba16.ff"), true, None)
            .expect("the header is read")
            .expect("a farbfeld is taken over");
        assert_eq!((info.width, info.height), (2, 2));
        assert_eq!(info.format, PixelFormat::Rgb16);
        assert_eq!(info.color_type, ColorType::Rgba16);
        assert_eq!(info.original_color_type, SourceColorType::Rgba16);
        assert_eq!(info.transform, Transform::IDENTITY);
        assert!(!info.has_icc_profile);
    }

    /// Every channel of every pixel, where the stream puts it: `R, G, B` in the
    /// colour planes and `A` in the alpha plane, each as a native-endian `u16`.
    /// Wrong byte order would read these back as 6144 or 13312 rather than 1000.
    #[test]
    fn the_samples_are_big_endian_on_disk_and_native_in_memory() {
        let frame = fill(2, 2, true);
        assert_eq!(
            samples(&frame.colour[0]),
            vec![1000, 4000, 7000, 10000],
            "red is the red channel of every pixel"
        );
        assert_eq!(
            samples(&frame.colour[1]),
            vec![2000, 5000, 8000, 11000],
            "green is the green channel of every pixel"
        );
        assert_eq!(
            samples(&frame.colour[2]),
            vec![3000, 6000, 9000, 12000],
            "blue is the blue channel of every pixel"
        );
        let alpha = frame.alpha.as_ref().expect("the alpha plane");
        assert_eq!(
            samples(alpha),
            vec![100, 200, 300, 400],
            "the fourth channel is the alpha clip"
        );
    }

    /// A call that hands out no alpha clip still reads the fourth channel of every
    /// pixel and drops it: the format has no three channel spelling, so the colour
    /// planes are the first three channels either way.
    #[test]
    fn a_call_with_no_alpha_clip_still_reads_the_picture() {
        let frame = fill(2, 2, false);
        assert!(frame.alpha.is_none(), "no alpha clip was asked for");
        assert_eq!(samples(&frame.colour[0]), vec![1000, 4000, 7000, 10000]);
        assert_eq!(samples(&frame.colour[1]), vec![2000, 5000, 8000, 11000]);
        assert_eq!(samples(&frame.colour[2]), vec![3000, 6000, 9000, 12000]);
    }

    /// A file that stops before the pixels it states is refused rather than
    /// handed out short. The check is the file's own length against its header,
    /// which is the only thing standing between a claim and a read past the end.
    #[test]
    fn a_truncated_file_is_refused() {
        let data = std::fs::read(fixture("alpha-rgba16.ff")).expect("the fixture is read");
        // The whole file reads, and every shorter prefix is refused: a header that
        // states four pixels is a claim about forty eight bytes.
        assert_eq!(data.len(), 48);
        prepare(&fixture("alpha-rgba16.ff")).expect("the whole file prepares");
        for missing in 1..=32 {
            let path = write_temp(
                &format!("short-{missing}.ff"),
                &data[..data.len() - missing],
            );
            let error = prepare(&path).expect_err("a short file is refused, not handed out short");
            assert!(
                error.to_string().contains("where the header states"),
                "{error}"
            );
            let _ = std::fs::remove_file(&path);
        }
    }

    /// A header that states no pixels, and one that is not a farbfeld at all.
    #[test]
    fn a_broken_header_is_an_error_and_a_foreign_file_is_declined() {
        // The magic, then a zero height.
        let mut data = MAGIC.to_vec();
        data.extend_from_slice(&2u32.to_be_bytes());
        data.extend_from_slice(&0u32.to_be_bytes());
        assert_eq!(dimensions(&data), Some((2, 0)));

        // Not the magic: declined rather than refused, so the probe can describe
        // it as whatever it really is.
        let mut foreign = b"notfarbf".to_vec();
        foreign.extend_from_slice(&2u32.to_be_bytes());
        foreign.extend_from_slice(&2u32.to_be_bytes());
        assert_eq!(dimensions(&foreign), None);

        // Too short even for a header.
        assert_eq!(dimensions(b"farbfeld"), None);
    }

    /// Only a `.ff` is taken over.
    #[test]
    fn only_a_farbfeld_is_taken_over() {
        assert!(owns(Path::new("a.ff")));
        assert!(owns(Path::new("a.FF")));
        assert!(!owns(Path::new("a.png")));
        assert!(!owns(Path::new("a.ffv")));
    }

    /// A file whose size changed after probing is refused.
    #[test]
    fn a_file_that_changed_after_probing_is_refused() {
        let mut info = image_info(&fixture("alpha-rgba16.ff"), true, None)
            .expect("the header is read")
            .expect("taken over");
        info.height += 1;
        let error = stream(&info).expect_err("the sizes disagree");
        assert!(
            error.to_string().contains("changed after probing"),
            "{error}"
        );
    }
}
