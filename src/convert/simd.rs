//! The vector kernels the yuv conversion runs.
//!
//! A conversion is a matrix over three planes, which is the shape a vector
//! register is for: eight or sixteen columns of a row go through the
//! interpolation, the matrix and the quantisation together instead of one
//! sample at a time. The scalar path in the parent module is what a kernel is
//! checked against and what a machine without one runs, not a fallback that
//! stopped being exercised: `tests/pngwrite.vpy` compares every sample of a
//! converted frame with `core.resize.Bilinear`'s own, and the unit tests
//! compare a kernel with the scalar path sample for sample.
//!
//! What a kernel does is decided by two properties of the frame rather than of
//! a sample.
//!
//! The first is the width of a source sample: a block loads eight bytes or
//! sixteen bit words and widens them once, so the two widths are one macro
//! expanded twice rather than two kernels kept in step by hand.
//!
//! The second is where the chroma samples sit, which is one of three patterns.
//! A 4:4:4 plane reads the sample its column names, and a subsampled plane
//! interpolates between two, either at the left sited position or half a luma
//! sample further in; every other position VapourSynth names is one of those
//! two once the axis that is not subsampled drops its phase.
//!
//! A block reads the chroma samples its columns bracket and moves each lane's
//! two taps into place with one permute, so the horizontal interpolation is a
//! permute, two multiplies and an add rather than a gather. The columns a block
//! cannot cover are the scalar path's: a kernel converts a range of a row and
//! says which range, and the caller reads the rest a sample at a time.

use std::ops::Range;

use super::{Converter, Rows};

/// Converts the columns a vector kernel covers, and says which range they are.
///
/// The range is a prefix of the row except where the first block of a centred
/// plane would read before its own first sample, and it is empty when this
/// machine has no kernel for the shape, or none at all.
pub(super) fn rgb_row(c: &Converter, rows: &Rows<'_>, out: &mut [u16]) -> Range<usize> {
    #[cfg(target_arch = "x86_64")]
    {
        x86::rgb_row(c, rows, out)
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = (c, rows, out);
        0..0
    }
}

/// One of the x86 kernels by name, which is what the parent module's test uses
/// to check both of them on a machine whose dispatch would pick the wider one.
///
/// A machine that cannot run the kernel it names gets an empty range, so the
/// test compares the scalar path with itself there rather than failing.
#[cfg(all(test, target_arch = "x86_64"))]
pub(super) fn rgb_row_forced(
    c: &Converter,
    rows: &Rows<'_>,
    out: &mut [u16],
    wide: bool,
) -> Range<usize> {
    x86::forced(c, rows, out, wide)
}

// Only the 64 bit x86 targets are built for: a 32 bit one carries the baseline
// library of the wheel, which is the scalar path, and the kernels below name
// registers that only the wider target has.
#[cfg(target_arch = "x86_64")]
mod x86 {
    use std::ops::Range;

    use super::super::{Converter, Rows};

    /// Where a block's two chroma taps are, as indices into the samples it
    /// loads.
    ///
    /// A lane reads the sample its column brackets and the one after it, so a
    /// pattern is a pair of index vectors. The first block of a centred plane
    /// is a case of its own: the column before its first chroma sample has no
    /// sample to read and clamps onto the first one.
    #[derive(Clone, Copy)]
    enum Pattern {
        /// A 4:4:4 plane, where the column names its own sample.
        Direct,
        /// A subsampled plane whose first sample sits on the first luma sample.
        Left,
        /// A subsampled plane whose first sample sits half a luma sample in.
        Centred,
    }

    impl Pattern {
        /// The pattern a frame's chroma position is, or `None` for one no
        /// kernel reads.
        ///
        /// A phase is already zero on an axis that is not subsampled, so the
        /// three cases here are the whole of what a 4:4:4, a 4:2:2 and a 4:2:0
        /// frame can be.
        fn of(c: &Converter) -> Option<Self> {
            match (c.sub_x, c.phase_x) {
                (1, _) => Some(Self::Direct),
                (2, 0.0) => Some(Self::Left),
                (2, 0.5) => Some(Self::Centred),
                _ => None,
            }
        }
    }

    pub(super) fn rgb_row(c: &Converter, rows: &Rows<'_>, out: &mut [u16]) -> Range<usize> {
        let Some(pattern) = Pattern::of(c) else {
            return 0..0;
        };
        // SAFETY: a kernel is entered only where the processor reports the
        // features it was built for, and each one reads a plane inside the
        // bounds its own geometry was resolved against.
        unsafe {
            // The byte and word extension of the 512 bit set is the one the
            // kernel needs, and it cannot be reported without the foundation
            // it is built on.
            if is_x86_feature_detected!("avx512bw") {
                avx512::rgb_row(c, rows, out, pattern)
            } else if is_x86_feature_detected!("avx2") {
                avx2::rgb_row(c, rows, out, pattern)
            } else {
                0..0
            }
        }
    }

    /// One kernel by name, for the parent module's test.
    #[cfg(test)]
    pub(super) fn forced(
        c: &Converter,
        rows: &Rows<'_>,
        out: &mut [u16],
        wide: bool,
    ) -> Range<usize> {
        let Some(pattern) = Pattern::of(c) else {
            return 0..0;
        };
        // SAFETY: the caller checks the feature of the kernel it names, and
        // this is only reached from a test on the machine it is built for.
        unsafe {
            if wide && is_x86_feature_detected!("avx512bw") {
                avx512::rgb_row(c, rows, out, pattern)
            } else if !wide && is_x86_feature_detected!("avx2") {
                avx2::rgb_row(c, rows, out, pattern)
            } else {
                0..0
            }
        }
    }

    mod avx2 {
        use std::arch::x86_64::*;
        use std::ops::Range;

        use super::super::super::{Converter, Rows};
        use super::Pattern;

        /// The columns one block covers.
        const LANES: usize = 8;

        /// A block's interpolation, given the samples it loaded: the two taps
        /// of every lane, and the line between them.
        ///
        /// The weight multiplies both taps rather than being added to one of
        /// them, which is the parent module's own `lerp` and what keeps a
        /// weight of zero exactly its left tap.
        #[target_feature(enable = "avx2")]
        #[inline]
        unsafe fn interpolate(
            samples: __m256,
            left: __m256i,
            right: __m256i,
            weight: __m256,
            one_minus: __m256,
        ) -> __m256 {
            _mm256_add_ps(
                _mm256_mul_ps(_mm256_permutevar8x32_ps(samples, left), one_minus),
                _mm256_mul_ps(_mm256_permutevar8x32_ps(samples, right), weight),
            )
        }

        /// One channel of a block as the levels a PNG stores.
        ///
        /// The clamp, the scale and the round are the scalar path's, in the
        /// same order and with the same constants, so a sample is the same
        /// level either way. The minimum after the round is that path's own
        /// guard against the one level that would not fit the word, and it is
        /// the same answer: a value below it is truncated as it stands.
        #[target_feature(enable = "avx2")]
        #[inline]
        unsafe fn levels(value: __m256, one: __m256, maximum: __m256, half: __m256) -> __m256i {
            let clamped = _mm256_max_ps(_mm256_min_ps(value, one), _mm256_setzero_ps());
            let scaled = _mm256_min_ps(
                _mm256_add_ps(_mm256_mul_ps(clamped, maximum), half),
                maximum,
            );
            _mm256_cvttps_epi32(scaled)
        }

        /// Stores one channel of a block into the row's own plane.
        #[target_feature(enable = "avx2")]
        #[inline]
        unsafe fn store(levels: __m256i, at: *mut u16) {
            let packed = _mm_packus_epi32(
                _mm256_castsi256_si128(levels),
                _mm256_extracti128_si256(levels, 1),
            );
            // SAFETY: the caller keeps the eight samples inside the row.
            unsafe { _mm_storeu_si128(at.cast(), packed) };
        }

        /// Loads eight samples of a plane as the values a matrix multiplies.
        #[target_feature(enable = "avx2")]
        #[inline]
        unsafe fn eight_u8(ptr: *const u8) -> __m256 {
            // SAFETY: the caller keeps the eight samples inside the plane.
            let bytes = unsafe { _mm_loadl_epi64(ptr.cast()) };
            _mm256_cvtepi32_ps(_mm256_cvtepu8_epi32(bytes))
        }

        /// The same for a plane of sixteen bit samples.
        #[target_feature(enable = "avx2")]
        #[inline]
        unsafe fn eight_u16(ptr: *const u16) -> __m256 {
            // SAFETY: as above.
            let words = unsafe { _mm_loadu_si128(ptr.cast()) };
            _mm256_cvtepi32_ps(_mm256_cvtepu16_epi32(words))
        }

        pub(super) unsafe fn rgb_row(
            c: &Converter,
            rows: &Rows<'_>,
            out: &mut [u16],
            pattern: Pattern,
        ) -> Range<usize> {
            // SAFETY: the caller checked the processor's features.
            unsafe {
                if c.bytes == 1 {
                    row_u8(c, rows, out, pattern)
                } else {
                    row_u16(c, rows, out, pattern)
                }
            }
        }

        /// The row kernel, once for each width of source sample.
        ///
        /// The two widths differ in one load and one pointer step and nothing
        /// else, so they are one macro expanded twice rather than two kernels
        /// that have to be kept in step by hand.
        macro_rules! kernel {
            ($name:ident, $sample:ty, $load:ident) => {
                #[target_feature(enable = "avx2")]
                unsafe fn $name(
                    c: &Converter,
                    rows: &Rows<'_>,
                    out: &mut [u16],
                    pattern: Pattern,
                ) -> Range<usize> {
                    let width = c.columns.len();
                    let chroma_width = c.chroma_width;
                    // The identity matrix reads all three planes as luma, which
                    // is what a studio swing r,g,b frame is; every other matrix
                    // scales the two chroma planes about their own zero.
                    let (mid_floor, mid_scale) = match c.coefficients {
                        None => (c.luma_floor, c.luma_scale),
                        Some(_) => (c.chroma_floor, c.chroma_scale),
                    };
                    // The matrix, as the four constants the scalar path
                    // multiplies by, formed in the same order so that a product
                    // is the same number.
                    let (kcr, kcb, k1, k2) = match c.coefficients {
                        None => (0.0, 0.0, 0.0, 0.0),
                        Some(m) => {
                            let kg = m.kg();
                            (
                                2.0 * (1.0 - m.kr),
                                2.0 * (1.0 - m.kb),
                                2.0 * m.kb * (1.0 - m.kb) / kg,
                                2.0 * m.kr * (1.0 - m.kr) / kg,
                            )
                        }
                    };
                    // SAFETY: every intrinsic below is one the feature this
                    // function is built for provides, every load is inside the
                    // plane of a block the loop's own bounds check keeps whole,
                    // and every store is inside the plane of the row.
                    unsafe {
                        let luma_floor = _mm256_set1_ps(c.luma_floor);
                        let luma_scale = _mm256_set1_ps(c.luma_scale);
                        let mid_floor = _mm256_set1_ps(mid_floor);
                        let mid_scale = _mm256_set1_ps(mid_scale);
                        let kcr = _mm256_set1_ps(kcr);
                        let kcb = _mm256_set1_ps(kcb);
                        let k1 = _mm256_set1_ps(k1);
                        let k2 = _mm256_set1_ps(k2);
                        let maximum = _mm256_set1_ps(((1u32 << c.precision) - 1) as f32);
                        let half = _mm256_set1_ps(0.5);
                        let one = _mm256_set1_ps(1.0);
                        let zero = _mm256_setzero_ps();
                        let identity = c.coefficients.is_none();

                        // How far the row sits between its two chroma rows, and
                        // whether it sits between them at all: a plane that is
                        // not subsampled vertically reads one row, and the
                        // scalar path's weight is then exactly zero.
                        let weighted = rows.weight != 0.0;
                        let vertical = _mm256_set1_ps(rows.weight);
                        let one_minus_vertical = _mm256_set1_ps(1.0 - rows.weight);

                        // The taps of a block, which are the same pair of index
                        // vectors for every block of a pattern.
                        let identity_index = _mm256_setr_epi32(0, 1, 2, 3, 4, 5, 6, 7);
                        let left_a = _mm256_setr_epi32(0, 0, 1, 1, 2, 2, 3, 3);
                        let left_b = _mm256_setr_epi32(0, 1, 1, 2, 2, 3, 3, 4);
                        let centred_first_a = _mm256_setr_epi32(0, 0, 0, 1, 1, 2, 2, 3);
                        let centred_b = _mm256_setr_epi32(1, 2, 2, 3, 3, 4, 4, 5);
                        let weight_left = _mm256_setr_ps(0.0, 0.5, 0.0, 0.5, 0.0, 0.5, 0.0, 0.5);
                        let weight_centred =
                            _mm256_setr_ps(0.75, 0.25, 0.75, 0.25, 0.75, 0.25, 0.75, 0.25);
                        let one_minus_left = _mm256_sub_ps(one, weight_left);
                        let one_minus_centred = _mm256_sub_ps(one, weight_centred);

                        let luma = rows.luma.as_ptr().cast::<$sample>();
                        let cb_above = rows.cb_above.as_ptr().cast::<$sample>();
                        let cb_below = rows.cb_below.as_ptr().cast::<$sample>();
                        let cr_above = rows.cr_above.as_ptr().cast::<$sample>();
                        let cr_below = rows.cr_below.as_ptr().cast::<$sample>();
                        let at = out.as_mut_ptr();

                        let mut x = 0usize;
                        while x + LANES <= width {
                            let (base, a_index, b_index, weight, one_minus) = match pattern {
                                Pattern::Direct => (x, identity_index, identity_index, zero, one),
                                Pattern::Left => {
                                    (x / 2, left_a, left_b, weight_left, one_minus_left)
                                }
                                Pattern::Centred if x == 0 => (
                                    0,
                                    centred_first_a,
                                    left_b,
                                    weight_centred,
                                    one_minus_centred,
                                ),
                                Pattern::Centred => (
                                    x / 2 - 1,
                                    left_b,
                                    centred_b,
                                    weight_centred,
                                    one_minus_centred,
                                ),
                            };
                            // A block reads `LANES` samples from `base`, so it
                            // is only taken where those are inside the plane.
                            if base + LANES > chroma_width {
                                break;
                            }
                            let y = _mm256_mul_ps(
                                _mm256_sub_ps($load(luma.add(x)), luma_floor),
                                luma_scale,
                            );
                            let cb = interpolate(
                                $load(cb_above.add(base)),
                                a_index,
                                b_index,
                                weight,
                                one_minus,
                            );
                            let cb = if weighted {
                                let below = interpolate(
                                    $load(cb_below.add(base)),
                                    a_index,
                                    b_index,
                                    weight,
                                    one_minus,
                                );
                                _mm256_add_ps(
                                    _mm256_mul_ps(cb, one_minus_vertical),
                                    _mm256_mul_ps(below, vertical),
                                )
                            } else {
                                cb
                            };
                            let cb = _mm256_mul_ps(_mm256_sub_ps(cb, mid_floor), mid_scale);
                            let cr = interpolate(
                                $load(cr_above.add(base)),
                                a_index,
                                b_index,
                                weight,
                                one_minus,
                            );
                            let cr = if weighted {
                                let below = interpolate(
                                    $load(cr_below.add(base)),
                                    a_index,
                                    b_index,
                                    weight,
                                    one_minus,
                                );
                                _mm256_add_ps(
                                    _mm256_mul_ps(cr, one_minus_vertical),
                                    _mm256_mul_ps(below, vertical),
                                )
                            } else {
                                cr
                            };
                            let cr = _mm256_mul_ps(_mm256_sub_ps(cr, mid_floor), mid_scale);

                            let (r, g, b) = if identity {
                                (y, cb, cr)
                            } else {
                                (
                                    _mm256_add_ps(y, _mm256_mul_ps(kcr, cr)),
                                    _mm256_sub_ps(
                                        _mm256_sub_ps(y, _mm256_mul_ps(k1, cb)),
                                        _mm256_mul_ps(k2, cr),
                                    ),
                                    _mm256_add_ps(y, _mm256_mul_ps(kcb, cb)),
                                )
                            };
                            store(levels(r, one, maximum, half), at.add(x));
                            store(levels(g, one, maximum, half), at.add(width + x));
                            store(levels(b, one, maximum, half), at.add(2 * width + x));
                            x += LANES;
                        }
                        0..x
                    }
                }
            };
        }

        kernel!(row_u8, u8, eight_u8);
        kernel!(row_u16, u16, eight_u16);
    }

    mod avx512 {
        use std::arch::x86_64::*;
        use std::ops::Range;

        use super::super::super::{Converter, Rows};
        use super::Pattern;

        /// The columns one block covers.
        const LANES: usize = 16;

        /// A block's interpolation, given the samples it loaded.
        ///
        /// The permute takes its index first here, which is the other way round
        /// from the 256 bit one.
        #[target_feature(enable = "avx512f,avx512bw")]
        #[inline]
        unsafe fn interpolate(
            samples: __m512,
            left: __m512i,
            right: __m512i,
            weight: __m512,
            one_minus: __m512,
        ) -> __m512 {
            _mm512_add_ps(
                _mm512_mul_ps(_mm512_permutexvar_ps(left, samples), one_minus),
                _mm512_mul_ps(_mm512_permutexvar_ps(right, samples), weight),
            )
        }

        /// One channel of a block as the levels a PNG stores.
        #[target_feature(enable = "avx512f,avx512bw")]
        #[inline]
        unsafe fn levels(value: __m512, one: __m512, maximum: __m512, half: __m512) -> __m256i {
            let clamped = _mm512_max_ps(_mm512_min_ps(value, one), _mm512_setzero_ps());
            let scaled = _mm512_min_ps(
                _mm512_add_ps(_mm512_mul_ps(clamped, maximum), half),
                maximum,
            );
            // Every level is inside `[0, maximum]` and `maximum` is a sixteen
            // bit word at most, so the truncation loses nothing.
            _mm512_cvtepi32_epi16(_mm512_cvttps_epi32(scaled))
        }

        /// Stores one channel of a block into the row's own plane.
        #[target_feature(enable = "avx512f,avx512bw")]
        #[inline]
        unsafe fn store(levels: __m256i, at: *mut u16) {
            // SAFETY: the caller keeps the sixteen samples inside the row.
            unsafe { _mm256_storeu_si256(at.cast(), levels) };
        }

        /// Loads sixteen samples of a plane as the values a matrix multiplies.
        #[target_feature(enable = "avx512f,avx512bw")]
        #[inline]
        unsafe fn sixteen_u8(ptr: *const u8) -> __m512 {
            // SAFETY: the caller keeps the sixteen samples inside the plane.
            let bytes = unsafe { _mm_loadu_si128(ptr.cast()) };
            _mm512_cvtepi32_ps(_mm512_cvtepu8_epi32(bytes))
        }

        /// The same for a plane of sixteen bit samples.
        #[target_feature(enable = "avx512f,avx512bw")]
        #[inline]
        unsafe fn sixteen_u16(ptr: *const u16) -> __m512 {
            // SAFETY: as above.
            let words = unsafe { _mm256_loadu_si256(ptr.cast()) };
            _mm512_cvtepi32_ps(_mm512_cvtepu16_epi32(words))
        }

        pub(super) unsafe fn rgb_row(
            c: &Converter,
            rows: &Rows<'_>,
            out: &mut [u16],
            pattern: Pattern,
        ) -> Range<usize> {
            // SAFETY: the caller checked the processor's features.
            unsafe {
                if c.bytes == 1 {
                    row_u8(c, rows, out, pattern)
                } else {
                    row_u16(c, rows, out, pattern)
                }
            }
        }

        /// The row kernel, once for each width of source sample.
        macro_rules! kernel {
            ($name:ident, $sample:ty, $load:ident) => {
                #[target_feature(enable = "avx512f,avx512bw")]
                unsafe fn $name(
                    c: &Converter,
                    rows: &Rows<'_>,
                    out: &mut [u16],
                    pattern: Pattern,
                ) -> Range<usize> {
                    let width = c.columns.len();
                    let chroma_width = c.chroma_width;
                    let (mid_floor, mid_scale) = match c.coefficients {
                        None => (c.luma_floor, c.luma_scale),
                        Some(_) => (c.chroma_floor, c.chroma_scale),
                    };
                    let (kcr, kcb, k1, k2) = match c.coefficients {
                        None => (0.0, 0.0, 0.0, 0.0),
                        Some(m) => {
                            let kg = m.kg();
                            (
                                2.0 * (1.0 - m.kr),
                                2.0 * (1.0 - m.kb),
                                2.0 * m.kb * (1.0 - m.kb) / kg,
                                2.0 * m.kr * (1.0 - m.kr) / kg,
                            )
                        }
                    };
                    // SAFETY: as in the 256 bit kernel above.
                    unsafe {
                        let luma_floor = _mm512_set1_ps(c.luma_floor);
                        let luma_scale = _mm512_set1_ps(c.luma_scale);
                        let mid_floor = _mm512_set1_ps(mid_floor);
                        let mid_scale = _mm512_set1_ps(mid_scale);
                        let kcr = _mm512_set1_ps(kcr);
                        let kcb = _mm512_set1_ps(kcb);
                        let k1 = _mm512_set1_ps(k1);
                        let k2 = _mm512_set1_ps(k2);
                        let maximum = _mm512_set1_ps(((1u32 << c.precision) - 1) as f32);
                        let half = _mm512_set1_ps(0.5);
                        let one = _mm512_set1_ps(1.0);
                        let zero = _mm512_setzero_ps();
                        let identity = c.coefficients.is_none();

                        let weighted = rows.weight != 0.0;
                        let vertical = _mm512_set1_ps(rows.weight);
                        let one_minus_vertical = _mm512_set1_ps(1.0 - rows.weight);

                        // Sixteen columns a block bracket seventeen chroma
                        // samples, which is one more than the eight a 256 bit
                        // block reads, so the index vectors run one further.
                        let identity_index =
                            _mm512_setr_epi32(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15);
                        let left_a =
                            _mm512_setr_epi32(0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7);
                        let left_b =
                            _mm512_setr_epi32(0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8);
                        let centred_first_a =
                            _mm512_setr_epi32(0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7);
                        let centred_b =
                            _mm512_setr_epi32(1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9);
                        let weight_left = _mm512_setr_ps(
                            0.0, 0.5, 0.0, 0.5, 0.0, 0.5, 0.0, 0.5, 0.0, 0.5, 0.0, 0.5, 0.0, 0.5,
                            0.0, 0.5,
                        );
                        let weight_centred = _mm512_setr_ps(
                            0.75, 0.25, 0.75, 0.25, 0.75, 0.25, 0.75, 0.25, 0.75, 0.25, 0.75, 0.25,
                            0.75, 0.25, 0.75, 0.25,
                        );
                        let one_minus_left = _mm512_sub_ps(one, weight_left);
                        let one_minus_centred = _mm512_sub_ps(one, weight_centred);

                        let luma = rows.luma.as_ptr().cast::<$sample>();
                        let cb_above = rows.cb_above.as_ptr().cast::<$sample>();
                        let cb_below = rows.cb_below.as_ptr().cast::<$sample>();
                        let cr_above = rows.cr_above.as_ptr().cast::<$sample>();
                        let cr_below = rows.cr_below.as_ptr().cast::<$sample>();
                        let at = out.as_mut_ptr();

                        let mut x = 0usize;
                        while x + LANES <= width {
                            let (base, a_index, b_index, weight, one_minus) = match pattern {
                                Pattern::Direct => (x, identity_index, identity_index, zero, one),
                                Pattern::Left => {
                                    (x / 2, left_a, left_b, weight_left, one_minus_left)
                                }
                                Pattern::Centred if x == 0 => (
                                    0,
                                    centred_first_a,
                                    left_b,
                                    weight_centred,
                                    one_minus_centred,
                                ),
                                Pattern::Centred => (
                                    x / 2 - 1,
                                    left_b,
                                    centred_b,
                                    weight_centred,
                                    one_minus_centred,
                                ),
                            };
                            if base + LANES > chroma_width {
                                break;
                            }
                            let y = _mm512_mul_ps(
                                _mm512_sub_ps($load(luma.add(x)), luma_floor),
                                luma_scale,
                            );
                            let cb = interpolate(
                                $load(cb_above.add(base)),
                                a_index,
                                b_index,
                                weight,
                                one_minus,
                            );
                            let cb = if weighted {
                                let below = interpolate(
                                    $load(cb_below.add(base)),
                                    a_index,
                                    b_index,
                                    weight,
                                    one_minus,
                                );
                                _mm512_add_ps(
                                    _mm512_mul_ps(cb, one_minus_vertical),
                                    _mm512_mul_ps(below, vertical),
                                )
                            } else {
                                cb
                            };
                            let cb = _mm512_mul_ps(_mm512_sub_ps(cb, mid_floor), mid_scale);
                            let cr = interpolate(
                                $load(cr_above.add(base)),
                                a_index,
                                b_index,
                                weight,
                                one_minus,
                            );
                            let cr = if weighted {
                                let below = interpolate(
                                    $load(cr_below.add(base)),
                                    a_index,
                                    b_index,
                                    weight,
                                    one_minus,
                                );
                                _mm512_add_ps(
                                    _mm512_mul_ps(cr, one_minus_vertical),
                                    _mm512_mul_ps(below, vertical),
                                )
                            } else {
                                cr
                            };
                            let cr = _mm512_mul_ps(_mm512_sub_ps(cr, mid_floor), mid_scale);

                            let (r, g, b) = if identity {
                                (y, cb, cr)
                            } else {
                                (
                                    _mm512_add_ps(y, _mm512_mul_ps(kcr, cr)),
                                    _mm512_sub_ps(
                                        _mm512_sub_ps(y, _mm512_mul_ps(k1, cb)),
                                        _mm512_mul_ps(k2, cr),
                                    ),
                                    _mm512_add_ps(y, _mm512_mul_ps(kcb, cb)),
                                )
                            };
                            store(levels(r, one, maximum, half), at.add(x));
                            store(levels(g, one, maximum, half), at.add(width + x));
                            store(levels(b, one, maximum, half), at.add(2 * width + x));
                            x += LANES;
                        }
                        0..x
                    }
                }
            };
        }

        kernel!(row_u8, u8, sixteen_u8);
        kernel!(row_u16, u16, sixteen_u16);
    }
}
