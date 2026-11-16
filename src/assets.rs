//! Asset discovery and `palette.act` loader for Furcadia data files.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// 256-color RGB palette loaded from `palette.act` (768 bytes).
#[derive(Debug, Clone)]
pub struct Palette {
    pub colors: [[u8; 3]; 256],
}

impl Default for Palette {
    fn default() -> Self {
        let mut colors = [[0u8; 3]; 256];
        for (i, slot) in colors.iter_mut().enumerate() {
            let v = i as u8;
            *slot = [v, v, v];
        }
        Self { colors }
    }
}

impl Palette {
    /// Parses a raw 768-byte Adobe Color Table (`.act`) file.
    pub fn from_bytes(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() < 768 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("palette.act too short: {} bytes (expected 768)", bytes.len()),
            ));
        }
        let mut colors = [[0u8; 3]; 256];
        for (i, slot) in colors.iter_mut().enumerate() {
            let base = i * 3;
            *slot = [bytes[base], bytes[base + 1], bytes[base + 2]];
        }
        Ok(Self { colors })
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        let bytes = fs::read(path)?;
        Self::from_bytes(&bytes)
    }

    /// Returns `[r, g, b]` for a palette index, or a deterministic fallback color.
    pub fn rgb(&self, index: u8) -> [u8; 3] {
        self.colors[index as usize]
    }
}

/// Locates Furcadia's local maps, patches, and palette assets.
#[derive(Debug, Clone)]
pub struct AssetLocator {
    pub root: Option<PathBuf>,
}

impl Default for AssetLocator {
    fn default() -> Self {
        Self::discover()
    }
}

impl AssetLocator {
    fn user_data_base() -> Option<PathBuf> {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
            .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
            .or_else(|| {
                std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join("AppData/Local"))
            })
    }

    /// Returns `~/.local/share/furcadia-client/assets` (or equivalent XDG / Windows AppData dir).
    pub fn user_assets_dir() -> Option<PathBuf> {
        Some(Self::user_data_base()?.join("furcadia-client/assets"))
    }

    /// Returns `~/.local/share/furcadia-client/dreams` (or equivalent XDG / Windows AppData dir) for cached/downloaded dreams.
    pub fn dreams_cache_dir() -> Option<PathBuf> {
        Some(Self::user_data_base()?.join("furcadia-client/dreams"))
    }

    /// Returns `~/.local/share/furcadia-client/species` (or equivalent XDG / Windows AppData dir) for cached/downloaded species archives.
    pub fn species_cache_dir() -> Option<PathBuf> {
        Some(Self::user_data_base()?.join("furcadia-client/species"))
    }

    /// Searches standard workspace-relative, XDG user, and Windows installation paths for Furcadia assets.
    pub fn discover() -> Self {
        let mut candidates = vec![
            PathBuf::from("Furcadia Windows/program Files (x86)/Furcadia"),
            PathBuf::from("../Furcadia Windows/program Files (x86)/Furcadia"),
        ];
        if let Some(user_dir) = Self::user_assets_dir() {
            candidates.push(user_dir);
        }
        if let Some(pf86) = std::env::var_os("ProgramFiles(x86)").map(PathBuf::from) {
            candidates.push(pf86.join("Furcadia"));
        }
        if let Some(pf) = std::env::var_os("ProgramFiles").map(PathBuf::from) {
            candidates.push(pf.join("Furcadia"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
            candidates.push(local.join("Programs/Furcadia"));
            candidates.push(local.join("Furcadia"));
        }
        if let Some(profile) = std::env::var_os("USERPROFILE").map(PathBuf::from) {
            candidates.push(profile.join("Furcadia"));
        }
        candidates.push(PathBuf::from(r"C:\Program Files (x86)\Furcadia"));
        candidates.push(PathBuf::from(r"C:\Program Files\Furcadia"));
        let root = candidates.into_iter().find(|p| p.is_dir());
        Self { root }
    }

    /// Copies `maps/`, `patches/default/`, and `patches/legacy/` from an existing Furcadia installation into the user assets directory.
    pub fn import_assets_from(src_root: &Path) -> io::Result<usize> {
        let dst_root = Self::user_assets_dir().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "could not resolve user data directory")
        })?;
        let mut copied = 0usize;
        for sub in ["maps", "patches/default", "patches/legacy"] {
            let src_dir = src_root.join(sub);
            if !src_dir.is_dir() {
                continue;
            }
            let dst_dir = dst_root.join(sub);
            fs::create_dir_all(&dst_dir)?;
            for entry in fs::read_dir(&src_dir)?.flatten() {
                let path = entry.path();
                if path.is_file() {
                    if let Some(name) = path.file_name() {
                        fs::copy(&path, dst_dir.join(name))?;
                        copied += 1;
                    }
                }
            }
        }
        Ok(copied)
    }

    pub fn load_default_palette(&self) -> Option<Palette> {
        let root = self.root.as_ref()?;
        let path = root.join("patches/default/palette.act");
        Palette::load(&path).ok()
    }

    /// Returns the path to a default patch file (e.g. `"floors.fox"`).
    pub fn default_patch_path(&self, filename: &str) -> Option<PathBuf> {
        let root = self.root.as_ref()?;
        let path = root.join("patches/default").join(filename);
        path.is_file().then_some(path)
    }

    /// Returns the path to a legacy patch file in `patches/legacy/` (e.g. `"floors.fox"`, `"junction.fsh"`).
    pub fn legacy_patch_path(&self, filename: &str) -> Option<PathBuf> {
        let root = self.root.as_ref()?;
        let dir = root.join("patches/legacy");
        let exact = dir.join(filename);
        if exact.is_file() {
            return Some(exact);
        }
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_file()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.eq_ignore_ascii_case(filename))
                {
                    return Some(p);
                }
            }
        }
        None
    }

    /// Returns the path to a species FOX archive (e.g. `species1.fox`),
    /// preferring any newer downloaded file in `species_cache_dir()` over bundled `patches/default/`.
    pub fn species_patch_path(&self, species_id: usize) -> Option<PathBuf> {
        let fname = format!("species{species_id}.fox");
        if let Some(cached) = Self::species_cache_dir()
            .map(|d| d.join(&fname))
            .filter(|p| p.is_file())
        {
            return Some(cached);
        }
        self.default_patch_path(&fname)
    }

    /// Returns the path to a default local-species FOX archive (e.g. `local1.fox` for `local_idx == 1`).
    pub fn local_species_patch_path(&self, local_idx: usize) -> Option<PathBuf> {
        if local_idx == 0 {
            return None;
        }
        self.default_patch_path(&format!("local{local_idx}.fox"))
    }

    /// Resolves a server map name (e.g. `"arco 99"`, `"pmfurn"`, `"allegria"`) to a local `.map` path.
    pub fn resolve_map_path(&self, server_map_name: &str) -> Option<PathBuf> {
        let normalized = server_map_name.trim().to_ascii_lowercase();
        let raw_word = normalized.split_whitespace().next().unwrap_or("");
        let first_word = raw_word.strip_prefix("pm").unwrap_or(raw_word);
        let stem = if normalized.starts_with("arco")
            || first_word.starts_with("arco")
            || first_word.starts_with("acrop")
        {
            "acropoli"
        } else if first_word.contains("vinca") {
            "NewVinca"
        } else if first_word == "furn" || normalized.contains("furrabian") {
            "FurN"
        } else if first_word.starts_with("imagin") {
            "imagine"
        } else if first_word.starts_with("allegri") {
            "allegria"
        } else if first_word.starts_with("meov") {
            "meovanni"
        } else if first_word.starts_with("hawth") {
            "hawthorn"
        } else {
            first_word
        };

        let mut search_dirs: Vec<PathBuf> = Vec::new();
        if let Some(dreams_dir) = Self::dreams_cache_dir() {
            search_dirs.push(dreams_dir);
        }
        search_dirs.push(PathBuf::from(
            "Furcadia Windows/users/appdata/local/Dragon's Eye Productions/Furcadia- original data/Permanent Maps",
        ));
        search_dirs.push(PathBuf::from(
            "Furcadia Windows/users/appdata/local/Dragon's Eye Productions/Furcadia- original data/Temporary Dreams",
        ));
        if let Some(root) = &self.root {
            search_dirs.push(root.join("maps"));
        }

        for dir in search_dirs {
            if !dir.is_dir() {
                continue;
            }
            for candidate in [raw_word, stem] {
                if candidate.is_empty() {
                    continue;
                }
                let exact = dir.join(format!("{candidate}.map"));
                if exact.is_file() {
                    return Some(exact);
                }
            }
            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("map") {
                        if let Some(file_stem) = path.file_stem().and_then(|s| s.to_str()) {
                            if file_stem.eq_ignore_ascii_case(raw_word)
                                || file_stem.eq_ignore_ascii_case(stem)
                            {
                                return Some(path);
                            }
                        }
                    }
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_workspace_assets_if_present() {
        let locator = AssetLocator::discover();
        if let Some(root) = &locator.root {
            assert!(root.join("maps").is_dir());
            assert!(locator.resolve_map_path("arco 99").is_some());
            assert!(locator.resolve_map_path("pmfurn").is_some());
            assert!(locator.resolve_map_path("pmvinca").is_some());
            assert!(locator.load_default_palette().is_some());
        }
    }
}
