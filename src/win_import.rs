//! Importer for Windows/Wine Furcadia installations, Pounce watchlists, character INIs, and logs.

use std::fs;
use std::path::{Path, PathBuf};

use crate::assets::AssetLocator;
use crate::logs::default_logs_dir;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedCharIni {
    pub name: String,
    pub desc: String,
    pub colors: String,
}

#[derive(Debug, Default, Clone)]
pub struct WinImportSummary {
    pub assets_copied: usize,
    pub pounce_added: Vec<String>,
    pub characters_found: Vec<ImportedCharIni>,
    pub logs_copied: usize,
    pub warnings: Vec<String>,
}

fn push_unique_name(target: &mut Vec<String>, existing: &[String], raw: &str) {
    let cleaned = raw
        .trim()
        .trim_matches('"')
        .replace('|', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if cleaned.is_empty() || cleaned.len() > 48 {
        return;
    }
    let already_existing = existing.iter().any(|x| x.eq_ignore_ascii_case(&cleaned));
    let already_added = target.iter().any(|x| x.eq_ignore_ascii_case(&cleaned));
    if !already_existing && !already_added {
        target.push(cleaned);
    }
}

/// Parses a Windows `pounce.txt` or `online.ini` text blob for watched character names.
pub fn parse_pounce_file_contents(text: &str, existing: &[String]) -> Vec<String> {
    let mut added = Vec::new();
    let mut in_pounce_section = false;
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let sec = line[1..line.len() - 1].trim().to_ascii_lowercase();
            in_pounce_section = matches!(
                sec.as_str(),
                "pounce" | "watchlist" | "friends" | "buddies" | "online"
            );
            continue;
        }
        if line.contains('\t') {
            let mut cols = line.split('\t');
            let first = cols.next().unwrap_or("").trim();
            if first.eq_ignore_ascii_case("Furre") {
                if let Some(name) = cols.next() {
                    push_unique_name(&mut added, existing, name);
                }
            }
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let key = k.trim().to_ascii_lowercase();
            if in_pounce_section
                || key.contains("pounce")
                || key.contains("buddy")
                || key.contains("friend")
                || key.contains("watch")
                || key.starts_with("user")
            {
                for part in v.split(',') {
                    push_unique_name(&mut added, existing, part);
                }
            }
        } else {
            // Plain line-by-line `pounce.txt` format.
            push_unique_name(&mut added, existing, line);
        }
    }
    added
}

/// Parses a Furcadia Windows character `.ini` file if it contains character metadata.
pub fn parse_character_ini(text: &str) -> Option<ImportedCharIni> {
    let mut name = String::new();
    let mut desc = String::new();
    let mut colors = String::new();
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if let Some((k, v)) = line.split_once('=') {
            let key = k.trim().to_ascii_lowercase();
            let val = v.trim().trim_matches('"');
            match key.as_str() {
                "name" | "character" | "charname" => name = val.replace('|', " "),
                "desc" | "description" => desc = val.to_owned(),
                "colors" | "color" => colors = val.to_owned(),
                _ => {}
            }
        }
    }
    if name.is_empty() {
        None
    } else {
        Some(ImportedCharIni { name, desc, colors })
    }
}

fn push_unique_dir(dirs: &mut Vec<PathBuf>, p: PathBuf) {
    if p.is_dir() && !dirs.iter().any(|existing| existing == &p) {
        dirs.push(p);
    }
}

fn find_child_ci(dir: &Path, name: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir()
            && p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.eq_ignore_ascii_case(name))
        {
            return Some(p);
        }
    }
    None
}

fn collect_windows_user_data_roots(base: &Path, out: &mut Vec<PathBuf>) {
    if let Some(users_dir) = find_child_ci(base, "users") {
        // Direct layout: `users/appdata/local/Dragon's Eye Productions`
        if let Some(appdata) = find_child_ci(&users_dir, "appdata") {
            if let Some(local) = find_child_ci(&appdata, "local") {
                if let Some(dep) = find_child_ci(&local, "Dragon's Eye Productions") {
                    push_unique_dir(out, dep);
                }
            }
        }
        if let Some(docs) = find_child_ci(&users_dir, "Documents") {
            if let Some(furc) = find_child_ci(&docs, "Furcadia") {
                push_unique_dir(out, furc);
            }
        }
        // Per-user layout: `Users/<user>/AppData/Local/Dragon's Eye Productions`
        if let Ok(entries) = fs::read_dir(&users_dir) {
            for entry in entries.flatten() {
                let user_dir = entry.path();
                if !user_dir.is_dir() {
                    continue;
                }
                if let Some(appdata) = find_child_ci(&user_dir, "AppData") {
                    if let Some(local) = find_child_ci(&appdata, "Local") {
                        if let Some(dep) = find_child_ci(&local, "Dragon's Eye Productions") {
                            push_unique_dir(out, dep);
                        }
                    }
                }
                if let Some(docs) = find_child_ci(&user_dir, "Documents") {
                    if let Some(furc) = find_child_ci(&docs, "Furcadia") {
                        push_unique_dir(out, furc);
                    }
                }
            }
        }
    }
    if let Some(dep) = find_child_ci(base, "Dragon's Eye Productions") {
        push_unique_dir(out, dep);
    }
}

/// Discovers sibling Windows AppData (`Dragon's Eye Productions`) and Documents roots from `src`.
pub fn discover_windows_companion_dirs(src: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    push_unique_dir(&mut roots, src.to_path_buf());
    let mut cur = Some(src);
    for _ in 0..4 {
        if let Some(dir) = cur {
            collect_windows_user_data_roots(dir, &mut roots);
            cur = dir.parent();
        }
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        let wine_c = home.join(".wine").join("drive_c");
        if wine_c.is_dir() {
            collect_windows_user_data_roots(&wine_c, &mut roots);
        }
    }
    roots
}

/// Scans `src` (Windows `Program Files (x86)/Furcadia`, `Documents/Furcadia`, or Wine prefix)
/// and any sibling `AppData/Local/Dragon's Eye Productions` / `Documents/Furcadia` folders
/// and imports assets, Pounce watchlists, character `.ini` profiles, and chat logs.
pub fn import_windows_furcadia_dir(src: &Path, existing_watchlist: &[String]) -> WinImportSummary {
    let mut summary = WinImportSummary::default();
    if !src.exists() {
        summary
            .warnings
            .push(format!("Path does not exist: {}", src.display()));
        return summary;
    }

    match AssetLocator::import_assets_from(src) {
        Ok(count) => summary.assets_copied = count,
        Err(err) => summary
            .warnings
            .push(format!("Asset import skipped/failed: {err}")),
    }

    let mut combined_existing: Vec<String> = existing_watchlist.to_vec();
    let mut stack: Vec<(PathBuf, usize)> = discover_windows_companion_dirs(src)
        .into_iter()
        .map(|p| (p, 0))
        .collect();
    let logs_dest = default_logs_dir().map(|d| d.join("imported"));

    while let Some((dir, depth)) = stack.pop() {
        if depth > 5 {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push((path, depth + 1));
                continue;
            }
            let Some(fname) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let lower = fname.to_ascii_lowercase();
            let is_pounce_file = lower == "pounce.txt"
                || lower == "pounce.ini"
                || lower == "online.ini"
                || (lower.starts_with("online_") && lower.ends_with(".ini"));
            if is_pounce_file {
                if let Ok(text) = fs::read_to_string(&path) {
                    let found = parse_pounce_file_contents(&text, &combined_existing);
                    for name in found {
                        combined_existing.push(name.clone());
                        summary.pounce_added.push(name);
                    }
                }
            } else if lower.ends_with(".ini") {
                if let Ok(text) = fs::read_to_string(&path) {
                    if let Some(ch) = parse_character_ini(&text) {
                        summary.characters_found.push(ch);
                    }
                }
            } else if (lower.ends_with(".txt") || lower.ends_with(".log"))
                && path
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.eq_ignore_ascii_case("logs"))
            {
                if let Some(dest_dir) = &logs_dest {
                    if fs::create_dir_all(dest_dir).is_ok()
                        && fs::copy(&path, dest_dir.join(fname)).is_ok()
                    {
                        summary.logs_copied += 1;
                    }
                }
            }
        }
    }

    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pounce_formats() {
        let existing = vec!["Alice".to_owned()];
        let text = "Alice\nBob|the|Fox\n[Pounce]\nUser1=Charlie\n";
        let added = parse_pounce_file_contents(text, &existing);
        assert_eq!(added, vec!["Bob the Fox".to_owned(), "Charlie".to_owned()]);
    }

    #[test]
    fn parse_online_ini_furre_rows() {
        let existing = vec!["Gemeinhardt".to_owned()];
        let text = "FurcadiaOn\t1.00\nShowOffline\t0\nWindowPos\t1\t2\t3\t4\t0\nFurre\tGemeinhardt\t\t1\t1\t0\nFurre\tHeavy Rain\t\t1\t1\t0\nFurre\tViolet Paws \t\t1\t0\t1\n";
        let added = parse_pounce_file_contents(text, &existing);
        assert_eq!(added, vec!["Heavy Rain".to_owned(), "Violet Paws".to_owned()]);
    }

    #[test]
    fn parse_char_ini_basic() {
        let text = "Name=TestHero\nColors=t2123456\nDesc=A white horse.\n";
        let parsed = parse_character_ini(text).unwrap();
        assert_eq!(parsed.name, "TestHero");
        assert_eq!(parsed.colors, "t2123456");
    }
}
