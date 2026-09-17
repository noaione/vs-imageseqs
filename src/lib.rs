mod color;
mod decoder;
mod error;
mod pixel;
mod source;

use source::ImageSequence;

vapoursynth4_rs::declare_plugin!(
    c"xyz.n4o.imgseqs",
    c"imgseqs",
    c"Rust-based image sequence reader",
    (0, 1),
    vapoursynth4_rs::VAPOURSYNTH_API_VERSION,
    0,
    (ImageSequence, None)
);
