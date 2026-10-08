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
    // Every aarch64 processor has NEON, so there is nothing to detect and the
    // one kernel is the dispatched path rather than a branch inside it.
    #[cfg(target_arch = "aarch64")]
    {
        neon::rgb_row(c, rows, out)
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        let _ = (c, rows, out);
        0..0
    }
}

/// Where a block's two chroma taps are, as indices into the samples it
/// loads.
///
/// A lane reads the sample its column brackets and the one after it, so a
/// pattern is a pair of index vectors. The first block of a centred plane
/// is a case of its own: the column before its first chroma sample has no
/// sample to read and clamps onto the first one.
///
/// A phase is already zero on an axis that is not subsampled, so the three
/// cases here are the whole of what a 4:4:4, a 4:2:2 and a 4:2:0 frame can
/// be. Both architectures' kernels read their taps by this one name.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[derive(Clone, Copy)]
enum Pattern {
    /// A 4:4:4 plane, where the column names its own sample.
    Direct,
    /// A subsampled plane whose first sample sits on the first luma sample.
    Left,
    /// A subsampled plane whose first sample sits half a luma sample in.
    Centred,
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
impl Pattern {
    /// The pattern a frame's chroma position is, or `None` for one no
    /// kernel reads.
    fn of(c: &Converter) -> Option<Self> {
        match (c.sub_x, c.phase_x) {
            (1, _) => Some(Self::Direct),
            (2, 0.0) => Some(Self::Left),
            (2, 0.5) => Some(Self::Centred),
            _ => None,
        }
    }
}

/// One vector kernel by name, which is what the parent module's test uses to
/// check a kernel the dispatch on this machine would not pick.
///
/// A machine that cannot run the kernel it names gets an empty range, so the
/// test compares the scalar path with itself there rather than failing.
#[cfg(all(test, any(target_arch = "x86_64", target_arch = "aarch64")))]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Kernel {
    #[cfg(target_arch = "x86_64")]
    Avx2,
    #[cfg(target_arch = "x86_64")]
    Avx512,
    #[cfg(target_arch = "aarch64")]
    Neon,
}

/// Every kernel this architecture can be asked to run by name, with the name
/// a failure reports it under.
#[cfg(all(test, target_arch = "x86_64"))]
pub(super) const KERNELS: &[(&str, Kernel)] = &[("avx2", Kernel::Avx2), ("avx512", Kernel::Avx512)];
#[cfg(all(test, target_arch = "aarch64"))]
pub(super) const KERNELS: &[(&str, Kernel)] = &[("neon", Kernel::Neon)];

/// One kernel by name, for the parent module's test.
#[cfg(all(test, any(target_arch = "x86_64", target_arch = "aarch64")))]
pub(super) fn rgb_row_forced(
    c: &Converter,
    rows: &Rows<'_>,
    out: &mut [u16],
    kernel: Kernel,
) -> Range<usize> {
    #[cfg(target_arch = "x86_64")]
    {
        x86::forced(c, rows, out, kernel)
    }
    #[cfg(target_arch = "aarch64")]
    {
        // The one kernel this architecture has, so the name is
        // irrefutable rather than a branch that could miss.
        let Kernel::Neon = kernel;
        neon::rgb_row(c, rows, out)
    }
}

// Only the 64 bit x86 targets are built for: a 32 bit one carries the baseline
// library of the wheel, which is the scalar path, and the kernels below name
// registers that only the wider target has.
#[cfg(target_arch = "x86_64")]
mod x86 {
    use std::ops::Range;

    use super::super::{Converter, Rows};
    use super::Pattern;
    // A kernel by name is the parent module's test's, so only a test build
    // has the type to name one with.
    #[cfg(test)]
    use super::Kernel;

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
        kernel: Kernel,
    ) -> Range<usize> {
        let Some(pattern) = Pattern::of(c) else {
            return 0..0;
        };
        // SAFETY: the caller checks the feature of the kernel it names, and
        // this is only reached from a test on the machine it is built for.
        unsafe {
            match kernel {
                Kernel::Avx512 if is_x86_feature_detected!("avx512bw") => {
                    avx512::rgb_row(c, rows, out, pattern)
                }
                Kernel::Avx2 if is_x86_feature_detected!("avx2") => {
                    avx2::rgb_row(c, rows, out, pattern)
                }
                _ => 0..0,
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

// Every aarch64 processor has NEON as part of the baseline it is required to
// have, so this needs no feature detection and no runtime dispatch: the one
// kernel is the architecture's.
#[cfg(target_arch = "aarch64")]
mod neon {
    use std::arch::aarch64::*;
    use std::ops::Range;

    use super::super::{Converter, Rows};
    use super::Pattern;

    /// The columns one block covers.
    ///
    /// A NEON register is 128 bits, so one holds four `f32` lanes: a block is
    /// four columns where the 256 bit and 512 bit kernels take eight and
    /// sixteen.
    const LANES: usize = 4;

    /// The taps of a block, as the byte indices a table lookup gathers its
    /// four lanes by: lane `i` of the result is the `f32` that starts at byte
    /// `4 * index[i]` of the samples the block loaded.
    ///
    /// The patterns are the parent module's. A 4:4:4 plane reads the sample
    /// its column names; a subsampled one interpolates the two either side of
    /// the position its column is at, which is the sample it brackets and the
    /// one after it for the left sited position and shifted half a sample for
    /// the centred one.
    const TAPS_DIRECT: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
    /// `{s0, s0, s1, s1}`, the left tap of a left sited plane.
    const TAPS_LEFT: [u8; 16] = [0, 1, 2, 3, 0, 1, 2, 3, 4, 5, 6, 7, 4, 5, 6, 7];
    /// `{s0, s1, s1, s2}`, the right tap of a left sited plane and the left
    /// tap of a centred one past its first block.
    const TAPS_MID: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 4, 5, 6, 7, 8, 9, 10, 11];
    /// `{s0, s0, s0, s1}`, the left tap of a centred plane's first block,
    /// whose leading column sits before the first chroma sample and clamps
    /// onto it.
    const TAPS_FIRST: [u8; 16] = [0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3, 4, 5, 6, 7];
    /// `{s1, s2, s2, s3}`, the right tap of a centred plane past its first
    /// block.
    const TAPS_RIGHT: [u8; 16] = [4, 5, 6, 7, 8, 9, 10, 11, 8, 9, 10, 11, 12, 13, 14, 15];

    /// The weight of the right tap of a left sited plane: half for every odd
    /// column, which is the column between two chroma samples, and nothing
    /// for every even one, which is a sample itself.
    const WEIGHT_LEFT: [f32; 4] = [0.0, 0.5, 0.0, 0.5];
    /// The same for a centred plane, where the position is half a chroma
    /// sample further in and the first column lands before the first sample.
    const WEIGHT_CENTRED: [f32; 4] = [0.75, 0.25, 0.75, 0.25];

    /// A block's interpolation, given the samples it loaded: the two taps of
    /// every lane, and the line between them.
    ///
    /// The weight multiplies both taps rather than being added to one of
    /// them, which is the parent module's own `lerp` and what keeps a weight
    /// of zero exactly its left tap.
    ///
    /// SAFETY: `left` and `right` name bytes of `samples`, which is one
    /// register, so the table lookup reads out of it.
    #[inline]
    unsafe fn interpolate(
        samples: float32x4_t,
        left: uint8x16_t,
        right: uint8x16_t,
        weight: float32x4_t,
        one_minus: float32x4_t,
    ) -> float32x4_t {
        // SAFETY: the caller passes this module's own tap vectors, and the
        // table lookup reads out of the one register they index.
        unsafe {
            let bytes = vreinterpretq_u8_f32(samples);
            vaddq_f32(
                vmulq_f32(vreinterpretq_f32_u8(vqtbl1q_u8(bytes, left)), one_minus),
                vmulq_f32(vreinterpretq_f32_u8(vqtbl1q_u8(bytes, right)), weight),
            )
        }
    }

    /// One channel of a block as the levels a PNG stores.
    ///
    /// The clamp, the scale and the round are the scalar path's, in the same
    /// order and with the same constants, so a sample is the same level
    /// either way. The minimum after the round is that path's own guard
    /// against the one level that would not fit the word, and it is the same
    /// answer: a value below it is truncated as it stands.
    ///
    /// The narrowing conversion rounds toward zero, which is what the scalar
    /// path's cast does and what the x86 kernels' `cvttps` does.
    #[inline]
    unsafe fn levels(
        value: float32x4_t,
        zero: float32x4_t,
        one: float32x4_t,
        maximum: float32x4_t,
        half: float32x4_t,
    ) -> uint32x4_t {
        // SAFETY: arithmetic on the caller's own registers.
        unsafe {
            let clamped = vmaxq_f32(vminq_f32(value, one), zero);
            let scaled = vminq_f32(vaddq_f32(vmulq_f32(clamped, maximum), half), maximum);
            vcvtq_u32_f32(scaled)
        }
    }

    /// Stores one channel of a block into the row's own plane.
    ///
    /// A level is inside the word already -- the clamp above is what keeps it
    /// there -- so the narrowing saturates rather than wrapping.
    ///
    /// SAFETY: the caller keeps the four samples inside the row.
    #[inline]
    unsafe fn store(levels: uint32x4_t, at: *mut u16) {
        // SAFETY: as above.
        unsafe { vst1_u16(at, vqmovn_u32(levels)) };
    }

    /// Loads four samples of a plane as the values a matrix multiplies.
    ///
    /// SAFETY: the caller keeps the four samples inside the plane.
    #[inline]
    unsafe fn four_u8(ptr: *const u8) -> float32x4_t {
        // SAFETY: the caller's own bound.
        unsafe {
            // Four bytes read as one word, so the load is exactly the block's
            // own samples rather than a word of them and the beginning of the
            // next.
            let word = std::ptr::read_unaligned(ptr.cast::<u32>());
            let bytes = vreinterpret_u8_u32(vdup_n_u32(word));
            vcvtq_f32_u32(vmovl_u16(vget_low_u16(vmovl_u8(bytes))))
        }
    }

    /// The same for a plane of sixteen bit samples.
    ///
    /// SAFETY: as above.
    #[inline]
    unsafe fn four_u16(ptr: *const u16) -> float32x4_t {
        // SAFETY: the caller's own bound.
        unsafe { vcvtq_f32_u32(vmovl_u16(vld1_u16(ptr))) }
    }

    pub(super) fn rgb_row(c: &Converter, rows: &Rows<'_>, out: &mut [u16]) -> Range<usize> {
        let Some(pattern) = Pattern::of(c) else {
            return 0..0;
        };
        // SAFETY: every aarch64 processor has NEON, and each kernel reads a
        // plane inside the bounds its own geometry was resolved against.
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
            /// Converts the columns one block at a time, and says which of
            /// them it filled.
            ///
            /// SAFETY: every plane the rows name is inside the frame whose
            /// geometry the converter was resolved against.
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
                // SAFETY: every intrinsic below is one this architecture
                // provides, every load is inside the plane of a block the
                // loop's own bounds check keeps whole, and every store is
                // inside the plane of the row.
                unsafe {
                    let zero = vdupq_n_f32(0.0);
                    let one = vdupq_n_f32(1.0);
                    let half = vdupq_n_f32(0.5);
                    let luma_floor = vdupq_n_f32(c.luma_floor);
                    let luma_scale = vdupq_n_f32(c.luma_scale);
                    let mid_floor = vdupq_n_f32(mid_floor);
                    let mid_scale = vdupq_n_f32(mid_scale);
                    let kcr = vdupq_n_f32(kcr);
                    let kcb = vdupq_n_f32(kcb);
                    let k1 = vdupq_n_f32(k1);
                    let k2 = vdupq_n_f32(k2);
                    let maximum = vdupq_n_f32(((1u32 << c.precision) - 1) as f32);
                    let identity = c.coefficients.is_none();

                    // How far the row sits between its two chroma rows, and
                    // whether it sits between them at all: a plane that is
                    // not subsampled vertically reads one row, and the
                    // scalar path's weight is then exactly zero.
                    let weighted = rows.weight != 0.0;
                    let vertical = vdupq_n_f32(rows.weight);
                    let one_minus_vertical = vdupq_n_f32(1.0 - rows.weight);

                    // The taps of a block, which are the same vectors for
                    // every block of a pattern.
                    let direct = vld1q_u8(TAPS_DIRECT.as_ptr());
                    let left = vld1q_u8(TAPS_LEFT.as_ptr());
                    let mid = vld1q_u8(TAPS_MID.as_ptr());
                    let first = vld1q_u8(TAPS_FIRST.as_ptr());
                    let right = vld1q_u8(TAPS_RIGHT.as_ptr());
                    let weight_left = vld1q_f32(WEIGHT_LEFT.as_ptr());
                    let weight_centred = vld1q_f32(WEIGHT_CENTRED.as_ptr());
                    let one_minus_left = vsubq_f32(one, weight_left);
                    let one_minus_centred = vsubq_f32(one, weight_centred);

                    let luma = rows.luma.as_ptr().cast::<$sample>();
                    let cb_above = rows.cb_above.as_ptr().cast::<$sample>();
                    let cb_below = rows.cb_below.as_ptr().cast::<$sample>();
                    let cr_above = rows.cr_above.as_ptr().cast::<$sample>();
                    let cr_below = rows.cr_below.as_ptr().cast::<$sample>();
                    let at = out.as_mut_ptr();

                    let mut x = 0usize;
                    while x + LANES <= width {
                        let (base, first_tap, second_tap, weight, one_minus) = match pattern {
                            Pattern::Direct => (x, direct, direct, zero, one),
                            Pattern::Left => (x / 2, left, mid, weight_left, one_minus_left),
                            Pattern::Centred if x == 0 => {
                                (0, first, mid, weight_centred, one_minus_centred)
                            }
                            Pattern::Centred => {
                                (x / 2 - 1, mid, right, weight_centred, one_minus_centred)
                            }
                        };
                        // A block reads `LANES` samples from `base`, so it
                        // is only taken where those are inside the plane.
                        if base + LANES > chroma_width {
                            break;
                        }
                        let y = vmulq_f32(vsubq_f32($load(luma.add(x)), luma_floor), luma_scale);
                        let cb = interpolate(
                            $load(cb_above.add(base)),
                            first_tap,
                            second_tap,
                            weight,
                            one_minus,
                        );
                        let cb = if weighted {
                            let below = interpolate(
                                $load(cb_below.add(base)),
                                first_tap,
                                second_tap,
                                weight,
                                one_minus,
                            );
                            vaddq_f32(
                                vmulq_f32(cb, one_minus_vertical),
                                vmulq_f32(below, vertical),
                            )
                        } else {
                            cb
                        };
                        let cb = vmulq_f32(vsubq_f32(cb, mid_floor), mid_scale);
                        let cr = interpolate(
                            $load(cr_above.add(base)),
                            first_tap,
                            second_tap,
                            weight,
                            one_minus,
                        );
                        let cr = if weighted {
                            let below = interpolate(
                                $load(cr_below.add(base)),
                                first_tap,
                                second_tap,
                                weight,
                                one_minus,
                            );
                            vaddq_f32(
                                vmulq_f32(cr, one_minus_vertical),
                                vmulq_f32(below, vertical),
                            )
                        } else {
                            cr
                        };
                        let cr = vmulq_f32(vsubq_f32(cr, mid_floor), mid_scale);

                        let (r, g, b) = if identity {
                            (y, cb, cr)
                        } else {
                            (
                                vaddq_f32(y, vmulq_f32(kcr, cr)),
                                vsubq_f32(vsubq_f32(y, vmulq_f32(k1, cb)), vmulq_f32(k2, cr)),
                                vaddq_f32(y, vmulq_f32(kcb, cb)),
                            )
                        };
                        store(levels(r, zero, one, maximum, half), at.add(x));
                        store(levels(g, zero, one, maximum, half), at.add(width + x));
                        store(levels(b, zero, one, maximum, half), at.add(2 * width + x));
                        x += LANES;
                    }
                    0..x
                }
            }
        };
    }

    kernel!(row_u8, u8, four_u8);
    kernel!(row_u16, u16, four_u16);
}
