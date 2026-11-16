//! Furcadia `.map` (V01.40 / V01.50) binary map parser and tile accessor.

use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

/// Parsed Furcadia map with up to 7 tile layers (`floors`, `items`, `walls`, `regions`, `effects`, `lighting`, `ambience`).
#[derive(Debug, Clone)]
pub struct FurcadiaMap {
    pub version: String,
    pub width: usize,
    pub height: usize,
    pub name: String,
    pub rating: String,
    pub is_modern: Option<bool>,
    pub floors: Vec<u16>,
    pub items: Vec<u16>,
    pub walls: Vec<u16>,
    pub regions: Vec<u16>,
    pub effects: Vec<u16>,
    pub lighting: Vec<u16>,
    pub ambience: Vec<u16>,
}

/// Errors returned when parsing a `.map` file.
#[derive(Debug)]
pub enum MapError {
    Io(io::Error),
    MissingBody,
    InvalidHeader(&'static str),
    TruncatedBody { expected: usize, actual: usize },
}

impl fmt::Display for MapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::MissingBody => write!(f, "missing BODY marker in .map file"),
            Self::InvalidHeader(field) => write!(f, "invalid or missing header field: {field}"),
            Self::TruncatedBody { expected, actual } => {
                write!(f, "truncated map body: expected {expected} bytes, got {actual}")
            }
        }
    }
}

impl std::error::Error for MapError {}

impl From<io::Error> for MapError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl FurcadiaMap {
    /// Loads and parses a `.map` file from disk.
    pub fn load(path: &Path) -> Result<Self, MapError> {
        let bytes = fs::read(path)?;
        Self::from_bytes(&bytes)
    }

    /// Parses a `.map` file from raw bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, MapError> {
        let (header_bytes, body) = split_header_and_body(bytes).ok_or(MapError::MissingBody)?;
        let header_str = String::from_utf8_lossy(header_bytes);

        let mut lines = header_str.lines();
        let version = lines
            .next()
            .ok_or(MapError::InvalidHeader("version"))?
            .trim()
            .to_owned();

        let mut width: Option<usize> = None;
        let mut height: Option<usize> = None;
        let mut name = String::new();
        let mut rating = String::new();
        let mut encoded = false;
        let mut is_modern: Option<bool> = None;

        for line in lines {
            if let Some((k, v)) = line.split_once('=') {
                match k.trim() {
                    "width" => width = v.trim().parse().ok(),
                    "height" => height = v.trim().parse().ok(),
                    "name" => name = v.trim().to_owned(),
                    "rating" => rating = v.trim().to_owned(),
                    "encoded" => encoded = v.trim() == "1",
                    "ismodern" => is_modern = Some(v.trim() == "1"),
                    _ => {}
                }
            }
        }

        let width = width.ok_or(MapError::InvalidHeader("width"))?;
        let height = height.ok_or(MapError::InvalidHeader("height"))?;
        let tile_count = width
            .checked_mul(height)
            .ok_or(MapError::InvalidHeader("dimensions"))?;
        let layer_bytes = tile_count * 2;
        let expected_7_layers = layer_bytes * 7;
        let expected_5_layers = layer_bytes * 5;
        let expected_3_layers = layer_bytes * 3;
        let ver_num = parse_version_hundredths(&version);

        let decoded_body;
        let plain_body: &[u8] = if encoded {
            decoded_body = decrypt_map_body(body, ver_num)?;
            &decoded_body
        } else {
            body
        };

        if plain_body.len() < expected_3_layers {
            return Err(MapError::TruncatedBody {
                expected: expected_3_layers,
                actual: plain_body.len(),
            });
        }

        let big_endian_u16 = encoded && ver_num >= 120;
        let reverse_y = encoded && ver_num >= 120;

        let floors = unpack_u16_layer(&plain_body[0..layer_bytes], width, height, big_endian_u16, reverse_y);
        let items = unpack_u16_layer(
            &plain_body[layer_bytes..layer_bytes * 2],
            width,
            height,
            big_endian_u16,
            reverse_y,
        );
        // Walls are stored as `2 * width` columns of `height` single bytes:
        // column `2 * x` is the NW wall byte and column `2 * x + 1` is the NE wall byte.
        let walls = unpack_wall_layer(
            &plain_body[layer_bytes * 2..layer_bytes * 3],
            width,
            height,
            reverse_y,
        );
        let (regions, effects) = if plain_body.len() >= expected_5_layers {
            (
                unpack_u16_layer(
                    &plain_body[layer_bytes * 3..layer_bytes * 4],
                    width,
                    height,
                    big_endian_u16,
                    reverse_y,
                ),
                unpack_u16_layer(
                    &plain_body[layer_bytes * 4..layer_bytes * 5],
                    width,
                    height,
                    big_endian_u16,
                    reverse_y,
                ),
            )
        } else {
            (vec![0u16; tile_count], vec![0u16; tile_count])
        };

        let (lighting, ambience) = if plain_body.len() >= expected_7_layers {
            (
                unpack_u16_layer(
                    &plain_body[layer_bytes * 5..layer_bytes * 6],
                    width,
                    height,
                    big_endian_u16,
                    reverse_y,
                ),
                unpack_u16_layer(
                    &plain_body[layer_bytes * 6..layer_bytes * 7],
                    width,
                    height,
                    big_endian_u16,
                    reverse_y,
                ),
            )
        } else {
            (vec![0u16; tile_count], vec![0u16; tile_count])
        };

        Ok(Self {
            version,
            width,
            height,
            name,
            rating,
            is_modern,
            floors,
            items,
            walls,
            regions,
            effects,
            lighting,
            ambience,
        })
    }

    #[inline]
    pub fn index(&self, x: usize, y: usize) -> Option<usize> {
        if x < self.width && y < self.height {
            Some(y * self.width + x)
        } else {
            None
        }
    }

    pub fn floor_at(&self, x: usize, y: usize) -> u16 {
        self.index(x, y).map_or(0, |i| self.floors[i])
    }

    pub fn item_at(&self, x: usize, y: usize) -> u16 {
        self.index(x, y).map_or(0, |i| self.items[i])
    }

    pub fn wall_at(&self, x: usize, y: usize) -> u16 {
        self.index(x, y).map_or(0, |i| self.walls[i])
    }

    pub fn region_at(&self, x: usize, y: usize) -> u16 {
        self.index(x, y).map_or(0, |i| self.regions[i])
    }

    pub fn effect_at(&self, x: usize, y: usize) -> u16 {
        self.index(x, y).map_or(0, |i| self.effects[i])
    }

    pub fn lighting_at(&self, x: usize, y: usize) -> u16 {
        self.index(x, y).map_or(0, |i| self.lighting[i])
    }

    pub fn ambience_at(&self, x: usize, y: usize) -> u16 {
        self.index(x, y).map_or(0, |i| self.ambience[i])
    }

    pub fn set_floor(&mut self, x: usize, y: usize, val: u16) {
        if let Some(i) = self.index(x, y) {
            self.floors[i] = val;
        }
    }

    pub fn set_item(&mut self, x: usize, y: usize, val: u16) {
        if let Some(i) = self.index(x, y) {
            self.items[i] = val;
        }
    }

    pub fn set_wall(&mut self, x: usize, y: usize, val: u16) {
        if let Some(i) = self.index(x, y) {
            self.walls[i] = val;
        }
    }

    pub fn set_region(&mut self, x: usize, y: usize, val: u16) {
        if let Some(i) = self.index(x, y) {
            self.regions[i] = val;
        }
    }

    pub fn set_effect(&mut self, x: usize, y: usize, val: u16) {
        if let Some(i) = self.index(x, y) {
            self.effects[i] = val;
        }
    }

    pub fn set_lighting(&mut self, x: usize, y: usize, val: u16) {
        if let Some(i) = self.index(x, y) {
            self.lighting[i] = val;
        }
    }

    pub fn set_ambience(&mut self, x: usize, y: usize, val: u16) {
        if let Some(i) = self.index(x, y) {
            self.ambience[i] = val;
        }
    }

    /// Serializes the map back into `.map` binary format for Dream Editor compatibility.
    pub fn serialize(&self) -> Vec<u8> {
        let modern_line = match self.is_modern {
            Some(true) => "ismodern=1\n",
            Some(false) => "ismodern=0\n",
            None => "",
        };
        let mut out = format!(
            "{}\nheight={}\nwidth={}\nname={}\nrating={}\n{}encoded=0\nBODY\n",
            self.version, self.height, self.width, self.name, self.rating, modern_line
        )
        .into_bytes();

        let has_extra =
            self.lighting.iter().any(|&v| v != 0) || self.ambience.iter().any(|&v| v != 0);
        let write_u16_layer = |buf: &mut Vec<u8>, layer: &[u16]| {
            for x in 0..self.width {
                for y in 0..self.height {
                    let val = layer[y * self.width + x];
                    buf.extend_from_slice(&val.to_le_bytes());
                }
            }
        };

        write_u16_layer(&mut out, &self.floors);
        write_u16_layer(&mut out, &self.items);
        // Walls: `2 * width` columns of `height` single bytes (`2 * x` = NW, `2 * x + 1` = NE).
        for x in 0..self.width {
            for y in 0..self.height {
                let w = self.walls[y * self.width + x];
                out.push((w & 0xff) as u8);
            }
            for y in 0..self.height {
                let w = self.walls[y * self.width + x];
                out.push(((w >> 8) & 0xff) as u8);
            }
        }
        write_u16_layer(&mut out, &self.regions);
        write_u16_layer(&mut out, &self.effects);
        if has_extra {
            write_u16_layer(&mut out, &self.lighting);
            write_u16_layer(&mut out, &self.ambience);
        }
        out
    }
}

fn parse_version_hundredths(version_line: &str) -> u32 {
    for token in version_line.split_whitespace() {
        let trimmed = token.trim_start_matches('V').trim_start_matches('v');
        if let Some((maj, min)) = trimmed.split_once('.') {
            if let (Ok(m), Ok(n)) = (maj.parse::<u32>(), min.parse::<u32>()) {
                return m * 100 + n;
            }
        }
    }
    140
}

fn crc32_step(mut crc: u32, byte: u8) -> u32 {
    crc ^= byte as u32;
    for _ in 0..8 {
        if (crc & 1) != 0 {
            crc = (crc >> 1) ^ 0xedb8_8320;
        } else {
            crc >>= 1;
        }
    }
    crc
}

const PERM_NEW: [usize; 16] = [1, 15, 8, 12, 5, 11, 7, 4, 0, 14, 10, 2, 6, 3, 9, 13];
const PERM_OLD: [usize; 16] = [1, 12, 4, 8, 15, 0, 11, 2, 14, 7, 6, 9, 13, 3, 10, 5];

fn decrypt_map_body(body: &[u8], ver_num: u32) -> Result<Vec<u8>, MapError> {
    if body.is_empty() {
        return Err(MapError::TruncatedBody {
            expected: 1,
            actual: 0,
        });
    }
    let perm = if ver_num >= 120 { &PERM_NEW } else { &PERM_OLD };
    let mut out = Vec::with_capacity(body.len());
    let b0 = body[0];
    out.push(b0);
    let mut crc = crc32_step(0xffff_ffffu32, b0);

    for blk in body[1..].chunks_exact(16) {
        for &idx in perm {
            let pb = blk[idx].wrapping_sub(crc as u8);
            crc = crc32_step(crc, pb);
            out.push(pb);
        }
    }
    Ok(out)
}

fn split_header_and_body(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    for marker in [b"BODY\r\n".as_slice(), b"BODY\n".as_slice()] {
        if let Some(pos) = bytes.windows(marker.len()).position(|w| w == marker) {
            return Some((&bytes[..pos], &bytes[pos + marker.len()..]));
        }
    }
    None
}

fn unpack_u16_layer(
    bytes: &[u8],
    width: usize,
    height: usize,
    big_endian: bool,
    reverse_y: bool,
) -> Vec<u16> {
    let mut grid = vec![0u16; width * height];
    for x in 0..width {
        for y in 0..height {
            let file_y = if reverse_y { height - 1 - y } else { y };
            let src_idx = (x * height + file_y) * 2;
            let val = if big_endian {
                u16::from_be_bytes([bytes[src_idx], bytes[src_idx + 1]])
            } else {
                u16::from_le_bytes([bytes[src_idx], bytes[src_idx + 1]])
            };
            grid[y * width + x] = val;
        }
    }
    grid
}

fn unpack_wall_layer(bytes: &[u8], width: usize, height: usize, reverse_y: bool) -> Vec<u16> {
    let mut grid = vec![0u16; width * height];
    for x in 0..width {
        for y in 0..height {
            let file_y = if reverse_y { height - 1 - y } else { y };
            let nw = bytes[(2 * x) * height + file_y] as u16;
            let ne = bytes[(2 * x + 1) * height + file_y] as u16;
            grid[y * width + x] = nw | (ne << 8);
        }
    }
    grid
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::AssetLocator;

    #[test]
    fn parse_synthetic_map() {
        let mut raw = b"MAP V01.40 Furcadia\nheight=2\nwidth=2\nname=Test\nBODY\n".to_vec();
        // 5 layers stored column-major (x=0..2, y=0..2), walls stored as `2 * width` u8 columns
        for layer in 0u16..5 {
            if layer == 2 {
                for x in 0u16..2 {
                    for y in 0u16..2 {
                        let tile = y * 2 + x;
                        raw.push((20 + tile) as u8);
                    }
                    for y in 0u16..2 {
                        let tile = y * 2 + x;
                        raw.push((80 + tile) as u8);
                    }
                }
            } else {
                for x in 0u16..2 {
                    for y in 0u16..2 {
                        let tile = y * 2 + x;
                        raw.extend_from_slice(&(layer * 10 + tile).to_le_bytes());
                    }
                }
            }
        }
        let map = FurcadiaMap::from_bytes(&raw).unwrap();
        assert_eq!(map.width, 2);
        assert_eq!(map.height, 2);
        assert_eq!(map.floor_at(1, 0), 1);
        assert_eq!(map.item_at(0, 1), 12);
        assert_eq!(map.wall_at(1, 1), 23u16 | (83u16 << 8));
        assert_eq!(map.wall_at(0, 1), 22u16 | (82u16 << 8));
        assert_eq!(map.region_at(0, 0), 30);
        assert_eq!(map.effect_at(1, 1), 43);

        let rt = FurcadiaMap::from_bytes(&map.serialize()).unwrap();
        assert_eq!(rt.item_at(0, 1), 12);
        assert_eq!(rt.wall_at(1, 1), 23u16 | (83u16 << 8));
        assert_eq!(rt.wall_at(0, 1), 22u16 | (82u16 << 8));
    }

    #[test]
    fn parse_synthetic_v150_seven_layers_and_ismodern() {
        let mut raw =
            b"MAP V01.50 Furcadia\nheight=2\nwidth=2\nname=ModernTest\nismodern=1\nBODY\n".to_vec();
        for layer in 0u16..7 {
            if layer == 2 {
                for x in 0u16..2 {
                    for y in 0u16..2 {
                        let tile = y * 2 + x;
                        raw.push((10 + tile) as u8);
                    }
                    for y in 0u16..2 {
                        let tile = y * 2 + x;
                        raw.push((50 + tile) as u8);
                    }
                }
            } else {
                for x in 0u16..2 {
                    for y in 0u16..2 {
                        let tile = y * 2 + x;
                        raw.extend_from_slice(&(layer * 100 + tile).to_le_bytes());
                    }
                }
            }
        }
        let map = FurcadiaMap::from_bytes(&raw).unwrap();
        assert_eq!(map.is_modern, Some(true));
        assert_eq!(map.wall_at(1, 0), 11u16 | (51u16 << 8));
        assert_eq!(map.lighting_at(1, 0), 501);
        assert_eq!(map.ambience_at(0, 1), 602);
        let rt = FurcadiaMap::from_bytes(&map.serialize()).unwrap();
        assert_eq!(rt.is_modern, Some(true));
        assert_eq!(rt.wall_at(1, 0), 11u16 | (51u16 << 8));
        assert_eq!(rt.lighting_at(1, 0), 501);
        assert_eq!(rt.ambience_at(0, 1), 602);
    }

    #[test]
    fn unpack_wall_layer_reverses_2w_columns_separately() {
        // 2x3 map => 2 * width = 4 columns of height = 3 bytes:
        // col 0 (x=0 NW): file_y 0..3 -> tile y=2,1,0
        // col 1 (x=0 NE): file_y 0..3 -> tile y=2,1,0
        // col 2 (x=1 NW): file_y 0..3 -> tile y=2,1,0
        // col 3 (x=1 NE): file_y 0..3 -> tile y=2,1,0
        let raw = [
            12, 11, 10, // x=0 NW (y=2, 1, 0)
            42, 41, 40, // x=0 NE (y=2, 1, 0)
            22, 21, 20, // x=1 NW (y=2, 1, 0)
            52, 51, 50, // x=1 NE (y=2, 1, 0)
        ];
        let walls = unpack_wall_layer(&raw, 2, 3, true);
        assert_eq!(walls[0], 10u16 | (40u16 << 8));
        assert_eq!(walls[4], 12u16 | (42u16 << 8));
        assert_eq!(walls[1 * 2 + 1], 21u16 | (51u16 << 8));
    }

    #[test]
    fn parse_encoded_v150_pmfurn_if_present() {
        let path = Path::new("/tmp/pmfurn.bin");
        if !path.exists() {
            return;
        }
        let bytes = fs::read(path).unwrap();
        let entries = crate::rch::parse_fr01(&bytes).unwrap();
        let map_entry = entries
            .iter()
            .find(|e| e.clean_name == "default.map")
            .expect("default.map in pmfurn");
        let map = FurcadiaMap::from_bytes(&map_entry.data).expect("decode pmfurn V01.50 map");
        assert_eq!(map.width, 141);
        assert_eq!(map.height, 260);
        assert_eq!(map.floor_at(64, 49), 522);
        assert_eq!(map.floor_at(21, 39), 575);
    }

    #[test]
    fn parse_workspace_acropoli_map_if_present() {
        let locator = AssetLocator::discover();
        if let Some(path) = locator.resolve_map_path("arco 99") {
            let map = FurcadiaMap::load(&path).unwrap();
            assert_eq!(map.width, 104);
            assert_eq!(map.height, 166);
            assert_eq!(map.floors.len(), 104 * 166);
        }
    }

    #[test]
    fn parse_workspace_furn_map_if_present() {
        let locator = AssetLocator::discover();
        if let Some(path) = locator.resolve_map_path("pmfurn") {
            let map = FurcadiaMap::load(&path).unwrap();
            assert_eq!(map.width, 51);
            assert_eq!(map.height, 150);
            assert_eq!(map.floors.len(), 51 * 150);
        }
    }
}
