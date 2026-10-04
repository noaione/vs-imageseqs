//! The decoded layouts, source descriptions and orientations the plugin names.
//!
//! These used to be `image` crate types, and two of them are public behavior
//! rather than internal bookkeeping: `ColorType` is part of [`Pixels`], and the
//! spelling [`SourceColorType::label`] writes is the `ImgSeqOriginalColorType`
//! frame property. The variants and the label of each one are therefore kept
//! exactly as the `image` crate named them, because a reader that replaced the
//! crate must not rename a property a graph can already see. The eight
//! orientations keep the exif codes they always carried.
//!
//! [`Pixels`]: crate::decoder::Pixels

/// How the channels of one decoded buffer are stored.
///
/// This is the layout of the buffer a decoder hands over, which is not the
/// format the frame is written as: `RGBA16` is four sixteen-bit words while the
/// colour clip is three planar channels, and the alpha channel sits inside the
/// buffer rather than in a plane of its own.
/// Its helpers are used by the format adapters and the frame writer as they
/// migrate off `image`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ColorType {
    /// One eight-bit gray sample per pixel.
    L8,
    /// An eight-bit gray sample and an eight-bit alpha sample.
    La8,
    /// Three eight-bit samples per pixel.
    Rgb8,
    /// Three eight-bit samples and an eight-bit alpha sample.
    Rgba8,
    /// One sixteen-bit gray sample per pixel.
    L16,
    /// A sixteen-bit gray sample and a sixteen-bit alpha sample.
    La16,
    /// Three sixteen-bit samples per pixel.
    Rgb16,
    /// Three sixteen-bit samples and a sixteen-bit alpha sample.
    Rgba16,
    /// Three `f32` samples per pixel.
    Rgb32F,
    /// Three `f32` samples and an `f32` alpha sample.
    Rgba32F,
}

#[allow(dead_code, reason = "used by the adapters that are still migrating")]
impl ColorType {
    /// Bytes one pixel of this layout occupies.
    #[must_use]
    pub const fn bytes_per_pixel(self) -> usize {
        let word = self.bytes_per_sample();
        self.channels() * word
    }

    /// Bytes one sample of this layout occupies.
    #[must_use]
    pub const fn bytes_per_sample(self) -> usize {
        match self {
            Self::L8 | Self::La8 | Self::Rgb8 | Self::Rgba8 => 1,
            Self::L16 | Self::La16 | Self::Rgb16 | Self::Rgba16 => 2,
            Self::Rgb32F | Self::Rgba32F => 4,
        }
    }

    /// Channels one pixel of this layout holds.
    #[must_use]
    pub const fn channels(self) -> usize {
        match self {
            Self::L8 | Self::L16 => 1,
            Self::La8 | Self::La16 => 2,
            Self::Rgb8 | Self::Rgb16 | Self::Rgb32F => 3,
            Self::Rgba8 | Self::Rgba16 | Self::Rgba32F => 4,
        }
    }

    /// Where this layout keeps an alpha channel, for a layout that has one.
    #[must_use]
    pub const fn alpha_channel(self) -> Option<usize> {
        match self {
            Self::La8 | Self::La16 => Some(1),
            Self::Rgba8 | Self::Rgba16 | Self::Rgba32F => Some(3),
            Self::L8 | Self::L16 | Self::Rgb8 | Self::Rgb16 | Self::Rgb32F => None,
        }
    }

    /// The same picture laid out in sixteen-bit words.
    ///
    /// An APNG composes its frames at the depth its own chunks state, which is
    /// one step wider than the eight-bit canvas the first frame may have
    /// suggested.
    #[must_use]
    #[allow(dead_code, reason = "the apng compositor widens a canvas it composes")]
    pub const fn wide(self) -> Self {
        match self {
            Self::L8 => Self::L16,
            Self::La8 => Self::La16,
            Self::Rgb8 => Self::Rgb16,
            Self::Rgba8 | Self::L16 | Self::La16 | Self::Rgb16 | Self::Rgba16 => Self::Rgba16,
            Self::Rgb32F | Self::Rgba32F => self,
        }
    }

    /// Whether a pixel of this layout carries an alpha channel.
    #[must_use]
    pub const fn has_alpha(self) -> bool {
        self.alpha_channel().is_some()
    }
}

/// The encoding a file holds, which is not always the layout it decodes to.
///
/// A palette png, a one-bit fax and a CMYK jpeg all decode to something else,
/// and the property that reports this one is a compatibility label rather than
/// a description of any buffer. See [`Self::label`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SourceColorType {
    /// An eight-bit alpha sample alone.
    A8,
    /// One-bit luminance.
    L1,
    /// One-bit luminance with alpha.
    La1,
    /// Three five-bit channels packed into sixteen bits.
    Rgb5x1,
    /// Eight-bit luminance.
    L8,
    /// Eight-bit luminance with alpha.
    La8,
    /// Eight-bit rgb.
    Rgb8,
    /// Eight-bit rgba.
    Rgba8,
    /// Sixteen-bit luminance.
    L16,
    /// Sixteen-bit luminance with alpha.
    La16,
    /// Sixteen-bit rgb.
    Rgb16,
    /// Sixteen-bit rgba.
    Rgba16,
    /// Three `f32` channels.
    Rgb32F,
    /// Three `f32` channels and an `f32` alpha channel.
    Rgba32F,
    /// Eight-bit cmyk.
    Cmyk8,
}

impl SourceColorType {
    /// The name `ImgSeqOriginalColorType` reports for this encoding.
    ///
    /// The spelling is the one this property has always reported, which is the
    /// name the `image` crate's own `Debug` wrote while it was the reader. It is
    /// public behavior: a graph that matched on `Rgb8` has to keep matching on
    /// it. It is written out rather than derived so that renaming a variant here
    /// cannot silently rename a frame property, which is why `color.rs` writes
    /// the property out of this table rather than out of `Debug`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::A8 => "A8",
            Self::L1 => "L1",
            Self::La1 => "La1",
            Self::Rgb5x1 => "Rgb5x1",
            Self::L8 => "L8",
            Self::La8 => "La8",
            Self::Rgb8 => "Rgb8",
            Self::Rgba8 => "Rgba8",
            Self::L16 => "L16",
            Self::La16 => "La16",
            Self::Rgb16 => "Rgb16",
            Self::Rgba16 => "Rgba16",
            Self::Rgb32F => "Rgb32F",
            Self::Rgba32F => "Rgba32F",
            Self::Cmyk8 => "Cmyk8",
        }
    }

    /// The label for the encoding a decoded layout came from.
    ///
    /// This is what a backend that reports its source encoding as a decoded
    /// layout uses, and it is what the nine labels a container stating no
    /// palette or packed encoding arrives at.
    #[must_use]
    #[allow(
        dead_code,
        reason = "a backend that reports its source as a decoded layout"
    )]
    pub const fn from_color_type(color_type: ColorType) -> Self {
        match color_type {
            ColorType::L8 => Self::L8,
            ColorType::La8 => Self::La8,
            ColorType::Rgb8 => Self::Rgb8,
            ColorType::Rgba8 => Self::Rgba8,
            ColorType::L16 => Self::L16,
            ColorType::La16 => Self::La16,
            ColorType::Rgb16 => Self::Rgb16,
            ColorType::Rgba16 => Self::Rgba16,
            ColorType::Rgb32F => Self::Rgb32F,
            ColorType::Rgba32F => Self::Rgba32F,
        }
    }
}

impl From<ColorType> for SourceColorType {
    fn from(color_type: ColorType) -> Self {
        match color_type {
            ColorType::L8 => Self::L8,
            ColorType::La8 => Self::La8,
            ColorType::Rgb8 => Self::Rgb8,
            ColorType::Rgba8 => Self::Rgba8,
            ColorType::L16 => Self::L16,
            ColorType::La16 => Self::La16,
            ColorType::Rgb16 => Self::Rgb16,
            ColorType::Rgba16 => Self::Rgba16,
            ColorType::Rgb32F => Self::Rgb32F,
            ColorType::Rgba32F => Self::Rgba32F,
        }
    }
}

/// The eight orientations a file can describe itself with.
///
/// The variants carry the exif code in their name as well as their number, and
/// the two are kept together so that `ImgSeqOrientation` reports the code the
/// file stated whether or not the pixels were rearranged.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Orientation {
    /// The stored picture, unrotated.
    NoTransforms,
    /// A quarter turn clockwise.
    Rotate90,
    /// A half turn.
    Rotate180,
    /// A quarter turn counter-clockwise.
    Rotate270,
    /// Mirrored left to right.
    FlipHorizontal,
    /// Mirrored top to bottom.
    FlipVertical,
    /// A quarter turn clockwise over a left-to-right mirror.
    Rotate90FlipH,
    /// A quarter turn counter-clockwise over a left-to-right mirror.
    Rotate270FlipH,
}

impl Orientation {
    /// The orientation an exif tag states, for a code that names one.
    #[must_use]
    #[allow(dead_code, reason = "the exif and jxl readers map codes in")]
    pub const fn from_exif(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::NoTransforms),
            2 => Some(Self::FlipHorizontal),
            3 => Some(Self::Rotate180),
            4 => Some(Self::FlipVertical),
            5 => Some(Self::Rotate90FlipH),
            6 => Some(Self::Rotate90),
            7 => Some(Self::Rotate270FlipH),
            8 => Some(Self::Rotate270),
            _ => None,
        }
    }

    /// The exif code this orientation is stated as.
    #[must_use]
    pub const fn to_exif(self) -> u8 {
        match self {
            Self::NoTransforms => 1,
            Self::FlipHorizontal => 2,
            Self::Rotate180 => 3,
            Self::FlipVertical => 4,
            Self::Rotate90FlipH => 5,
            Self::Rotate90 => 6,
            Self::Rotate270FlipH => 7,
            Self::Rotate270 => 8,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ColorType, Orientation, SourceColorType};

    /// The property labels are public behavior, so they are pinned here rather
    /// than left to a derived name.
    #[test]
    fn every_source_label_is_the_one_the_property_always_wrote() {
        let labels = [
            (SourceColorType::A8, "A8"),
            (SourceColorType::L1, "L1"),
            (SourceColorType::La1, "La1"),
            (SourceColorType::Rgb5x1, "Rgb5x1"),
            (SourceColorType::L8, "L8"),
            (SourceColorType::La8, "La8"),
            (SourceColorType::Rgb8, "Rgb8"),
            (SourceColorType::Rgba8, "Rgba8"),
            (SourceColorType::L16, "L16"),
            (SourceColorType::La16, "La16"),
            (SourceColorType::Rgb16, "Rgb16"),
            (SourceColorType::Rgba16, "Rgba16"),
            (SourceColorType::Rgb32F, "Rgb32F"),
            (SourceColorType::Rgba32F, "Rgba32F"),
            (SourceColorType::Cmyk8, "Cmyk8"),
        ];
        assert_eq!(labels.len(), 15);
        for (value, label) in labels {
            assert_eq!(value.label(), label);
        }
    }

    /// A decoded layout always describes itself the way its own name reads.
    #[test]
    fn a_decoded_layout_describes_its_channels() {
        assert_eq!(ColorType::L8.bytes_per_pixel(), 1);
        assert_eq!(ColorType::La8.bytes_per_pixel(), 2);
        assert_eq!(ColorType::Rgb8.bytes_per_pixel(), 3);
        assert_eq!(ColorType::Rgba8.bytes_per_pixel(), 4);
        assert_eq!(ColorType::L16.bytes_per_pixel(), 2);
        assert_eq!(ColorType::Rgb16.bytes_per_pixel(), 6);
        assert_eq!(ColorType::Rgba32F.bytes_per_pixel(), 16);
        assert_eq!(ColorType::L8.alpha_channel(), None);
        assert_eq!(ColorType::La16.alpha_channel(), Some(1));
        assert_eq!(ColorType::Rgb8.alpha_channel(), None);
        assert_eq!(ColorType::Rgba8.alpha_channel(), Some(3));
        assert_eq!(ColorType::Rgba32F.alpha_channel(), Some(3));
        assert!(!ColorType::Rgb16.has_alpha());
        assert!(ColorType::Rgba16.has_alpha());
    }

    /// The eight exif codes and the eight orientations are the same eight in
    /// both directions, which is what lets the property keep reporting a code
    /// when the pixels were not rearranged.
    #[test]
    fn every_orientation_round_trips_through_its_exif_code() {
        for code in 1..=8u8 {
            let orientation = Orientation::from_exif(code).expect("a known exif code");
            assert_eq!(orientation.to_exif(), code);
        }
        assert_eq!(Orientation::from_exif(0), None);
        assert_eq!(Orientation::from_exif(9), None);
        assert_eq!(Orientation::from_exif(255), None);
        assert_eq!(Orientation::from_exif(6), Some(Orientation::Rotate90));
    }
}
