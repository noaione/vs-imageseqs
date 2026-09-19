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
//! - `decode(info)` — decode it into the interleaved buffer the frame writer
//!   expects, with the same timings and the same consistency checks as the
//!   `image` path

pub mod heif;
pub mod jxl;
pub mod webp;
