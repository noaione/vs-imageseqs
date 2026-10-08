//! The r,g,b a PNG stores, and what a frame has to become to reach it.
//!
//! A PNG holds integer gray, r,g,b or an index into a palette: it has no yuv
//! sample and no float one, so a frame that is either has to be converted
//! before it can be written. This module is that conversion. `PNGWrite` runs it
//! rather than asking the caller to put a `resize` in front, because the
//! readers hand a lossy webp, a colour avif and a colour heic out as the yuv
//! planes the container coded -- see
//! `docs/improvements/03-webp-yuv-output.md` -- so writing one of those to a
//! PNG is the ordinary case rather than a detour.
//!
//! Every choice a conversion has to make is stated rather than guessed:
//!
//! - the matrix is the frame's own `_Matrix`, which the readers write for every
//!   yuv frame they hand out, or the `matrix` argument a caller gives for one
//!   that states none. Nothing is inferred from the picture's size or from the
//!   format's name.
//! - the coefficients are the h.273 ones for that code. Only the
//!   non-constant-luminance matrices are converted: the constant luminance
//!   ones, the SMPTE 2085 one and the chromaticity-derived ones that would need
//!   a primaries table are refused by name.
//! - the range is the frame's own `_Range`, and a frame that states none is
//!   read as limited, which is what `resize` does with the same clip.
//! - chroma is upsampled with a triangle filter from the sample position the
//!   frame's `_ChromaLocation` names, and from the left-sited position `resize`
//!   defaults to when it names none.
//! - a sample is quantised once, at the end, to the precision the PNG stores,
//!   so an eight bit frame becomes an eight bit PNG and a ten bit one a sixteen
//!   bit PNG whose `sBIT` chunk says how many bits are meaningful, exactly as
//!   the gray and rgb paths already do.
//!
//! None of that is colour management: primaries and transfer are carried
//! through to the `cICP` chunk as the container stated them and no transfer
//! function is applied, which is the decision
//! `docs/improvements/38-png-write.md` records.

use vapoursynth4_rs::ffi;

use crate::error::{ImgSeqError, Result};
use std::ops::Range;

mod simd;

/// The `(kr, kb)` pair an h.273 matrix coefficient code names.
///
/// The three weights are not independent -- green is what is left of one -- so
/// a matrix is two numbers and the conversion is one expression rather than a
/// formula a matrix, and the pair is the whole of what a code means here.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coefficients {
    /// Weight of red in the luma the frame holds.
    pub kr: f32,
    /// Weight of blue in it.
    pub kb: f32,
}

impl Coefficients {
    /// The coefficients of `matrix`, `None` for the identity matrix, and an
    /// error naming the code for a matrix this module does not convert.
    ///
    /// The identity matrix is not a pair of coefficients: it says the three
    /// planes are already r, g and b, which is a different reading of the same
    /// samples rather than a conversion of them.
    pub fn of(matrix: ffi::VSMatrixCoefficients) -> Result<Option<Self>> {
        use ffi::VSMatrixCoefficients as M;
        Ok(match matrix {
            M::VSC_MATRIX_RGB => None,
            M::VSC_MATRIX_BT709 => Some(Self {
                kr: 0.2126,
                kb: 0.0722,
            }),
            M::VSC_MATRIX_FCC => Some(Self { kr: 0.30, kb: 0.11 }),
            // Smpte 170m is the code bt.470bg's own coefficients are also
            // written as, which is why the two share an arm.
            M::VSC_MATRIX_BT470_BG | M::VSC_MATRIX_ST170_M => Some(Self {
                kr: 0.299,
                kb: 0.114,
            }),
            M::VSC_MATRIX_ST240_M => Some(Self {
                kr: 0.212,
                kb: 0.087,
            }),
            // YCgCo weights both ends equally, which is what makes its two
            // chroma planes the difference of green and of red from the luma.
            M::VSC_MATRIX_YCGCO => Some(Self { kr: 0.25, kb: 0.25 }),
            M::VSC_MATRIX_BT2020_NCL => Some(Self {
                kr: 0.2627,
                kb: 0.0593,
            }),
            other => return Err(unsupported_matrix(other)),
        })
    }

    /// Weight of green, which is the remainder of the three.
    fn kg(self) -> f32 {
        1.0 - self.kr - self.kb
    }
}

/// The matrix an h.273 code names.
///
/// A frame states its matrix as the code point rather than as the enum, so this
/// is the lookup a property and a caller's argument both go through. A code
/// VapourSynth has no name for is refused here rather than turned into a
/// default, because a property whose value means nothing is worse than a
/// property that is not there.
pub fn matrix_of(code: i64) -> Result<ffi::VSMatrixCoefficients> {
    use ffi::VSMatrixCoefficients as M;
    Ok(match code {
        0 => M::VSC_MATRIX_RGB,
        1 => M::VSC_MATRIX_BT709,
        2 => M::VSC_MATRIX_UNSPECIFIED,
        4 => M::VSC_MATRIX_FCC,
        5 => M::VSC_MATRIX_BT470_BG,
        6 => M::VSC_MATRIX_ST170_M,
        7 => M::VSC_MATRIX_ST240_M,
        8 => M::VSC_MATRIX_YCGCO,
        9 => M::VSC_MATRIX_BT2020_NCL,
        10 => M::VSC_MATRIX_BT2020_CL,
        12 => M::VSC_MATRIX_CHROMATICITY_DERIVED_NCL,
        13 => M::VSC_MATRIX_CHROMATICITY_DERIVED_CL,
        14 => M::VSC_MATRIX_ICTCP,
        other => {
            return Err(ImgSeqError::new(format!(
                "the h.273 matrix code {other} is not one VapourSynth names"
            )));
        }
    })
}

/// The message a matrix this module does not convert is refused with.
///
/// The refusals are separated because they need different advice: a frame that
/// states no matrix is one a caller can fix by saying which one it is, while a
/// constant luminance frame cannot be written truthfully as an ordinary PNG at
/// all and only the caller's own upstream conversion can decide what to do with
/// it.
fn unsupported_matrix(matrix: ffi::VSMatrixCoefficients) -> ImgSeqError {
    use ffi::VSMatrixCoefficients as M;
    let code = matrix as u32;
    match matrix {
        M::VSC_MATRIX_UNSPECIFIED => ImgSeqError::new(
            "PNGWrite needs to know the matrix a yuv frame was coded with and this one states none; pass matrix= with the h.273 code the frame's own colour metadata names, or convert it upstream with core.resize.Bicubic(clip, format=vs.RGB24, matrix_in_s=\"601\")",
        ),
        M::VSC_MATRIX_BT2020_CL | M::VSC_MATRIX_CHROMATICITY_DERIVED_CL | M::VSC_MATRIX_ICTCP => {
            ImgSeqError::new(format!(
                "PNGWrite converts the non-constant-luminance matrices only and this frame states the constant luminance matrix {code}; convert it upstream with core.resize.Bicubic(clip, format=vs.RGB24)"
            ))
        }
        M::VSC_MATRIX_CHROMATICITY_DERIVED_NCL => ImgSeqError::new(format!(
            "PNGWrite cannot convert the chromaticity-derived matrix {code}, which is derived from the primaries rather than stored; convert it upstream with core.resize.Bicubic(clip, format=vs.RGB24)"
        )),
        _ => ImgSeqError::new(format!(
            "PNGWrite cannot convert the yuv matrix {code}; convert it upstream with core.resize.Bicubic(clip, format=vs.RGB24)"
        )),
    }
}

/// The position of a chroma sample within the luma samples it describes.
///
/// One cell of a subsampled plane is two luma samples an axis, and the six
/// positions VapourSynth names are its corners, its edges and its centre: left
/// and centre differ by half a luma sample horizontally, top and bottom by a
/// whole one vertically, and both of the first two sit between the two luma
/// rows. The position is only meaningful on an axis that is subsampled, which
/// is the caller's to apply.
fn phase(location: ffi::VSChromaLocation) -> (f32, f32) {
    use ffi::VSChromaLocation as C;
    match location {
        C::VSC_CHROMA_LEFT => (0.0, 0.5),
        C::VSC_CHROMA_CENTER => (0.5, 0.5),
        C::VSC_CHROMA_TOP_LEFT => (0.0, 0.0),
        C::VSC_CHROMA_TOP => (0.5, 0.0),
        C::VSC_CHROMA_BOTTOM_LEFT => (0.0, 1.0),
        C::VSC_CHROMA_BOTTOM => (0.5, 1.0),
    }
}

/// The two samples one axis interpolates between, and the weight of the second.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Taps {
    first: usize,
    second: usize,
    weight: f32,
}

/// The taps one output position reads.
///
/// The chroma grid puts its sample `i` at `sub * i + phase` in the axis's own
/// units, so a position asks for `(at - phase) / sub` in grid coordinates and
/// the two samples either side of that bracket it. A position before the first
/// sample or past the last one clamps, which is what makes an edge need no
/// special case: both taps then name the same sample and the interpolation is
/// exact there.
fn taps(at: usize, sub: usize, phase: f32, count: usize) -> Taps {
    let position = (at as f32 - phase) / sub as f32;
    let floor = position.floor();
    let weight = position - floor;
    let first = clamp_index(floor as i64, count);
    Taps {
        first,
        // A position that lands on a sample reads only that one. The two
        // ends of an interpolation a zero weight multiplies are the same
        // sample, and saying so is what keeps the second tap inside the plane
        // at the far edge as well as saving the read.
        second: if weight == 0.0 {
            first
        } else {
            clamp_index(floor as i64 + 1, count)
        },
        weight,
    }
}

/// One index into `count` samples, held inside them.
fn clamp_index(index: i64, count: usize) -> usize {
    usize::try_from(index.clamp(0, count as i64 - 1)).unwrap_or(0)
}

/// A straight line between two samples.
///
/// The weight multiplies both ends rather than being added to one of them, so a
/// weight of zero is exactly `first` and a weight of one is exactly `second`.
/// That matters at an edge, where the two taps are the same sample and the
/// result has to be that sample and not a level beside it.
fn lerp(first: f32, second: f32, weight: f32) -> f32 {
    first * (1.0 - weight) + second * weight
}

/// One source plane as a conversion reads it.
///
/// The stride is the plane's own, in bytes, so a frame's row padding is skipped
/// the way every other reader in this tree skips it, and `data` covers
/// `stride * height` bytes of that plane.
#[derive(Debug, Clone, Copy)]
pub struct Plane<'a> {
    pub data: &'a [u8],
    pub stride: usize,
}

impl<'a> Plane<'a> {
    /// One whole row of the plane, which is how a conversion reads it.
    ///
    /// Reading through a row rather than through the plane is what lets the
    /// compiler see that every sample a loop reads is inside it, and it applies
    /// the row's own stride once rather than once a sample.
    fn row(&self, y: usize, width: usize, bytes: usize) -> &'a [u8] {
        let at = y * self.stride;
        let data: &'a [u8] = self.data;
        &data[at..at + width * bytes]
    }
}

/// One sample of one whole row, as the number a conversion reads.
///
/// Sixteen bit samples are native endian, which is what VapourSynth stores and
/// what the PNG writer reorders rather than copies.
fn sample(row: &[u8], bytes: usize, x: usize) -> f32 {
    if bytes == 1 {
        f32::from(row[x])
    } else {
        f32::from(u16::from_ne_bytes([row[x * 2], row[x * 2 + 1]]))
    }
}

/// One float sample of a plane, with a NaN read as no picture at all.
///
/// A NaN is the one value that has no sample to become, so it is written as the
/// smallest one rather than as whatever a cast happens to make of it. An
/// infinity needs no case of its own: it is outside `[0, 1]` and is clamped
/// like any other value that is.
pub fn float_sample(data: &[u8], at: usize) -> f32 {
    let value = f32::from_ne_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
    if value.is_nan() { 0.0 } else { value }
}

/// The sample one value in `[0, 1]` becomes at `bits` bits.
///
/// The value is clamped, so a conversion that overshoots the scale -- which
/// every matrix does on some input -- comes out at the end of it rather than
/// wrapping, and the half added before the truncation rounds to nearest.
pub fn quantise(value: f32, bits: u32) -> u16 {
    let maximum = ((1u32 << bits) - 1) as f32;
    let scaled = value.clamp(0.0, 1.0) * maximum + 0.5;
    if scaled >= maximum {
        maximum as u16
    } else {
        scaled as u16
    }
}

/// One frame's yuv to r,g,b conversion, resolved once for every row.
///
/// The geometry is the frame's own, so the row this reads is the row the frame
/// holds; the horizontal taps are the same on every row, which is why they are
/// built once here rather than per row.
#[derive(Debug)]
pub struct Converter {
    /// The matrix, or `None` when the three planes are already r, g and b.
    coefficients: Option<Coefficients>,
    /// The code a luma sample counts its zero from, and the reciprocal of the
    /// span it uses: a sample costs one subtraction and one multiply, because
    /// three divides a pixel is more than this conversion can afford.
    luma_floor: f32,
    luma_scale: f32,
    /// The same pair for the two chroma planes.
    chroma_floor: f32,
    chroma_scale: f32,
    /// Bits the conversion quantises to, which is the precision the plan stores
    /// the samples at and may be below the source's.
    precision: u32,
    /// Bytes of one source sample.
    bytes: usize,
    /// The subsampling of each axis, in luma samples.
    sub_x: usize,
    sub_y: usize,
    /// Where a chroma sample sits inside its cell, in luma samples: the
    /// vertical one is what the row interpolation reads, and the horizontal
    /// one is what a vector kernel picks its pattern by.
    phase_x: f32,
    phase_y: f32,
    /// The size of the two chroma planes, which is what their rows are.
    chroma_width: usize,
    chroma_height: usize,
    /// The horizontal taps, which are the same on every row.
    columns: Vec<Taps>,
}

impl Converter {
    /// Resolves the conversion one frame's format and colour ask for.
    ///
    /// `sub_sampling` is VapourSynth's per-axis count of bits, so 4:2:0 arrives
    /// as `(1, 1)`; `location` is the frame's `_ChromaLocation`, which a
    /// subsampled frame usually does not state.
    /// `precision` is the depth the samples are quantised to, which is the
    /// frame's own unless the caller asked for a shallower PNG.
    // The shape and the colour of one frame are what a conversion *is*, and
    // bundling them would be a type for this one call site.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        bits: i32,
        precision: u32,
        sub_sampling: (i32, i32),
        width: usize,
        height: usize,
        matrix: ffi::VSMatrixCoefficients,
        full_range: bool,
        location: Option<ffi::VSChromaLocation>,
    ) -> Result<Self> {
        let bits = u32::try_from(bits).map_err(|_| {
            ImgSeqError::new("PNGWrite got a yuv frame with a negative sample depth")
        })?;
        if !(8..=16).contains(&bits) {
            return Err(ImgSeqError::new(format!(
                "PNGWrite converts 8 to 16 bit yuv frames, got {bits} bit samples"
            )));
        }
        if !(1..=16).contains(&precision) {
            return Err(ImgSeqError::new(format!(
                "PNGWrite quantises a conversion to 1 to 16 bits, got {precision}"
            )));
        }
        let (shift_x, shift_y) = sub_sampling;
        if !(0..=1).contains(&shift_x) || !(0..=1).contains(&shift_y) {
            return Err(ImgSeqError::new(format!(
                "PNGWrite converts 4:4:4, 4:2:2 and 4:2:0 yuv frames, and this one subsamples by {shift_x} and {shift_y} bits an axis"
            )));
        }
        let coefficients = Coefficients::of(matrix)?;
        // A position only means something on an axis that is subsampled: a
        // 4:4:4 plane has one chroma sample a luma sample, so it has no cell to
        // sit in and the phase is zero there whatever the property says.
        let (phase_x, phase_y) = location.map_or((0.0, 0.5), phase);
        let (sub_x, sub_y) = (1usize << shift_x, 1usize << shift_y);
        let (phase_x, phase_y) = (
            if shift_x == 0 { 0.0 } else { phase_x },
            if shift_y == 0 { 0.0 } else { phase_y },
        );
        // VapourSynth's own plane size, which truncates rather than rounding
        // up: a 4717 row frame of 4:2:0 has 2358 chroma rows, not 2359. The
        // odd luma row left over reads the edge chroma sample, which the tap
        // clamp is what makes exact.
        let chroma_width = width >> shift_x;
        let chroma_height = height >> shift_y;

        // A chroma plane with no sample in it is a frame the subsampling
        // cannot divide. VapourSynth does not hand one out, but a frame of a
        // variable clip is the only thing between the two, and a column has a
        // sample to clamp onto only where the plane holds one.
        if chroma_width == 0 || chroma_height == 0 {
            return Err(ImgSeqError::new(format!(
                "PNGWrite converts a yuv frame whose chroma planes hold a sample, and a {width}x{height} frame subsampled by {shift_x} and {shift_y} bits an axis has none"
            )));
        }
        let columns = (0..width)
            .map(|x| taps(x, sub_x, phase_x, chroma_width))
            .collect();
        // The studio ranges are the codes a sample counts from and the span it
        // uses, scaled by the depth; a full range sample counts from zero and
        // uses the whole word. Both are resolved to a floor and a reciprocal,
        // so the sample loop subtracts and multiplies rather than divides.
        let maximum = ((1u32 << bits) - 1) as f32;
        let step = 1u32 << (bits - 8);
        let (luma_floor, luma_scale, chroma_floor, chroma_scale) = if full_range {
            (
                0.0,
                1.0 / maximum,
                (1u32 << (bits - 1)) as f32,
                1.0 / maximum,
            )
        } else {
            (
                (16 * step) as f32,
                1.0 / (219 * step) as f32,
                (128 * step) as f32,
                1.0 / (224 * step) as f32,
            )
        };
        Ok(Self {
            coefficients,
            luma_floor,
            luma_scale,
            chroma_floor,
            chroma_scale,
            precision,
            bytes: if bits <= 8 { 1 } else { 2 },
            sub_x,
            sub_y,
            phase_x,
            phase_y,
            chroma_width,
            chroma_height,
            columns,
        })
    }

    /// The five rows one output row reads, which is the whole of the
    /// conversion's geometry for a row: the luma row the output row is, and
    /// the two chroma rows it sits between.
    fn rows<'a>(&self, row: usize, planes: [Plane<'a>; 3]) -> Rows<'a> {
        let [luma, cb, cr] = planes;
        let bytes = self.bytes;
        let chroma = taps(row, self.sub_y, self.phase_y, self.chroma_height);
        Rows {
            luma: luma.row(row, self.columns.len(), bytes),
            cb_above: cb.row(chroma.first, self.chroma_width, bytes),
            cb_below: cb.row(chroma.second, self.chroma_width, bytes),
            cr_above: cr.row(chroma.first, self.chroma_width, bytes),
            cr_below: cr.row(chroma.second, self.chroma_width, bytes),
            weight: chroma.weight,
        }
    }

    /// Fills one r,g,b row from the frame's three planes.
    ///
    /// `out` receives the row a plane at a time -- red, then green, then blue,
    /// each as wide as the frame -- at the frame's own precision, which the
    /// caller then stores. A vector kernel takes the columns it covers and the
    /// rest is read a sample at a time, so the two paths meet on a column
    /// rather than on a sample.
    pub fn rgb_row(&self, row: usize, planes: [Plane<'_>; 3], out: &mut [u16]) {
        let width = self.columns.len();
        debug_assert_eq!(out.len(), width * 3);
        let rows = self.rows(row, planes);
        let done = simd::rgb_row(self, &rows, out);
        self.scalar_columns(&rows, done, out);
    }

    /// The columns a vector kernel left, read one sample at a time.
    ///
    /// `done` is the range the kernel filled, which is a prefix of the row
    /// except where the first block of a centred plane would read before its
    /// own first sample.
    fn scalar_columns(&self, rows: &Rows<'_>, done: Range<usize>, out: &mut [u16]) {
        let bytes = self.bytes;
        let width = self.columns.len();
        debug_assert!(done.start <= done.end && done.end <= width);
        if done.is_empty() && self.sub_x == 1 && self.sub_y == 1 {
            // A 4:4:4 frame holds one chroma sample a luma sample, so there is
            // nothing between them to interpolate and the row is the matrix
            // applied sample by sample. This is the whole of the row on a
            // machine without a kernel for it, and a kernel that took the whole
            // row leaves this loop empty.
            for x in 0..width {
                let (r, g, b) = self.pixel(
                    sample(rows.luma, bytes, x),
                    sample(rows.cb_above, bytes, x),
                    sample(rows.cr_above, bytes, x),
                );
                out[x] = r;
                out[width + x] = g;
                out[2 * width + x] = b;
            }
            return;
        }
        for x in (0..done.start).chain(done.end..width) {
            let column = self.columns[x];
            let (r, g, b) = self.pixel(
                sample(rows.luma, bytes, x),
                interpolated(rows.cb_above, rows.cb_below, bytes, column, rows.weight),
                interpolated(rows.cr_above, rows.cr_below, bytes, column, rows.weight),
            );
            out[x] = r;
            out[width + x] = g;
            out[2 * width + x] = b;
        }
    }
}

/// The five rows one output row reads, and how far between two of them it
/// sits.
///
/// A 4:4:4 frame holds one chroma sample a luma sample, so both of its chroma
/// rows are the output row itself and the weight is zero, which is exact: the
/// scalar path's `lerp` reads the same sample twice there and a vector kernel
/// reads one row. Every other subsampling reads two rows and a weight in
/// `[0, 1)`.
#[derive(Debug, Clone, Copy)]
struct Rows<'a> {
    /// The row the output row is.
    luma: &'a [u8],
    cb_above: &'a [u8],
    cb_below: &'a [u8],
    cr_above: &'a [u8],
    cr_below: &'a [u8],
    /// How far the output row sits between the two, in luma rows.
    weight: f32,
}

/// One chroma sample, interpolated between two columns of two rows.
///
/// `weight` is how far the output row sits between them; the columns carry
/// their own weight.
fn interpolated(above: &[u8], below: &[u8], bytes: usize, column: Taps, weight: f32) -> f32 {
    lerp(
        lerp(
            sample(above, bytes, column.first),
            sample(above, bytes, column.second),
            column.weight,
        ),
        lerp(
            sample(below, bytes, column.first),
            sample(below, bytes, column.second),
            column.weight,
        ),
        weight,
    )
}

impl Converter {
    /// The three output samples one luma and two chroma samples become.
    ///
    /// The inputs are raw source samples rather than values, because that is
    /// what the interpolation produces too.
    fn pixel(&self, luma: f32, cb: f32, cr: f32) -> (u16, u16, u16) {
        let (r, g, b) = match self.coefficients {
            None => (
                self.scale_luma(luma),
                self.scale_luma(cb),
                self.scale_luma(cr),
            ),
            Some(c) => {
                let y = self.scale_luma(luma);
                let cb = self.scale_chroma(cb);
                let cr = self.scale_chroma(cr);
                let kg = c.kg();
                (
                    y + 2.0 * (1.0 - c.kr) * cr,
                    y - (2.0 * c.kb * (1.0 - c.kb) / kg) * cb
                        - (2.0 * c.kr * (1.0 - c.kr) / kg) * cr,
                    y + 2.0 * (1.0 - c.kb) * cb,
                )
            }
        };
        (
            quantise(r, self.precision),
            quantise(g, self.precision),
            quantise(b, self.precision),
        )
    }

    /// One luma sample as the value in `[0, 1]` a matrix multiplies.
    ///
    /// The identity matrix reads every plane this way, including the two it
    /// calls green and blue: a limited range identity is a studio swing r,g,b
    /// frame, where all three planes use the luma scaling.
    fn scale_luma(&self, sample: f32) -> f32 {
        (sample - self.luma_floor) * self.luma_scale
    }

    /// One chroma sample as the value in `[-0.5, 0.5]` a matrix multiplies.
    fn scale_chroma(&self, sample: f32) -> f32 {
        (sample - self.chroma_floor) * self.chroma_scale
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ffi::VSMatrixCoefficients as M;

    /// Every matrix that is a pair of weights rather than the identity.
    const MATRICES: [M; 7] = [
        M::VSC_MATRIX_BT709,
        M::VSC_MATRIX_FCC,
        M::VSC_MATRIX_BT470_BG,
        M::VSC_MATRIX_ST170_M,
        M::VSC_MATRIX_ST240_M,
        M::VSC_MATRIX_YCGCO,
        M::VSC_MATRIX_BT2020_NCL,
    ];

    /// The h.273 forward transform, in the normalised space a conversion works
    /// in, so a test can code a picture and ask for it back.
    ///
    /// It is written out here rather than shared with the conversion because a
    /// test that reused the module's own expression would only prove that the
    /// expression equals itself.
    fn forward(c: Coefficients, r: f32, g: f32, b: f32) -> (f32, f32, f32) {
        let y = c.kr * r + c.kg() * g + c.kb * b;
        (
            y,
            (b - y) / (2.0 * (1.0 - c.kb)),
            (r - y) / (2.0 * (1.0 - c.kr)),
        )
    }

    /// One normalised triple as the eight bit limited range codes a file holds
    /// it as.
    fn code(y: f32, cb: f32, cr: f32) -> (f32, f32, f32) {
        let quantise = |value: f32, floor: f32, span: f32| (value * span + floor + 0.5) as u16;
        (
            f32::from(quantise(y, 16.0, 219.0)),
            f32::from(quantise(cb, 128.0, 224.0)),
            f32::from(quantise(cr, 128.0, 224.0)),
        )
    }

    /// The sample a value in `[0, 1]` is expected to become at eight bits.
    fn level(value: f32) -> u16 {
        (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u16
    }

    fn convert(matrix: M, bits: i32, sub_sampling: (i32, i32), full_range: bool) -> Converter {
        let (w, h) = (4usize, 4usize);
        Converter::new(
            bits,
            u32::try_from(bits).unwrap(),
            sub_sampling,
            w,
            h,
            matrix,
            full_range,
            None,
        )
        .unwrap()
    }

    #[test]
    fn a_matrix_is_the_pair_of_weights_its_code_names() {
        // The two codes that share coefficients are the ones a reader is most
        // likely to write, so they are checked beside the rest.
        for (matrix, kr, kb) in [
            (M::VSC_MATRIX_BT709, 0.2126, 0.0722),
            (M::VSC_MATRIX_FCC, 0.30, 0.11),
            (M::VSC_MATRIX_BT470_BG, 0.299, 0.114),
            (M::VSC_MATRIX_ST170_M, 0.299, 0.114),
            (M::VSC_MATRIX_ST240_M, 0.212, 0.087),
            (M::VSC_MATRIX_YCGCO, 0.25, 0.25),
            (M::VSC_MATRIX_BT2020_NCL, 0.2627, 0.0593),
        ] {
            let coefficients = Coefficients::of(matrix).unwrap().unwrap();
            assert_eq!(coefficients, Coefficients { kr, kb }, "{matrix:?}");
        }
        // The identity matrix is not a pair at all, and the matrices that need
        // a primaries table or a constant luminance reading are refused rather
        // than approximated by one.
        assert_eq!(Coefficients::of(M::VSC_MATRIX_RGB).unwrap(), None);
        for refused in [
            M::VSC_MATRIX_UNSPECIFIED,
            M::VSC_MATRIX_BT2020_CL,
            M::VSC_MATRIX_CHROMATICITY_DERIVED_NCL,
            M::VSC_MATRIX_CHROMATICITY_DERIVED_CL,
            M::VSC_MATRIX_ICTCP,
        ] {
            let error = Coefficients::of(refused).unwrap_err().to_string();
            assert!(
                error.contains(&(refused as u32).to_string()) || error.contains("states none"),
                "{refused:?} was refused as '{error}'"
            );
        }
    }

    #[test]
    fn every_matrix_gives_back_the_picture_it_coded() {
        // The round trip is the whole of the arithmetic: a picture coded with
        // the forward transform has to come back out of the conversion, which
        // is what checks the coefficients, the studio offsets and the rounding
        // against each other rather than each on its own.
        let pictures = [
            (1.0, 0.0, 0.0),
            (0.0, 1.0, 0.0),
            (0.0, 0.0, 1.0),
            (1.0, 1.0, 1.0),
            (0.0, 0.0, 0.0),
            (0.5, 0.5, 0.5),
            (0.25, 0.6, 0.9),
            (0.9, 0.2, 0.4),
        ];
        for matrix in [
            M::VSC_MATRIX_BT709,
            M::VSC_MATRIX_FCC,
            M::VSC_MATRIX_BT470_BG,
            M::VSC_MATRIX_ST240_M,
            M::VSC_MATRIX_YCGCO,
            M::VSC_MATRIX_BT2020_NCL,
        ] {
            let coefficients = Coefficients::of(matrix).unwrap().unwrap();
            let converter = convert(matrix, 8, (0, 0), false);
            for (r, g, b) in pictures {
                let (y, cb, cr) = forward(coefficients, r, g, b);
                let (y, cb, cr) = code(y, cb, cr);
                let got = converter.pixel(y, cb, cr);
                for (got, want, channel) in [
                    (got.0, level(r), 'r'),
                    (got.1, level(g), 'g'),
                    (got.2, level(b), 'b'),
                ] {
                    assert!(
                        got.abs_diff(want) <= 1,
                        "{matrix:?} {channel} of {r},{g},{b}: got {got}, want {want}"
                    );
                }
            }
        }
    }

    #[test]
    fn black_and_white_stay_inside_the_scale() {
        // The studio range is the case where a clamping conversion would be
        // visible: the largest code has to reach full scale and not stop short
        // of it, whatever the matrix.
        for matrix in MATRICES {
            for bits in [8, 10, 16] {
                let converter = convert(matrix, bits, (0, 0), false);
                let scale = 1u32 << (bits - 8);
                let maximum = ((1u32 << bits) - 1) as u16;
                let full = f32::from(maximum);
                let white = converter.pixel(
                    (235 * scale) as f32,
                    (128 * scale) as f32,
                    (128 * scale) as f32,
                );
                assert_eq!(
                    white,
                    (maximum, maximum, maximum),
                    "{matrix:?} at {bits} bit"
                );
                let black = converter.pixel(
                    (16 * scale) as f32,
                    (128 * scale) as f32,
                    (128 * scale) as f32,
                );
                assert_eq!(black, (0, 0, 0), "{matrix:?} at {bits} bit");
                // The middle of the luma range is the middle of the scale, to
                // the level the studio range puts it at.
                let grey = converter.pixel(
                    (126 * scale) as f32,
                    (128 * scale) as f32,
                    (128 * scale) as f32,
                );
                let want = quantise((126.0 - 16.0) / 219.0, u32::try_from(bits).unwrap());
                assert_eq!(grey, (want, want, want), "{matrix:?} at {bits} bit");
                assert!(want > 0 && f32::from(want) < full, "the middle is inside");
            }
        }
    }

    #[test]
    fn an_identity_matrix_reads_the_planes_as_they_are() {
        // Full range identity is what a yuv frame carrying r,g,b looks like, so
        // every code has to come back as itself.
        let converter = convert(M::VSC_MATRIX_RGB, 8, (0, 0), true);
        assert_eq!(converter.pixel(0.0, 128.0, 255.0), (0, 128, 255));
        assert_eq!(converter.pixel(255.0, 255.0, 255.0), (255, 255, 255));
        // A limited range identity is a studio swing one, where the green and
        // blue planes use the luma scaling rather than the chroma one.
        let studio = convert(M::VSC_MATRIX_RGB, 8, (0, 0), false);
        assert_eq!(studio.pixel(235.0, 235.0, 235.0), (255, 255, 255));
        assert_eq!(studio.pixel(16.0, 16.0, 16.0), (0, 0, 0));
    }

    #[test]
    fn the_taps_bracket_a_position_and_clamp_at_the_ends() {
        // The first sample of a 4:2:0 plane sits half a luma sample in, so the
        // first luma position asks for a negative coordinate and has to clamp
        // to the sample it starts at.
        assert_eq!(
            taps(0, 2, 0.5, 3),
            Taps {
                first: 0,
                second: 0,
                weight: 0.75
            }
        );
        // A position that lands on a sample reads that one alone.
        assert_eq!(
            taps(1, 2, 0.0, 3),
            Taps {
                first: 0,
                second: 1,
                weight: 0.5
            }
        );
        // Past the last sample both taps are the last one, so the edge is the
        // edge sample rather than a level beside it.
        let last = taps(7, 2, 0.5, 3);
        assert_eq!(last.first, 2);
        assert_eq!(last.second, 2);
        // A plane that is not subsampled has no cell, so a position is its own
        // sample and the weight is zero.
        for at in 0..4 {
            assert_eq!(
                taps(at, 1, 0.0, 4),
                Taps {
                    first: at,
                    second: at,
                    weight: 0.0
                }
            );
        }
    }

    #[test]
    fn a_subsampled_plane_is_interpolated_between_its_samples() {
        /// One plane of a frame whose rows are the whole of it.
        fn plane(samples: &[u8]) -> Plane<'_> {
            Plane {
                data: samples,
                stride: samples.len(),
            }
        }

        // A 4:2:2 frame whose two chroma samples are the extremes: the luma
        // columns between them have to come out between them, and the luma
        // columns the edge sample covers are that sample rather than a level
        // beside it.
        let converter = convert(M::VSC_MATRIX_BT709, 8, (1, 0), true);
        let luma = [128u8; 4];
        let cb = [0u8, 255];
        let cr = [128u8, 128];
        let mut out = [0u16; 12];
        converter.rgb_row(0, [plane(&luma), plane(&cb), plane(&cr)], &mut out);
        // The blue difference is the plane that varies, so blue is the
        // channel that has to climb: it starts below the scale, ends above it
        // and is between the two where the samples are.
        let blue = |at: usize| out[8 + at];
        assert_eq!(blue(0), 0, "the first sample clamps at the bottom");
        assert_eq!(blue(3), 255, "the last sample clamps at the top");
        assert!(blue(0) < blue(1) && blue(1) < blue(3), "{out:?}");
        // Nothing else moves: the luma is one code throughout and the red
        // difference never changes, so red is the same level in every column
        // even though blue is not.
        let red = |at: usize| out[at];
        assert!(red(0) == red(1) && red(1) == red(3), "{out:?}");
        assert!(red(0) != blue(0) && red(3) != blue(3), "{out:?}");
        // Equal chroma across the row is one level everywhere, whatever the
        // phase is.
        let flat = [128u8, 128];
        let mut even = [0u16; 12];
        converter.rgb_row(0, [plane(&luma), plane(&flat), plane(&flat)], &mut even);
        assert!(even.iter().all(|sample| *sample == even[0]), "{even:?}");
    }

    #[test]
    fn a_float_sample_is_the_sample_it_names() {
        assert_eq!(quantise(0.0, 8), 0);
        assert_eq!(quantise(0.5, 8), 128);
        assert_eq!(quantise(1.0, 8), 255);
        assert_eq!(quantise(0.0, 16), 0);
        assert_eq!(quantise(1.0, 16), 65535);
        // Clamped rather than wrapped, and an infinity is just a value far
        // outside the scale.
        assert_eq!(quantise(-3.0, 8), 0);
        assert_eq!(quantise(7.0, 8), 255);
        assert_eq!(quantise(f32::NEG_INFINITY, 16), 0);
        assert_eq!(quantise(f32::INFINITY, 16), 65535);
        assert_eq!(quantise(f32::NAN, 16), 0);
        // A NaN read out of a plane becomes no picture before it is quantised,
        // which is the same answer by a different route.
        let mut bytes = [0u8; 8];
        bytes[..4].copy_from_slice(&1.0f32.to_ne_bytes());
        bytes[4..].copy_from_slice(&f32::NAN.to_ne_bytes());
        assert_eq!(float_sample(&bytes, 0), 1.0);
        assert_eq!(float_sample(&bytes, 4), 0.0);
        assert!(!float_sample(&bytes, 4).is_nan());
    }

    #[test]
    fn the_geometry_is_the_frames_own() {
        // An odd size loses the luma row and column the frame's own chroma
        // plane has none for, which is the truncation VapourSynth does, and
        // the columns are still the frame's width.
        let odd = Converter::new(8, 8, (1, 1), 5, 3, M::VSC_MATRIX_BT709, true, None).unwrap();
        // The last luma column has no chroma sample of its own, so it reads the
        // one the truncation left behind rather than a level beside it.
        assert_eq!((odd.columns[4].first, odd.columns[4].second), (1, 1));
        assert_eq!(odd.chroma_height, 1);
        assert_eq!(odd.columns.len(), 5);
        // A subsampling this module does not do is refused rather than read at
        // a stride that would not be the plane's.
        for sub in [(2, 0), (0, 2), (-1, 0)] {
            assert!(Converter::new(8, 8, sub, 4, 4, M::VSC_MATRIX_BT709, true, None).is_err());
        }
        // A depth no png can store, and the identity matrix's own depth, are
        // refused where they are read rather than at the end.
        assert!(Converter::new(4, 4, (0, 0), 4, 4, M::VSC_MATRIX_BT709, true, None).is_err());
        assert!(Converter::new(32, 32, (0, 0), 4, 4, M::VSC_MATRIX_BT709, true, None).is_err());
        // A shallower precision is quantised to rather than assumed: ten bits
        // asked for at eight is an eight bit sample and not a clamped ten bit
        // one, which is what a plan stores when the caller names a depth.
        let shallow = Converter::new(10, 8, (0, 0), 4, 4, M::VSC_MATRIX_BT709, true, None).unwrap();
        assert_eq!(shallow.pixel(1023.0, 512.0, 512.0), (255, 255, 255));
    }

    /// Bytes for a test's planes, from a generator written out here rather
    /// than borrowed: a test that needs a crate to make a picture is a test
    /// that stops being run.
    fn noise(length: usize, seed: u64) -> Vec<u8> {
        let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
        (0..length)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn a_kernel_is_the_scalar_path_sample_for_sample() {
        use ffi::VSChromaLocation as C;

        /// One plane of noise whose stride is not its own row, so that the
        /// padding between rows is part of what a conversion has to skip.
        fn plane(width: usize, height: usize, bytes: usize, seed: u64) -> Vec<u8> {
            noise((width * bytes + 5) * height, seed)
        }

        /// One frame of noise through every path, compared sample for
        /// sample.
        ///
        /// The dispatched path is the one a frame takes. The two x86 kernels
        /// are forced beside it, so that a machine whose dispatch picks the
        /// wider one still checks the narrower one.
        #[allow(clippy::too_many_arguments)]
        fn compare(
            bits: i32,
            precision: u32,
            sub: (i32, i32),
            matrix: M,
            full_range: bool,
            location: Option<C>,
            width: usize,
            height: usize,
            seed: u64,
        ) {
            let what = format!(
                "{matrix:?} at {bits} bit into {precision}, subsampling {sub:?}, \
                 range {full_range}, position {location:?}, {width}x{height}"
            );
            let bytes = if bits <= 8 { 1 } else { 2 };
            let (shift_x, shift_y) = sub;
            let stride = |width: usize| width * bytes + 5;
            let (chroma_width, chroma_height) = (width >> shift_x, height >> shift_y);
            let converter = Converter::new(
                bits, precision, sub, width, height, matrix, full_range, location,
            );
            if chroma_width == 0 || chroma_height == 0 {
                // A frame whose chroma plane holds no sample is refused
                // rather than read, and this is the only shape that reaches
                // it.
                assert!(converter.is_err(), "an empty chroma plane of {what}");
                return;
            }
            let converter = converter.unwrap();
            let (luma, cb, cr) = (
                plane(width, height, bytes, seed),
                plane(chroma_width, chroma_height, bytes, seed + 1),
                plane(chroma_width, chroma_height, bytes, seed + 2),
            );
            for row in 0..height {
                let planes = [
                    Plane {
                        data: &luma,
                        stride: stride(width),
                    },
                    Plane {
                        data: &cb,
                        stride: stride(chroma_width),
                    },
                    Plane {
                        data: &cr,
                        stride: stride(chroma_width),
                    },
                ];
                let mut want = vec![0u16; width * 3];
                let rows = converter.rows(row, planes);
                converter.scalar_columns(&rows, 0..0, &mut want);

                let mut got = vec![0u16; width * 3];
                converter.rgb_row(row, planes, &mut got);
                assert_eq!(got, want, "the dispatched path of {what}, row {row}");

                // A machine whose dispatch picks one kernel still checks
                // the other, and an arm build checks the one it has.
                #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
                for (name, kernel) in simd::KERNELS {
                    let mut got = vec![0u16; width * 3];
                    let rows = converter.rows(row, planes);
                    let done = simd::rgb_row_forced(&converter, &rows, &mut got, *kernel);
                    converter.scalar_columns(&rows, done.clone(), &mut got);
                    assert_eq!(got, want, "the {name} kernel of {what}, row {row}");
                    // A row this wide is one a kernel has to have taken a
                    // block of, so a kernel that quietly declined every block
                    // fails here rather than passing by comparing nothing.
                    if width >= 64 {
                        assert!(done.end > 0, "the {name} kernel took no block of {what}");
                    }
                }
            }
        }

        // Every chroma position at every subsampling, which is the whole of
        // what the horizontal pattern can be, over widths that land on a
        // block, between two of them and below one of them.
        let widths: Vec<usize> = (1..=40).chain([64, 65, 129]).collect();
        for sub in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            for location in [
                None,
                Some(C::VSC_CHROMA_LEFT),
                Some(C::VSC_CHROMA_CENTER),
                Some(C::VSC_CHROMA_TOP_LEFT),
                Some(C::VSC_CHROMA_TOP),
                Some(C::VSC_CHROMA_BOTTOM),
            ] {
                for &width in &widths {
                    compare(8, 8, sub, M::VSC_MATRIX_BT709, false, location, width, 5, 7);
                }
            }
        }

        // Every matrix, both ranges, a precision below and one above the word
        // the planes are in, and the identity matrix that reads all three
        // planes as luma.
        for matrix in [
            M::VSC_MATRIX_RGB,
            M::VSC_MATRIX_BT709,
            M::VSC_MATRIX_FCC,
            M::VSC_MATRIX_BT470_BG,
            M::VSC_MATRIX_ST240_M,
            M::VSC_MATRIX_YCGCO,
            M::VSC_MATRIX_BT2020_NCL,
        ] {
            for bits in [8, 10, 12, 16] {
                for precision in [bits as u32, 8] {
                    for full_range in [false, true] {
                        for sub in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                            compare(
                                bits,
                                precision,
                                sub,
                                matrix,
                                full_range,
                                Some(C::VSC_CHROMA_CENTER),
                                37,
                                9,
                                u64::from(bits.cast_unsigned()) + u64::from(precision),
                            );
                        }
                    }
                }
            }
        }
    }

    /// What each kernel costs, which is a measurement rather than a check.
    ///
    /// It is ignored because a wall clock in a test suite is a flaky test,
    /// and it lives here rather than under `target/bench/` because it calls
    /// the kernels directly:
    ///
    /// ```text
    /// cargo test --release --lib -- --ignored --nocapture the_kernels_measured
    /// ```
    #[test]
    #[ignore = "a measurement"]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn the_kernels_measured() {
        use std::time::Instant;

        const WIDTH: usize = 3672;
        const HEIGHT: usize = 5274;

        #[derive(Clone, Copy)]
        enum Path {
            Scalar,
            /// One vector kernel by name, which is the architecture's own
            /// set rather than a fixed list.
            Kernel(simd::Kernel),
            Dispatched,
        }

        /// Every path this build can measure, in the order it is reported.
        ///
        /// A kernel by name is the same list the parent module's test
        /// forces, so an x86 build measures both of its widths and an arm
        /// build the one NEON kernel it has.
        fn paths() -> Vec<(Path, &'static str)> {
            let mut paths = vec![(Path::Scalar, "scalar")];
            paths.extend(
                simd::KERNELS
                    .iter()
                    .map(|(name, kernel)| (Path::Kernel(*kernel), *name)),
            );
            paths.push((Path::Dispatched, "dispatched"));
            paths
        }

        fn measure(
            name: &str,
            bits: i32,
            sub: (i32, i32),
            location: Option<ffi::VSChromaLocation>,
            path: Path,
            repeats: u32,
        ) {
            let bytes = if bits <= 8 { 1 } else { 2 };
            let (shift_x, shift_y) = sub;
            let (chroma_width, chroma_height) = (WIDTH >> shift_x, HEIGHT >> shift_y);
            let converter = Converter::new(
                bits,
                bits.cast_unsigned(),
                sub,
                WIDTH,
                HEIGHT,
                M::VSC_MATRIX_BT470_BG,
                false,
                location,
            )
            .unwrap();
            let stride = |width: usize| width * bytes + 64;
            let (luma, cb, cr) = (
                noise(stride(WIDTH) * HEIGHT, 1),
                noise(stride(chroma_width) * chroma_height, 2),
                noise(stride(chroma_width) * chroma_height, 3),
            );
            let planes = [
                Plane {
                    data: &luma,
                    stride: stride(WIDTH),
                },
                Plane {
                    data: &cb,
                    stride: stride(chroma_width),
                },
                Plane {
                    data: &cr,
                    stride: stride(chroma_width),
                },
            ];
            let mut out = vec![0u16; WIDTH * 3];
            let started = Instant::now();
            for _ in 0..repeats {
                for row in 0..HEIGHT {
                    let rows = converter.rows(row, planes);
                    match path {
                        Path::Scalar => converter.scalar_columns(&rows, 0..0, &mut out),
                        Path::Kernel(kernel) => {
                            let done = simd::rgb_row_forced(&converter, &rows, &mut out, kernel);
                            converter.scalar_columns(&rows, done, &mut out);
                        }
                        Path::Dispatched => converter.rgb_row(row, planes, &mut out),
                    }
                }
            }
            let elapsed = started.elapsed().as_secs_f64();
            let samples = (WIDTH * HEIGHT * repeats as usize) as f64;
            println!("{name}: {:.3} ns a sample", elapsed / samples * 1e9);
        }

        for (name, bits, sub, location) in [
            (
                "8 bit 4:2:0 centred",
                8,
                (1, 1),
                Some(ffi::VSChromaLocation::VSC_CHROMA_CENTER),
            ),
            ("8 bit 4:2:0 left", 8, (1, 1), None),
            ("8 bit 4:4:4", 8, (0, 0), None),
            ("10 bit 4:2:0 left", 10, (1, 1), None),
        ] {
            for (path, label) in paths() {
                measure(&format!("{name}, {label}"), bits, sub, location, path, 2);
            }
        }
    }
}
