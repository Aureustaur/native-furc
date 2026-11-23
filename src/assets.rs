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
        Self {
            colors: crate::furc_colors::MASTER_PALETTE,
        }
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

    /// Returns a copy of this palette with Furcadia's classic 8-bit remappable index
    /// ranges encoded as FOX5 `[R = shade, G = channel, B = 0]` remap markers so
    /// 8-bit `FSH`/`FSHX` portraits can be recolored by [`crate::furc_colors::RemapLuts`].
    pub fn with_classic_remap_markers(&self) -> Self {
        let mut colors = self.colors;
        // Ensure non-remapped opaque palette indices never accidentally carry `B == 0`.
        for slot in colors.iter_mut().skip(1) {
            if slot[2] == 0 {
                slot[2] = 1;
            }
        }
        // 8-shade ramp slots: `build_8stop_lut` places `stops8[0..=7]` at `0xc0 - step * 0x18`.
        const RAMP_SLOTS: [(usize, u8); 8] = [
            (200, 15), // Fur      (200..=207 -> G=15)
            (136, 4),  // Markings (136..=143 -> G=4)
            (128, 13), // Hair     (128..=135 -> G=13)
            (72, 6),   // Vest     ( 72..= 79 -> G=6)
            (88, 9),   // Bracers  ( 88..= 95 -> G=9)
            (32, 2),   // Cape     ( 32..= 39 -> G=2)
            (16, 14),  // Boots    ( 16..= 23 -> G=14)
            (224, 12), // Trousers (224..=231 -> G=12)
        ];
        for (base, channel) in RAMP_SLOTS {
            for step in 0..8usize {
                let shade = (0xc0 - step * 0x18) as u8;
                colors[base + step] = [shade, channel, 0];
            }
        }
        // Single-index slots: `build_3stop_lut` places `mid` at `0x80`.
        colors[50] = [0x80, 3, 0]; // Eyes  (50 -> G=3)
        colors[11] = [0x80, 1, 0]; // Badge (11 -> G=1)
        Self { colors }
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

    /// Returns `~/.local/share/furcadia-client` (or equivalent XDG / Windows AppData dir).
    pub fn cache_root_dir() -> Option<PathBuf> {
        Some(Self::user_data_base()?.join("furcadia-client"))
    }

    /// Returns `~/.local/share/furcadia-client/assets` (or equivalent XDG / Windows AppData dir).
    pub fn user_assets_dir() -> Option<PathBuf> {
        Some(Self::cache_root_dir()?.join("assets"))
    }

    /// Returns `~/.local/share/furcadia-client/dreams` (or equivalent XDG / Windows AppData dir) for cached/downloaded dreams.
    pub fn dreams_cache_dir() -> Option<PathBuf> {
        Some(Self::cache_root_dir()?.join("dreams"))
    }

    /// Returns `~/.local/share/furcadia-client/species` (or equivalent XDG / Windows AppData dir) for cached/downloaded species archives.
    pub fn species_cache_dir() -> Option<PathBuf> {
        Some(Self::cache_root_dir()?.join("species"))
    }

    /// Returns `~/.local/share/furcadia-client/patches` (or equivalent XDG / Windows AppData dir)
    /// for cached stock world patches (`modern/` and `legacy/`).
    pub fn stock_patches_cache_dir() -> Option<PathBuf> {
        Some(Self::cache_root_dir()?.join("patches"))
    }

    /// Returns `<stock_patches_cache_dir>/<subdir>/<filename>` (e.g. `patches/legacy/floors.fox`).
    pub fn stock_patch_cache_path(subdir: &str, filename: &str) -> Option<PathBuf> {
        Some(Self::stock_patches_cache_dir()?.join(subdir).join(filename))
    }

    /// Official URL for a stock world patch archive (`modern/<file>` or `legacy/<file>`).
    pub fn stock_patch_url(subdir: &str, filename: &str) -> String {
        format!("https://play.furcadia.com/{subdir}/{filename}")
    }

    fn dir_has_any_file(dir: &Path) -> bool {
        let Ok(entries) = fs::read_dir(dir) else {
            return false;
        };
        entries.flatten().any(|entry| entry.path().is_file())
    }

    /// Returns true if `dir` looks like a populated Furcadia asset root rather than an empty folder skeleton.
    pub fn is_populated_assets_root(dir: &Path) -> bool {
        ["patches/default", "patches/legacy", "maps"]
            .into_iter()
            .any(|sub| Self::dir_has_any_file(&dir.join(sub)))
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
        let root = candidates
            .into_iter()
            .find(|p| Self::is_populated_assets_root(p));
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

    fn find_patch_in_dir(dir: &Path, filename: &str) -> Option<PathBuf> {
        let exact = dir.join(filename);
        if exact.is_file() {
            return Some(exact);
        }
        if let Ok(entries) = fs::read_dir(dir) {
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

    fn default_patch_path_with_stock_dir(
        &self,
        filename: &str,
        stock_patches_dir: Option<&Path>,
    ) -> Option<PathBuf> {
        if let Some(root) = self.root.as_ref() {
            if let Some(p) = Self::find_patch_in_dir(&root.join("patches/default"), filename) {
                return Some(p);
            }
        }
        let dir = stock_patches_dir?.join("modern");
        Self::find_patch_in_dir(&dir, filename)
    }

    fn legacy_patch_path_with_stock_dir(
        &self,
        filename: &str,
        stock_patches_dir: Option<&Path>,
    ) -> Option<PathBuf> {
        if let Some(root) = self.root.as_ref() {
            if let Some(p) = Self::find_patch_in_dir(&root.join("patches/legacy"), filename) {
                return Some(p);
            }
        }
        let dir = stock_patches_dir?.join("legacy");
        Self::find_patch_in_dir(&dir, filename)
    }

    /// Returns the path to a default patch file (e.g. `"floors.fox"`), preferring
    /// imported/bundled `patches/default/` and falling back to cached `patches/modern/`.
    pub fn default_patch_path(&self, filename: &str) -> Option<PathBuf> {
        let stock_dir = Self::stock_patches_cache_dir();
        self.default_patch_path_with_stock_dir(filename, stock_dir.as_deref())
    }

    /// Returns the path to a legacy patch file in `patches/legacy/` (e.g. `"floors.fox"`, `"junction.fsh"`),
    /// falling back to cached `patches/legacy/`.
    pub fn legacy_patch_path(&self, filename: &str) -> Option<PathBuf> {
        let stock_dir = Self::stock_patches_cache_dir();
        self.legacy_patch_path_with_stock_dir(filename, stock_dir.as_deref())
    }

    /// Returns `(file_count, total_bytes)` for everything under `dir` (recursive). Missing dir → `(0, 0)`.
    pub fn dir_usage(dir: &Path) -> (usize, u64) {
        let mut files = 0usize;
        let mut bytes = 0u64;
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            let Ok(entries) = fs::read_dir(&d) else {
                continue;
            };
            for entry in entries.flatten() {
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                if meta.is_dir() {
                    stack.push(entry.path());
                } else {
                    files += 1;
                    bytes += meta.len();
                }
            }
        }
        (files, bytes)
    }

    /// Deletes everything inside `dir` (keeping `dir` itself). Returns `(files_removed, bytes_removed)`.
    pub fn clear_dir(dir: &Path) -> io::Result<(usize, u64)> {
        let usage = Self::dir_usage(dir);
        if dir.is_dir() {
            for entry in fs::read_dir(dir)?.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    fs::remove_dir_all(&path)?;
                } else {
                    fs::remove_file(&path)?;
                }
            }
        }
        Ok(usage)
    }

    /// Returns `<species_cache_dir>/common` — cache for stock `speciesN.fox` / `localN.fox`
    /// archives downloaded from the official web-client asset host.
    ///
    /// Kept separate from the root of `species_cache_dir()`, where older releases wrongly
    /// stored `DPlayerN.fox` dynamic-avatar archives under the name `speciesN.fox`.
    pub fn stock_species_cache_dir() -> Option<PathBuf> {
        Some(Self::species_cache_dir()?.join("common"))
    }

    /// Official URL for a stock species archive (`speciesN.fox` or `localN.fox`).
    pub fn stock_species_url(filename: &str) -> String {
        format!("https://play.furcadia.com/common/{filename}")
    }

    /// Removes `speciesN.fox` files left in the root of `species_cache_dir()` by releases up to v1.1.5.
    /// Those were `DPlayerN.fox` dynamic avatars saved under a stock species name, which made every
    /// avatar render as the wrong species on installs without bundled Furcadia patches.
    pub fn purge_mislabeled_species_cache() {
        if let Some(dir) = Self::species_cache_dir() {
            Self::purge_mislabeled_species_cache_in(&dir);
        }
    }

    /// Removes root-level `speciesN.fox` files from `dir` while leaving subdirectories
    /// (such as `common/`), `dplayer*.fox`, portraits, and chat images untouched.
    pub fn purge_mislabeled_species_cache_in(dir: &Path) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let lower = name.to_ascii_lowercase();
            let is_stock_name = lower
                .strip_prefix("species")
                .and_then(|rest| rest.strip_suffix(".fox"))
                .is_some_and(|num| !num.is_empty() && num.bytes().all(|b| b.is_ascii_digit()));
            if is_stock_name {
                let _ = fs::remove_file(&path);
            }
        }
    }

    /// Returns the path to a species FOX archive (e.g. `species1.fox`),
    /// preferring any newer downloaded file in `stock_species_cache_dir()` over bundled `patches/default/`.
    pub fn species_patch_path(&self, species_id: usize) -> Option<PathBuf> {
        let fname = format!("species{species_id}.fox");
        if let Some(cached) = Self::stock_species_cache_dir()
            .map(|d| d.join(&fname))
            .filter(|p| p.is_file())
        {
            return Some(cached);
        }
        self.default_patch_path(&fname)
    }

    /// Returns the path to a local-species FOX archive (e.g. `local1.fox` for `local_idx == 1`),
    /// preferring a downloaded copy in `stock_species_cache_dir()` over bundled `patches/default/`.
    pub fn local_species_patch_path(&self, local_idx: usize) -> Option<PathBuf> {
        if local_idx == 0 {
            return None;
        }
        let fname = format!("local{local_idx}.fox");
        if let Some(cached) = Self::stock_species_cache_dir()
            .map(|d| d.join(&fname))
            .filter(|p| p.is_file())
        {
            return Some(cached);
        }
        self.default_patch_path(&fname)
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

    #[test]
    fn default_palette_matches_furcadia_master_palette() {
        let pal = Palette::default();
        assert_eq!(pal.rgb(1), [0, 247, 55]);
        assert_eq!(pal.rgb(10), [255, 55, 55]);
        if let Some(loaded) = AssetLocator::discover().load_default_palette() {
            assert_eq!(pal.colors, loaded.colors);
        }
    }

    #[test]
    fn dir_usage_and_clear_dir_manage_nested_cache_files() {
        let root = std::env::temp_dir().join(format!(
            "furc_assets_test_clear_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        let sub = root.join("common");
        fs::create_dir_all(&sub).expect("create temp sub");
        fs::write(root.join("a.bin"), b"hello").expect("write a");
        fs::write(sub.join("b.bin"), b"world!").expect("write b");

        assert_eq!(AssetLocator::dir_usage(&root), (2, 11));
        let cleared = AssetLocator::clear_dir(&root).expect("clear_dir");
        assert_eq!(cleared, (2, 11));
        assert!(root.is_dir());
        assert_eq!(AssetLocator::dir_usage(&root), (0, 0));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn purge_mislabeled_species_cache_only_removes_root_stock_names() {
        let root = std::env::temp_dir().join(format!(
            "furc_assets_test_purge_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        let common = root.join("common");
        fs::create_dir_all(&common).expect("create common");
        fs::write(root.join("species4.fox"), b"bad1").expect("write species4");
        fs::write(root.join("SPECIES108.FOX"), b"bad2").expect("write SPECIES108");
        fs::write(root.join("dplayer10.fox"), b"dp").expect("write dplayer10");
        fs::write(root.join("pp123.fprt"), b"pp").expect("write pp123");
        fs::write(root.join("chat_img_00.bin"), b"img").expect("write chat img");
        fs::write(common.join("species4.fox"), b"good").expect("write common/species4");

        AssetLocator::purge_mislabeled_species_cache_in(&root);

        assert!(!root.join("species4.fox").exists());
        assert!(!root.join("SPECIES108.FOX").exists());
        assert!(root.join("dplayer10.fox").is_file());
        assert!(root.join("pp123.fprt").is_file());
        assert!(root.join("chat_img_00.bin").is_file());
        assert!(common.join("species4.fox").is_file());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn is_populated_assets_root_rejects_empty_skeleton_and_accepts_populated_dir() {
        let root = std::env::temp_dir().join(format!(
            "furc_assets_test_root_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("patches/default")).expect("create default skeleton");
        fs::create_dir_all(root.join("patches/legacy")).expect("create legacy skeleton");
        fs::create_dir_all(root.join("maps")).expect("create maps skeleton");

        assert!(!AssetLocator::is_populated_assets_root(&root));

        fs::write(root.join("patches/legacy/floors.fox"), b"test").expect("write legacy patch");
        assert!(AssetLocator::is_populated_assets_root(&root));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn stock_patch_url_targets_play_furcadia_host() {
        assert_eq!(
            AssetLocator::stock_patch_url("legacy", "floors.fox"),
            "https://play.furcadia.com/legacy/floors.fox"
        );
        assert_eq!(
            AssetLocator::stock_patch_url("modern", "items.fox"),
            "https://play.furcadia.com/modern/items.fox"
        );
    }

    #[test]
    fn patch_paths_fall_back_to_stock_cache_dir_when_root_missing_or_partial() {
        let base = std::env::temp_dir().join(format!(
            "furc_assets_test_fallback_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&base);
        let stock_dir = base.join("stock");
        let root_dir = base.join("imported");
        fs::create_dir_all(stock_dir.join("modern")).expect("create stock modern");
        fs::create_dir_all(stock_dir.join("legacy")).expect("create stock legacy");
        fs::create_dir_all(root_dir.join("patches/default")).expect("create root default");
        fs::create_dir_all(root_dir.join("patches/legacy")).expect("create root legacy");

        let stock_modern_floors = stock_dir.join("modern/floors.fox");
        let stock_legacy_floors = stock_dir.join("legacy/floors.fox");
        let stock_legacy_floors2 = stock_dir.join("legacy/floors2.fox");
        let stock_legacy_junc = stock_dir.join("legacy/junction.fox");
        fs::write(&stock_modern_floors, b"stock_mod").expect("write stock modern floors");
        fs::write(&stock_legacy_floors, b"stock_leg").expect("write stock legacy floors");
        fs::write(&stock_legacy_floors2, b"stock_leg2").expect("write stock legacy floors2");
        fs::write(&stock_legacy_junc, b"stock_junc").expect("write stock legacy junc");

        let no_root = AssetLocator { root: None };
        assert_eq!(
            no_root.default_patch_path_with_stock_dir("floors.fox", Some(&stock_dir)),
            Some(stock_modern_floors.clone())
        );
        assert_eq!(
            no_root.legacy_patch_path_with_stock_dir("floors.fox", Some(&stock_dir)),
            Some(stock_legacy_floors.clone())
        );
        assert_eq!(
            no_root.legacy_patch_path_with_stock_dir("JUNCTION.FOX", Some(&stock_dir)),
            Some(stock_legacy_junc)
        );

        let root_modern_floors = root_dir.join("patches/default/floors.fox");
        fs::write(&root_modern_floors, b"root_mod").expect("write root modern floors");
        let partial_root = AssetLocator {
            root: Some(root_dir),
        };
        assert_eq!(
            partial_root.default_patch_path_with_stock_dir("floors.fox", Some(&stock_dir)),
            Some(root_modern_floors)
        );
        assert_eq!(
            partial_root.legacy_patch_path_with_stock_dir("floors2.fox", Some(&stock_dir)),
            Some(stock_legacy_floors2)
        );

        let _ = fs::remove_dir_all(&base);
    }
}
