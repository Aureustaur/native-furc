//! Furcadia sprite archive parser (`FOX5`, `FSHX`, and classic `.fsh`), lazy LZMA
//! decoder, and KitterSpeak animation VM.
//!
//! # Supported Archive Formats
//! - **FOX5 (`.fox`)**: Modern archive with a trailing `"FOX5"` footer, LZMA-compressed
//!   command block (optionally RC4-encrypted with a per-chunk key derived from the
//!   footer's 16-byte `base_key` and compressed/uncompressed sizes), an image table of
//!   LZMA-compressed indexed or 32-bit ARGB sprites, and shape/animation descriptors.
//! - **FSHX (`.fox` legacy)**: Starts with `"FSHX"`, optionally applies a 96-byte
//!   block permutation and per-object XOR/subtraction mask, and stores bottom-up
//!   8-bit indexed frames plus KitterSpeak animation steps.
//! - **Classic FSH (`.fsh`)**: Little-endian `u16` sprite count, `u16` chunk-size
//!   table, and bottom-up 8-bit indexed pixel chunks with `(w, h, ox, oy)` headers.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io::{self, BufReader, Write};
use std::path::Path;

use crate::assets::Palette;

/// Maximum pixel dimension along either axis for a single FOX5 sprite (prevents 17 GB OOM on crafted headers).
const MAX_FOX_IMAGE_DIM: u16 = 4096;
/// Maximum decompressed byte size for any single LZMA stream in a `.fox` archive (64 MiB).
const MAX_LZMA_OUTPUT_BYTES: usize = 64 * 1024 * 1024;

/// Metadata for a single image stored inside a `FOX5` archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FoxImageEntry {
    pub offset: usize,
    pub compressed_size: usize,
    pub width: u16,
    pub height: u16,
    pub format: u8,
}

/// Decoded RGBA8888 sprite image.
#[derive(Debug, Clone)]
pub struct FoxSprite {
    pub width: u16,
    pub height: u16,
    pub rgba: Vec<u8>,
}

/// Shape reference for a FOX5 object (1x image index, direction, FoxEd offset, and elevation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FoxShapeRef {
    pub image_idx: usize,
    pub dir: u8,
    pub offset: (i16, i16),
    pub elev: (i16, i16),
    pub rule: (u8, u8),
}

/// Single drawable sprite layer inside a FOX5 animation frame (`L 0x04`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FoxLayer {
    /// Layer purpose/category (`0x0030` body, `0x0050` shadow, etc.).
    pub category: u16,
    /// Zero-based index into `FoxArchive::images`.
    pub image_idx: usize,
    /// Per-layer hotspot tweak (`'O'`).
    pub layer_offset: (i16, i16),
}

/// Single animation frame (`L 0x03`) of a FOX5 shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoxFrame {
    pub offset: (i16, i16),
    pub elev: (i16, i16),
    pub layers: Vec<FoxLayer>,
}

/// Single KitterSpeak timeline instruction (`'K'`) of a FOX5 shape (three big-endian `u16`s).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FoxAnimStep {
    pub op: u16,
    pub arg1: u16,
    pub arg2: u16,
}

/// Single active frame slot produced by KitterSpeak `'K'` playback (`wt/bt/gt/yt`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FoxActiveSlot {
    /// Slot index `0..=3` (`0,1` draw behind avatars; `2,3` draw in front of avatars).
    pub slot: u8,
    /// Zero-based index into `FoxAvatarShape::frames`.
    pub frame_idx: usize,
    /// Frame offset `(x, y)` (with any active KitterSpeak op `5/6/18/19` override applied).
    pub offset: (i16, i16),
}

/// Complete 4-slot KitterSpeak animation snapshot for a shape at a given timestamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoxResolvedSlots {
    /// Concurrent layer slots `[slot0, slot1, slot2, slot3]`.
    pub slots: [Option<FoxActiveSlot>; 4],
    /// Effect ordering relative to item (`_t`, op `33`): `1` = effect before item, `2` = after.
    pub effect_pass: u8,
    /// Elevation `(x, y)` from active slot's frame or KitterSpeak op `7/8/20/21` override.
    pub elev: (i16, i16),
    /// Opacity modulation `0..=255` (`Ct`, KitterSpeak op `17/22`).
    pub alpha: u8,
}

impl FoxResolvedSlots {
    /// Slots rendered behind avatars (`slot 0`, `slot 1`).
    pub fn pass1(&self) -> impl Iterator<Item = &FoxActiveSlot> {
        self.slots[0..2].iter().flatten()
    }

    /// Slots rendered in front of avatars (`slot 2`, `slot 3`).
    pub fn pass2(&self) -> impl Iterator<Item = &FoxActiveSlot> {
        self.slots[2..4].iter().flatten()
    }

    /// Primary frame index across `[slot1, slot0, slot2, slot3]`.
    pub fn primary_frame_idx(&self) -> Option<usize> {
        [1, 0, 2, 3]
            .into_iter()
            .find_map(|s| self.slots[s].as_ref().map(|a| a.frame_idx))
    }
}

#[derive(Debug, Clone, Copy)]
struct KsTween {
    start_ms: u64,
    end_ms: u64,
    start_val: i16,
    end_val: i16,
}

impl KsTween {
    fn evaluate(self, now_ms: u64) -> (i16, bool) {
        if now_ms >= self.end_ms || self.end_ms <= self.start_ms {
            return (self.end_val, true);
        }
        let t = (now_ms - self.start_ms) as f64 / (self.end_ms - self.start_ms) as f64;
        let v = self.start_val as i32 + (((self.end_val as i32 - self.start_val as i32) as f64) * t).floor() as i32;
        (v.clamp(i16::MIN as i32, i16::MAX as i32) as i16, false)
    }
}

fn ks_delay_ms(min_v: i32, max_v: i32) -> u64 {
    let lo = min_v.min(max_v).max(0);
    let hi = min_v.max(max_v).max(0);
    ((lo + hi) / 2) as u64
}

fn ks_shift_slot(slots: &mut [Option<usize>; 4], frame_val: i32, dir: i8) {
    if frame_val < 0 {
        return;
    }
    let target = frame_val as usize;
    let Some(cur) = slots.iter().position(|&s| s == Some(target)) else {
        return;
    };
    match dir {
        -1 => match cur {
            1 => {
                slots[1] = slots[0];
                slots[0] = Some(target);
            }
            2 => {
                if slots[1].is_none() {
                    slots[1] = Some(target);
                    slots[2] = None;
                } else if slots[0].is_none() {
                    slots[0] = slots[1];
                    slots[1] = Some(target);
                    slots[2] = None;
                } else {
                    slots[2] = slots[1];
                    slots[1] = Some(target);
                }
            }
            3 => {
                if slots[2].is_none() {
                    slots[2] = Some(target);
                    slots[3] = None;
                } else if slots[1].is_none() {
                    slots[1] = slots[2];
                    slots[2] = Some(target);
                    slots[3] = None;
                } else if slots[0].is_none() {
                    slots[0] = slots[1];
                    slots[1] = slots[2];
                    slots[2] = Some(target);
                    slots[3] = None;
                } else {
                    slots[3] = slots[2];
                    slots[2] = Some(target);
                }
            }
            _ => {}
        },
        1 => match cur {
            2 => {
                slots[2] = slots[3];
                slots[3] = Some(target);
            }
            1 => {
                if slots[2].is_none() {
                    slots[2] = Some(target);
                    slots[1] = None;
                } else if slots[3].is_none() {
                    slots[3] = slots[2];
                    slots[2] = Some(target);
                    slots[1] = None;
                } else {
                    slots[1] = slots[2];
                    slots[2] = Some(target);
                }
            }
            0 => {
                if slots[1].is_none() {
                    slots[1] = Some(target);
                    slots[0] = None;
                } else if slots[2].is_none() {
                    slots[2] = slots[1];
                    slots[1] = Some(target);
                    slots[0] = None;
                } else if slots[3].is_none() {
                    slots[3] = slots[2];
                    slots[2] = slots[1];
                    slots[1] = Some(target);
                    slots[0] = None;
                } else {
                    slots[0] = slots[1];
                    slots[1] = Some(target);
                }
            }
            _ => {}
        },
        _ => {}
    }
}

/// Directional/pose/animated shape entry (`L 0x02`) for avatars and multi-layer/animated objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoxAvatarShape {
    pub purpose: u8,
    pub state: u8,
    pub dir: u8,
    pub rule: (u8, u8),
    pub offset: (i16, i16),
    pub elev: (i16, i16),
    pub layers: Vec<FoxLayer>,
    pub frames: Vec<FoxFrame>,
    pub anim_steps: Vec<FoxAnimStep>,
}

impl FoxAvatarShape {
    /// Evaluates the full 4-slot KitterSpeak `'K'` state at `elapsed_ms`.
    pub fn slots_at(&self, elapsed_ms: u64) -> Option<FoxResolvedSlots> {
        if self.frames.is_empty() {
            return None;
        }
        let mut slots: [Option<usize>; 4] = [None, Some(0), None, None];
        let mut active_slot: usize = 1;
        let mut effect_pass: u8 = 1;
        let mut delay_min: i32 = 0;
        let mut delay_max: i32 = 0;

        let mut ov_x: Option<i16> = None;
        let mut ov_y: Option<i16> = None;
        let mut ov_ex: Option<i16> = None;
        let mut ov_ey: Option<i16> = None;
        let mut ov_alpha: Option<u8> = None;
        let mut tw_x: Option<KsTween> = None;
        let mut tw_y: Option<KsTween> = None;
        let mut tw_ex: Option<KsTween> = None;
        let mut tw_ey: Option<KsTween> = None;
        let mut tw_alpha: Option<KsTween> = None;

        if !self.anim_steps.is_empty() {
            let steps = &self.anim_steps;
            let mut pc: usize = 0;
            let mut next_tick_ms: u64 = 0;
            let mut loop_counts = vec![0i32; steps.len()];
            let mut zero_progress: u32 = 0;
            let mut skipped_cycle = false;

            #[derive(Hash, PartialEq, Eq)]
            struct VmKey {
                pc: usize,
                slots: [Option<usize>; 4],
                active_slot: usize,
                effect_pass: u8,
                delay_min: i32,
                delay_max: i32,
                ov_x: Option<i16>,
                ov_y: Option<i16>,
                ov_ex: Option<i16>,
                ov_ey: Option<i16>,
                ov_alpha: Option<u8>,
                loop_counts: Vec<i32>,
            }
            let mut seen: HashMap<VmKey, u64> = HashMap::new();

            for _ in 0..8192 {
                if elapsed_ms < next_tick_ms || zero_progress >= 64 {
                    break;
                }
                if !skipped_cycle
                    && tw_x.is_none()
                    && tw_y.is_none()
                    && tw_ex.is_none()
                    && tw_ey.is_none()
                    && tw_alpha.is_none()
                {
                    let key = VmKey {
                        pc,
                        slots,
                        active_slot,
                        effect_pass,
                        delay_min,
                        delay_max,
                        ov_x,
                        ov_y,
                        ov_ex,
                        ov_ey,
                        ov_alpha,
                        loop_counts: loop_counts.clone(),
                    };
                    if let Some(&first_t) = seen.get(&key) {
                        if next_tick_ms > first_t {
                            let period = next_tick_ms - first_t;
                            let rem = elapsed_ms - next_tick_ms;
                            let skip = rem / period;
                            if skip > 0 {
                                next_tick_ms = next_tick_ms.saturating_add(skip * period);
                            }
                            skipped_cycle = true;
                            if elapsed_ms < next_tick_ms {
                                break;
                            }
                        }
                    } else if seen.len() < 512 {
                        seen.insert(key, next_tick_ms);
                    }
                }

                let tick_before = next_tick_ms;
                let step = steps[pc];
                let a1 = step.arg1 as i16;
                let a2 = step.arg2 as i16;
                let mut halted = false;
                let mut jumped_pc: Option<usize> = None;

                match step.op {
                    1 | 23 | 29 | 30 | 24 => {
                        let slot = match step.op {
                            23 => 0,
                            29 => 1,
                            30 => 2,
                            24 => 3,
                            _ => active_slot,
                        };
                        let fidx = (a1 as i32).clamp(0, self.frames.len() as i32 - 1) as usize;
                        slots[slot] = Some(fidx);
                        for (c, sv) in slots.iter_mut().enumerate() {
                            if c != slot && *sv == Some(fidx) {
                                *sv = None;
                            }
                        }
                        if !steps.get(pc + 1).is_some_and(|n| n.op == 2) {
                            next_tick_ms =
                                next_tick_ms.saturating_add(ks_delay_ms(delay_min, delay_max));
                        }
                    }
                    3 => {
                        loop_counts[pc] += 1;
                        if loop_counts[pc] <= a2 as i32 {
                            let jumped = (a1 as i32 - 1).min(steps.len() as i32).max(-1);
                            jumped_pc = Some((jumped + 1) as usize);
                        } else {
                            loop_counts[pc] = 0;
                        }
                    }
                    4 => {
                        let jumped = (a1 as i32 - 1).min(steps.len() as i32).max(-1);
                        jumped_pc = Some((jumped + 1) as usize);
                    }
                    12 => {
                        halted = true;
                    }
                    2 => {
                        next_tick_ms = next_tick_ms.saturating_add((a1 as i32).max(0) as u64);
                    }
                    15 => {
                        next_tick_ms = next_tick_ms.saturating_add(ks_delay_ms(a1 as i32, a2 as i32));
                    }
                    11 => {
                        let d = (a1 as i32).max(0);
                        delay_min = d;
                        delay_max = d;
                    }
                    14 => {
                        delay_min = (a1 as i32).max(0);
                        delay_max = (a2 as i32).max(0);
                    }
                    5 => {
                        tw_x = None;
                        ov_x = Some(a1);
                    }
                    6 => {
                        tw_y = None;
                        ov_y = Some(a1);
                    }
                    7 => {
                        tw_ex = None;
                        ov_ex = Some(a1);
                    }
                    8 => {
                        tw_ey = None;
                        ov_ey = Some(a1);
                    }
                    17 => {
                        tw_alpha = None;
                        ov_alpha = Some((a1 as i32).clamp(0, 255) as u8);
                    }
                    18 => {
                        let cur = ov_x
                            .or_else(|| slots[1].and_then(|i| self.frames.get(i)).map(|f| f.offset.0))
                            .unwrap_or(0);
                        tw_x = Some(KsTween {
                            start_ms: next_tick_ms,
                            end_ms: next_tick_ms.saturating_add((a2 as i32).max(0) as u64),
                            start_val: cur,
                            end_val: a1,
                        });
                        ov_x = Some(cur);
                    }
                    19 => {
                        let cur = ov_y
                            .or_else(|| slots[1].and_then(|i| self.frames.get(i)).map(|f| f.offset.1))
                            .unwrap_or(0);
                        tw_y = Some(KsTween {
                            start_ms: next_tick_ms,
                            end_ms: next_tick_ms.saturating_add((a2 as i32).max(0) as u64),
                            start_val: cur,
                            end_val: a1,
                        });
                        ov_y = Some(cur);
                    }
                    20 => {
                        let cur = ov_ex
                            .or_else(|| {
                                slots[active_slot]
                                    .and_then(|i| self.frames.get(i))
                                    .map(|f| f.elev.0)
                            })
                            .unwrap_or(0);
                        tw_ex = Some(KsTween {
                            start_ms: next_tick_ms,
                            end_ms: next_tick_ms.saturating_add((a2 as i32).max(0) as u64),
                            start_val: cur,
                            end_val: a1,
                        });
                        ov_ex = Some(cur);
                    }
                    21 => {
                        let cur = ov_ey
                            .or_else(|| {
                                slots[active_slot]
                                    .and_then(|i| self.frames.get(i))
                                    .map(|f| f.elev.1)
                            })
                            .unwrap_or(0);
                        tw_ey = Some(KsTween {
                            start_ms: next_tick_ms,
                            end_ms: next_tick_ms.saturating_add((a2 as i32).max(0) as u64),
                            start_val: cur,
                            end_val: a1,
                        });
                        ov_ey = Some(cur);
                    }
                    22 => {
                        let cur = ov_alpha.unwrap_or(255) as i16;
                        tw_alpha = Some(KsTween {
                            start_ms: next_tick_ms,
                            end_ms: next_tick_ms.saturating_add((a2 as i32).max(0) as u64),
                            start_val: cur,
                            end_val: (a1 as i32).clamp(0, 255) as i16,
                        });
                        ov_alpha = Some(cur as u8);
                    }
                    27 => slots[0] = None,
                    28 => slots[3] = None,
                    31 => ks_shift_slot(&mut slots, a1 as i32, 1),
                    32 => ks_shift_slot(&mut slots, a1 as i32, -1),
                    9 => {
                        if active_slot != 2 {
                            active_slot = 2;
                            slots[2] = slots[1];
                            slots[1] = None;
                        }
                    }
                    10 => {
                        if active_slot != 1 {
                            active_slot = 1;
                            slots[1] = slots[2];
                            slots[2] = None;
                        }
                    }
                    33
                        if (step.arg1 == 1 || step.arg1 == 2) => {
                            effect_pass = step.arg1 as u8;
                        }
                    _ => {}
                }

                if let Some(tw) = tw_x {
                    let (v, done) = tw.evaluate(elapsed_ms);
                    ov_x = Some(v);
                    if done {
                        tw_x = None;
                    }
                }
                if let Some(tw) = tw_y {
                    let (v, done) = tw.evaluate(elapsed_ms);
                    ov_y = Some(v);
                    if done {
                        tw_y = None;
                    }
                }
                if let Some(tw) = tw_ex {
                    let (v, done) = tw.evaluate(elapsed_ms);
                    ov_ex = Some(v);
                    if done {
                        tw_ex = None;
                    }
                }
                if let Some(tw) = tw_ey {
                    let (v, done) = tw.evaluate(elapsed_ms);
                    ov_ey = Some(v);
                    if done {
                        tw_ey = None;
                    }
                }
                if let Some(tw) = tw_alpha {
                    let (v, done) = tw.evaluate(elapsed_ms);
                    ov_alpha = Some((v as i32).clamp(0, 255) as u8);
                    if done {
                        tw_alpha = None;
                    }
                }

                if halted {
                    break;
                }
                if next_tick_ms > tick_before {
                    zero_progress = 0;
                } else {
                    zero_progress += 1;
                }
                pc = match jumped_pc {
                    Some(j) if j < steps.len() => j,
                    Some(_) => 0,
                    None => {
                        let nxt = pc + 1;
                        if nxt >= steps.len() { 0 } else { nxt }
                    }
                };
            }
        }

        let mut out_slots: [Option<FoxActiveSlot>; 4] = [None, None, None, None];
        for (slot_idx, slot_frame) in slots.into_iter().enumerate() {
            if let Some(fidx) = slot_frame {
                if let Some(frame) = self.frames.get(fidx) {
                    out_slots[slot_idx] = Some(FoxActiveSlot {
                        slot: slot_idx as u8,
                        frame_idx: fidx,
                        offset: (ov_x.unwrap_or(frame.offset.0), ov_y.unwrap_or(frame.offset.1)),
                    });
                }
            }
        }
        let base_elev = slots[active_slot]
            .and_then(|fidx| self.frames.get(fidx))
            .map(|f| f.elev)
            .unwrap_or((0, 0));

        Some(FoxResolvedSlots {
            slots: out_slots,
            effect_pass,
            elev: (ov_ex.unwrap_or(base_elev.0), ov_ey.unwrap_or(base_elev.1)),
            alpha: ov_alpha.unwrap_or(255),
        })
    }

    /// Returns the active animation frame for `elapsed_ms`, following KitterSpeak `'K'` steps.
    pub fn frame_at(&self, elapsed_ms: u64) -> Option<&FoxFrame> {
        self.slots_at(elapsed_ms)
            .and_then(|resolved| resolved.primary_frame_idx())
            .and_then(|fidx| self.frames.get(fidx))
            .or_else(|| self.frames.first())
    }
}

/// Errors returned when loading or decoding a `FOX5` archive.
#[derive(Debug)]
pub enum FoxError {
    Io(io::Error),
    InvalidFooter,
    Lzma(String),
    InvalidCommandBlock,
    ImageOutOfBounds(usize),
}

impl fmt::Display for FoxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::InvalidFooter => write!(f, "invalid or unsupported FOX5 footer"),
            Self::Lzma(msg) => write!(f, "LZMA decompression error: {msg}"),
            Self::InvalidCommandBlock => write!(f, "invalid FOX5 command block"),
            Self::ImageOutOfBounds(idx) => write!(f, "image index {idx} out of bounds"),
        }
    }
}

impl std::error::Error for FoxError {}

impl From<io::Error> for FoxError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

/// Format tag inside `FoxImageEntry::format` for pre-decoded top-down 8-bit indexed sprites (`FSHX`/`.fsh`).
const FOX_FMT_PREDECODED_INDEXED: u8 = 254;

/// 96-byte block permutation table for encrypted `FSHX` archives (`class ci`).
const FSHX_PERM96: [usize; 96] = [
    0, 2, 1, 5, 4, 3, 7, 8, 6, 11, 10, 9, 12, 13, 15, 14, 16, 18, 17, 19, 23, 21, 22, 20, 24, 27,
    25, 26, 28, 30, 29, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49,
    50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73,
    74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95,
];

/// Loaded `FOX5`, `FSHX`, or `.fsh` archive supporting on-demand LZMA/RC4 or pre-decoded indexed sprite decoding.
pub struct FoxArchive {
    raw: Vec<u8>,
    enc_flag: u8,
    base_key: [u8; 16],
    predecoded_indexed: Vec<Vec<u8>>,
    /// True when loaded from a legacy `FSHX` (`FOX1`) or `.fsh` archive rather than `FOX5`.
    pub is_fshx_or_fsh: bool,
    pub images: Vec<FoxImageEntry>,
    /// Mapping from object/tile ID to its default 1x image index.
    pub object_to_image: HashMap<u16, usize>,
    /// Mapping from `(object_id, dir)` to its 1x shape reference (`image_idx` + `offset`).
    pub object_shapes: HashMap<(u16, u8), FoxShapeRef>,
    /// Ordered multi-layer avatar/pose shapes per object (`L 0x02`).
    pub object_avatar_shapes: HashMap<u16, Vec<FoxAvatarShape>>,
    /// Per-object FOX5 `'!'` flags byte (`bit 0` clamps sit/stand/walk poses to `2`, `[+0x68] & 1`).
    pub object_flags: HashMap<u16, u8>,
    cache: HashMap<usize, FoxSprite>,
}

impl FoxArchive {
    /// Loads a `FOX5`, `FSHX`, or `.fsh` archive from disk and parses its image directory.
    pub fn load(path: &Path) -> Result<Self, FoxError> {
        let raw = fs::read(path)?;
        Self::from_bytes(raw)
    }

    /// Parses a `FOX5`, `FSHX`, or `.fsh` archive from raw bytes.
    pub fn from_bytes(raw: Vec<u8>) -> Result<Self, FoxError> {
        if raw.len() >= 28 && &raw[0..4] == b"FSHX" {
            return Self::from_fshx_bytes(&raw);
        }
        if raw.len() >= 20 && raw[raw.len() - 8..].starts_with(b"FOX5") {
            return Self::from_fox5_bytes(raw);
        }
        if let Some(archive) = Self::try_from_fsh_bytes(&raw) {
            return Ok(archive);
        }
        Err(FoxError::InvalidFooter)
    }

    fn from_fox5_bytes(raw: Vec<u8>) -> Result<Self, FoxError> {
        let enc_flag = raw[raw.len() - 19];
        let footer = &raw[raw.len() - 16..raw.len() - 8];
        let comp_cmd_size = u32::from_be_bytes([footer[0], footer[1], footer[2], footer[3]]) as usize;
        let uncomp_cmd_size = u32::from_be_bytes([footer[4], footer[5], footer[6], footer[7]]);
        if comp_cmd_size == 0 || comp_cmd_size > raw.len() - 20 {
            return Err(FoxError::InvalidFooter);
        }

        let mut base_key = [0u8; 16];
        let cmd = if enc_flag == 1 {
            if raw.len() < 36 {
                return Err(FoxError::InvalidFooter);
            }
            base_key.copy_from_slice(&raw[raw.len() - 36..raw.len() - 20]);
            let key = derive_fox_key(&base_key, comp_cmd_size as u32, uncomp_cmd_size);
            let dec = rc4_crypt(&key, &raw[..comp_cmd_size]);
            decompress_lzma(&dec)?
        } else {
            decompress_lzma(&raw[..comp_cmd_size])?
        };
        // Expect 4 reserved null bytes + 'L' (level 0) + 4-byte ID + 1-byte count? + 'S' + u32 count.
        let s_pos = cmd
            .windows(2)
            .position(|w| w == [0x01, b'S'])
            .map(|p| p + 1)
            .ok_or(FoxError::InvalidCommandBlock)?;

        if s_pos + 5 > cmd.len() {
            return Err(FoxError::InvalidCommandBlock);
        }
        let count = u32::from_be_bytes([
            cmd[s_pos + 1],
            cmd[s_pos + 2],
            cmd[s_pos + 3],
            cmd[s_pos + 4],
        ]) as usize;

        let table_start = s_pos + 5;
        let table_bytes = count
            .checked_mul(9)
            .ok_or(FoxError::InvalidCommandBlock)?;
        let table_end = table_start
            .checked_add(table_bytes)
            .ok_or(FoxError::InvalidCommandBlock)?;
        if table_end > cmd.len() {
            return Err(FoxError::InvalidCommandBlock);
        }

        let mut images = Vec::with_capacity(count);
        let mut img_offset = comp_cmd_size;
        for chunk in cmd[table_start..table_end].as_chunks::<9>().0 {
            let csize = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as usize;
            let width = u16::from_be_bytes([chunk[4], chunk[5]]);
            let height = u16::from_be_bytes([chunk[6], chunk[7]]);
            let format = chunk[8];
            if width > MAX_FOX_IMAGE_DIM || height > MAX_FOX_IMAGE_DIM {
                return Err(FoxError::InvalidCommandBlock);
            }
            images.push(FoxImageEntry {
                offset: img_offset,
                compressed_size: csize,
                width,
                height,
                format,
            });
            img_offset = img_offset
                .checked_add(csize)
                .ok_or(FoxError::InvalidCommandBlock)?;
        }

        let (object_to_image, object_shapes, object_avatar_shapes, object_flags) =
            extract_object_shapes(&cmd[table_end..], images.len());

        Ok(Self {
            raw,
            enc_flag,
            base_key,
            predecoded_indexed: Vec::new(),
            is_fshx_or_fsh: false,
            images,
            object_to_image,
            object_shapes,
            object_avatar_shapes,
            object_flags,
            cache: HashMap::new(),
        })
    }

    fn from_fshx_bytes(raw: &[u8]) -> Result<Self, FoxError> {
        if raw.len() < 28 || &raw[0..4] != b"FSHX" {
            return Err(FoxError::InvalidFooter);
        }
        let version = i32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]);
        let obj_count = i32::from_le_bytes([raw[8], raw[9], raw[10], raw[11]]);
        let enc_mode = i32::from_le_bytes([raw[16], raw[17], raw[18], raw[19]]);
        if version != 1 || !(0..=10_000).contains(&obj_count) {
            return Err(FoxError::InvalidCommandBlock);
        }

        let payload_slice: std::borrow::Cow<'_, [u8]> = if enc_mode != 0 {
            let src = &raw[28..];
            let mut out = src.to_vec();
            let mut r = 0usize;
            while r + 96 <= out.len() {
                for t in 0..96 {
                    out[r + t] = src[r + FSHX_PERM96[t]];
                }
                r += 96;
            }
            std::borrow::Cow::Owned(out)
        } else {
            std::borrow::Cow::Borrowed(&raw[28..])
        };

        let data = payload_slice.as_ref();
        let mut h = 0usize;
        let obj_count = obj_count as usize;
        let mut predecoded_indexed = Vec::new();
        let mut images = Vec::new();
        let mut object_to_image = HashMap::new();
        let mut object_shapes = HashMap::new();
        let mut object_avatar_shapes = HashMap::new();
        let mut object_flags = HashMap::new();

        for seq_idx in 0..obj_count {
            if h + 8 > data.len() {
                return Err(FoxError::InvalidCommandBlock);
            }
            let raw_flags = u16::from_le_bytes([data[h], data[h + 1]]);
            let flags = (raw_flags & 7) as u8;
            let number = i16::from_le_bytes([data[h + 2], data[h + 3]]) as i32;
            let frame_count = u16::from_le_bytes([data[h + 4], data[h + 5]]) as usize;
            let anim_count = u16::from_le_bytes([data[h + 6], data[h + 7]]) as usize;
            h += 8;

            let obj_id = if number > -1 {
                number as u16
            } else {
                seq_idx as u16
            };
            object_flags.insert(obj_id, flags);

            let (mask_xor, mask_sub) = if enc_mode != 0 {
                let mut u = (number & 0xff) as u8;
                if u == 0 {
                    u = 4;
                }
                let l = if enc_mode >= 2 {
                    255u8.wrapping_sub(u)
                } else {
                    255u8
                };
                (l, u)
            } else {
                (0u8, 0u8)
            };

            // Validate minimum remaining bytes before pre-allocating `frames`.
            if h.saturating_add(frame_count.saturating_mul(18)) > data.len() {
                return Err(FoxError::InvalidCommandBlock);
            }
            let mut frames = Vec::with_capacity(frame_count);
            for _ in 0..frame_count {
                if h + 18 > data.len() {
                    return Err(FoxError::InvalidCommandBlock);
                }
                let w = u16::from_le_bytes([data[h + 2], data[h + 3]]);
                let c = u16::from_le_bytes([data[h + 4], data[h + 5]]);
                let offset_x = i16::from_le_bytes([data[h + 6], data[h + 7]]);
                let offset_y = i16::from_le_bytes([data[h + 8], data[h + 9]]);
                let elev_x = i16::from_le_bytes([data[h + 10], data[h + 11]]);
                let elev_y = i16::from_le_bytes([data[h + 12], data[h + 13]]);
                let mut a = u32::from_le_bytes([data[h + 14], data[h + 15], data[h + 16], data[h + 17]]);
                h += 18;

                if enc_mode != 0 {
                    a = ((a & 0xffff) << 16) | ((a & 0xffff_0000) >> 16);
                }
                let w_us = w as usize;
                let c_us = c as usize;
                let a_us = a as usize;
                if w > 640 || c > 480 || a_us != w_us * c_us || h + a_us > data.len() {
                    return Err(FoxError::InvalidCommandBlock);
                }

                let mut top_down = vec![0u8; a_us];
                if w_us > 0 && c_us > 0 {
                    for src_row in 0..c_us {
                        let src_off = h + src_row * w_us;
                        let dst_off = (c_us - 1 - src_row) * w_us;
                        for col in 0..w_us {
                            top_down[dst_off + col] =
                                (data[src_off + col] ^ mask_xor).wrapping_sub(mask_sub);
                        }
                    }
                }
                h += a_us;

                let pre_idx = predecoded_indexed.len();
                predecoded_indexed.push(top_down);
                let img_idx = images.len();
                images.push(FoxImageEntry {
                    offset: pre_idx,
                    compressed_size: a_us,
                    width: w,
                    height: c,
                    format: FOX_FMT_PREDECODED_INDEXED,
                });
                frames.push(FoxFrame {
                    offset: (offset_x, offset_y),
                    elev: (elev_x, elev_y),
                    layers: vec![FoxLayer {
                        category: 0,
                        image_idx: img_idx,
                        layer_offset: (0, 0),
                    }],
                });
            }

            if h + anim_count.saturating_mul(6) > data.len() {
                return Err(FoxError::InvalidCommandBlock);
            }
            let mut anim_steps = Vec::with_capacity(anim_count);
            for _ in 0..anim_count {
                let op = u16::from_le_bytes([data[h], data[h + 1]]);
                let arg1 = i16::from_le_bytes([data[h + 2], data[h + 3]]) as u16;
                let arg2 = i16::from_le_bytes([data[h + 4], data[h + 5]]) as u16;
                h += 6;
                anim_steps.push(FoxAnimStep { op, arg1, arg2 });
            }

            if let Some(first_frame) = frames.first() {
                if let Some(first_layer) = first_frame.layers.first() {
                    object_to_image.insert(obj_id, first_layer.image_idx);
                    object_shapes.insert(
                        (obj_id, 0),
                        FoxShapeRef {
                            image_idx: first_layer.image_idx,
                            dir: 0,
                            offset: first_frame.offset,
                            elev: first_frame.elev,
                            rule: (1, 1),
                        },
                    );
                }
                object_avatar_shapes.insert(
                    obj_id,
                    vec![FoxAvatarShape {
                        purpose: 0,
                        state: 0,
                        dir: 0,
                        rule: (1, 1),
                        offset: first_frame.offset,
                        elev: first_frame.elev,
                        layers: first_frame.layers.clone(),
                        frames,
                        anim_steps,
                    }],
                );
            }
        }

        Ok(Self {
            raw: Vec::new(),
            enc_flag: 0,
            base_key: [0; 16],
            predecoded_indexed,
            is_fshx_or_fsh: true,
            images,
            object_to_image,
            object_shapes,
            object_avatar_shapes,
            object_flags,
            cache: HashMap::new(),
        })
    }

    fn try_from_fsh_bytes(raw: &[u8]) -> Option<Self> {
        let chunks = parse_fsh_indexed_chunks(raw)?;
        let mut predecoded_indexed = Vec::with_capacity(chunks.len());
        let mut images = Vec::with_capacity(chunks.len());
        let mut object_to_image = HashMap::with_capacity(chunks.len());
        let mut object_shapes = HashMap::with_capacity(chunks.len());
        let mut object_avatar_shapes = HashMap::with_capacity(chunks.len());

        for (i, ch) in chunks.into_iter().enumerate() {
            let pre_idx = predecoded_indexed.len();
            predecoded_indexed.push(ch.top_down_indexed);
            let img_idx = images.len();
            images.push(FoxImageEntry {
                offset: pre_idx,
                compressed_size: (ch.width as usize) * (ch.height as usize),
                width: ch.width,
                height: ch.height,
                format: FOX_FMT_PREDECODED_INDEXED,
            });
            let obj_id = i as u16;
            let offset = ch.offset;
            let layer = FoxLayer {
                category: 0,
                image_idx: img_idx,
                layer_offset: (0, 0),
            };
            object_to_image.insert(obj_id, img_idx);
            object_shapes.insert(
                (obj_id, 0),
                FoxShapeRef {
                    image_idx: img_idx,
                    dir: 0,
                    offset,
                    elev: (0, 0),
                    rule: (1, 1),
                },
            );
            object_avatar_shapes.insert(
                obj_id,
                vec![FoxAvatarShape {
                    purpose: 0,
                    state: 0,
                    dir: 0,
                    rule: (1, 1),
                    offset,
                    elev: (0, 0),
                    layers: vec![layer],
                    frames: vec![FoxFrame {
                        offset,
                        elev: (0, 0),
                        layers: vec![layer],
                    }],
                    anim_steps: Vec::new(),
                }],
            );
        }

        Some(Self {
            raw: Vec::new(),
            enc_flag: 0,
            base_key: [0; 16],
            predecoded_indexed,
            is_fshx_or_fsh: true,
            images,
            object_to_image,
            object_shapes,
            object_avatar_shapes,
            object_flags: HashMap::new(),
            cache: HashMap::new(),
        })
    }

    /// Decodes (and caches) image `idx` into RGBA8888 pixels.
    pub fn decode_image(&mut self, idx: usize, palette: &Palette) -> Result<&FoxSprite, FoxError> {
        if self.cache.contains_key(&idx) {
            return Ok(&self.cache[&idx]);
        }
        let entry = *self
            .images
            .get(idx)
            .ok_or(FoxError::ImageOutOfBounds(idx))?;

        if entry.width > MAX_FOX_IMAGE_DIM || entry.height > MAX_FOX_IMAGE_DIM {
            return Err(FoxError::InvalidCommandBlock);
        }
        let pixel_count = (entry.width as usize)
            .checked_mul(entry.height as usize)
            .ok_or(FoxError::InvalidCommandBlock)?;
        let rgba_bytes = pixel_count
            .checked_mul(4)
            .ok_or(FoxError::InvalidCommandBlock)?;
        let mut rgba = vec![0u8; rgba_bytes];

        if entry.format == FOX_FMT_PREDECODED_INDEXED {
            let indexed = self
                .predecoded_indexed
                .get(entry.offset)
                .ok_or(FoxError::ImageOutOfBounds(idx))?;
            if indexed.len() >= pixel_count {
                decode_indexed_to_rgba(&indexed[..pixel_count], palette, &mut rgba);
            }
        } else {
            let end = entry.offset.saturating_add(entry.compressed_size);
            if end > self.raw.len() {
                return Err(FoxError::ImageOutOfBounds(idx));
            }

            let raw_slice = &self.raw[entry.offset..end];
            let decompressed = if self.enc_flag == 1 {
                let bpp: u32 = if entry.format == 1 { 4 } else { 1 };
                let uncomp_bytes = u32::from(entry.width)
                    .checked_mul(u32::from(entry.height))
                    .and_then(|n| n.checked_mul(bpp))
                    .ok_or(FoxError::InvalidCommandBlock)?;
                let key = derive_fox_key(&self.base_key, entry.compressed_size as u32, uncomp_bytes);
                let dec = rc4_crypt(&key, raw_slice);
                decompress_lzma(&dec)?
            } else {
                decompress_lzma(raw_slice)?
            };

            if entry.format == 1 && decompressed.len() >= pixel_count * 4 {
                // 32-bit ARGB -> RGBA
                for (i, src) in decompressed[..pixel_count * 4].as_chunks::<4>().0.iter().enumerate() {
                    let dst = i * 4;
                    rgba[dst] = src[1];
                    rgba[dst + 1] = src[2];
                    rgba[dst + 2] = src[3];
                    rgba[dst + 3] = src[0];
                }
            } else if decompressed.len() >= pixel_count {
                decode_indexed_to_rgba(&decompressed[..pixel_count], palette, &mut rgba);
            }
        }

        self.cache.insert(
            idx,
            FoxSprite {
                width: entry.width,
                height: entry.height,
                rgba,
            },
        );
        Ok(&self.cache[&idx])
    }

    /// Resolves a floor/object ID to its 1x image index.
    pub fn resolve_image_index(&self, object_id: u16) -> Option<usize> {
        self.object_to_image.get(&object_id).copied()
    }

    /// Chooses the best matching shape for `object_id` using 1:1-scale preference
    /// and optional `(purpose, dir, state)` scoring (`+8` / `+4` / `+2` / `+1`).
    pub fn pick_shape(
        &self,
        object_id: u16,
        purpose: Option<u8>,
        dir: Option<u8>,
        state: Option<u8>,
    ) -> Option<&FoxAvatarShape> {
        self.pick_shape_filtered(object_id, purpose, dir, state, false)
    }

    fn pick_shape_filtered(
        &self,
        object_id: u16,
        purpose: Option<u8>,
        dir: Option<u8>,
        state: Option<u8>,
        require_purpose: bool,
    ) -> Option<&FoxAvatarShape> {
        let shapes = self.object_avatar_shapes.get(&object_id)?;
        let mut best: Option<&FoxAvatarShape> = None;
        let mut best_score: i32 = -1;
        for s in shapes {
            if s.frames.is_empty() && s.layers.is_empty() {
                continue;
            }
            if require_purpose && purpose.is_some_and(|p| s.purpose != p) {
                continue;
            }
            let mut score: i32 = 0;
            if s.rule.0 == s.rule.1 || (s.rule.1 == 0 && s.rule.0 <= 1) {
                score += 8;
            }
            if purpose.is_some_and(|p| s.purpose == p) {
                score += 4;
            }
            if dir.is_some_and(|d| s.dir == d) {
                score += 2;
            }
            if state.is_some_and(|st| s.state == st) {
                score += 1;
            }
            if score > best_score {
                best = Some(s);
                best_score = score;
            }
        }
        best
    }

    /// Resolves `(object_id, dir)` to its 1x shape reference (image index + offset + elev).
    /// Uses `pick_shape` scoring and falls back to `object_shapes` / `object_to_image`.
    pub fn resolve_shape(&self, object_id: u16, dir: u8) -> Option<FoxShapeRef> {
        if let Some(shape) = self.pick_shape(object_id, None, Some(dir), None) {
            let layers = if !shape.layers.is_empty() {
                shape.layers.as_slice()
            } else {
                shape
                    .frames
                    .iter()
                    .find(|f| !f.layers.is_empty())
                    .map_or(&[][..], |f| f.layers.as_slice())
            };
            if let Some(layer) = layers
                .iter()
                .find(|l| l.category != 0x0050)
                .or_else(|| layers.first())
            {
                return Some(FoxShapeRef {
                    image_idx: layer.image_idx,
                    dir: shape.dir,
                    offset: shape.offset,
                    elev: shape.elev,
                    rule: shape.rule,
                });
            }
        }
        if let Some(&shape) = self.object_shapes.get(&(object_id, dir)) {
            return Some(shape);
        }
        if dir != 0 {
            if let Some(&shape) = self.object_shapes.get(&(object_id, 0)) {
                return Some(shape);
            }
        }
        self.object_to_image.get(&object_id).map(|&image_idx| FoxShapeRef {
            image_idx,
            dir: 0,
            offset: (0, 0),
            elev: (0, 0),
            rule: (1, 1),
        })
    }

    /// Resolves full multi-frame / multi-layer 1x shape data for `(object_id, dir)`.
    pub fn resolve_object_shape(&self, object_id: u16, dir: u8) -> Option<&FoxAvatarShape> {
        self.pick_shape(object_id, None, Some(dir), None)
    }

    /// Resolves a specific `(purpose, optional state, dir)` 1x shape (used for `portals.fox` `p=28` pad & `p=29` animated swirl, and species avatars).
    pub fn resolve_object_shape_ps(
        &self,
        object_id: u16,
        purpose: u8,
        state: Option<u8>,
        dir: u8,
    ) -> Option<&FoxAvatarShape> {
        self.pick_shape_filtered(object_id, Some(purpose), Some(dir), state, true)
    }

    /// Returns whether FOX5 object `'!'` flag bit 0 is set (`[+0x68] & 1`),
    /// which clamps avatar walk/stand poses `1..=4` to `2`.
    pub fn object_pose_clamp(&self, object_id: u16) -> bool {
        self.object_flags
            .get(&object_id)
            .or_else(|| self.object_flags.get(&0))
            .copied()
            .unwrap_or(0)
            & 1
            != 0
    }

    /// Returns true if `object_id` declares any 1x portal pad (`p=28`) or swirl (`p=29`) shape.
    /// Returns true if this archive uses paired sequential (FSH-style D=0) wall objects
    /// rather than directional (D=7 / D=9) wall shapes.
    pub fn uses_paired_wall_ids(&self) -> bool {
        self.is_fshx_or_fsh
            || !self
                .object_avatar_shapes
                .values()
                .flatten()
                .any(|s| s.dir == 7 || s.dir == 9)
    }

    pub fn has_portal_shapes(&self, object_id: u16) -> bool {
        self.object_avatar_shapes.get(&object_id).is_some_and(|shapes| {
            shapes.iter().any(|s| {
                s.rule == (1, 1)
                    && matches!(s.purpose, 28 | 29)
                    && (!s.frames.is_empty() || !s.layers.is_empty())
            })
        })
    }

    /// Resolves the best matching `p=29, R=(1,1)` portal swirl shape with `state <= dream_slot`,
    /// matching `Furcadia.exe` `0x435950`.
    pub fn resolve_portal_swirl_shape(
        &self,
        object_id: u16,
        dream_slot: usize,
    ) -> Option<&FoxAvatarShape> {
        let shapes = self.object_avatar_shapes.get(&object_id)?;
        let mut best: Option<&FoxAvatarShape> = None;
        for s in shapes {
            if s.rule == (1, 1)
                && s.purpose == 29
                && (!s.frames.is_empty() || !s.layers.is_empty())
                && (s.state as usize) <= dream_slot
                && best.is_none_or(|prev| s.state >= prev.state)
            {
                best = Some(s);
            }
        }
        best
    }

    /// Resolves an avatar `FoxAvatarShape` (`p=11`, `R=(1,1)` preferred) supporting both static
    /// layers and animated KitterSpeak `frames`. When `allow_obj0_fallback` is false, only checks
    /// the exact `object_id` so multi-archive callers can try `dream` then `default` before `0`.
    pub fn resolve_avatar_shape_opt(
        &self,
        object_id: u16,
        gender: u8,
        dir: u8,
        pose: u8,
        allow_obj0_fallback: bool,
    ) -> Option<&FoxAvatarShape> {
        let shapes = self.object_avatar_shapes.get(&object_id).or_else(|| {
            if allow_obj0_fallback {
                self.object_avatar_shapes.get(&0)
            } else {
                None
            }
        })?;
        let d_idx = (dir & 3) as usize;
        const WIRE_TO_FOX_DIR: [u8; 4] = [1, 3, 7, 9];
        let fox_dir = WIRE_TO_FOX_DIR[d_idx];
        let pose_hi = match pose {
            0 | 5 => 0x4,
            1 => 0x1,
            2 | 4 => 0x3,
            3 => 0x5,
            6 => 0x2,
            _ => 0x3,
        };
        const GENDER_BLOCK: [usize; 3] = [1, 2, 0];
        const GENDER_NIBBLE: [u8; 3] = [1, 2, 4];
        let g_idx = (gender as usize).min(2);
        let want_nibble = GENDER_NIBBLE[g_idx];

        let find_by_tags = |nibble: u8| -> Option<&FoxAvatarShape> {
            shapes.iter().find(|s| {
                s.purpose == 11
                    && s.rule == (1, 1)
                    && s.dir == fox_dir
                    && (s.state >> 4) == pose_hi
                    && (s.state & 0x0f) == nibble
                    && (!s.layers.is_empty() || !s.frames.is_empty())
            })
        };

        if let Some(sh) = find_by_tags(want_nibble)
            .or_else(|| find_by_tags(0x4))
            .or_else(|| find_by_tags(0x2))
            .or_else(|| find_by_tags(0x1))
            .or_else(|| find_by_tags(0x0))
        {
            return Some(sh);
        }

        // Positional fallback matching 0x4043a0 + 0x580c58[gender] * 20
        let base_slot = match pose {
            0..=3 => d_idx * 4 + pose as usize,
            4 => d_idx * 4 + 2,
            5 => d_idx * 4,
            6 => {
                let d6 = if (dir & 2) != 0 { (dir & 3) ^ 1 } else { dir & 3 };
                16 + d6 as usize
            }
            _ => d_idx * 4 + 2,
        };
        let avatar_1x: Vec<&FoxAvatarShape> = shapes
            .iter()
            .filter(|s| s.purpose == 11 && s.rule == (1, 1) && (!s.layers.is_empty() || !s.frames.is_empty()))
            .collect();
        let gender_blocks = (avatar_1x.len() / 20).max(1);
        let block = GENDER_BLOCK[g_idx];
        let slot_sh = if block < gender_blocks {
            avatar_1x.get(block * 20 + base_slot)
        } else {
            None
        };
        slot_sh
            .or_else(|| avatar_1x.get(base_slot))
            .or_else(|| avatar_1x.first())
            .copied()
    }

    /// Resolves an avatar object's directional/pose layers `(shape_offset, &[FoxLayer])`.
    /// Matches Furcadia.exe's `0x431fb0` + `0x4043a0` lookup (`gender` nibble `[1, 2, 4]`,
    /// wire `dir` `0..3 -> D [1, 3, 7, 9]`, and `pose` `0..=6`), preferring 1x (`R=(1,1)`).
    pub fn resolve_avatar_layers(
        &self,
        object_id: u16,
        gender: u8,
        dir: u8,
        pose: u8,
    ) -> Option<((i16, i16), &[FoxLayer])> {
        let sh = self.resolve_avatar_shape_opt(object_id, gender, dir, pose, true)?;
        if sh.layers.is_empty() {
            return None;
        }
        Some((sh.offset, sh.layers.as_slice()))
    }
}

struct FshIndexedChunk {
    width: u16,
    height: u16,
    offset: (i16, i16),
    top_down_indexed: Vec<u8>,
}

fn decode_indexed_to_rgba(indexed: &[u8], palette: &Palette, rgba: &mut [u8]) {
    for (i, &pal_idx) in indexed.iter().enumerate() {
        let dst = i * 4;
        if pal_idx == 0 {
            rgba[dst + 3] = 0;
        } else {
            let [r, g, b] = palette.rgb(pal_idx);
            rgba[dst] = r;
            rgba[dst + 1] = g;
            rgba[dst + 2] = b;
            rgba[dst + 3] = 255;
        }
    }
}

fn parse_fsh_indexed_chunks(raw: &[u8]) -> Option<Vec<FshIndexedChunk>> {
    if raw.len() < 2 {
        return None;
    }
    let count = u16::from_le_bytes([raw[0], raw[1]]) as usize;
    let table_end = 2usize.checked_add(count.checked_mul(2)?)?;
    if !(1..=4096).contains(&count) || table_end > raw.len() {
        return None;
    }
    let mut out = Vec::with_capacity(count);
    let mut pos = table_end;
    for i in 0..count {
        let sz = u16::from_le_bytes([raw[2 + i * 2], raw[3 + i * 2]]) as usize;
        if pos + sz > raw.len() {
            return None;
        }
        if sz == 0 {
            out.push(FshIndexedChunk {
                width: 0,
                height: 0,
                offset: (0, 0),
                top_down_indexed: Vec::new(),
            });
            continue;
        }
        if sz < 4 {
            return None;
        }
        let chunk = &raw[pos..pos + sz];
        pos += sz;
        let w = chunk[0] as usize;
        let h = chunk[1] as usize;
        let ox = chunk[2] as i8 as i16;
        let oy = chunk[3] as i8 as i16;
        let pixels = &chunk[4..];
        if pixels.len() != w * h {
            return None;
        }
        let mut top_down = vec![0u8; w * h];
        for dst_y in 0..h {
            let src_y = h - 1 - dst_y;
            let src_row = src_y * w;
            let dst_row = dst_y * w;
            top_down[dst_row..dst_row + w].copy_from_slice(&pixels[src_row..src_row + w]);
        }
        out.push(FshIndexedChunk {
            width: w as u16,
            height: h as u16,
            offset: (ox, oy),
            top_down_indexed: top_down,
        });
    }
    Some(out)
}

/// Decodes a classic Furcadia `.fsh` indexed image archive (`smileys.fsh`, `smileys2.fsh`) into RGBA sprites.
pub fn decode_fsh_sprites(raw: &[u8], palette: &Palette) -> Vec<FoxSprite> {
    if raw.len() < 2 {
        return Vec::new();
    }
    let count = u16::from_le_bytes([raw[0], raw[1]]) as usize;
    let table_end = 2usize.saturating_add(count.saturating_mul(2));
    if table_end > raw.len() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(count);
    let mut pos = table_end;
    for i in 0..count {
        let sz = u16::from_le_bytes([raw[2 + i * 2], raw[3 + i * 2]]) as usize;
        if pos + sz > raw.len() || sz < 4 {
            break;
        }
        let chunk = &raw[pos..pos + sz];
        pos += sz;
        let w = chunk[0] as usize;
        let h = chunk[1] as usize;
        let pixels = &chunk[4..];
        if pixels.len() < w * h {
            continue;
        }
        let mut top_down = vec![0u8; w * h];
        for dst_y in 0..h {
            let src_y = h - 1 - dst_y;
            top_down[dst_y * w..(dst_y + 1) * w].copy_from_slice(&pixels[src_y * w..(src_y + 1) * w]);
        }
        let mut rgba = vec![0u8; w * h * 4];
        decode_indexed_to_rgba(&top_down, palette, &mut rgba);
        out.push(FoxSprite {
            width: w as u16,
            height: h as u16,
            rgba,
        });
    }
    out
}

const FOX_C0: [u8; 8] = [0x32, 0xba, 0xbd, 0xbb, 0xea, 0x4f, 0x9e, 0x06];
const FOX_C1: [u8; 8] = [0x66, 0x55, 0x0f, 0xbc, 0x66, 0xc9, 0xb6, 0x6f];
const FOX_C2: [u8; 8] = [0xff, 0x77, 0x4e, 0x39, 0x8a, 0x18, 0xff, 0xdb];
const FOX_C3: [u8; 8] = [0x69, 0x28, 0xeb, 0xe6, 0x2b, 0x25, 0xc3, 0xaa];

fn derive_fox_key(base16: &[u8; 16], compressed_size: u32, uncompressed_size: u32) -> [u8; 16] {
    let mut key = *base16;
    let edx = if (compressed_size & 4) != 0 { &FOX_C2 } else { &FOX_C3 };
    let ecx = if (compressed_size & 8) != 0 { &FOX_C0 } else { &FOX_C1 };
    for i in 0..8 {
        key[i] ^= edx[i];
        key[i + 8] ^= ecx[i];
    }
    let ub = uncompressed_size.to_be_bytes();
    for i in 0..4 {
        key[i + 4] ^= ub[i];
    }
    key
}

fn rc4_crypt(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut s = [0u8; 256];
    for (i, slot) in s.iter_mut().enumerate() {
        *slot = i as u8;
    }
    let mut j = 0u8;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, j as usize);
    }
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0u8;
    j = 0;
    for &b in data {
        i = i.wrapping_add(1);
        j = j.wrapping_add(s[i as usize]);
        s.swap(i as usize, j as usize);
        let k = s[(s[i as usize].wrapping_add(s[j as usize])) as usize];
        out.push(b ^ k);
    }
    out
}

/// `Write` adapter that aborts with `ErrorKind::InvalidData` if output exceeds `limit` bytes.
struct BoundedWriter {
    buf: Vec<u8>,
    limit: usize,
}

impl BoundedWriter {
    fn new(limit: usize) -> Self {
        Self {
            buf: Vec::new(),
            limit,
        }
    }
}

impl Write for BoundedWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.buf.len().saturating_add(data.len()) > self.limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("decompressed LZMA stream exceeded {}-byte safety limit", self.limit),
            ));
        }
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn decompress_lzma(compressed: &[u8]) -> Result<Vec<u8>, FoxError> {
    let mut reader = BufReader::new(compressed);
    let mut out = BoundedWriter::new(MAX_LZMA_OUTPUT_BYTES);
    lzma_rs::lzma_decompress(&mut reader, &mut out).map_err(|e| FoxError::Lzma(e.to_string()))?;
    Ok(out.buf)
}

#[derive(Default)]
struct ShapeBuilder {
    purpose: u8,
    state: u8,
    dir: u8,
    rule: (u8, u8),
    offset: (i16, i16),
    elev: (i16, i16),
    layers: Vec<FoxLayer>,
    frames: Vec<FoxFrame>,
    anim_steps: Vec<FoxAnimStep>,
}

/// Skips the payload of a non-container (`L` / `<`) FOX5 opcode safely.
fn skip_fox_op_payload(op: u8, s: &[u8], pos: &mut usize) {
    match op {
        b'e' => {}
        b'g' | b'l' | b'N' | b'p' | b's' | b'D' | b'!' | b't' => {
            *pos = (*pos + 1).min(s.len());
        }
        b'R' | b'F' | b'r' | b'C' | b'c' => {
            *pos = (*pos + 2).min(s.len());
        }
        b'i' | b'o' | b'f' | b'S' | b'O' | b'?' => {
            *pos = (*pos + 4).min(s.len());
        }
        b'T' => {
            *pos = (*pos + 8).min(s.len());
        }
        b'u' => {
            if *pos + 6 <= s.len() {
                let l =
                    u32::from_be_bytes([s[*pos + 2], s[*pos + 3], s[*pos + 4], s[*pos + 5]])
                        as usize;
                *pos = (*pos + 6).saturating_add(l).min(s.len());
            } else {
                *pos = s.len();
            }
        }
        b'U' => {
            if *pos < s.len() {
                let cnt = s[*pos] as usize;
                *pos = (*pos + 1 + cnt * 7).min(s.len());
            } else {
                *pos = s.len();
            }
        }
        b'n' | b'd' | b'P' | b'h' => {
            if *pos + 2 <= s.len() {
                let l = u16::from_be_bytes([s[*pos], s[*pos + 1]]) as usize;
                *pos = (*pos + 2 + l).min(s.len());
            } else {
                *pos = s.len();
            }
        }
        b'a' | b'k' => {
            if *pos + 2 <= s.len() {
                let cnt = u16::from_be_bytes([s[*pos], s[*pos + 1]]) as usize;
                *pos += 2;
                for _ in 0..cnt {
                    if *pos + 2 > s.len() {
                        *pos = s.len();
                        break;
                    }
                    let l = u16::from_be_bytes([s[*pos], s[*pos + 1]]) as usize;
                    *pos = (*pos + 2 + l).min(s.len());
                }
            } else {
                *pos = s.len();
            }
        }
        b'K' => {
            if *pos + 2 <= s.len() {
                let steps = u16::from_be_bytes([s[*pos], s[*pos + 1]]) as usize;
                *pos = (*pos + 2 + steps * 6).min(s.len());
            } else {
                *pos = s.len();
            }
        }
        _ => {}
    }
}

fn read_be_u16(s: &[u8], pos: &mut usize) -> Option<u16> {
    if *pos + 2 > s.len() {
        *pos = s.len();
        return None;
    }
    let v = u16::from_be_bytes([s[*pos], s[*pos + 1]]);
    *pos += 2;
    Some(v)
}

fn read_be_i16_pair(s: &[u8], pos: &mut usize) -> Option<(i16, i16)> {
    if *pos + 4 > s.len() {
        *pos = s.len();
        return None;
    }
    let x = i16::from_be_bytes([s[*pos], s[*pos + 1]]);
    let y = i16::from_be_bytes([s[*pos + 2], s[*pos + 3]]);
    *pos += 4;
    Some((x, y))
}

fn read_level_header(s: &[u8], pos: &mut usize) -> Option<(u8, usize)> {
    if *pos + 5 > s.len() {
        *pos = s.len();
        return None;
    }
    let lvl = s[*pos];
    let cnt = u32::from_be_bytes([s[*pos + 1], s[*pos + 2], s[*pos + 3], s[*pos + 4]]) as usize;
    *pos += 5;
    Some((lvl, cnt))
}

/// Parses the FOX5 `L 0x01 <obj_count>` command tree into `(object_to_image, object_shapes, object_avatar_shapes, object_flags)`.
#[allow(clippy::type_complexity)]
fn extract_object_shapes(
    stream: &[u8],
    image_count: usize,
) -> (
    HashMap<u16, usize>,
    HashMap<(u16, u8), FoxShapeRef>,
    HashMap<u16, Vec<FoxAvatarShape>>,
    HashMap<u16, u8>,
) {
    let mut to_image = HashMap::new();
    let mut shapes = HashMap::new();
    let mut avatar_shapes = HashMap::new();
    let mut object_flags = HashMap::new();

    let Some(l1_off) = stream.windows(2).position(|w| w == [b'L', 1]) else {
        return (to_image, shapes, avatar_shapes, object_flags);
    };
    if l1_off + 6 > stream.len() {
        return (to_image, shapes, avatar_shapes, object_flags);
    }
    let obj_count = u32::from_be_bytes([
        stream[l1_off + 2],
        stream[l1_off + 3],
        stream[l1_off + 4],
        stream[l1_off + 5],
    ]) as usize;
    let mut pos = l1_off + 6;
    let mut next_seq_id: u16 = 0;

    for _ in 0..obj_count {
        if pos >= stream.len() {
            break;
        }
        if parse_one_fox_object(
            stream,
            &mut pos,
            &mut next_seq_id,
            image_count,
            &mut to_image,
            &mut shapes,
            &mut avatar_shapes,
            &mut object_flags,
        )
        .is_none()
        {
            break;
        }
    }

    (to_image, shapes, avatar_shapes, object_flags)
}

#[allow(clippy::too_many_arguments)]
fn parse_one_fox_object(
    s: &[u8],
    pos: &mut usize,
    next_seq_id: &mut u16,
    image_count: usize,
    to_image: &mut HashMap<u16, usize>,
    shapes: &mut HashMap<(u16, u8), FoxShapeRef>,
    avatar_shapes: &mut HashMap<u16, Vec<FoxAvatarShape>>,
    object_flags: &mut HashMap<u16, u8>,
) -> Option<()> {
    let mut explicit_id: Option<u32> = None;
    let mut flags_byte: Option<u8> = None;
    let mut built_shapes: Vec<ShapeBuilder> = Vec::new();

    while *pos < s.len() {
        let op = s[*pos];
        *pos += 1;
        match op {
            b'<' => break,
            b'L' => {
                let (lvl, cnt) = read_level_header(s, pos)?;
                if lvl == 2 {
                    for _ in 0..cnt {
                        if let Some(sb) = parse_level2_shape(s, pos, image_count) {
                            built_shapes.push(sb);
                        }
                    }
                } else {
                    for _ in 0..cnt {
                        skip_fox_level(s, pos);
                    }
                }
            }
            b'i' => {
                if *pos + 4 <= s.len() {
                    explicit_id = Some(u32::from_be_bytes([
                        s[*pos],
                        s[*pos + 1],
                        s[*pos + 2],
                        s[*pos + 3],
                    ]));
                    *pos += 4;
                }
            }
            b'!' if *pos < s.len() => {
                flags_byte = Some(flags_byte.unwrap_or(0) | s[*pos]);
                *pos += 1;
            }
            b'?' if *pos + 4 <= s.len() => {
                let v = u32::from_be_bytes([s[*pos], s[*pos + 1], s[*pos + 2], s[*pos + 3]]);
                flags_byte = Some(flags_byte.unwrap_or(0) | ((v & 0xFF) as u8));
                *pos += 4;
            }
            _ => skip_fox_op_payload(op, s, pos),
        }
    }

    let obj_id = explicit_id.map(|v| v as u16).unwrap_or(*next_seq_id);
    *next_seq_id = obj_id.saturating_add(1);
    if let Some(flags) = flags_byte {
        object_flags.insert(obj_id, flags);
    }
    let mut av_list = Vec::with_capacity(built_shapes.len());
    for sb in built_shapes {
        let Some(first_non_empty) = sb.frames.iter().find(|f| !f.layers.is_empty()) else {
            continue;
        };
        let rep_offset = if sb.frames.first().is_some_and(|f| !f.layers.is_empty()) {
            sb.offset
        } else {
            first_non_empty.offset
        };
        let rep_elev = if sb.frames.first().is_some_and(|f| !f.layers.is_empty()) {
            sb.elev
        } else {
            first_non_empty.elev
        };
        let rep_layers = if !sb.layers.is_empty() {
            sb.layers
        } else {
            first_non_empty.layers.clone()
        };
        // Prefer first non-shadow layer (`C != 0x0050`) as representative object sprite.
        let primary_img = rep_layers
            .iter()
            .find(|l| l.category != 0x0050)
            .or_else(|| rep_layers.first())
            .map(|l| l.image_idx);
        if let Some(image_idx) = primary_img {
            let candidate = FoxShapeRef {
                image_idx,
                dir: sb.dir,
                offset: rep_offset,
                elev: rep_elev,
                rule: sb.rule,
            };
            let replace = match shapes.get(&(obj_id, sb.dir)) {
                None => true,
                Some(prev) => prev.rule != (1, 1) && sb.rule == (1, 1),
            };
            if replace {
                shapes.insert((obj_id, sb.dir), candidate);
            }
            let replace_default = match to_image.get(&obj_id) {
                None => true,
                Some(_) => sb.rule == (1, 1) && sb.dir == 0,
            };
            if replace_default {
                to_image.insert(obj_id, image_idx);
            }
        }
        av_list.push(FoxAvatarShape {
            purpose: sb.purpose,
            state: sb.state,
            dir: sb.dir,
            rule: sb.rule,
            offset: rep_offset,
            elev: rep_elev,
            layers: rep_layers,
            frames: sb.frames,
            anim_steps: sb.anim_steps,
        });
    }
    if !av_list.is_empty() {
        avatar_shapes.insert(obj_id, av_list);
    }
    Some(())
}

fn parse_level2_shape(s: &[u8], pos: &mut usize, image_count: usize) -> Option<ShapeBuilder> {
    let mut sb = ShapeBuilder {
        rule: (1, 1),
        ..Default::default()
    };
    while *pos < s.len() {
        let op = s[*pos];
        *pos += 1;
        match op {
            b'<' => break,
            b'L' => {
                let (lvl, cnt) = read_level_header(s, pos)?;
                if lvl == 3 {
                    for idx in 0..cnt {
                        let frame = parse_level3_dir(s, pos, image_count);
                        if idx == 0 {
                            sb.offset = frame.offset;
                            sb.elev = frame.elev;
                            sb.layers = frame.layers.clone();
                        }
                        sb.frames.push(frame);
                    }
                } else {
                    for _ in 0..cnt {
                        skip_fox_level(s, pos);
                    }
                }
            }
            b'p' if *pos < s.len() => {
                sb.purpose = s[*pos];
                *pos += 1;
            }
            b's' if *pos < s.len() => {
                sb.state = s[*pos];
                *pos += 1;
            }
            b'D' if *pos < s.len() => {
                sb.dir = s[*pos];
                *pos += 1;
            }
            b'R' => {
                if *pos + 2 <= s.len() {
                    sb.rule = (s[*pos], s[*pos + 1]);
                    *pos += 2;
                }
            }
            b'K' => {
                if let Some(steps) = read_be_u16(s, pos) {
                    for _ in 0..steps {
                        if *pos + 6 > s.len() {
                            *pos = s.len();
                            break;
                        }
                        let op = u16::from_be_bytes([s[*pos], s[*pos + 1]]);
                        let arg1 = u16::from_be_bytes([s[*pos + 2], s[*pos + 3]]);
                        let arg2 = u16::from_be_bytes([s[*pos + 4], s[*pos + 5]]);
                        *pos += 6;
                        sb.anim_steps.push(FoxAnimStep { op, arg1, arg2 });
                    }
                }
            }
            _ => skip_fox_op_payload(op, s, pos),
        }
    }
    Some(sb)
}

fn parse_level3_dir(s: &[u8], pos: &mut usize, image_count: usize) -> FoxFrame {
    let mut offset: Option<(i16, i16)> = None;
    let mut elev: Option<(i16, i16)> = None;
    let mut layers: Vec<FoxLayer> = Vec::new();
    while *pos < s.len() {
        let op = s[*pos];
        *pos += 1;
        match op {
            b'<' => break,
            b'L' => {
                let Some((lvl, cnt)) = read_level_header(s, pos) else {
                    break;
                };
                if lvl == 4 {
                    let mut prev_category: u16 = 0;
                    let mut next_image_one_based: u16 = 0;
                    for _ in 0..cnt {
                        if let Some(layer) = parse_level4_frame(
                            s,
                            pos,
                            image_count,
                            &mut prev_category,
                            &mut next_image_one_based,
                        ) {
                            layers.push(layer);
                        }
                    }
                } else {
                    for _ in 0..cnt {
                        skip_fox_level(s, pos);
                    }
                }
            }
            b'o' => {
                if let Some(pair) = read_be_i16_pair(s, pos) {
                    if offset.is_none() {
                        offset = Some(pair);
                    }
                }
            }
            b'f' => {
                if let Some(pair) = read_be_i16_pair(s, pos) {
                    if elev.is_none() {
                        elev = Some(pair);
                    }
                }
            }
            _ => skip_fox_op_payload(op, s, pos),
        }
    }
    FoxFrame {
        offset: offset.unwrap_or((0, 0)),
        elev: elev.unwrap_or((0, 0)),
        layers,
    }
}

fn parse_level4_frame(
    s: &[u8],
    pos: &mut usize,
    image_count: usize,
    prev_category: &mut u16,
    next_image_one_based: &mut u16,
) -> Option<FoxLayer> {
    let mut layer_offset: (i16, i16) = (0, 0);
    while *pos < s.len() {
        let op = s[*pos];
        *pos += 1;
        match op {
            b'<' => break,
            b'L' => {
                let Some((_, cnt)) = read_level_header(s, pos) else {
                    break;
                };
                for _ in 0..cnt {
                    skip_fox_level(s, pos);
                }
            }
            b'C' => {
                if let Some(v) = read_be_u16(s, pos) {
                    *prev_category = v;
                }
            }
            b'O' => {
                if let Some(pair) = read_be_i16_pair(s, pos) {
                    layer_offset = pair;
                }
            }
            b'c' => {
                if let Some(img_one_based) = read_be_u16(s, pos) {
                    *next_image_one_based = img_one_based;
                }
            }
            _ => skip_fox_op_payload(op, s, pos),
        }
    }
    let category = *prev_category;
    let img_one_based = *next_image_one_based;
    *next_image_one_based = img_one_based.saturating_add(1);
    if (category & 0x80) != 0 {
        return None;
    }
    let idx = (img_one_based as usize).checked_sub(1)?;
    (idx < image_count).then_some(FoxLayer {
        category,
        image_idx: idx,
        layer_offset,
    })
}

fn skip_fox_level(s: &[u8], pos: &mut usize) {
    while *pos < s.len() {
        let op = s[*pos];
        *pos += 1;
        match op {
            b'<' => break,
            b'L' => {
                let Some((_, cnt)) = read_level_header(s, pos) else {
                    break;
                };
                for _ in 0..cnt {
                    skip_fox_level(s, pos);
                }
            }
            _ => skip_fox_op_payload(op, s, pos),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::AssetLocator;

    #[test]
    fn decode_workspace_floors_fox_if_present() {
        let locator = AssetLocator::discover();
        let Some(ref root) = locator.root else {
            return;
        };
        let floors_path = root.join("patches/default/floors.fox");
        if !floors_path.is_file() {
            return;
        }
        let palette = locator.load_default_palette().unwrap_or_default();
        let mut fox = FoxArchive::load(&floors_path).unwrap();
        assert_eq!(fox.images.len(), 1875);

        let sprite = fox.decode_image(0, &palette).unwrap();
        assert_eq!(sprite.width, 62);
        assert_eq!(sprite.height, 32);
        assert_eq!(sprite.rgba.len(), 62 * 32 * 4);
        // Top-left corner pixel is transparent, center pixel is opaque.
        assert_eq!(sprite.rgba[3], 0);
        let mid = (16 * 62 + 31) * 4;
        assert_eq!(sprite.rgba[mid + 3], 255);
    }

    #[test]
    fn decode_workspace_species1_fox_if_present() {
        let locator = AssetLocator::discover();
        let Some(ref root) = locator.root else {
            return;
        };
        let path = root.join("patches/default/species1.fox");
        if !path.is_file() {
            return;
        }
        let palette = locator.load_default_palette().unwrap_or_default();
        let mut fox = FoxArchive::load(&path).unwrap();
        println!(
            "species1.fox: images={}, object_to_image={}",
            fox.images.len(),
            fox.object_to_image.len()
        );
        if let Some(first) = fox.images.first() {
            println!("first image: {}x{} fmt={}", first.width, first.height, first.format);
        }
        assert!(!fox.images.is_empty());
        let sprite = fox.decode_image(0, &palette).unwrap();
        assert!(sprite.width > 0 && sprite.height > 0);
        // Verify avatar layer resolution across genders & poses.
        let (off, layers) = fox.resolve_avatar_layers(0, 0, 0, 2).expect("avatar stand pose");
        assert!(off != (0, 0));
        assert!(layers.iter().any(|l| l.category == 0x0030));
    }

    #[test]
    fn decode_pmfurn_encrypted_fox_patches_if_present() {
        let path = Path::new("/tmp/pmfurn.bin");
        if !path.exists() {
            return;
        }
        let palette = AssetLocator::discover().load_default_palette().unwrap_or_default();
        let bytes = fs::read(path).unwrap();
        let entries = crate::rch::parse_fr01(&bytes).unwrap();
        for entry in entries {
            if !entry.clean_name.ends_with(".fox") {
                continue;
            }
            let mut fox = FoxArchive::from_bytes(entry.data)
                .unwrap_or_else(|e| panic!("failed to parse {}: {e}", entry.clean_name));
            assert!(!fox.object_shapes.is_empty() || !fox.object_to_image.is_empty());
            let total = fox.images.len();
            for idx in (0..total.min(8)).chain(total.saturating_sub(4)..total) {
                fox.decode_image(idx, &palette)
                    .unwrap_or_else(|e| panic!("{} image[{idx}] decode failed: {e}", entry.clean_name));
            }
        }
    }

    #[test]
    fn resolve_directional_walls_and_item_offsets_if_present() {
        let locator = AssetLocator::discover();
        let Some(ref root) = locator.root else {
            return;
        };
        let wall_path = root.join("patches/default/wall.fox");
        if wall_path.is_file() {
            let wall = FoxArchive::load(&wall_path).unwrap();
            let nw = wall.resolve_shape(0, 7).expect("NW wall shape");
            let ne = wall.resolve_shape(0, 9).expect("NE wall shape");
            assert_ne!(nw.image_idx, ne.image_idx);
            assert_eq!(nw.rule, (1, 1));
        }
        let items_path = root.join("patches/default/items.fox");
        if items_path.is_file() {
            let items = FoxArchive::load(&items_path).unwrap();
            assert_eq!(items.object_to_image.len(), 1397);
            // item_id 1337 is 1-based -> items.fox object 1336 ('Invisable Walkable Item', 1x1)
            if let Some(shape) = items.resolve_shape(1336, 0) {
                let entry = items.images[shape.image_idx];
                assert_eq!((entry.width, entry.height), (1, 1));
            }
        }
    }

    #[test]
    fn junction_solid_walls_and_species_gender0_lie_layers_if_present() {
        let locator = AssetLocator::discover();
        let Some(ref root) = locator.root else {
            return;
        };
        let junc_path = root.join("patches/default/junction.fox");
        if junc_path.is_file() {
            let junc = FoxArchive::load(&junc_path).unwrap();
            for obj_id in [6u16, 9, 21, 24] {
                let shape = junc
                    .resolve_shape(obj_id, 7)
                    .unwrap_or_else(|| panic!("missing junction solid wall obj {obj_id}"));
                let img = junc.images[shape.image_idx];
                assert!(img.width > 30 && img.height > 70);
            }
        }
        let sp1_path = root.join("patches/default/species1.fox");
        if sp1_path.is_file() {
            let sp1 = FoxArchive::load(&sp1_path).unwrap();
            let (_off, layers) = sp1
                .resolve_avatar_layers(0, 0, 0, 6)
                .expect("species1 lie pose");
            assert!(layers.iter().any(|l| l.category == 0x0030));
            assert!(layers.iter().any(|l| l.category == 0x0050));
        }
        let sp102_path = root.join("patches/default/species102.fox");
        if sp102_path.is_file() {
            let sp102 = FoxArchive::load(&sp102_path).unwrap();
            for g in 0..=2u8 {
                for pose in [0u8, 1, 2, 3, 6] {
                    let (_off, layers) = sp102
                        .resolve_avatar_layers(0, g, 0, pose)
                        .unwrap_or_else(|| panic!("species102 g={g} pose={pose} failed"));
                    assert!(!layers.is_empty());
                }
            }
        }
    }

    #[test]
    fn verify_floors_portals_elev_and_walls_if_present() {
        let locator = AssetLocator::discover();
        let Some(ref root) = locator.root else {
            return;
        };
        let floors_path = root.join("patches/default/floors.fox");
        if floors_path.is_file() {
            let floors = FoxArchive::load(&floors_path).unwrap();
            let s160 = floors.resolve_shape(160, 0).expect("floors[160] (Void)");
            let s161 = floors.resolve_shape(161, 0).expect("floors[161]");
            println!(
                "floors[160] img={} {:?}, floors[161] img={} {:?}",
                s160.image_idx,
                (floors.images[s160.image_idx].width, floors.images[s160.image_idx].height),
                s161.image_idx,
                (floors.images[s161.image_idx].width, floors.images[s161.image_idx].height),
            );
            assert_ne!(s160.image_idx, s161.image_idx);
        }
        let items_path = root.join("patches/default/items.fox");
        if items_path.is_file() {
            let items = FoxArchive::load(&items_path).unwrap();
            let i0 = items.resolve_shape(0, 0).expect("items[0]");
            assert_eq!(i0.elev, (0, -4));
        }
        let portals_path = root.join("patches/default/portals.fox");
        if portals_path.is_file() {
            let portals = FoxArchive::load(&portals_path).unwrap();
            let pad = portals
                .resolve_object_shape_ps(0, 28, Some(0), 0)
                .expect("portals[0] p=28 base pad");
            assert_eq!(pad.offset, (10, 12));
            let swirl = portals
                .resolve_object_shape_ps(0, 29, Some(0), 0)
                .expect("portals[0] p=29 swirl");
            assert!(swirl.frames.len() >= 10);
            assert!(!swirl.anim_steps.is_empty());
            assert!(swirl.frame_at(0).is_some());
            assert!(swirl.frame_at(500).is_some());
        }
        let effects_path = root.join("patches/default/effects.fox");
        if effects_path.is_file() {
            let effects = FoxArchive::load(&effects_path).unwrap();
            let e0 = effects.resolve_object_shape(0, 0).expect("effects[0]");
            assert_eq!(e0.frames.len(), 8);
            assert!(e0.frame_at(200).is_some());
        }
        let wall3_path = root.join("patches/default/wall_3.fox");
        if wall3_path.is_file() {
            let w3 = FoxArchive::load(&wall3_path).unwrap();
            let nw = w3.resolve_shape(0, 9).expect("wall_3[0] D=9");
            let ne = w3.resolve_shape(0, 7).expect("wall_3[0] D=7");
            assert_ne!(nw.image_idx, ne.image_idx);
            assert_eq!(nw.offset, (16, -25));
        }
    }

    #[test]
    fn gender_nibble_map_and_flags_match_furcadia_exe() {
        // Test sequential auto-increment object IDs + '!' pose-clamp flag + p=28/29 portal helpers
        // via direct FoxArchive command stream decoding.
        // Object 0: explicit i=5, !=1 (pose clamp), L2(p=11, s=0x41, D=1, R=(1,1), L3->L4(c=1))
        // Object 1: implicit ID=6 (5+1), L2(p=29, s=3, D=0, R=(1,1), L3->L4(c=1))
        let mut cmd = Vec::new();
        // Version / header bytes don't matter to extract_object_shapes: starts at L 0x01 <count=2>
        cmd.extend_from_slice(b"L\x01");
        cmd.extend_from_slice(&2u32.to_be_bytes());
        // Object 0
        cmd.extend_from_slice(b"i");
        cmd.extend_from_slice(&5u32.to_be_bytes());
        cmd.extend_from_slice(b"!\x01");
        cmd.extend_from_slice(b"L\x02");
        cmd.extend_from_slice(&3u32.to_be_bytes());
        // Three shapes: male (nibble 1 -> wire 0), unspec (nibble 2 -> wire 1), female (nibble 4 -> wire 2)
        for (nibble, dir_val) in [(1u8, 1u8), (2u8, 3u8), (4u8, 7u8)] {
            cmd.extend_from_slice(&[b'p', 11, b's', 0x40 | nibble, b'D', dir_val, b'R', 1, 1]);
            cmd.extend_from_slice(b"L\x03");
            cmd.extend_from_slice(&1u32.to_be_bytes());
            cmd.extend_from_slice(b"o\x00\x01\x00\x02L\x04");
            cmd.extend_from_slice(&1u32.to_be_bytes());
            cmd.extend_from_slice(b"c\x00\x01<<<");
        }
        cmd.push(b'<');
        // Object 1 (auto-incremented ID = 6) with p=29 portal swirl state=3
        cmd.extend_from_slice(b"L\x02");
        cmd.extend_from_slice(&1u32.to_be_bytes());
        cmd.extend_from_slice(&[b'p', 29, b's', 3, b'D', 0, b'R', 1, 1]);
        cmd.extend_from_slice(b"L\x03");
        cmd.extend_from_slice(&1u32.to_be_bytes());
        cmd.extend_from_slice(b"L\x04");
        cmd.extend_from_slice(&1u32.to_be_bytes());
        cmd.extend_from_slice(b"c\x00\x01<<<<");

        let (to_image, shapes, avatar_shapes, object_flags) = extract_object_shapes(&cmd, 1);
        let archive = FoxArchive {
            raw: Vec::new(),
            enc_flag: 0,
            base_key: [0; 16],
            predecoded_indexed: Vec::new(),
            is_fshx_or_fsh: false,
            images: vec![FoxImageEntry {
                offset: 0,
                compressed_size: 0,
                width: 1,
                height: 1,
                format: 0,
            }],
            object_to_image: to_image,
            object_shapes: shapes,
            object_avatar_shapes: avatar_shapes,
            object_flags,
            cache: HashMap::new(),
        };
        assert!(archive.object_pose_clamp(5));
        assert!(!archive.object_pose_clamp(6));
        assert!(archive.has_portal_shapes(6));
        assert!(archive.resolve_portal_swirl_shape(6, 2).is_none());
        assert_eq!(archive.resolve_portal_swirl_shape(6, 3).unwrap().state, 3);
        // Wire gender 1 selects nibble 0x2 (stored under D=3 in this synthetic object).
        let (_off, layers) = archive.resolve_avatar_layers(5, 1, 1, 0).unwrap();
        assert_eq!(layers[0].image_idx, 0);
        // Wire gender 2 selects nibble 0x4 (stored under D=7 in this synthetic object).
        let (_off, layers2) = archive.resolve_avatar_layers(5, 2, 2, 0).unwrap();
        assert_eq!(layers2[0].image_idx, 0);
        // Real species1 gender 1 vs 2 distinction if assets are present on disk.
        let locator = AssetLocator::discover();
        if let Some(sp1_path) = locator.species_patch_path(1) {
            if sp1_path.is_file() {
                let sp1 = FoxArchive::load(&sp1_path).unwrap();
                let (o1, l1) = sp1.resolve_avatar_layers(0, 1, 0, 2).unwrap();
                let (o2, l2) = sp1.resolve_avatar_layers(0, 2, 0, 2).unwrap();
                assert!(o1 != o2 || l1[0].image_idx != l2[0].image_idx);
            }
        }
    }

    #[test]
    fn kitterspeak_four_slots_and_fox5_hidden_layer_and_lu_opcodes() {
        // Test op 9 (move slot 1 -> slot 2 foreground) and multi-slot concurrency (23/30) + op 33.
        let make_frame = |ox: i16, ex: i16| FoxFrame {
            offset: (ox, 0),
            elev: (ex, 0),
            layers: vec![FoxLayer {
                category: 0x0010,
                image_idx: 0,
                layer_offset: (0, 0),
            }],
        };
        let fg_only = FoxAvatarShape {
            purpose: 1,
            state: 0,
            dir: 0,
            rule: (1, 1),
            offset: (0, 0),
            elev: (0, 0),
            layers: vec![],
            frames: vec![make_frame(3, -12)],
            anim_steps: vec![FoxAnimStep {
                op: 9,
                arg1: 0,
                arg2: 1,
            }],
        };
        let s = fg_only.slots_at(0).unwrap();
        assert_eq!(s.pass1().count(), 0);
        let p2: Vec<_> = s.pass2().collect();
        assert_eq!(p2.len(), 1);
        assert_eq!(p2[0].slot, 2);
        assert_eq!(p2[0].frame_idx, 0);
        assert_eq!(s.elev, (-12, 0));

        // Dual slot animation + effect_pass=2 + fast-forward at huge timestamp
        let dual = FoxAvatarShape {
            purpose: 1,
            state: 0,
            dir: 0,
            rule: (1, 1),
            offset: (0, 0),
            elev: (0, 0),
            layers: vec![],
            frames: vec![make_frame(1, 0), make_frame(2, -4), make_frame(3, -8)],
            anim_steps: vec![
                FoxAnimStep {
                    op: 33,
                    arg1: 2,
                    arg2: 0,
                },
                FoxAnimStep {
                    op: 11,
                    arg1: 100,
                    arg2: 0,
                },
                FoxAnimStep {
                    op: 23,
                    arg1: 0,
                    arg2: 0,
                },
                FoxAnimStep {
                    op: 30,
                    arg1: 1,
                    arg2: 0,
                },
                FoxAnimStep {
                    op: 2,
                    arg1: 200,
                    arg2: 0,
                },
                FoxAnimStep {
                    op: 30,
                    arg1: 2,
                    arg2: 0,
                },
                FoxAnimStep {
                    op: 4,
                    arg1: 2,
                    arg2: 0,
                },
            ],
        };
        let t150 = dual.slots_at(150).unwrap();
        assert_eq!(t150.effect_pass, 2);
        assert!(t150.slots[0].is_some_and(|a| a.frame_idx == 0));
        assert!(t150.slots[1].is_none()); // frame 0 cleared from slot 1 when assigned to slot 0
        assert!(t150.slots[2].is_some_and(|a| a.frame_idx == 1));
        let t_huge = dual.slots_at(1_000_000_150).unwrap();
        assert_eq!(t_huge.effect_pass, 2);
        assert!(t_huge.slots[2].is_some());

        // Test 'l' (1 byte), 'u' (6 + len bytes), and category & 0x80 skip inside level-4 layers.
        let mut cmd = Vec::new();
        cmd.extend_from_slice(b"L\x01");
        cmd.extend_from_slice(&1u32.to_be_bytes());
        cmd.extend_from_slice(b"i");
        cmd.extend_from_slice(&42u32.to_be_bytes());
        cmd.extend_from_slice(b"l\x05");
        cmd.extend_from_slice(b"u\x00\x01\x00\x00\x00\x03XYZ");
        cmd.extend_from_slice(b"L\x02");
        cmd.extend_from_slice(&1u32.to_be_bytes());
        cmd.extend_from_slice(&[b'p', 1, b's', 0, b'D', 0, b'R', 1, 1]);
        cmd.extend_from_slice(b"L\x03");
        cmd.extend_from_slice(&1u32.to_be_bytes());
        cmd.extend_from_slice(b"L\x04");
        cmd.extend_from_slice(&2u32.to_be_bytes());
        // Child 0: visible shadow 0x50, c=1 -> image 0
        cmd.extend_from_slice(b"C\x00\x50c\x00\x01<");
        // Child 1: hidden 0x90 (0x80 set), c=1 -> MUST BE SKIPPED!
        cmd.extend_from_slice(b"C\x00\x90c\x00\x01<");
        cmd.extend_from_slice(b"<<<");

        let (_to_img, _shapes, av, _flags) = extract_object_shapes(&cmd, 2);
        let sh = &av.get(&42).expect("object 42 shape")[0];
        assert_eq!(sh.frames[0].layers.len(), 1);
        assert_eq!(sh.frames[0].layers[0].category, 0x0050);
    }

    #[test]
    fn decode_smiley_fsh_archives_if_present() {
        let locator = AssetLocator::discover();
        let pal = locator.load_default_palette().unwrap_or_default();
        if let Some(p1) = locator.default_patch_path("smileys.fsh") {
            let raw = fs::read(p1).unwrap();
            let imgs = decode_fsh_sprites(&raw, &pal);
            assert_eq!(imgs.len(), 16);
            assert_eq!((imgs[0].width, imgs[0].height), (15, 9));
        }
        if let Some(p2) = locator.default_patch_path("smileys2.fsh") {
            let raw = fs::read(p2).unwrap();
            let imgs = decode_fsh_sprites(&raw, &pal);
            assert_eq!(imgs.len(), 38);
            assert_eq!((imgs[0].width, imgs[0].height), (17, 12));
        }
    }

    #[test]
    fn decode_legacy_fshx_and_fsh_patches_if_present() {
        let locator = AssetLocator::discover();
        let Some(ref root) = locator.root else {
            return;
        };
        let pal = locator.load_default_palette().unwrap_or_default();
        for (rel, expect_min_objs) in [
            ("patches/legacy/floors.fox", 200usize),
            ("patches/legacy/floors2.fox", 20),
            ("patches/legacy/item.fox", 300),
            ("patches/legacy/item2.fox", 100),
            ("patches/legacy/item3.fox", 100),
            ("patches/legacy/item4.fox", 40),
            ("patches/legacy/wall.fox", 48),
            ("patches/legacy/wall_4.fox", 18),
            ("patches/legacy/effects.fox", 200),
            ("patches/legacy/junction.fsh", 30),
            ("patches/default/buttons.fox", 50),
        ] {
            let path = root.join(rel);
            if !path.is_file() {
                continue;
            }
            let mut arc = FoxArchive::load(&path)
                .unwrap_or_else(|e| panic!("failed to load {rel}: {e}"));
            assert!(arc.is_fshx_or_fsh, "{rel} should mark is_fshx_or_fsh");
            assert!(
                arc.object_to_image.len() >= expect_min_objs,
                "{rel}: expected >= {expect_min_objs} objects, got {}",
                arc.object_to_image.len()
            );
            if let Some(sh) = arc.resolve_shape(1, 0) {
                let sp = arc
                    .decode_image(sh.image_idx, &pal)
                    .unwrap_or_else(|e| panic!("decode {rel} obj 1 failed: {e}"));
                assert_eq!(sp.rgba.len(), sp.width as usize * sp.height as usize * 4);
            }
        }
    }

    #[test]
    fn bounded_writer_rejects_oversized_decompression() {
        let mut w = BoundedWriter::new(16);
        assert!(w.write_all(&[0u8; 16]).is_ok());
        assert!(w.write_all(&[1u8]).is_err());
    }
}
