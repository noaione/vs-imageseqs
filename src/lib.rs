mod animation;
mod clip;
mod color;
mod decoder;
mod error;
mod exif;
mod format;
mod formats;
mod layout;
mod pixel;
mod prefetch;
mod source;

use source::{Read, ReadAlpha};

vapoursynth4_rs::declare_plugin!(
    c"xyz.n4o.imgseqs",
    c"imgseqs",
    c"Rust-based image sequence reader",
    (0, 2), // Version
    vapoursynth4_rs::VAPOURSYNTH_API_VERSION,
    0,
    (Read, None),
    (ReadAlpha, None)
);
