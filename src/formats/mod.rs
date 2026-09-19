//! Decoding paths that belong to one container or codec family.
//!
//! Everything else goes through the `image` crate and the decoder hooks
//! registered in [`decoder`](crate::decoder). A module here takes over a file
//! when the `image` integration cannot express what a container holds, or when
//! a native decoder the `image` path cannot reach does the job better, and is
//! selected by file extension.
//!
//! A module exposes two entry points for [`decoder::decode`](crate::decoder::decode):
//!
//! - `handles(info)` — whether this module owns that image
//! - `decode(info)` — decode it into the planes, or the interleaved buffer, the
//!   frame writer expects, with the same timings and the same consistency checks
//!   as the `image` path
//!
//! A module that can describe a file from its own container without decoding it
//! also exposes `image_info(path)` for the probe, and `output_format` for the
//! files it describes but leaves to the `image` path: [`heif`] answers both, and
//! [`avif`] answers them for a file the `image` decoder would have had to decode
//! whole before it could report a size.
//!
//! A module that only reads a container exposes neither, because the `image`
//! path decodes those files: [`png`] answers what a png states about the colour
//! of its samples and nothing else.

pub mod avif;
pub mod heif;
pub mod jp2;
pub mod jxl;
pub mod png;
pub mod webp;
