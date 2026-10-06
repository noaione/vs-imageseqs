//! What a sequence container states about its own timeline.
//!
//! An avif or heif image sequence stores its pictures in a visual track rather
//! than as items, and the timing that track states lives in the ISO base media
//! file format's sample tables. Two things are read here rather than asked of
//! `libheif`:
//!
//! - **the per-sample timing.** The embedded libheif 1.23.1 build reports the
//!   wrong duration for every avif sample: it looks the timing up by the sample
//!   being fed into the decoder instead of by the one being returned, and the
//!   two run apart. The `stts` box is where that timing is stated, and no
//!   published `libheif-sys` carries the fix, so the table is read here. The
//!   same table is also what says how many samples a track holds, which
//!   `libheif-rs 3.0.0` exposes no other way; see
//!   `docs/improvements/21-animated-images.md`.
//! - **the presentation crop.** A track's coded pictures can be larger than
//!   the picture it presents, and the `clap` clean-aperture box is what states
//!   the difference. `decode_next_image()` hands back the coded pixels, so the
//!   aperture has to be applied here.
//!
//! Every offset and length is checked against the file before it is read, for
//! the same reason [`crate::formats::avif`]'s item walker is: a malformed
//! container has to be refused rather than read past its own bounds.

use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
};

use crate::{
    decoder::image_error,
    error::{ImgSeqError, Result},
};

/// A box header: its kind and the range its payload covers.
#[derive(Clone, Copy, Debug)]
struct Box_ {
    kind: [u8; 4],
    /// Offset of the payload, which is past the header.
    start: usize,
    /// Offset one past the payload.
    end: usize,
}

impl Box_ {
    fn is(self, kind: &[u8; 4]) -> bool {
        &self.kind == kind
    }
}

/// Walks the boxes directly inside `[start, end)`.
///
/// A box whose declared size runs past its parent ends the walk, which is what
/// makes a truncated or malformed container yield the boxes before the damage
/// rather than a slice of somebody else's data.
fn boxes(data: &[u8], start: usize, end: usize) -> Vec<Box_> {
    let mut found = Vec::new();
    let mut at = start;
    while at + 8 <= end {
        let size = u32::from_be_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
        let kind = [data[at + 4], data[at + 5], data[at + 6], data[at + 7]];
        let (header, size) = match size {
            // A size of one means a 64 bit size follows the kind.
            1 => {
                if at + 16 > end {
                    break;
                }
                let mut wide = [0u8; 8];
                wide.copy_from_slice(&data[at + 8..at + 16]);
                (16usize, u64::from_be_bytes(wide))
            }
            // A size of zero means the box runs to the end of its parent.
            0 => (8usize, (end - at) as u64),
            size => (8usize, u64::from(size)),
        };
        let Ok(size) = usize::try_from(size) else {
            break;
        };
        if size < header || at.saturating_add(size) > end {
            break;
        }
        found.push(Box_ {
            kind,
            start: at + header,
            end: at + size,
        });
        at += size;
    }
    found
}

/// The first direct child box of `kind`.
fn child(data: &[u8], parent: Box_, kind: &[u8; 4]) -> Option<Box_> {
    boxes(data, parent.start, parent.end)
        .into_iter()
        .find(|candidate| candidate.is(kind))
}

/// Timing of one visual track, as its sample tables state it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrackTiming {
    /// Ticks a second for every duration below.
    pub timescale: u32,
    /// How long each sample is displayed, in timeline order.
    pub durations: Vec<u32>,
    /// Composition offsets, one per sample, or empty when the track states
    /// none.
    ///
    /// A sample's presentation time is its decode time plus its offset, and the
    /// offset may move it before zero: the caller clamps that rather than
    /// wrapping, because a version zero box states the offsets unsigned and
    /// reading one as the difference it stands for is what a subtraction gets
    /// wrong.
    pub offsets: Vec<i64>,
    /// The media tick an edit list ends the track at, or `None` for all of it.
    ///
    /// An edit list is where a file says which part of its media it displays;
    /// the shapes this reader follows are described on [`edit_list`].
    pub shown: Option<i64>,
}

impl TrackTiming {
    /// Number of samples the track holds.
    #[must_use]
    pub fn samples(&self) -> usize {
        self.durations.len()
    }
}

/// The clean aperture a track's samples state, when they state one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Crop {
    pub width: u32,
    pub height: u32,
    pub x: u32,
    pub y: u32,
}

/// What a sequence container states about its first visual track.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Sequence {
    pub timing: TrackTiming,
    /// The aperture the samples state, when they state one.
    pub crop: Option<Crop>,
    /// Size of the coded pictures before the aperture is applied.
    pub coded: Option<(u32, u32)>,
}

/// Reads what the first visual track of `path` states about its timeline.
///
/// Returns `None` for a file with no movie box, which is a still image rather
/// than a sequence.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file holds a movie box that cannot be read.
pub fn read(file: &mut BufReader<File>, path: &Path) -> Result<Option<Sequence>> {
    let length = file
        .seek(SeekFrom::End(0))
        .map_err(|error| image_error("read", path, error))?;
    // Only the box structure is wanted, and an `mdat` of coded pictures can be
    // most of the file and sit either side of the boxes that state a timeline.
    // `data` is as long as the file up to the limit, so every offset a box
    // states is the offset it has there, and what it holds is filled in from the
    // file rather than read whole; see [`fill_structure`].
    let wanted = length.min(METADATA_LIMIT);
    let mut data = vec![0u8; usize::try_from(wanted).unwrap_or(0)];
    fill_structure(file, &mut data, path)?;

    let Some(moov) = child(
        &data,
        Box_ {
            kind: *b"file",
            start: 0,
            end: data.len(),
        },
        b"moov",
    ) else {
        return Ok(None);
    };

    // The picture track is the one whose handler says so; an auxiliary track is
    // an alpha or depth plane of it and carries no timeline of its own that
    // this reader needs.
    for trak in boxes(&data, moov.start, moov.end)
        .into_iter()
        .filter(|candidate| candidate.is(b"trak"))
    {
        if !is_picture_track(&data, trak) {
            continue;
        }
        let Some(timing) = track_timing(&data, trak) else {
            continue;
        };
        // The edit list is a box beside the sample tables rather than in them,
        // and one this reader cannot replay is refused here rather than played
        // wrongly.
        let shown = edit_list(&data, trak, path)?;
        let crop = crop_of(&data, Some(trak));
        let coded = coded_size(&data);
        return Ok(Some(Sequence {
            timing: TrackTiming { shown, ..timing },
            crop,
            coded,
        }));
    }

    Ok(None)
}

/// Fills `data` with the box structure of a file and the payloads of the boxes a
/// description reads.
///
/// The walk below steps over a box by the size the box itself states, so a
/// payload left as zeros is never interpreted: `data` holds the header of every
/// top level box, and the payload of `moov` -- the timeline -- and of `meta` --
/// the coded size and the aperture. Everything else, an `mdat` of coded pictures
/// above all, is skipped by seeking past it, which is what keeps a description
/// from reading a file's pixels to find its own box list.
///
/// The sizes are read the way [`boxes`] reads them, including the two the format
/// spells differently: a size of one means a 64 bit size follows the kind, and a
/// size of zero means the box runs to the end of what is here. A box that states
/// a size that does not fit ends the walk, which leaves the same list [`boxes`]
/// builds from the same bytes.
///
/// # Errors
///
/// Returns [`ImgSeqError`] when the file cannot be read.
fn fill_structure(file: &mut BufReader<File>, data: &mut [u8], path: &Path) -> Result<()> {
    let end = u64::try_from(data.len()).unwrap_or(u64::MAX);
    let mut at = 0u64;
    while at + 8 <= end {
        let mut header = [0u8; 16];
        file.seek(SeekFrom::Start(at))
            .map_err(|error| image_error("read", path, error))?;
        file.read_exact(&mut header[..8])
            .map_err(|error| image_error("read", path, error))?;
        let kind = [header[4], header[5], header[6], header[7]];
        let size = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
        let (header_size, size) = match size {
            1 => {
                if at + 16 > end {
                    break;
                }
                file.read_exact(&mut header[8..16])
                    .map_err(|error| image_error("read", path, error))?;
                let mut wide = [0u8; 8];
                wide.copy_from_slice(&header[8..16]);
                (16u64, u64::from_be_bytes(wide))
            }
            0 => (8u64, end - at),
            size => (8u64, u64::from(size)),
        };
        if size < header_size || at + size > end {
            break;
        }
        let header_size = usize::try_from(header_size).unwrap_or(0);
        let start = usize::try_from(at).unwrap_or(0);
        data[start..start + header_size].copy_from_slice(&header[..header_size]);
        if &kind == b"moov" || &kind == b"meta" {
            let from = start + header_size;
            let payload = usize::try_from(size)
                .unwrap_or(0)
                .saturating_sub(header_size);
            file.seek(SeekFrom::Start(at + header_size as u64))
                .map_err(|error| image_error("read", path, error))?;
            file.read_exact(&mut data[from..from + payload])
                .map_err(|error| image_error("read", path, error))?;
        }
        at += size;
    }
    Ok(())
}

/// Whether a track's media handler says it holds pictures.
fn is_picture_track(data: &[u8], trak: Box_) -> bool {
    let Some(mdia) = child(data, trak, b"mdia") else {
        return false;
    };
    let Some(hdlr) = child(data, mdia, b"hdlr") else {
        return false;
    };
    // version and flags, then a pre-defined word, then the handler type.
    let payload = &data[hdlr.start..hdlr.end];
    if payload.len() < 12 {
        return false;
    }
    &payload[8..12] == b"pict"
}

/// The timing table of one track.
fn track_timing(data: &[u8], trak: Box_) -> Option<TrackTiming> {
    let mdia = child(data, trak, b"mdia")?;
    let mdhd = child(data, mdia, b"mdhd")?;
    let timescale = read_timescale(data, mdhd)?;
    let minf = child(data, mdia, b"minf")?;
    let stbl = child(data, minf, b"stbl")?;
    // A track that states no composition offsets is the common case, and every
    // sample is then presented where it was decoded.
    let offsets = match child(data, stbl, b"ctts") {
        Some(ctts) => read_ctts(data, ctts)?,
        None => Vec::new(),
    };
    let stts = child(data, stbl, b"stts")?;
    let durations = read_stts(data, stts)?;
    if timescale == 0 || durations.is_empty() {
        return None;
    }
    Some(TrackTiming {
        timescale,
        durations,
        offsets,
        // The edit list is a box beside the sample tables, and the caller
        // reads it: a track that states none shows all of its media.
        shown: None,
    })
}

/// The timescale a media header box states.
fn read_timescale(data: &[u8], mdhd: Box_) -> Option<u32> {
    let payload = &data[mdhd.start..mdhd.end];
    let version = *payload.first()?;
    // version, flags, then either two 32 bit times or two 64 bit ones, then the
    // timescale and the duration.
    let at = if version == 1 { 20 } else { 12 };
    let bytes = payload.get(at..at + 4)?;
    Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// The per-sample durations a time-to-sample box states.
///
/// The box is a table of runs: `count` samples of `delta` ticks each, repeated.
/// A run whose `count` is zero would make the expansion loop forever, so it is
/// refused rather than followed.
fn read_stts(data: &[u8], stts: Box_) -> Option<Vec<u32>> {
    let payload = &data[stts.start..stts.end];
    let count = u32::from_be_bytes([
        *payload.get(4)?,
        *payload.get(5)?,
        *payload.get(6)?,
        *payload.get(7)?,
    ]);
    let mut durations = Vec::new();
    let mut at = 8usize;
    for _ in 0..count {
        let entry = payload.get(at..at + 8)?;
        let samples = u32::from_be_bytes([entry[0], entry[1], entry[2], entry[3]]);
        let delta = u32::from_be_bytes([entry[4], entry[5], entry[6], entry[7]]);
        if samples == 0 {
            return None;
        }
        // A table cannot describe more samples than the file could hold, so the
        // expansion is bounded before it allocates.
        let total = durations.len().checked_add(samples as usize)?;
        if total > MAX_SAMPLES {
            return None;
        }
        durations.extend(std::iter::repeat_n(delta, samples as usize));
        at += 8;
    }
    Some(durations)
}

/// The composition offsets a composition-to-sample box states.
///
/// The box is the same table of runs [`read_stts`] reads, with an offset where
/// the delta goes, and the offsets are signed in version one and unsigned in
/// version zero: a version zero box cannot state a negative offset at all, so
/// both are read into the signed type the caller adds to a decode time.
fn read_ctts(data: &[u8], ctts: Box_) -> Option<Vec<i64>> {
    let payload = &data[ctts.start..ctts.end];
    let version = *payload.first()?;
    let count = u32::from_be_bytes([
        *payload.get(4)?,
        *payload.get(5)?,
        *payload.get(6)?,
        *payload.get(7)?,
    ]);
    let mut offsets = Vec::new();
    let mut at = 8usize;
    for _ in 0..count {
        let entry = payload.get(at..at + 8)?;
        let samples = u32::from_be_bytes([entry[0], entry[1], entry[2], entry[3]]);
        if samples == 0 {
            return None;
        }
        let raw = [entry[4], entry[5], entry[6], entry[7]];
        let offset = if version == 1 {
            i64::from(i32::from_be_bytes(raw))
        } else {
            i64::from(u32::from_be_bytes(raw))
        };
        // The same bound `read_stts` keeps, for the same reason.
        let total = offsets.len().checked_add(samples as usize)?;
        if total > MAX_SAMPLES {
            return None;
        }
        offsets.extend(std::iter::repeat_n(offset, samples as usize));
        at += 8;
    }
    Some(offsets)
}

/// The media tick a track's edit list ends it at, or `None` for all of it.
///
/// An edit list is a list of edits: an *empty* one, whose media time is -1,
/// holds nothing for its duration, and a normal one plays the media from its
/// media time for its segment duration.
///
/// Three shapes of that are read, and one of them is refused:
///
/// - an empty edit is a **delay**. A clip has one frame per output tick, so
///   there is nowhere for held ticks to go: the pictures are all there, only
///   later, and the delay is read and not acted on.
/// - a normal edit that starts partway into the media is a **leading skip**,
///   which is refused. The pictures are decoded in order, so there is no way to
///   hand out the first ones for the second half of the media.
/// - a normal edit shorter than the media that follows it is a **truncation of
///   the end**, which is what this returns; a duration of zero runs to the end
///   of the media, which is the identity every encoder here writes.
///
/// # Errors
///
/// Returns [`ImgSeqError`] for a leading skip, and for a list that edits the
/// media more than once: neither is a timeline this reader can replay.
fn edit_list(data: &[u8], trak: Box_, path: &Path) -> Result<Option<i64>> {
    let Some(edts) = child(data, trak, b"edts") else {
        return Ok(None);
    };
    let Some(elst) = child(data, edts, b"elst") else {
        return Ok(None);
    };
    let payload = &data[elst.start..elst.end];
    let version = payload.first().copied().unwrap_or(0);
    // One entry is a duration, a media time, a rate and a fraction, and version
    // one widens the first two to 64 bits: a media time of -1 is an empty edit,
    // which is why the field is signed however wide it is.
    let wide = version == 1;
    let width = if wide { 8 } else { 4 };
    let entry = 2 * width + 4;
    // The count is always four bytes; a version one entry widens only the two
    // fields after it.
    let number = |at: usize, width: usize| -> Option<u64> {
        let bytes = payload.get(at..at + width)?;
        let mut value = 0u64;
        for byte in bytes {
            value = (value << 8) | u64::from(*byte);
        }
        Some(value)
    };
    let Some(count) = number(4, 4).and_then(|count| u32::try_from(count).ok()) else {
        return Ok(None);
    };
    let mut shown = None;
    let mut edits = 0usize;
    for index in 0..count {
        let at = 8 + entry * index as usize;
        let (Some(duration), Some(media_time)) = (number(at, width), number(at + width, width))
        else {
            return Ok(None);
        };
        let media_time = if wide {
            media_time as i64
        } else {
            i64::from(media_time as u32 as i32)
        };
        if media_time < 0 {
            // An empty edit, which is the delay described above.
            continue;
        }
        edits += 1;
        if edits > 1 {
            return Err(ImgSeqError::new(format!(
                "the edit list of '{}' edits its media more than once, which this reader cannot replay",
                path.display()
            )));
        }
        if media_time > 0 {
            return Err(ImgSeqError::new(format!(
                "the edit list of '{}' starts its media {media_time} ticks in, which this reader cannot replay",
                path.display()
            )));
        }
        if duration > 0 {
            shown = Some(duration as i64);
        }
    }
    Ok(shown)
}

/// The clean aperture that applies to a track's pictures.
///
/// Two places can state it, and a file may use either. A track's sample
/// description can carry a `clap` of its own, and the primary item can carry one
/// as an item property; the fixture used here states it on the item, which is
/// the same place a still image of either container states its aperture. The
/// track's own description is preferred when it has one because it is the more
/// specific statement about the samples being decoded.
fn crop_of(data: &[u8], trak: Option<Box_>) -> Option<Crop> {
    if let Some(trak) = trak
        && let Some(crop) = sample_crop(data, trak)
    {
        return Some(crop);
    }
    item_crop(data)
}

/// The clean aperture of the first sample description of a track.
fn sample_crop(data: &[u8], trak: Box_) -> Option<Crop> {
    let mdia = child(data, trak, b"mdia")?;
    let minf = child(data, mdia, b"minf")?;
    let stbl = child(data, minf, b"stbl")?;
    let stsd = child(data, stbl, b"stsd")?;
    // A sample description box is version and flags, an entry count, then the
    // entries themselves.
    let entry = boxes(data, stsd.start + 8, stsd.end.min(data.len()))
        .into_iter()
        .next()?;
    let clap = child(data, entry, b"clap")?;
    read_clap(data, clap)
}

/// The clean aperture the primary item states as an item property.
fn item_crop(data: &[u8]) -> Option<Crop> {
    let root = Box_ {
        kind: *b"file",
        start: 0,
        end: data.len(),
    };
    let meta = child(data, root, b"meta")?;
    // A meta box is a full box: version and flags come before its children.
    let meta = Box_ {
        kind: *b"meta",
        start: meta.start.checked_add(4)?,
        end: meta.end,
    };
    let pitm = child(data, meta, b"pitm")?;
    let primary = read_pitm(&data[pitm.start..pitm.end])?;
    let iprp = child(data, meta, b"iprp")?;
    let ipco = child(data, iprp, b"ipco")?;
    let properties = boxes(data, ipco.start, ipco.end);
    let ipma = child(data, iprp, b"ipma")?;
    let indices = read_ipma(&data[ipma.start..ipma.end], primary)?;
    // The properties are numbered from one, in the order they are written.
    for index in indices {
        let Some(property) = usize::try_from(index)
            .ok()
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| properties.get(index))
        else {
            continue;
        };
        if property.is(b"clap") {
            return read_clap(data, *property);
        }
    }
    None
}

/// The primary item a `pitm` box names.
fn read_pitm(payload: &[u8]) -> Option<u32> {
    match payload.first()? {
        0 => Some(u32::from(u16::from_be_bytes(
            payload.get(4..6)?.try_into().ok()?,
        ))),
        _ => Some(u32::from_be_bytes(payload.get(4..8)?.try_into().ok()?)),
    }
}

/// The property indices `ipma` associates with one item.
fn read_ipma(payload: &[u8], wanted: u32) -> Option<Vec<u32>> {
    let version = *payload.first()?;
    let wide = *payload.get(3)? & 1 != 0;
    let mut at = 4;
    let count = u32::from_be_bytes(payload.get(at..at + 4)?.try_into().ok()?);
    at += 4;
    for _ in 0..count {
        let item = if version < 1 {
            let item = u32::from(u16::from_be_bytes(
                payload.get(at..at + 2)?.try_into().ok()?,
            ));
            at += 2;
            item
        } else {
            let item = u32::from_be_bytes(payload.get(at..at + 4)?.try_into().ok()?);
            at += 4;
            item
        };
        let associations = usize::from(*payload.get(at)?);
        at += 1;
        let mut indices = Vec::with_capacity(associations);
        for _ in 0..associations {
            let association = if wide {
                let association = u16::from_be_bytes(payload.get(at..at + 2)?.try_into().ok()?);
                at += 2;
                u32::from(association & 0x7fff)
            } else {
                let association = u32::from(*payload.get(at)?);
                at += 1;
                association & 0x7f
            };
            indices.push(association);
        }
        if item == wanted {
            return Some(indices);
        }
    }
    None
}

/// The aperture a clean-aperture box states.
///
/// The fields are 32 bit numerators over a shared 32 bit denominator, so a
/// rational aperture is read as the whole pixels it covers.
fn read_clap(data: &[u8], clap: Box_) -> Option<Crop> {
    let payload = &data[clap.start..clap.end];
    let field = |index: usize| -> Option<i64> {
        let at = index * 8;
        let numerator = payload.get(at..at + 4)?;
        let denominator = payload.get(at + 4..at + 8)?;
        let numerator = i64::from(i32::from_be_bytes([
            numerator[0],
            numerator[1],
            numerator[2],
            numerator[3],
        ]));
        let denominator = i64::from(i32::from_be_bytes([
            denominator[0],
            denominator[1],
            denominator[2],
            denominator[3],
        ]));
        if denominator == 0 {
            return None;
        }
        Some(numerator / denominator)
    };
    let width = field(0)?;
    let height = field(1)?;
    let x = field(2)?;
    let y = field(3)?;
    if width <= 0 || height <= 0 {
        return None;
    }
    Some(Crop {
        width: u32::try_from(width).ok()?,
        height: u32::try_from(height).ok()?,
        // A negative offset places the aperture before the coded picture, which
        // names no pixels to hand out, so it is read as the start.
        x: u32::try_from(x.max(0)).ok()?,
        y: u32::try_from(y.max(0)).ok()?,
    })
}

/// Size of the coded picture, before any aperture is applied.
///
/// This is what `decode_next_image()` hands back, and it is what a crop has to
/// be applied to. The primary item's `ispe` states it: a track's own sample
/// entry record states the *presentation* size instead, which the fixture shows
/// as 16x12 beside the 64x64 the item property names.
fn coded_size(data: &[u8]) -> Option<(u32, u32)> {
    let root = Box_ {
        kind: *b"file",
        start: 0,
        end: data.len(),
    };
    let meta = child(data, root, b"meta")?;
    let meta = Box_ {
        kind: *b"meta",
        start: meta.start.checked_add(4)?,
        end: meta.end,
    };
    let pitm = child(data, meta, b"pitm")?;
    let primary = read_pitm(&data[pitm.start..pitm.end])?;
    let iprp = child(data, meta, b"iprp")?;
    let ipco = child(data, iprp, b"ipco")?;
    let properties = boxes(data, ipco.start, ipco.end);
    let ipma = child(data, iprp, b"ipma")?;
    let indices = read_ipma(&data[ipma.start..ipma.end], primary)?;
    for index in indices {
        let Some(property) = usize::try_from(index)
            .ok()
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| properties.get(index))
        else {
            continue;
        };
        if property.is(b"ispe") {
            return read_ispe(&data[property.start..property.end]);
        }
    }
    None
}

/// The spatial extents an `ispe` box states.
fn read_ispe(payload: &[u8]) -> Option<(u32, u32)> {
    // A full box: version and flags, then the width and the height.
    let width = u32::from_be_bytes(payload.get(4..8)?.try_into().ok()?);
    let height = u32::from_be_bytes(payload.get(8..12)?.try_into().ok()?);
    if width == 0 || height == 0 {
        return None;
    }
    Some((width, height))
}

/// How much of a file the box walk reads.
///
/// A sequence's movie box sits in front of its media data, and this is far past
/// any real one; a file whose boxes run beyond it is refused by the walk rather
/// than by an allocation of the file's whole size.
const METADATA_LIMIT: u64 = 32 * 1024 * 1024;

/// Upper bound on the samples a timing table may describe.
const MAX_SAMPLES: usize = 1 << 22;

#[cfg(test)]
mod tests {
    use std::{
        fs::File,
        io::BufReader,
        path::{Path, PathBuf},
    };

    use super::{Crop, Sequence, read};
    use crate::{decoder::image_error, error::Result};

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    /// The walk through an open of its own, which is what a probe hands it.
    fn read_file(path: &Path) -> Result<Option<Sequence>> {
        let file = File::open(path).map_err(|error| image_error("open", path, error))?;
        read(&mut BufReader::new(file), path)
    }

    /// An avif sequence states four samples whose durations are the ones the
    /// `stts` box holds, which is what the embedded libheif reports wrongly.
    #[test]
    fn an_avif_sequence_states_its_own_sample_timing() {
        let sequence = read_file(&fixture("animation.avif"))
            .expect("the fixture reads")
            .expect("the fixture is a sequence");
        assert_eq!(sequence.timing.timescale, 1000);
        assert_eq!(sequence.timing.durations, [80, 170, 110, 240]);
        assert_eq!(sequence.timing.durations.iter().sum::<u32>(), 600);
        assert_eq!(sequence.timing.samples(), 4);
    }

    /// A track that states composition offsets presents its samples where the
    /// `ctts` box says rather than where they were decoded.
    ///
    /// The fixture is `animation.avif` with a version one box inserted by
    /// `tests/make-animation-fixtures.py`, whose offsets are `-40, 100, 0, 0`:
    /// the first sample is composed forty ticks before zero, which is clamped,
    /// and the second a hundred after its decode time. The decode times the
    /// durations make are 0, 80, 250 and 360, so the presentation times the
    /// offsets ask for are 0, 180, 250 and 360.
    #[test]
    fn a_composition_offset_moves_a_sample() {
        let sequence = read_file(&fixture("animation-ctts.avif"))
            .expect("the fixture reads")
            .expect("the fixture is a sequence");
        assert_eq!(sequence.timing.timescale, 1000);
        assert_eq!(sequence.timing.durations, [80, 170, 110, 240]);
        assert_eq!(sequence.timing.offsets, [-40, 100, 0, 0]);
        assert_eq!(sequence.timing.samples(), 4);
    }

    /// A track with no composition offsets states none, which is the control:
    /// the common case has to read exactly as it did before the box was
    /// understood.
    #[test]
    fn a_track_without_composition_offsets_states_none() {
        let sequence = read_file(&fixture("animation.avif"))
            .expect("the fixture reads")
            .expect("the fixture is a sequence");
        assert!(sequence.timing.offsets.is_empty());
        // Every encoder here writes an identity edit list: the whole media, from
        // its start, which is the shape that changes nothing.
        assert_eq!(sequence.timing.shown, Some(600));
    }

    /// An edit list shorter than the media ends the track early, which is a
    /// truncation of the timeline rather than of the pictures.
    #[test]
    fn an_edit_list_shorter_than_the_media_ends_the_track() {
        let sequence = read_file(&fixture("animation-elst-short.avif"))
            .expect("the fixture reads")
            .expect("the fixture is a sequence");
        assert_eq!(sequence.timing.durations, [80, 170, 110, 240]);
        assert_eq!(sequence.timing.shown, Some(300));
    }

    /// An edit list that starts partway into the media is a leading skip this
    /// reader cannot replay: the pictures are decoded in order, so it is refused
    /// by name rather than played.
    #[test]
    fn an_edit_list_that_skips_the_start_is_refused() {
        let error =
            read_file(&fixture("animation-elst-skip.avif")).expect_err("a leading skip is refused");
        assert!(error.to_string().contains("starts its media"), "{error}");
    }

    /// The heic fixture states the same sample count at a different rate, and
    /// its coded picture is larger than the aperture it presents.
    #[test]
    fn a_heic_sequence_states_its_aperture() {
        let sequence = read_file(&fixture("animation.heic"))
            .expect("the fixture reads")
            .expect("the fixture is a sequence");
        assert_eq!(sequence.timing.timescale, 1000);
        assert_eq!(sequence.timing.durations, [150, 150, 150, 150]);
        assert_eq!(sequence.timing.durations.iter().sum::<u32>(), 600);
        assert_eq!(
            sequence.crop,
            Some(Crop {
                width: 16,
                height: 12,
                x: 0,
                y: 0
            })
        );
        assert_eq!(sequence.coded, Some((64, 64)));
        // The aperture is smaller than the coded picture, which is exactly the
        // case that has to be cropped before a frame is handed out.
        assert_ne!(
            sequence.crop.expect("a crop"),
            Crop {
                width: 64,
                height: 64,
                x: 0,
                y: 0,
            }
        );
    }

    /// A still image of the same container has no movie box at all.
    #[test]
    fn a_still_image_states_no_sequence() {
        for name in ["alpha-rgba8.avif", "alpha-rgba8.heic"] {
            let path = fixture(name);
            if !path.exists() {
                continue;
            }
            assert!(
                read_file(&path).expect("the fixture reads").is_none(),
                "{name}"
            );
        }
    }
}
