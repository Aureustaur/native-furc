//! Live world state and Base220 / Base95 world packet decoding.

use std::collections::BTreeMap;

use crate::encoding::{
    decode_b220_raw, decode_b220_str_raw, decode_base220, decode_base95, decode_base95_signed,
    decode_win1252,
};
use crate::furc_colors::{
    apply_color_bytes, color_code_byte_len, parse_color_bytes_with_len, AvatarAppearance,
};
use crate::map::FurcadiaMap;

/// An avatar currently visible in the dream.
#[derive(Debug, Clone, PartialEq)]
pub struct Avatar {
    pub uid: u32,
    pub name: String,
    pub x: u16,
    pub y: u16,
    /// Wire direction byte (`0..3`, mapped to FOX5 dirs `[1, 3, 7, 9]`).
    pub dir: u8,
    /// Wire pose byte (`0..=6`, e.g. `2`/`4` stand, `3` sit, `6` lie).
    pub pose: u8,
    /// Lossy string representation of the color code (`w...`, `t...`, etc.) for diagnostics.
    pub colors: String,
    /// Exact raw bytes of the color code without UTF-8 lossy replacement.
    pub raw_colors: Vec<u8>,
    /// Decoded species, gender, and 12 palette/gradient color slots.
    pub appearance: AvatarAppearance,
    /// Sprite draw scale factor (`1.0` normal; set via `<` or `]_`).
    pub scale: f32,
    /// Fine pixel offset `(dx, dy)` in world pixels (set via `]H`).
    pub offset: (i32, i32),
    /// Optional gloaming RGBA tint `(r, g, b, alpha)` (set via `]O`).
    pub gloaming: Option<(u8, u8, u8, u8)>,
    /// Whether the avatar is currently visible on the map (`false` after `'C'` hide or tile displacement).
    pub visible: bool,
}

/// Layer selector for region visibility / fallback resolution (`]W` + `'9'`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionLayer {
    Floor,
    Item,
    Wall,
    Effect,
    Lighting,
    Ambience,
}

/// Region visibility & fallback tile settings parsed from `]W`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionDefaults {
    pub outdoor_item_fallback: u16,
    pub outdoor_wall_fallback: u16,
    pub outdoor_floor_fallback: u16,
    pub outdoor_effect_fallback: u16,
    pub indoor_item_fallback: u16,
    pub indoor_wall_fallback: u16,
    pub indoor_floor_fallback: u16,
    pub indoor_effect_fallback: u16,
    pub indoor_threshold: u16,
    pub show_other_outdoor: bool,
    pub show_outdoor_from_indoor: bool,
    pub outdoor_lighting_fallback: u16,
    pub outdoor_ambience_fallback: u16,
    pub indoor_lighting_fallback: u16,
    pub indoor_ambience_fallback: u16,
    /// Remaps for local species slots `1001..=1080` (`(species_id, gender)`) parsed via `0x422810`.
    pub local_species_remap: [(u16, u8); 80],
}

impl Default for RegionDefaults {
    fn default() -> Self {
        // Matches Furcadia.exe initialization at 0x43cccc.
        Self {
            outdoor_item_fallback: 0,
            outdoor_wall_fallback: 0,
            outdoor_floor_fallback: 161,
            outdoor_effect_fallback: 0,
            indoor_item_fallback: 0,
            indoor_wall_fallback: 0,
            indoor_floor_fallback: 161,
            indoor_effect_fallback: 0,
            indoor_threshold: u16::MAX,
            show_other_outdoor: true,
            show_outdoor_from_indoor: true,
            outdoor_lighting_fallback: 0,
            outdoor_ambience_fallback: 0,
            indoor_lighting_fallback: 0,
            indoor_ambience_fallback: 0,
            local_species_remap: [(0, 0); 80],
        }
    }
}

impl RegionDefaults {
    /// Parses raw binary payload after `]W` using Base220 fields (matching `0x4181b7` + `0x422810`).
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        // Need at least 11 fields: 9x2B + 2x1B = 20 bytes after `]W`.
        if data.len() < 20 {
            return None;
        }
        let mut d = Self::default();
        let mut pos = 0usize;
        let next_b220 = |pos: &mut usize, len: usize| -> Option<u16> {
            if *pos + len > data.len() {
                return None;
            }
            let val = decode_b220_raw(&data[*pos..*pos + len]);
            *pos += len;
            Some(val as u16)
        };
        d.outdoor_item_fallback = next_b220(&mut pos, 2)?;
        d.outdoor_wall_fallback = next_b220(&mut pos, 2)?;
        d.outdoor_floor_fallback = next_b220(&mut pos, 2)?;
        d.outdoor_effect_fallback = next_b220(&mut pos, 2)?;
        d.indoor_item_fallback = next_b220(&mut pos, 2)?;
        d.indoor_wall_fallback = next_b220(&mut pos, 2)?;
        d.indoor_floor_fallback = next_b220(&mut pos, 2)?;
        d.indoor_effect_fallback = next_b220(&mut pos, 2)?;
        d.indoor_threshold = next_b220(&mut pos, 2)?;
        d.show_other_outdoor = next_b220(&mut pos, 1)? != 0;
        d.show_outdoor_from_indoor = next_b220(&mut pos, 1)? != 0;

        // Full packet length >= 28 bytes after `]W` (`0x1e` total including `]W`) carries lighting/ambience fallbacks.
        if pos + 8 <= data.len() {
            d.outdoor_lighting_fallback = next_b220(&mut pos, 2)?;
            d.outdoor_ambience_fallback = next_b220(&mut pos, 2)?;
            d.indoor_lighting_fallback = next_b220(&mut pos, 2)?;
            d.indoor_ambience_fallback = next_b220(&mut pos, 2)?;
        }
        if pos < data.len() && data[pos] != 0 {
            if let Some(cnt) = next_b220(&mut pos, 1) {
                for i in 0..(cnt as usize).min(80) {
                    if pos + 3 > data.len() {
                        break;
                    }
                    let species = decode_b220_raw(&data[pos..pos + 2]) as u16;
                    let gender = (decode_b220_raw(&data[pos + 2..pos + 3]) % 3) as u8;
                    pos += 3;
                    d.local_species_remap[i] = (species, gender);
                }
            }
        }
        Some(d)
    }
}

/// Floating dream portal title/subtitle annotation received via `]s` and removed via `]t`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DreamLabel {
    pub x: u16,
    pub y: u16,
    pub kind: i32,
    pub title: String,
    pub subtitle: String,
    pub state_flag: i32,
}

/// Decoded appearance and display name for a looked-at player (`]f`).
#[derive(Debug, Clone, PartialEq)]
pub struct LookedPlayerInfo {
    pub name: String,
    pub appearance: AvatarAppearance,
}

/// Live dream state: loaded map, self identity, region metadata, dream labels, and avatars.
#[derive(Debug)]
pub struct WorldState {
    pub map_name: Option<String>,
    pub map: Option<FurcadiaMap>,
    pub map_ready: bool,
    pub is_modern: bool,
    pub pending_map_packets: Vec<(u8, Vec<u8>)>,
    pub self_uid: Option<u32>,
    pub self_name: Option<String>,
    pub self_pos: Option<(u16, u16)>,
    pub avatars: BTreeMap<u32, Avatar>,
    pub region_defaults: RegionDefaults,
    pub region_flags: Vec<u16>,
    pub dream_labels: BTreeMap<(u16, u16), DreamLabel>,
    /// Local species remap slots (`1001..=1080` -> `(species_id, gender)`), reset on `]W` / dream switch.
    pub local_species_remap: [(u16, u8); 80],
}

impl Default for WorldState {
    fn default() -> Self {
        Self {
            map_name: None,
            map: None,
            map_ready: false,
            is_modern: false,
            pending_map_packets: Vec::new(),
            self_uid: None,
            self_name: None,
            self_pos: None,
            avatars: BTreeMap::new(),
            region_defaults: RegionDefaults::default(),
            region_flags: Vec::new(),
            dream_labels: BTreeMap::new(),
            local_species_remap: [(0, 0); 80],
        }
    }
}

#[inline]
fn decode_dir_pose(data: &[u8]) -> (u8, u8) {
    let dir = (decode_b220_raw(&data[0..1]) as u8) & 3;
    let pose = decode_b220_raw(&data[1..2]) as u8;
    (dir, pose)
}

#[inline]
fn parse_hex_pair(bytes: &[u8]) -> u8 {
    if bytes.len() < 2 {
        return 0;
    }
    let hex_val = |b: u8| -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    };
    match (hex_val(bytes[0]), hex_val(bytes[1])) {
        (Some(hi), Some(lo)) => (hi << 4) | lo,
        _ => 0,
    }
}

impl WorldState {
    /// Resets per-dream state (`region_defaults`, `region_flags`, `dream_labels`, `pending_map_packets`, `local_species_remap`)
    /// when transitioning to a new dream.
    pub fn begin_dream(&mut self, map_name: String) {
        self.map_name = Some(map_name);
        self.map = None;
        self.map_ready = false;
        self.is_modern = false;
        self.pending_map_packets.clear();
        self.region_defaults = RegionDefaults::default();
        self.region_flags.clear();
        self.dream_labels.clear();
        self.local_species_remap = [(0, 0); 80];
    }

    /// Attaches a parsed `FurcadiaMap` and replays any buffered tile mutation packets that arrived
    /// while the dream package was downloading.
    pub fn attach_map(&mut self, map: FurcadiaMap) {
        if let Some(m) = map.is_modern {
            self.is_modern = m;
        }
        self.map = Some(map);
        self.map_ready = true;
        let queued = std::mem::take(&mut self.pending_map_packets);
        for (op, data) in queued {
            self.apply_tile_packet(op, &data);
        }
    }

    /// Applies a `]W` binary region defaults directive.
    pub fn apply_region_defaults(&mut self, data: &[u8]) {
        self.local_species_remap = [(0, 0); 80];
        if let Some(parsed) = RegionDefaults::from_bytes(data) {
            self.local_species_remap = parsed.local_species_remap;
            self.region_defaults = parsed;
        }
    }

    /// Resolves `(species_id, gender)` after applying `]W` local-species remapping (`1001..=1080`),
    /// returning `None` for invisible `species == 1000` (matching `0x4253f0`).
    pub fn resolved_avatar_species_gender(&self, av: &Avatar) -> Option<(u16, u8)> {
        let sp = av.appearance.species_id;
        if sp == 1000 {
            return None;
        }
        if (1001..=1080).contains(&sp) {
            let (mapped_sp, mapped_g) = self.local_species_remap[(sp - 1001) as usize];
            if mapped_sp > 0 {
                return Some((mapped_sp, mapped_g));
            }
        }
        Some((sp, av.appearance.gender))
    }

    /// Updates self avatar appearance from a `]}` directive (matching `0x41758b` / `0x418d78`).
    pub fn apply_self_color_bytes(&mut self, data: &[u8]) -> Option<AvatarAppearance> {
        let uid = self.self_uid?;
        let av = self.avatars.get_mut(&uid)?;
        let consumed = apply_color_bytes(&mut av.appearance, data)?;
        if consumed > 0 {
            av.raw_colors = data[..consumed].to_vec();
            av.colors = String::from_utf8_lossy(&data[..consumed]).into_owned();
        } else if matches!(data.first(), Some(b'u' | b'v')) {
            av.raw_colors = vec![data[0]];
            av.colors = (data[0] as char).to_string();
        }
        Some(av.appearance.clone())
    }

    /// Parses a `]f` looked-at-player directive (`<color_code><short_name>`) without mutating self.
    pub fn parse_looked_player_bytes(data: &[u8]) -> Option<LookedPlayerInfo> {
        let mut appearance = AvatarAppearance::default();
        let consumed = apply_color_bytes(&mut appearance, data)?;
        let name_offset = if consumed > 0 {
            consumed
        } else if matches!(data.first(), Some(b'u' | b'v')) {
            1
        } else {
            return None;
        };
        let raw_name = decode_win1252(data.get(name_offset..)?).replace('|', " ");
        let name = raw_name.trim().to_owned();
        if name.is_empty() {
            return None;
        }
        Some(LookedPlayerInfo { name, appearance })
    }

    /// Parses batched `]_` avatar scale directives (`4B uid, 1B scale_pct` matching `0x418e15`).
    pub fn apply_avatar_scale(&mut self, data: &[u8]) {
        let mut rem = data;
        while rem.len() >= 5 {
            let uid = decode_b220_raw(&rem[0..4]) as u32;
            let pct = decode_b220_raw(&rem[4..5]) as u8;
            if let Some(av) = self.avatars.get_mut(&uid) {
                av.scale = (pct as f32) / 100.0;
            }
            rem = &rem[5..];
        }
    }

    /// Parses batched `]H` avatar pixel offset directives (`4B uid, 2B dx+256, 2B dy+256` matching `0x41769e`).
    pub fn apply_avatar_offset(&mut self, data: &[u8]) {
        let mut rem = data;
        while rem.len() >= 8 {
            let uid = decode_b220_raw(&rem[0..4]) as u32;
            let dx = decode_b220_raw(&rem[4..6]) as i32 - 256;
            let dy = decode_b220_raw(&rem[6..8]) as i32 - 256;
            if let Some(av) = self.avatars.get_mut(&uid) {
                av.offset = (dx, dy);
            }
            rem = &rem[8..];
        }
    }

    /// Parses batched `]O` avatar gloaming color directives (`4B uid, 2H R, 2H G, 2H B, 2B alpha<=255` matching `0x417bdc`).
    pub fn apply_avatar_gloaming(&mut self, data: &[u8]) {
        let mut rem = data;
        while rem.len() >= 12 {
            let uid = decode_b220_raw(&rem[0..4]) as u32;
            let r = parse_hex_pair(&rem[4..6]);
            let g = parse_hex_pair(&rem[6..8]);
            let b = parse_hex_pair(&rem[8..10]);
            let alpha = (decode_b220_raw(&rem[10..12]) as u32).min(255) as u8;
            if let Some(av) = self.avatars.get_mut(&uid) {
                av.gloaming = if alpha == 0 {
                    None
                } else {
                    Some((r, g, b, alpha))
                };
            }
            rem = &rem[12..];
        }
    }

    /// Parses and inserts a `]s` dream label directive from raw bytes (`2B b95 x, 2B b95 y, "%d %s %s %s %s"`).
    pub fn apply_show_dream_name_bytes(&mut self, bytes: &[u8]) {
        if bytes.len() < 5 {
            return;
        }
        let x_i = decode_base95_signed(bytes[0], bytes[1]);
        let y_i = decode_base95_signed(bytes[2], bytes[3]);
        if x_i < 0 || y_i < 0 {
            return;
        }
        let (x, y) = (x_i as u16, y_i as u16);
        let text = decode_win1252(&bytes[4..]);
        let mut parts = text.split_whitespace();
        let kind: i32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1);
        let title = parts.next().unwrap_or("").replace('|', " ");
        let subtitle = parts.next().unwrap_or("").replace('|', " ");
        let _owner_or_unused = parts.next().unwrap_or("");
        let state_flag: i32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        self.dream_labels.insert(
            (x, y),
            DreamLabel {
                x,
                y,
                kind,
                title,
                subtitle,
                state_flag,
            },
        );
    }

    /// Parses and inserts a `]s` dream label directive (`2B b95 x`, `2B b95 y`, `"%d %s %s %s %s"`).
    pub fn apply_show_dream_name(&mut self, payload: &str) {
        self.apply_show_dream_name_bytes(payload.as_bytes());
    }

    /// Parses and removes a `]t` dream label directive from raw bytes (`2B b95 x`, `2B b95 y`).
    pub fn apply_remove_dream_name_bytes(&mut self, bytes: &[u8]) {
        if bytes.len() < 4 {
            return;
        }
        let x_i = decode_base95_signed(bytes[0], bytes[1]);
        let y_i = decode_base95_signed(bytes[2], bytes[3]);
        if x_i >= 0 && y_i >= 0 {
            self.dream_labels.remove(&(x_i as u16, y_i as u16));
        }
    }

    /// Parses and removes a `]t` dream label directive (`2B b95 x`, `2B b95 y`).
    pub fn apply_remove_dream_name(&mut self, payload: &str) {
        self.apply_remove_dream_name_bytes(payload.as_bytes());
    }

    /// Returns region bitflags (`'9'` packet) for `region_id`, defaulting to `0`.
    #[inline]
    pub fn region_flag(&self, region_id: u16) -> u16 {
        self.region_flags.get(region_id as usize).copied().unwrap_or(0)
    }

    /// Resolves the effective tile/layer ID at `(tile_region)` given `cur_region`, matching
    /// `0x424748` (walls), `0x424b60` (floor/item/effect), and `0x42512e`/`0x425270` (lighting/ambience).
    pub fn resolve_tile_layer(
        &self,
        cur_region: u16,
        tile_region: u16,
        layer: RegionLayer,
        raw_val: u16,
    ) -> u16 {
        let d = &self.region_defaults;
        let (bit, indoor_fb, outdoor_fb) = match layer {
            RegionLayer::Floor => (0x0001u16, d.indoor_floor_fallback, d.outdoor_floor_fallback),
            RegionLayer::Item => (0x0002u16, d.indoor_item_fallback, d.outdoor_item_fallback),
            RegionLayer::Wall => (0x0004u16, d.indoor_wall_fallback, d.outdoor_wall_fallback),
            RegionLayer::Effect => (0x0008u16, d.indoor_effect_fallback, d.outdoor_effect_fallback),
            RegionLayer::Lighting => (
                0x0100u16,
                d.indoor_lighting_fallback,
                d.outdoor_lighting_fallback,
            ),
            RegionLayer::Ambience => (
                0x0200u16,
                d.indoor_ambience_fallback,
                d.outdoor_ambience_fallback,
            ),
        };
        let thresh = d.indoor_threshold;
        let flags = self.region_flag(tile_region);
        let fallback = || {
            if tile_region < thresh {
                outdoor_fb
            } else {
                indoor_fb
            }
        };
        if (flags & bit) != 0 {
            return fallback();
        }
        if layer == RegionLayer::Wall {
            if cur_region >= thresh {
                if tile_region == cur_region || d.show_other_outdoor {
                    raw_val
                } else {
                    fallback()
                }
            } else if tile_region < thresh || d.show_outdoor_from_indoor {
                raw_val
            } else {
                indoor_fb
            }
        } else if cur_region >= thresh {
            if tile_region == cur_region {
                raw_val
            } else {
                fallback()
            }
        } else if tile_region < thresh {
            raw_val
        } else {
            indoor_fb
        }
    }

    /// Applies a raw Base220 / Base95 world packet.
    pub fn apply_world_packet(&mut self, opcode: u8, data: &[u8]) {
        match opcode {
            b'<' => self.handle_spawn(data),
            b'/' | b'A' => self.handle_move(data),
            b'D' => self.handle_dream_avatar(data),
            b'B' => self.handle_appearance(data),
            b'C' => self.handle_hide(data),
            b')' => self.handle_remove(data),
            b'@' => self.handle_self_move(data),
            b'9' => self.handle_region_flags(data),
            b'>' | b'1' | b'2' | b'4' | b'5' | b'E' | b'F' => {
                if self.map_ready && self.map.is_some() {
                    self.apply_tile_packet(opcode, data);
                } else {
                    self.pending_map_packets.push((opcode, data.to_vec()));
                }
            }
            b'6' | b'7' | b'8' => {} // DS / misc world packets; not tile layers.
            _ => {}
        }
    }

    fn apply_tile_packet(&mut self, opcode: u8, data: &[u8]) {
        let Some(ref mut map) = self.map else {
            return;
        };
        let height = map.height;
        if height == 0 || map.width == 0 {
            return;
        }
        let stride_x = if opcode == b'2' { 2 } else { 1 };
        let total_slots = map.width * stride_x * height;
        let max_val: u32 = match opcode {
            b'>' => 3400,
            b'1' => 1000,
            b'2' => 256,
            b'4' | b'5' | b'E' | b'F' => 65536,
            _ => 0,
        };

        let mut pos = 0usize;
        while pos + 6 <= data.len() && data[pos] > 0x1f {
            let raw_x = decode_b220_raw(&data[pos..pos + 2]);
            let raw_y = decode_b220_raw(&data[pos + 2..pos + 4]);
            let val = decode_b220_raw(&data[pos + 4..pos + 6]) as u32;
            pos += 6;

            if val >= max_val {
                continue;
            }
            let val = val as u16;
            let count = ((raw_x / 1000) * 48 + (raw_y / 1000)) as usize;
            let x_rem = (raw_x % 1000) as usize;
            let start_y = (raw_y % 1000) as usize;
            if start_y >= height {
                continue;
            }

            let mut t = x_rem * height + start_y;
            let end_t = t.saturating_add(count + 1).min(total_slots);
            while t < end_t {
                let col = t / height;
                let ty = t % height;
                t += 1;
                if opcode == b'2' {
                    let tile_x = col / 2;
                    let side = col & 1;
                    let cur = map.wall_at(tile_x, ty).to_le_bytes();
                    let updated = if side == 0 {
                        u16::from_le_bytes([val as u8, cur[1]])
                    } else {
                        u16::from_le_bytes([cur[0], val as u8])
                    };
                    map.set_wall(tile_x, ty, updated);
                } else {
                    match opcode {
                        b'>' => map.set_item(col, ty, val),
                        b'1' => map.set_floor(col, ty, val),
                        b'4' => map.set_region(col, ty, val),
                        b'5' => map.set_effect(col, ty, val),
                        b'E' => map.set_lighting(col, ty, val),
                        b'F' => map.set_ambience(col, ty, val),
                        _ => {}
                    }
                }
            }
        }
    }

    fn handle_region_flags(&mut self, data: &[u8]) {
        let mut pos = 0usize;
        while pos + 8 <= data.len() && data[pos] > 0x1f {
            let mut idx = decode_b220_raw(&data[pos..pos + 3]) as usize;
            let val = decode_b220_raw(&data[pos + 3..pos + 6]) as u16;
            let cnt = decode_b220_raw(&data[pos + 6..pos + 8]);
            pos += 8;
            if idx < 65536 {
                let max_end = (idx + cnt as usize).min(65535);
                if self.region_flags.len() <= max_end {
                    self.region_flags.resize(max_end + 1, 0);
                }
                for _ in 0..=cnt {
                    if idx >= 65536 {
                        break;
                    }
                    self.region_flags[idx] = val;
                    idx += 1;
                }
            }
        }
    }

    fn handle_self_move(&mut self, data: &[u8]) {
        // `@` can be 4 bytes `(x, y)` or 8 bytes `(from_x, from_y, to_x, to_y)`;
        // always apply the final destination pair so the camera does not jump back.
        let Some(chunk) = data.chunks_exact(4).last() else {
            return;
        };
        if let (Ok(x), Ok(y)) = (
            decode_base95(&chunk[0..2]).map(|v| v as u16),
            decode_base95(&chunk[2..4]).map(|v| v as u16),
        ) {
            self.self_pos = Some((x, y));
        }
    }

    fn handle_spawn(&mut self, data: &[u8]) {
        let mut rem = data;
        while rem.len() >= 10 {
            let uid = decode_b220_raw(&rem[0..4]) as u32;
            let x = decode_b220_raw(&rem[4..6]) as u16;
            let y = decode_b220_raw(&rem[6..8]) as u16;
            let (dir, pose) = decode_dir_pose(&rem[8..10]);

            let Some((name_bytes, after_name)) = decode_b220_str_raw(&rem[10..]) else {
                break;
            };
            let name = decode_win1252(name_bytes);
            let Some((appearance, color_len)) = parse_color_bytes_with_len(after_name) else {
                break;
            };
            let raw_colors = after_name[..color_len].to_vec();
            let colors = String::from_utf8_lossy(&raw_colors).into_owned();
            let after_colors = &after_name[color_len..];

            let (scale, next_rem) = if after_colors.len() >= 6 {
                let scale_pct = decode_b220_raw(&after_colors[5..6]) as u8;
                let s = if scale_pct == 0 {
                    1.0
                } else {
                    scale_pct as f32 / 100.0
                };
                (s, &after_colors[6..])
            } else {
                (1.0, &[][..])
            };

            if self.self_uid == Some(uid) {
                self.self_pos = Some((x, y));
            }
            self.displace_tile_occupant(uid, x, y);
            self.avatars.insert(
                uid,
                Avatar {
                    uid,
                    name,
                    x,
                    y,
                    dir,
                    pose,
                    colors,
                    raw_colors,
                    appearance,
                    scale,
                    offset: (0, 0),
                    gloaming: None,
                    visible: true,
                },
            );

            if next_rem.is_empty() {
                break;
            }
            rem = next_rem;
        }
    }

    fn displace_tile_occupant(&mut self, mover_uid: u32, x: u16, y: u16) {
        for other in self.avatars.values_mut() {
            if other.uid != mover_uid && other.visible && other.x == x && other.y == y {
                other.visible = false;
            }
        }
    }

    fn upsert_avatar_pose(
        &mut self,
        uid: u32,
        x: u16,
        y: u16,
        dir: u8,
        pose: u8,
        species_opt: Option<u16>,
        reset_offset: bool,
    ) {
        if self.self_uid == Some(uid) {
            self.self_pos = Some((x, y));
        }
        self.displace_tile_occupant(uid, x, y);

        if let Some(av) = self.avatars.get_mut(&uid) {
            av.x = x;
            av.y = y;
            av.dir = dir;
            av.pose = pose;
            av.visible = true;
            if reset_offset {
                av.offset = (0, 0);
            }
            if let Some(sp) = species_opt.filter(|&s| s > 0) {
                av.appearance.species_id = sp;
            }
        } else {
            let mut appearance = AvatarAppearance::default();
            if let Some(sp) = species_opt.filter(|&s| s > 0) {
                appearance.species_id = sp;
            }
            self.avatars.insert(
                uid,
                Avatar {
                    uid,
                    name: format!("#{uid}"),
                    x,
                    y,
                    dir,
                    pose,
                    colors: String::new(),
                    raw_colors: Vec::new(),
                    appearance,
                    scale: 1.0,
                    offset: (0, 0),
                    gloaming: None,
                    visible: true,
                },
            );
        }
    }

    fn handle_move(&mut self, data: &[u8]) {
        let mut rem = data;
        while rem.len() >= 10 {
            let uid = decode_b220_raw(&rem[0..4]) as u32;
            let x = decode_b220_raw(&rem[4..6]) as u16;
            let y = decode_b220_raw(&rem[6..8]) as u16;
            let (dir, pose) = decode_dir_pose(&rem[8..10]);
            self.upsert_avatar_pose(uid, x, y, dir, pose, None, true);
            rem = &rem[10..];
        }
    }

    fn handle_dream_avatar(&mut self, data: &[u8]) {
        let mut rem = data;
        while rem.len() >= 22 {
            let uid = decode_b220_raw(&rem[0..4]) as u32;
            let x = decode_b220_raw(&rem[4..6]) as u16;
            let y = decode_b220_raw(&rem[6..8]) as u16;
            let (dir, pose) = decode_dir_pose(&rem[8..10]);
            let species = decode_b220_raw(&rem[20..22]) as u16;
            self.upsert_avatar_pose(uid, x, y, dir, pose, Some(species), false);
            rem = &rem[22..];
        }
    }

    fn handle_appearance(&mut self, data: &[u8]) {
        let mut rem = data;
        while rem.len() >= 6 {
            let uid = decode_b220_raw(&rem[0..4]) as u32;
            let (dir, pose) = decode_dir_pose(&rem[4..6]);
            let color_slice = &rem[6..];
            if let Some(av) = self.avatars.get_mut(&uid) {
                av.dir = dir;
                av.pose = pose;
                let Some(consumed) = apply_color_bytes(&mut av.appearance, color_slice) else {
                    break;
                };
                if consumed > 0 {
                    av.raw_colors = color_slice[..consumed].to_vec();
                    av.colors = String::from_utf8_lossy(&color_slice[..consumed]).into_owned();
                    rem = &color_slice[consumed..];
                } else {
                    if let Some(&first) = color_slice.first() {
                        av.raw_colors = vec![first];
                        av.colors = (first as char).to_string();
                    }
                    break;
                }
            } else {
                let consumed = color_code_byte_len(color_slice);
                if consumed == 0 {
                    break;
                }
                rem = &color_slice[consumed..];
            }
        }
    }

    fn handle_hide(&mut self, data: &[u8]) {
        let mut rem = data;
        while rem.len() >= 8 {
            let uid = decode_b220_raw(&rem[0..4]) as u32;
            let x = decode_b220_raw(&rem[4..6]) as u16;
            let y = decode_b220_raw(&rem[6..8]) as u16;
            if self.self_uid == Some(uid) {
                self.self_pos = Some((x, y));
            }
            if let Some(av) = self.avatars.get_mut(&uid) {
                av.x = x;
                av.y = y;
                av.visible = false;
            }
            for other in self.avatars.values_mut() {
                if other.uid != uid && other.x == x && other.y == y {
                    other.visible = false;
                }
            }
            rem = &rem[8..];
        }
    }

    fn handle_remove(&mut self, data: &[u8]) {
        if data.len() < 4 {
            return;
        }
        if let Ok(uid) = decode_base220(&data[0..4]).map(|v| v as u32) {
            self.avatars.remove(&uid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::{encode_b220_str, encode_base220, encode_base95};

    fn make_test_map(w: usize, h: usize) -> FurcadiaMap {
        let mut raw = format!("MAP V01.50\nheight={h}\nwidth={w}\nname=T\nismodern=1\nBODY\n").into_bytes();
        raw.extend(vec![0u8; w * h * 2 * 7]);
        FurcadiaMap::from_bytes(&raw).unwrap()
    }

    #[test]
    fn decode_set_item_and_self_move_packets() {
        let mut world = WorldState::default();
        // Observed live: ">C#W#K+" -> set item 1800 (Default Dream Pad portal) at (32, 52),
        // queued until the dream map attaches!
        world.apply_world_packet(b'>', b"C#W#K+");
        world.attach_map(make_test_map(60, 80));
        assert_eq!(world.map.as_ref().unwrap().item_at(32, 52), 1800);

        let mut at = encode_base95(34, 2).unwrap();
        at.extend(encode_base95(55, 2).unwrap());
        world.apply_world_packet(b'@', &at);
        assert_eq!(world.self_pos, Some((34, 55)));

        // 8-byte `@` `(from_x, from_y, to_x, to_y)` must move to `(to_x, to_y)`.
        let mut at8 = encode_base95(34, 2).unwrap();
        at8.extend(encode_base95(55, 2).unwrap());
        at8.extend(encode_base95(36, 2).unwrap());
        at8.extend(encode_base95(57, 2).unwrap());
        world.apply_world_packet(b'@', &at8);
        assert_eq!(world.self_pos, Some((36, 57)));
    }

    #[test]
    fn decode_floor_wall_repeat_lighting_ambience_and_region_flags() {
        let mut world = WorldState::default();
        world.attach_map(make_test_map(20, 20));

        // Repeat +y count: count = 2 -> raw_y = 2005 -> (x=3, y=5..=7) get floor 42.
        let mut p1 = Vec::new();
        p1.extend(encode_base220(3, 2).unwrap());
        p1.extend(encode_base220(2005, 2).unwrap());
        p1.extend(encode_base220(42, 2).unwrap());
        world.apply_world_packet(b'1', &p1);
        for y in 5..=7 {
            assert_eq!(world.map.as_ref().unwrap().floor_at(3, y), 42);
        }

        // Walls use half-tile x: side=0 (x_half = 4*2+0 = 8) NW, side=1 (x_half = 4*2+1 = 9) NE.
        let mut pw = Vec::new();
        pw.extend(encode_base220(8, 2).unwrap());
        pw.extend(encode_base220(1002, 2).unwrap()); // count=1 -> y=2..=3
        pw.extend(encode_base220(17, 2).unwrap());
        pw.extend(encode_base220(9, 2).unwrap());
        pw.extend(encode_base220(2, 2).unwrap()); // y=2
        pw.extend(encode_base220(29, 2).unwrap());
        world.apply_world_packet(b'2', &pw);
        assert_eq!(world.map.as_ref().unwrap().wall_at(4, 2).to_le_bytes(), [17, 29]);
        assert_eq!(world.map.as_ref().unwrap().wall_at(4, 3).to_le_bytes(), [17, 0]);

        // Lighting ('E') & Ambience ('F').
        let mut pe = Vec::new();
        pe.extend(encode_base220(1, 2).unwrap());
        pe.extend(encode_base220(2, 2).unwrap());
        pe.extend(encode_base220(7, 2).unwrap());
        world.apply_world_packet(b'E', &pe);
        assert_eq!(world.map.as_ref().unwrap().lighting_at(1, 2), 7);

        let mut pf = Vec::new();
        pf.extend(encode_base220(1, 2).unwrap());
        pf.extend(encode_base220(2, 2).unwrap());
        pf.extend(encode_base220(11, 2).unwrap());
        world.apply_world_packet(b'F', &pf);
        assert_eq!(world.map.as_ref().unwrap().ambience_at(1, 2), 11);

        // Region flags ('9'): 3B idx=5, 3B val=0x03 (hide floor+item), 2B cnt=2 -> regions 5..=7.
        let mut p9 = Vec::new();
        p9.extend(encode_base220(5, 3).unwrap());
        p9.extend(encode_base220(0x03, 3).unwrap());
        p9.extend(encode_base220(2, 2).unwrap());
        world.apply_world_packet(b'9', &p9);
        assert_eq!(world.region_flag(5), 3);
        assert_eq!(world.region_flag(7), 3);
        assert_eq!(world.resolve_tile_layer(0, 6, RegionLayer::Floor, 99), 161);
        assert_eq!(world.resolve_tile_layer(0, 6, RegionLayer::Wall, 99), 99);
    }

    #[test]
    fn decode_region_defaults_and_dream_labels() {
        let mut world = WorldState::default();
        let mut pw = Vec::new();
        for v in [0u64, 0, 161, 0, 0, 0, 5, 0, 10] {
            pw.extend(encode_base220(v, 2).unwrap());
        }
        pw.extend(encode_base220(0, 1).unwrap()); // show_other_outdoor = false
        pw.extend(encode_base220(0, 1).unwrap()); // show_outdoor_from_indoor = false
        world.apply_region_defaults(&pw);
        assert_eq!(world.region_defaults.indoor_threshold, 10);
        assert_eq!(world.region_defaults.indoor_floor_fallback, 5);
        assert_eq!(world.resolve_tile_layer(12, 0, RegionLayer::Floor, 40), 161);
        assert_eq!(world.resolve_tile_layer(0, 12, RegionLayer::Floor, 40), 5);
        assert_eq!(world.resolve_tile_layer(12, 12, RegionLayer::Floor, 40), 40);

        let mut ps = encode_base95(18, 2).unwrap();
        ps.extend(encode_base95(42, 2).unwrap());
        ps.extend_from_slice(b"1 Cosmic|Star|Bar LGBTQA|Bar - 0");
        world.apply_show_dream_name(&String::from_utf8(ps).unwrap());
        let lbl = world.dream_labels.get(&(18, 42)).expect("dream label");
        assert_eq!(lbl.title, "Cosmic Star Bar");
        assert_eq!(lbl.subtitle, "LGBTQA Bar");

        let mut pt = encode_base95(18, 2).unwrap();
        pt.extend(encode_base95(42, 2).unwrap());
        world.apply_remove_dream_name(&String::from_utf8(pt).unwrap());
        assert!(!world.dream_labels.contains_key(&(18, 42)));
    }

    #[test]
    fn spawn_move_and_remove_avatar() {
        let mut world = WorldState::default();
        let mut spawn = Vec::new();
        spawn.extend(encode_base220(12345, 4).unwrap());
        spawn.extend(encode_base220(10, 2).unwrap());
        spawn.extend(encode_base220(20, 2).unwrap());
        spawn.extend(encode_base220(1, 1).unwrap()); // dir = 1
        spawn.extend(encode_base220(2, 1).unwrap()); // pose = 2 (stand)
        spawn.extend(encode_b220_str("Tactful").unwrap());
        spawn.extend_from_slice(b"w#$##########$%#!"); // raw 16-byte color code + flags

        world.apply_world_packet(b'<', &spawn);
        let av = world.avatars.get(&12345).unwrap();
        assert_eq!(av.name, "Tactful");
        assert_eq!((av.x, av.y), (10, 20));
        assert_eq!((av.dir, av.pose), (1, 2));
        assert_eq!(av.colors, "w#$##########$%#");
        assert_eq!((av.appearance.gender, av.appearance.species_id), (1, 2));

        let mut mv = Vec::new();
        mv.extend(encode_base220(12345, 4).unwrap());
        mv.extend(encode_base220(12, 2).unwrap());
        mv.extend(encode_base220(22, 2).unwrap());
        mv.extend(encode_base220(3, 1).unwrap());
        mv.extend(encode_base220(3, 1).unwrap());
        world.apply_world_packet(b'A', &mv);
        assert_eq!(
            (
                world.avatars[&12345].x,
                world.avatars[&12345].y,
                world.avatars[&12345].dir,
                world.avatars[&12345].pose
            ),
            (12, 22, 3, 3)
        );

        let mut app = Vec::new();
        app.extend(encode_base220(12345, 4).unwrap());
        app.extend(encode_base220(0, 1).unwrap());
        app.extend(encode_base220(4, 1).unwrap());
        app.extend_from_slice(b"w%%%%%%%%%%%%%%#");
        world.apply_world_packet(b'B', &app);
        assert_eq!((world.avatars[&12345].dir, world.avatars[&12345].pose), (0, 4));
        assert_eq!(world.avatars[&12345].colors, "w%%%%%%%%%%%%%%#");

        let rm = encode_base220(12345, 4).unwrap();
        world.apply_world_packet(b')', &rm);
        assert!(world.avatars.is_empty());
    }

    #[test]
    fn batched_world_packets_raw_bytes_and_extended_directives() {
        let mut world = WorldState::default();
        world.self_uid = Some(100);

        // Two avatars in one batched '<' spawn packet:
        // #1: uid=100, non-UTF-8 Win-1252 name byte 0x92, raw color byte 0x85, scale=150
        // #2: uid=200, species=1001 (to be remapped by ]W)
        let mut spawn = Vec::new();
        spawn.extend(encode_base220(100, 4).unwrap());
        spawn.extend(encode_base220(10, 2).unwrap());
        spawn.extend(encode_base220(12, 2).unwrap());
        spawn.extend(encode_base220(1, 1).unwrap());
        spawn.extend(encode_base220(2, 1).unwrap());
        spawn.push(35 + 4); // name len 4
        spawn.extend_from_slice(&[b'A', 0x92, b's', b'h']);
        let mut col1 = vec![b'w', 0x85];
        col1.extend_from_slice(b"###########$%#"); // total 16 bytes
        spawn.extend_from_slice(&col1);
        spawn.extend(encode_base220(0, 1).unwrap()); // flag
        spawn.extend(encode_base220(0, 4).unwrap()); // extra
        spawn.extend(encode_base220(150, 1).unwrap()); // scale 1.5

        spawn.extend(encode_base220(200, 4).unwrap());
        spawn.extend(encode_base220(14, 2).unwrap());
        spawn.extend(encode_base220(16, 2).unwrap());
        spawn.extend(encode_base220(2, 1).unwrap());
        spawn.extend(encode_base220(3, 1).unwrap());
        spawn.extend(encode_b220_str("Bob").unwrap());
        // species 1001 in Base220 LE: 1001 = 121 + 4*220 -> [35+121=156, 35+4=39]
        spawn.extend_from_slice(&[b'w', b'#', b'#', b'#', b'#', b'#', b'#', b'#', b'#', b'#', b'#', b'#', b'#', b'#', 35 + 121, 35 + 4]);
        spawn.extend(encode_base220(0, 1).unwrap());
        spawn.extend(encode_base220(0, 4).unwrap());
        spawn.extend(encode_base220(0, 1).unwrap()); // 0 -> default scale 1.0

        world.apply_world_packet(b'<', &spawn);
        assert_eq!(world.avatars.len(), 2);
        assert_eq!(world.self_pos, Some((10, 12)));
        let av1 = &world.avatars[&100];
        assert_eq!(av1.name, "A’sh");
        assert_eq!(av1.appearance.fur, 0x85 - 35);
        assert!((av1.scale - 1.5).abs() < 1e-6);
        let av2 = &world.avatars[&200];
        assert_eq!(av2.name, "Bob");
        assert_eq!(av2.appearance.species_id, 1001);
        assert!((av2.scale - 1.0).abs() < 1e-6);

        // Batched 'C' updates (x, y) for both avatars without altering dir/pose.
        let mut c_pkt = Vec::new();
        c_pkt.extend(encode_base220(100, 4).unwrap());
        c_pkt.extend(encode_base220(11, 2).unwrap());
        c_pkt.extend(encode_base220(13, 2).unwrap());
        c_pkt.extend(encode_base220(200, 4).unwrap());
        c_pkt.extend(encode_base220(15, 2).unwrap());
        c_pkt.extend(encode_base220(17, 2).unwrap());
        world.apply_world_packet(b'C', &c_pkt);
        assert_eq!((world.avatars[&100].x, world.avatars[&100].y, world.avatars[&100].pose), (11, 13, 2));
        assert_eq!((world.avatars[&200].x, world.avatars[&200].y, world.avatars[&200].pose), (15, 17, 3));

        // Batched 'D' (22 bytes per record) updates uid=100 species to 42 and spawns uid=300.
        let mut d_pkt = Vec::new();
        for (uid, x, y, dir, pose, sp) in [(100u64, 20u64, 21u64, 0u64, 4u64, 42u64), (300, 30, 31, 3, 2, 1000)] {
            d_pkt.extend(encode_base220(uid, 4).unwrap());
            d_pkt.extend(encode_base220(x, 2).unwrap());
            d_pkt.extend(encode_base220(y, 2).unwrap());
            d_pkt.extend(encode_base220(dir, 1).unwrap());
            d_pkt.extend(encode_base220(pose, 1).unwrap());
            d_pkt.extend(encode_base220(0, 4).unwrap());
            d_pkt.extend(encode_base220(0, 4).unwrap());
            d_pkt.extend(encode_base220(0, 2).unwrap());
            d_pkt.extend(encode_base220(sp, 2).unwrap());
        }
        world.apply_world_packet(b'D', &d_pkt);
        assert_eq!(world.avatars[&100].appearance.species_id, 42);
        assert_eq!(world.self_pos, Some((20, 21)));
        assert_eq!(world.avatars[&300].appearance.species_id, 1000);
        assert_eq!(world.resolved_avatar_species_gender(&world.avatars[&300]), None);

        // ]W local species remap: remap slot 0 (species 1001) -> (species 77, gender 2).
        let mut pw = Vec::new();
        for _ in 0..9 {
            pw.extend(encode_base220(0, 2).unwrap());
        }
        pw.extend(encode_base220(1, 1).unwrap());
        pw.extend(encode_base220(1, 1).unwrap());
        for _ in 0..4 {
            pw.extend(encode_base220(0, 2).unwrap());
        }
        pw.extend(encode_base220(1, 1).unwrap()); // 1 remap entry
        pw.extend(encode_base220(77, 2).unwrap());
        pw.extend(encode_base220(2, 1).unwrap());
        world.apply_region_defaults(&pw);
        assert_eq!(world.resolved_avatar_species_gender(&world.avatars[&200]), Some((77, 2)));

        // ]_, ]H, ]O, ]f
        let mut scale_pkt = encode_base220(100, 4).unwrap();
        scale_pkt.extend(encode_base220(75, 1).unwrap());
        world.apply_avatar_scale(&scale_pkt);
        assert!((world.avatars[&100].scale - 0.75).abs() < 1e-6);

        let mut off_pkt = encode_base220(100, 4).unwrap();
        off_pkt.extend(encode_base220(246, 2).unwrap()); // dx = -10
        off_pkt.extend(encode_base220(270, 2).unwrap()); // dy = +14
        world.apply_avatar_offset(&off_pkt);
        assert_eq!(world.avatars[&100].offset, (-10, 14));

        let mut gloam_pkt = encode_base220(100, 4).unwrap();
        gloam_pkt.extend_from_slice(b"FF8040");
        gloam_pkt.extend(encode_base220(128, 2).unwrap());
        world.apply_avatar_gloaming(&gloam_pkt);
        assert_eq!(world.avatars[&100].gloaming, Some((0xFF, 0x80, 0x40, 128)));

        let updated_app = world.apply_self_color_bytes(b"w$$$$$$$$$$$$").unwrap();
        assert_eq!(updated_app.fur, 1);
        assert_eq!(updated_app.species_id, 42);
    }

    #[test]
    fn tile_packet_column_major_wrap_and_value_bounds() {
        let mut world = WorldState::default();
        world.attach_map(make_test_map(4, 5));

        // Item '>' repeat wrapping from (x=1, y=3) across (1,4) -> (2,0) -> (2,1): count=3 (4 writes).
        let mut p_item = Vec::new();
        p_item.extend(encode_base220(1, 2).unwrap());
        p_item.extend(encode_base220(3003, 2).unwrap()); // count=3, start_y=3
        p_item.extend(encode_base220(777, 2).unwrap());
        // Out-of-bounds item >= 3400 must be ignored
        p_item.extend(encode_base220(0, 2).unwrap());
        p_item.extend(encode_base220(0, 2).unwrap());
        p_item.extend(encode_base220(3400, 2).unwrap());
        world.apply_world_packet(b'>', &p_item);

        let m = world.map.as_ref().unwrap();
        assert_eq!(m.item_at(0, 0), 0);
        assert_eq!(m.item_at(1, 3), 777);
        assert_eq!(m.item_at(1, 4), 777);
        assert_eq!(m.item_at(2, 0), 777);
        assert_eq!(m.item_at(2, 1), 777);
        assert_eq!(m.item_at(2, 2), 0);

        // Wall '2' repeat wrapping from (half_x=2 -> tile_x=1 side=0, y=4) into (half_x=3 -> tile_x=1 side=1, y=0): count=1 (2 writes).
        let mut p_wall = Vec::new();
        p_wall.extend(encode_base220(2, 2).unwrap());
        p_wall.extend(encode_base220(1004, 2).unwrap()); // count=1, start_y=4
        p_wall.extend(encode_base220(19, 2).unwrap());
        world.apply_world_packet(b'2', &p_wall);

        let m = world.map.as_ref().unwrap();
        assert_eq!(m.wall_at(1, 4).to_le_bytes(), [19, 0]);
        assert_eq!(m.wall_at(1, 0).to_le_bytes(), [0, 19]);
    }

    #[test]
    fn hide_displacement_visibility_and_looked_player_decoding() {
        let mut world = WorldState::default();
        world.self_uid = Some(100);

        // Spawn self (100) at (10, 10) and peer (200) at (12, 12).
        let mut spawn = Vec::new();
        spawn.extend(encode_base220(100, 4).unwrap());
        spawn.extend(encode_base220(10, 2).unwrap());
        spawn.extend(encode_base220(10, 2).unwrap());
        spawn.extend_from_slice(b"!%");
        spawn.push(b'#' + 4);
        spawn.extend_from_slice(b"Self");
        spawn.extend_from_slice(b"w$$$$$$$$$$$$#!$");
        world.apply_world_packet(b'<', &spawn);

        let mut spawn2 = Vec::new();
        spawn2.extend(encode_base220(200, 4).unwrap());
        spawn2.extend(encode_base220(12, 2).unwrap());
        spawn2.extend(encode_base220(12, 2).unwrap());
        spawn2.extend_from_slice(b"!%");
        spawn2.push(b'#' + 4);
        spawn2.extend_from_slice(b"Peer");
        spawn2.extend_from_slice(b"w%%%%%%%%%%%%#!$");
        world.apply_world_packet(b'<', &spawn2);
        assert!(world.avatars[&100].visible);
        assert!(world.avatars[&200].visible);

        // Set a fine pixel offset on uid=200, then move uid=200 onto uid=100's tile (10, 10):
        // uid=100 must be displaced (visible=false) and uid=200's offset reset to (0, 0).
        let mut off_pkt = encode_base220(200, 4).unwrap();
        off_pkt.extend(encode_base220(266, 2).unwrap());
        off_pkt.extend(encode_base220(246, 2).unwrap());
        world.apply_avatar_offset(&off_pkt);
        assert_eq!(world.avatars[&200].offset, (10, -10));

        let mut mv = Vec::new();
        mv.extend(encode_base220(200, 4).unwrap());
        mv.extend(encode_base220(10, 2).unwrap());
        mv.extend(encode_base220(10, 2).unwrap());
        mv.extend_from_slice(b"!%");
        world.apply_world_packet(b'/', &mv);
        assert!(!world.avatars[&100].visible);
        assert!(world.avatars[&200].visible);
        assert_eq!(world.avatars[&200].offset, (0, 0));

        // 'C' hide marks avatar invisible while keeping its pose intact.
        let mut hide = Vec::new();
        hide.extend(encode_base220(200, 4).unwrap());
        hide.extend(encode_base220(15, 2).unwrap());
        hide.extend(encode_base220(16, 2).unwrap());
        world.apply_world_packet(b'C', &hide);
        assert!(!world.avatars[&200].visible);
        assert_eq!((world.avatars[&200].x, world.avatars[&200].y), (15, 16));
        assert_eq!(world.avatars[&200].pose, 2);

        // Parsing `]f` decodes the looked-at player without mutating self colors.
        let looked = WorldState::parse_looked_player_bytes(b"w&&&&&&&&&&&&$&%Looked|Furre").unwrap();
        assert_eq!(looked.name, "Looked Furre");
        assert_eq!(looked.appearance.fur, 3);
        assert_eq!(looked.appearance.gender, 1);
        assert_eq!(world.avatars[&100].appearance.fur, 1);
    }
}
