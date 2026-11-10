//! The one pass that takes an interleaved row apart into the planes it belongs
//! in.
//!
//! A decoder hands out a picture as the container stored it -- three or four
//! samples a pixel, side by side -- and a frame holds one plane a channel. The
//! walk between them is the same loop in every reader that buffers a picture
//! (`src/formats/png.rs`'s channel walk, `src/decoder.rs`'s row sinks,
//! `src/pixel.rs`'s `write_channel`), and the shape decides whether it is
//! cheap: reading the row once and writing every channel is one pass, while
//! reading it once per channel is a stride-`C` gather that reads every byte
//! three or four times.
//!
//! Two functions cover the shapes the tree writes, chosen by a channel count
//! and a sample width:
//!
//! * [`place8`] takes an eight bit row apart.
//! * [`place16be`] does it for sixteen bit big-endian samples and folds the
//!   swap into the store, which is what a sixteen bit png and a farbfeld hand
//!   over.
//!
//! Each writes only the leading `planes.len()` channels of the source, because
//! a call that hands out no alpha clip reads the fourth channel and drops it.
//!
//! The scalar walk is one pass and a plane count at a time: a zip over the
//! source's pixels and the plane rows it fills. That is the shape LLVM
//! vectorises on its own -- for three and four channels it emits the same
//! `ld3`/`ld4` a hand kernel would, in a tighter loop -- which is why the
//! aarch64 kernel below is only used for the shapes where it wins, and the
//! measurement is in `docs/improvements/48-decoder-vectorisation.md`:
//!
//! * eight bits, two channels: the kernel is 0.028 ns a pixel against the
//!   walk's 0.041, so it runs.
//! * eight bits, three and four channels: the kernel's own block is 0.101 and
//!   0.086 against the walk's 0.060 and 0.080, so the walk runs and the
//!   kernel is only reached by name from the test.
//! * sixteen bits, big endian: the compiler builds each sample from two
//!   indexed bytes and copies it, so the swap defeats its vectoriser and the
//!   kernel is 1.3x to 4.3x the walk. It runs for every channel count.
//!
//! The dispatch is a `#[cfg]` because NEON is in the aarch64 baseline: one
//! library carries the kernel, nothing is probed, and every other target runs
//! the scalar walk.
//!
//! Both are held to a naive per-channel gather, sample for sample, by the test
//! at the bottom of this file; the kernel is also reachable by name so the test
//! runs it even where the dispatch on this machine would not pick it.

/// Writes `C` interleaved eight bit channels into the leading planes.
///
/// `planes` holds one row of every plane the call hands out, in channel order,
/// and may be shorter than `C`: a channel with no plane is read and dropped.
/// Every plane row is one sample a pixel, so the row is as long as the first.
pub fn place8<const C: usize>(source: &[u8], planes: &mut [&mut [u8]]) {
    debug_assert!((1..=4).contains(&C), "a pixel has at most four channels");
    debug_assert!(planes.len() <= C, "more planes than the row has channels");
    #[cfg(target_arch = "aarch64")]
    if C == 2 {
        // SAFETY: the kernel reads `pixels * 2` bytes from `source`, which the
        // caller sized to the row's own channel count, and writes `pixels`
        // bytes to each plane, which it sized to the row's sample count.
        let done = unsafe { neon::place8::<2>(source, planes) };
        tail8::<C>(source, planes, done);
        return;
    }
    scalar8::<C>(source, planes);
}

/// The eight bit one pass, one plane count at a time.
///
/// A zip per plane count rather than a loop over the planes: the count is
/// small but it is a runtime value, and a zip gives the compiler a fixed
/// number of destination streams to fuse into one `st1` a block.
fn scalar8<const C: usize>(source: &[u8], planes: &mut [&mut [u8]]) {
    let pixels = source.as_chunks::<C>().0.iter();
    match planes.len() {
        0 => {}
        1 => {
            let (first, _) = planes.split_first_mut().unwrap();
            for (pixel, slot) in pixels.zip(first.iter_mut()) {
                *slot = pixel[0];
            }
        }
        2 => {
            let (first, rest) = planes.split_first_mut().unwrap();
            let (second, _) = rest.split_first_mut().unwrap();
            let targets = first.iter_mut().zip(second.iter_mut());
            for (pixel, (a, b)) in pixels.zip(targets) {
                *a = pixel[0];
                *b = pixel[1];
            }
        }
        3 => {
            let (first, rest) = planes.split_first_mut().unwrap();
            let (second, rest) = rest.split_first_mut().unwrap();
            let (third, _) = rest.split_first_mut().unwrap();
            let targets = first
                .iter_mut()
                .zip(second.iter_mut())
                .zip(third.iter_mut());
            for (pixel, ((a, b), c)) in pixels.zip(targets) {
                *a = pixel[0];
                *b = pixel[1];
                *c = pixel[2];
            }
        }
        4 => {
            let (first, rest) = planes.split_first_mut().unwrap();
            let (second, rest) = rest.split_first_mut().unwrap();
            let (third, rest) = rest.split_first_mut().unwrap();
            let (fourth, _) = rest.split_first_mut().unwrap();
            let targets = first
                .iter_mut()
                .zip(second.iter_mut())
                .zip(third.iter_mut())
                .zip(fourth.iter_mut());
            for (pixel, (((a, b), c), d)) in pixels.zip(targets) {
                *a = pixel[0];
                *b = pixel[1];
                *c = pixel[2];
                *d = pixel[3];
            }
        }
        _ => unreachable!("a pixel has at most four channels"),
    }
}

/// Writes `C` interleaved sixteen bit native-order channels into the leading
/// planes.
///
/// The samples are already in the machine's byte order, which is what a
/// buffered decoder hands over, so this is the eight bit walk with two byte
/// samples and no swap to fold in. A hand block does not win here: the swap is
/// what made the big-endian kernel worthwhile, and the one pass is 1.5x to 1.6x
/// the per channel gather it replaces.
pub fn place16<const C: usize>(source: &[u8], planes: &mut [&mut [u8]]) {
    debug_assert!((1..=4).contains(&C), "a pixel has at most four channels");
    debug_assert!(planes.len() <= C, "more planes than the row has channels");
    scalar16::<C>(source, planes);
}

/// The sixteen bit walk, a zip per plane count the way the eight bit one is.
fn scalar16<const C: usize>(source: &[u8], planes: &mut [&mut [u8]]) {
    let pixels = source.chunks_exact(C * 2);
    match planes.len() {
        0 => {}
        1 => {
            let (first, _) = planes.split_first_mut().unwrap();
            let targets = first.as_chunks_mut::<2>().0.iter_mut();
            for (pixel, a) in pixels.zip(targets) {
                a.copy_from_slice(&pixel[..2]);
            }
        }
        2 => {
            let (first, rest) = planes.split_first_mut().unwrap();
            let (second, _) = rest.split_first_mut().unwrap();
            let targets = first
                .as_chunks_mut::<2>()
                .0
                .iter_mut()
                .zip(second.as_chunks_mut::<2>().0.iter_mut());
            for (pixel, (a, b)) in pixels.zip(targets) {
                a.copy_from_slice(&pixel[..2]);
                b.copy_from_slice(&pixel[2..4]);
            }
        }
        3 => {
            let (first, rest) = planes.split_first_mut().unwrap();
            let (second, rest) = rest.split_first_mut().unwrap();
            let (third, _) = rest.split_first_mut().unwrap();
            let targets = first
                .as_chunks_mut::<2>()
                .0
                .iter_mut()
                .zip(second.as_chunks_mut::<2>().0.iter_mut())
                .zip(third.as_chunks_mut::<2>().0.iter_mut());
            for (pixel, ((a, b), c)) in pixels.zip(targets) {
                a.copy_from_slice(&pixel[..2]);
                b.copy_from_slice(&pixel[2..4]);
                c.copy_from_slice(&pixel[4..6]);
            }
        }
        4 => {
            let (first, rest) = planes.split_first_mut().unwrap();
            let (second, rest) = rest.split_first_mut().unwrap();
            let (third, rest) = rest.split_first_mut().unwrap();
            let (fourth, _) = rest.split_first_mut().unwrap();
            let targets = first
                .as_chunks_mut::<2>()
                .0
                .iter_mut()
                .zip(second.as_chunks_mut::<2>().0.iter_mut())
                .zip(third.as_chunks_mut::<2>().0.iter_mut())
                .zip(fourth.as_chunks_mut::<2>().0.iter_mut());
            for (pixel, (((a, b), c), d)) in pixels.zip(targets) {
                a.copy_from_slice(&pixel[..2]);
                b.copy_from_slice(&pixel[2..4]);
                c.copy_from_slice(&pixel[4..6]);
                d.copy_from_slice(&pixel[6..8]);
            }
        }
        _ => unreachable!("a pixel has at most four channels"),
    }
}

/// Writes `C` interleaved sixteen bit big-endian channels into the leading
/// planes, swapping each sample into the machine's byte order as it goes.
///
/// A frame holds a sample the way the machine reads it, so a big-endian source
/// is one swap a sample. The swap rides on the same pass that takes the pixel
/// apart rather than running over the picture afterwards, and on aarch64 it is
/// part of the vector kernel: `ld4` gives four planes of eight samples and
/// `rev16` puts every word the right way round before the store.
pub fn place16be<const C: usize>(source: &[u8], planes: &mut [&mut [u8]]) {
    debug_assert!((1..=4).contains(&C), "a pixel has at most four channels");
    debug_assert!(planes.len() <= C, "more planes than the row has channels");
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: the kernel reads eight pixels of `C` two-byte samples from
        // `source` and writes eight words to each plane, both of which the
        // caller sized for the row.
        let done = unsafe { neon::place16be::<C>(source, planes) };
        tail16be::<C>(source, planes, done);
    }
    #[cfg(not(target_arch = "aarch64"))]
    scalar16be::<C>(source, planes);
}

/// The scalar sixteen bit big-endian walk a machine without the kernel runs.
#[cfg(not(target_arch = "aarch64"))]
fn scalar16be<const C: usize>(source: &[u8], planes: &mut [&mut [u8]]) {
    for (index, pixel) in source.chunks_exact(C * 2).enumerate() {
        for (channel, plane) in planes.iter_mut().enumerate() {
            let at = channel * 2;
            plane[index * 2] = pixel[at + 1];
            plane[index * 2 + 1] = pixel[at];
        }
    }
}

/// The pixels a block of 16 could not cover, eight bit.
#[cfg(target_arch = "aarch64")]
#[inline]
fn tail8<const C: usize>(source: &[u8], planes: &mut [&mut [u8]], done: usize) {
    let pixels = row_pixels8(planes);
    for index in done..pixels {
        for (channel, plane) in planes.iter_mut().enumerate() {
            plane[index] = source[index * C + channel];
        }
    }
}

/// The pixels a block of 8 could not cover, sixteen bit big endian.
#[cfg(target_arch = "aarch64")]
#[inline]
fn tail16be<const C: usize>(source: &[u8], planes: &mut [&mut [u8]], done: usize) {
    let pixels = row_pixels16(planes);
    for index in done..pixels {
        for (channel, plane) in planes.iter_mut().enumerate() {
            let at = (index * C + channel) * 2;
            plane[index * 2] = source[at + 1];
            plane[index * 2 + 1] = source[at];
        }
    }
}

/// Samples in one row of an eight bit plane.
#[cfg(target_arch = "aarch64")]
#[inline]
fn row_pixels8(planes: &[&mut [u8]]) -> usize {
    planes.first().map_or(0, |plane| plane.len())
}

/// Samples in one row of a sixteen bit plane.
#[cfg(target_arch = "aarch64")]
#[inline]
fn row_pixels16(planes: &[&mut [u8]]) -> usize {
    planes.first().map_or(0, |plane| plane.len() / 2)
}

#[cfg(target_arch = "aarch64")]
mod neon {
    use std::arch::aarch64::*;

    use super::{row_pixels8, row_pixels16};

    /// Sixteen pixels a block: one `ld1` to `ld4` gives the channels and a
    /// store each puts them down.
    ///
    /// A 128 bit register holds sixteen eight bit samples, so a block is
    /// sixteen pixels whatever the channel count. The plane count may be
    /// shorter than the channel count, which is a colour-only read of a file
    /// with alpha: the load reads every channel and the store skips the ones a
    /// call does not hand out.
    ///
    /// # Safety
    ///
    /// `source` holds `pixels * C` bytes and every plane holds `pixels` bytes,
    /// where `pixels` is a row of `planes[0]`.
    pub(super) unsafe fn place8<const C: usize>(source: &[u8], planes: &mut [&mut [u8]]) -> usize {
        debug_assert!((1..=4).contains(&C));
        debug_assert!(planes.len() <= C);
        let pixels = row_pixels8(planes);
        let mut index = 0usize;
        while index + 16 <= pixels {
            let at = unsafe { source.as_ptr().add(index * C) };
            // SAFETY: the block reads `16 * C` bytes from the row and writes
            // sixteen bytes to every plane the caller sized to `pixels`.
            unsafe {
                match C {
                    1 => store8(planes, 0, index, vld1q_u8(at)),
                    2 => {
                        let pixel = vld2q_u8(at);
                        store8(planes, 0, index, pixel.0);
                        store8(planes, 1, index, pixel.1);
                    }
                    3 => {
                        let pixel = vld3q_u8(at);
                        store8(planes, 0, index, pixel.0);
                        store8(planes, 1, index, pixel.1);
                        store8(planes, 2, index, pixel.2);
                    }
                    4 => {
                        let pixel = vld4q_u8(at);
                        store8(planes, 0, index, pixel.0);
                        store8(planes, 1, index, pixel.1);
                        store8(planes, 2, index, pixel.2);
                        store8(planes, 3, index, pixel.3);
                    }
                    _ => unreachable!(),
                }
            }
            index += 16;
        }
        index
    }

    /// Eight pixels a block, with each sample swapped into the machine's byte
    /// order.
    ///
    /// A 128 bit register holds eight sixteen bit samples, so the block is half
    /// as wide as the eight bit one. `rev16` reverses the bytes of every word
    /// in the register, which is the whole swap.
    ///
    /// # Safety
    ///
    /// `source` holds `pixels * C * 2` bytes and every plane holds
    /// `pixels * 2` bytes, where `pixels` is a row of `planes[0]`.
    pub(super) unsafe fn place16be<const C: usize>(
        source: &[u8],
        planes: &mut [&mut [u8]],
    ) -> usize {
        debug_assert!((1..=4).contains(&C));
        debug_assert!(planes.len() <= C);
        let pixels = row_pixels16(planes);
        let mut index = 0usize;
        while index + 8 <= pixels {
            let at = unsafe { source.as_ptr().add(index * C * 2).cast() };
            // SAFETY: the block reads `8 * C * 2` bytes from the row and writes
            // sixteen bytes to every plane the caller sized to `pixels`.
            unsafe {
                match C {
                    1 => store16be(planes, 0, index, vld1q_u16(at)),
                    2 => {
                        let pixel = vld2q_u16(at);
                        store16be(planes, 0, index, pixel.0);
                        store16be(planes, 1, index, pixel.1);
                    }
                    3 => {
                        let pixel = vld3q_u16(at);
                        store16be(planes, 0, index, pixel.0);
                        store16be(planes, 1, index, pixel.1);
                        store16be(planes, 2, index, pixel.2);
                    }
                    4 => {
                        let pixel = vld4q_u16(at);
                        store16be(planes, 0, index, pixel.0);
                        store16be(planes, 1, index, pixel.1);
                        store16be(planes, 2, index, pixel.2);
                        store16be(planes, 3, index, pixel.3);
                    }
                    _ => unreachable!(),
                }
            }
            index += 8;
        }
        index
    }

    /// Stores one channel's sixteen samples at `index`, skipping a channel the
    /// call hands out no plane for.
    ///
    /// # Safety
    ///
    /// `planes[channel]` holds sixteen bytes from `index`, or there is no such
    /// plane.
    #[inline]
    unsafe fn store8(planes: &mut [&mut [u8]], channel: usize, index: usize, value: uint8x16_t) {
        if let Some(plane) = planes.get_mut(channel) {
            unsafe {
                vst1q_u8(plane.as_mut_ptr().add(index), value);
            }
        }
    }

    /// As [`store8`], for eight words with the swap applied.
    ///
    /// # Safety
    ///
    /// `planes[channel]` holds sixteen bytes from `index * 2`, or there is no
    /// such plane.
    #[inline]
    unsafe fn store16be(planes: &mut [&mut [u8]], channel: usize, index: usize, value: uint16x8_t) {
        if let Some(plane) = planes.get_mut(channel) {
            unsafe {
                vst1q_u8(
                    plane.as_mut_ptr().add(index * 2),
                    vrev16q_u8(vreinterpretq_u8_u16(value)),
                );
            }
        }
    }
}

/// One kernel by name, which is what this module's test uses to run the kernel
/// the dispatch on this machine would not pick.
#[cfg(all(test, target_arch = "aarch64"))]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kernel {
    Neon,
}

/// Every kernel this architecture can be asked to run by name.
#[cfg(all(test, target_arch = "aarch64"))]
pub const KERNELS: &[(&str, Kernel)] = &[("neon", Kernel::Neon)];

/// Runs the named eight bit kernel over the whole row, dispatching nothing.
#[cfg(all(test, target_arch = "aarch64"))]
pub fn place8_forced<const C: usize>(source: &[u8], planes: &mut [&mut [u8]], kernel: Kernel) {
    let Kernel::Neon = kernel;
    // SAFETY: the test sizes `source` and every plane row the way `place8`
    // documents, and this is the only machine the kernel is built for.
    let done = unsafe { neon::place8::<C>(source, planes) };
    tail8::<C>(source, planes, done);
}

/// Runs the named sixteen bit big-endian kernel over the whole row.
#[cfg(all(test, target_arch = "aarch64"))]
pub fn place16be_forced<const C: usize>(source: &[u8], planes: &mut [&mut [u8]], kernel: Kernel) {
    let Kernel::Neon = kernel;
    // SAFETY: as above.
    let done = unsafe { neon::place16be::<C>(source, planes) };
    tail16be::<C>(source, planes, done);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The naive shape both walks replace: one plane at a time, one sample at
    /// a time. Independent of either, so a shared mistake cannot hide.
    fn reference<const C: usize>(source: &[u8], planes: &mut [&mut [u8]], kind: Kind) {
        let sample_bytes = kind.sample_bytes();
        let pixels = planes.first().map_or(0, |plane| plane.len() / sample_bytes);
        for (channel, plane) in planes.iter_mut().enumerate() {
            for index in 0..pixels {
                let at = (index * C + channel) * sample_bytes;
                match kind {
                    Kind::Eight => plane[index] = source[at],
                    Kind::SixteenBe => {
                        plane[index * 2] = source[at + 1];
                        plane[index * 2 + 1] = source[at];
                    }
                    Kind::Sixteen => {
                        plane[index * 2] = source[at];
                        plane[index * 2 + 1] = source[at + 1];
                    }
                }
            }
        }
    }

    /// The three walks a row can take: eight bit, sixteen bit big endian and
    /// sixteen bit native.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Kind {
        Eight,
        SixteenBe,
        Sixteen,
    }

    impl Kind {
        fn sample_bytes(self) -> usize {
            match self {
                Self::Eight => 1,
                Self::SixteenBe | Self::Sixteen => 2,
            }
        }

        fn label(self) -> &'static str {
            match self {
                Self::Eight => "eight bit",
                Self::SixteenBe => "sixteen bit big endian",
                Self::Sixteen => "sixteen bit native",
            }
        }

        fn place<const C: usize>(self, source: &[u8], planes: &mut [&mut [u8]]) {
            match self {
                Self::Eight => place8::<C>(source, planes),
                Self::SixteenBe => place16be::<C>(source, planes),
                Self::Sixteen => place16::<C>(source, planes),
            }
        }
    }

    /// A row long enough to exercise every block and every tail.
    fn lengths() -> Vec<usize> {
        (0..=40).chain([64, 127, 128, 1029]).collect()
    }

    /// Runs every walk and every named kernel for one shape and compares them
    /// with the reference.
    fn check<const C: usize>(kind: Kind, pixels: usize) {
        let sample_bytes = kind.sample_bytes();
        let source: Vec<u8> = (0..pixels * C * sample_bytes)
            .map(|index| (index % 251) as u8)
            .collect();
        let row = pixels * sample_bytes;
        for count in 0..=C {
            let mut expected = vec![vec![0u8; row]; count];
            {
                let mut planes: Vec<&mut [u8]> = expected
                    .iter_mut()
                    .map(|plane| plane.as_mut_slice())
                    .collect();
                reference::<C>(&source, &mut planes, kind);
            }
            let mut actual = vec![vec![0u8; row]; count];
            let path = format!(
                "{C} channels, {pixels} pixels, {count} planes, {}",
                kind.label()
            );
            {
                let mut planes: Vec<&mut [u8]> = actual
                    .iter_mut()
                    .map(|plane| plane.as_mut_slice())
                    .collect();
                kind.place::<C>(&source, &mut planes);
            }
            assert!(actual[..count] == expected[..count], "public path: {path}");

            #[cfg(target_arch = "aarch64")]
            if matches!(kind, Kind::Eight | Kind::SixteenBe) {
                for (name, kernel) in KERNELS {
                    let mut forced = vec![vec![0u8; row]; count];
                    let mut planes: Vec<&mut [u8]> = forced
                        .iter_mut()
                        .map(|plane| plane.as_mut_slice())
                        .collect();
                    match kind {
                        Kind::Eight => place8_forced::<C>(&source, &mut planes, *kernel),
                        Kind::SixteenBe => place16be_forced::<C>(&source, &mut planes, *kernel),
                        Kind::Sixteen => unreachable!("no named kernel for native samples"),
                    }
                    assert!(
                        forced[..count] == expected[..count],
                        "{name} kernel: {path}"
                    );
                }
            }
        }
    }

    /// Every shape the tree writes, every row length that has a tail, every
    /// plane count a call can hand out, against the naive walk.
    #[test]
    fn a_walk_is_the_naive_gather_sample_for_sample() {
        for pixels in lengths() {
            for kind in [Kind::Eight, Kind::SixteenBe, Kind::Sixteen] {
                check::<1>(kind, pixels);
                check::<2>(kind, pixels);
                check::<3>(kind, pixels);
                check::<4>(kind, pixels);
            }
        }
    }

    /// The kernel must actually take blocks, or the test above proves nothing
    /// about it: a kernel that covers no row is the scalar path compared with
    /// itself.
    #[cfg(target_arch = "aarch64")]
    #[test]
    fn a_kernel_covers_a_row_wide_enough_to_have_a_block() {
        for (name, kernel) in KERNELS {
            let source = vec![7u8; 17 * 2];
            let mut plane0 = vec![0u8; 17];
            let mut plane1 = vec![0u8; 17];
            {
                let mut planes: Vec<&mut [u8]> = vec![&mut plane0, &mut plane1];
                place8_forced::<2>(&source, &mut planes, *kernel);
            }
            assert!(
                plane0[..16]
                    .iter()
                    .chain(&plane1[..16])
                    .all(|byte| *byte == 7),
                "{name} filled no 16 pixel block"
            );

            let source = vec![7u8; 24 * 8];
            let mut plane0 = vec![0u8; 24 * 2];
            let mut plane1 = vec![0u8; 24 * 2];
            let mut plane2 = vec![0u8; 24 * 2];
            {
                let mut planes: Vec<&mut [u8]> = vec![&mut plane0, &mut plane1, &mut plane2];
                place16be_forced::<3>(&source, &mut planes, *kernel);
            }
            assert!(
                plane0[..16]
                    .iter()
                    .chain(&plane1[..16])
                    .chain(&plane2[..16])
                    .all(|byte| *byte == 7),
                "{name} filled no 8 pixel block"
            );
        }
    }
}
