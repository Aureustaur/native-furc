//! Per-character and Furcadia-style shared chat and whisper log persistence and retention management.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Special virtual character label used in the Log Viewer to browse shared Furcadia-style logs.
pub const SHARED_LOG_GROUP: &str = "Shared (All Characters)";

/// Returns the shared logs root, or the normal per-user logs folder when sync is disabled.
pub fn default_logs_root() -> Option<PathBuf> {
    if let Some(config_path) = crate::config::ClientConfig::default_path() {
        if let Ok(config) = crate::config::ClientConfig::load(&config_path) {
            if !config.sync_folder.trim().is_empty() {
                return Some(PathBuf::from(config.sync_folder).join("logs"));
            }
        }
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
        .or_else(|| {
            std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join("AppData/Local"))
        })?;
    Some(base.join("furcadia-native").join("logs"))
}

/// Returns this device's log folder. Synced installs write beneath a unique device directory.
pub fn default_logs_dir() -> Option<PathBuf> {
    let root = default_logs_root()?;
    if let Some(config_path) = crate::config::ClientConfig::default_path() {
        if let Ok(config) = crate::config::ClientConfig::load(&config_path) {
            if !config.sync_folder.trim().is_empty() {
                let device = if config.sync_device_id.trim().is_empty() {
                    "device-unknown"
                } else {
                    config.sync_device_id.trim()
                };
                return Some(root.join(device));
            }
        }
    }
    Some(root)
}

/// Copies existing logs into a new destination without replacing cloud-synced files.
pub fn copy_logs_if_missing(source: &Path, destination: &Path) -> io::Result<usize> {
    if source == destination || destination.starts_with(source) || source.starts_with(destination) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "source and destination log directories overlap",
        ));
    }
    if !source.is_dir() {
        return Ok(0);
    }
    let mut copied = 0;
    let mut stack = vec![(source.to_path_buf(), destination.to_path_buf())];
    while let Some((src, dst)) = stack.pop() {
        fs::create_dir_all(&dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let src_path = entry.path();
            let dst_path = dst.join(entry.file_name());
            if src_path.is_dir() {
                stack.push((src_path, dst_path));
            } else if !dst_path.exists() {
                fs::copy(src_path, dst_path)?;
                copied += 1;
            }
        }
    }
    Ok(copied)
}

pub(crate) fn sanitize_segment(raw: &str) -> String {
    let cleaned: String = raw
        .trim()
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | ' ' => c,
            _ => '_',
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "Unknown".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Normalizes a character or peer name to a Furcadia shortname key (lowercase alphanumeric only)
/// so `"Sunni McCalister"`, `"Sunni|McCalister"`, and `"sunnimccalister"` all match the same log file.
pub fn peer_shortname_key(raw: &str) -> String {
    let key: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if key.is_empty() {
        "unknown".to_owned()
    } else {
        key
    }
}

/// Converts a `SystemTime` into `(year, month, day, hour, minute, second)` in UTC.
pub fn system_time_to_ymd_hms(t: SystemTime) -> (i32, u32, u32, u32, u32, u32) {
    let secs = t
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400) as u32;
    let hour = rem / 3600;
    let min = (rem % 3600) / 60;
    let sec = rem % 60;

    // Howard Hinnant's civil-from-days algorithm.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = (y + if m <= 2 { 1 } else { 0 }) as i32;
    (year, m as u32, d as u32, hour, min, sec)
}

fn append_line_to(path: &Path, line: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{line}")
}

fn default_client_timestamp(t: SystemTime) -> String {
    let (y, m, d, hh, mm, ss) = system_time_to_ymd_hms(t);
    format!("{m:02}/{d:02}/{:02} {hh:02}:{mm:02}:{ss:02}*", y.rem_euclid(100))
}

/// Appends a timestamped line to `<logs_dir>/<character>/YYYY-MM-DD.txt`.
pub fn append_chat_log(base_dir: &Path, character: &str, text: &str) -> io::Result<()> {
    let now = SystemTime::now();
    let (y, m, d, _, _, _) = system_time_to_ymd_hms(now);
    let char_dir = sanitize_segment(character);
    let file = base_dir
        .join(char_dir)
        .join(format!("{y:04}-{m:02}-{d:02}.txt"));
    append_line_to(&file, &format!("{} {text}", default_client_timestamp(now)))
}

/// Appends a timestamped line to `<logs_dir>/YYYY-MM-DD.txt` (Furcadia-style shared chat log).
pub fn append_shared_chat_log(base_dir: &Path, character: &str, text: &str) -> io::Result<()> {
    let now = SystemTime::now();
    let (y, m, d, _, _, _) = system_time_to_ymd_hms(now);
    let file = base_dir.join(format!("{y:04}-{m:02}-{d:02}.txt"));
    let _ = append_chat_log(base_dir, character, text);
    append_line_to(&file, &format!("{} {text}", default_client_timestamp(now)))
}

/// Appends a timestamped line to `<logs_dir>/<character>/whispers/<peer>.txt`.
pub fn append_whisper_log(
    base_dir: &Path,
    character: &str,
    peer: &str,
    text: &str,
) -> io::Result<()> {
    let now = SystemTime::now();
    let char_dir = sanitize_segment(character);
    let peer_file = sanitize_segment(peer);
    let file = base_dir
        .join(char_dir)
        .join("whispers")
        .join(format!("{peer_file}.txt"));
    append_line_to(
        &file,
        &format!("{} {text}", default_client_timestamp(now)),
    )
}

/// Appends a timestamped whisper line to the Furcadia-style shared folder `<logs_dir>/whispers/<peer_shortname>.txt`
/// (and also updates the per-character whisper file so switching modes never loses lines).
pub fn append_shared_whisper_log(
    base_dir: &Path,
    character: &str,
    peer: &str,
    text: &str,
) -> io::Result<()> {
    let now = SystemTime::now();
    let stamp = default_client_timestamp(now);
    let formatted = format!("{stamp} {text}");
    let _ = append_whisper_log(base_dir, character, peer, text);
    let peer_key = peer_shortname_key(peer);
    let shared_file = base_dir.join("whispers").join(format!("{peer_key}.txt"));
    append_line_to(&shared_file, &formatted)
}

fn read_matching_whisper_files_in_dir(
    whispers_dir: &Path,
    peer_file: &str,
    peer_key: &str,
    lines: &mut Vec<String>,
) {
    let Ok(entries) = fs::read_dir(whispers_dir) else {
        return;
    };
    for path in entries.flatten().map(|e| e.path()).filter(|p| {
        p.is_file()
            && p.extension().is_some_and(|e| {
                e.eq_ignore_ascii_case("txt") || e.eq_ignore_ascii_case("log")
            })
            && p.file_stem().and_then(|s| s.to_str()).is_some_and(|s| {
                s.eq_ignore_ascii_case(peer_file) || peer_shortname_key(s) == peer_key
            })
    }) {
        if let Ok(bytes) = fs::read(path) {
            lines.extend(
                String::from_utf8_lossy(&bytes)
                    .lines()
                    .map(str::trim_end)
                    .filter(|l| !l.is_empty())
                    .map(ToOwned::to_owned),
            );
        }
    }
}

/// Loads up to `max_lines` most recent lines from `<logs_dir>/<character>/whispers/<peer>.txt`.
pub fn load_whisper_log(
    base_dir: &Path,
    character: &str,
    peer: &str,
    max_lines: usize,
) -> Vec<String> {
    if max_lines == 0 {
        return Vec::new();
    }
    let peer_file = sanitize_segment(peer);
    let peer_key = peer_shortname_key(peer);
    let mut lines = Vec::new();
    for char_dir in character_log_dirs(base_dir, character) {
        let whispers_dir = char_dir.join("whispers");
        read_matching_whisper_files_in_dir(&whispers_dir, &peer_file, &peer_key, &mut lines);
    }
    lines.sort();
    lines.dedup();
    if lines.len() > max_lines {
        lines.drain(..lines.len() - max_lines);
    }
    lines
}

/// Loads up to `max_lines` most recent whisper lines with `peer` across the entire log directory
/// (including shared `<logs_root>/whispers/`, device `<device>/whispers/`, and every character's `whispers/` folder),
/// matching official Furcadia's global whisper history behavior.
pub fn load_shared_whisper_log(
    base_dir: &Path,
    peer: &str,
    max_lines: usize,
) -> Vec<String> {
    if max_lines == 0 {
        return Vec::new();
    }
    let peer_file = sanitize_segment(peer);
    let peer_key = peer_shortname_key(peer);
    let mut lines = Vec::new();

    for whispers_dir in all_whisper_dirs(base_dir) {
        read_matching_whisper_files_in_dir(&whispers_dir, &peer_file, &peer_key, &mut lines);
    }

    lines.sort();
    lines.dedup();
    if lines.len() > max_lines {
        lines.drain(..lines.len() - max_lines);
    }
    lines
}

fn all_whisper_dirs(base_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut push_if_dir = |p: PathBuf| {
        if p.is_dir() && !dirs.contains(&p) {
            dirs.push(p);
        }
    };

    // 1. Root-level shared whispers (`<logs_root>/whispers`)
    push_if_dir(base_dir.join("whispers"));

    let Ok(entries) = fs::read_dir(base_dir) else {
        return dirs;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if path
            .file_name()
            .is_some_and(|n| n.eq_ignore_ascii_case("whispers"))
        {
            push_if_dir(path);
            continue;
        }
        // 2. Direct character whispers (`<logs_root>/<char>/whispers`) or device shared whispers (`<logs_root>/<device>/whispers`)
        push_if_dir(path.join("whispers"));

        // 3. Synced device character whispers (`<logs_root>/<device>/<char>/whispers`)
        if let Ok(sub_entries) = fs::read_dir(&path) {
            for sub in sub_entries.flatten() {
                let sub_path = sub.path();
                if sub_path.is_dir() {
                    push_if_dir(sub_path.join("whispers"));
                }
            }
        }
    }
    dirs
}

/// Removes log files older than `retention_days` (no-op when `retention_days == 0`).
pub fn prune_old_logs(base_dir: &Path, retention_days: u32) -> usize {
    if retention_days == 0 || !base_dir.is_dir() {
        return 0;
    }
    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(u64::from(retention_days) * 86_400))
        .unwrap_or(UNIX_EPOCH);
    let mut removed = 0;
    let mut stack = vec![base_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("txt") || e.eq_ignore_ascii_case("log"))
            {
                if let Ok(meta) = entry.metadata() {
                    if let Ok(modified) = meta.modified() {
                        if modified < cutoff && fs::remove_file(&path).is_ok() {
                            removed += 1;
                        }
                    }
                }
            }
        }
    }
    removed
}

fn has_shared_root_logs(base_dir: &Path) -> bool {
    if !all_whisper_dirs(base_dir).is_empty() {
        return true;
    }
    let Ok(entries) = fs::read_dir(base_dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_file()
            && p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("txt") || e.eq_ignore_ascii_case("log"))
        {
            return true;
        }
    }
    false
}

/// Lists all character log folders under `base_dir` (sorted), plus `SHARED_LOG_GROUP` when shared/whisper logs exist.
pub fn list_log_characters(base_dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(base_dir) else {
        return Vec::new();
    };
    let entries: Vec<_> = entries.flatten().collect();
    let mut out = Vec::new();
    out.extend(
        entries
            .iter()
            .filter(|e| e.path().is_dir() && looks_like_character_log_dir(&e.path()))
            .filter_map(|e| e.file_name().into_string().ok()),
    );
    // Synced logs live under `<root>/<device>/<character>`. Discover that
    // layout from the files themselves so a missing settings snapshot cannot
    // hide whisper history imported before sync was initialized.
    for device_dir in entries.iter().filter(|e| e.path().is_dir()) {
        if let Ok(chars) = fs::read_dir(device_dir.path()) {
            out.extend(
                chars
                    .flatten()
                    .filter(|e| e.path().is_dir() && looks_like_character_log_dir(&e.path()))
                    .filter_map(|e| e.file_name().into_string().ok()),
            );
        }
    }
    out.sort();
    out.dedup();
    if has_shared_root_logs(base_dir) {
        out.insert(0, SHARED_LOG_GROUP.to_owned());
    }
    out
}

fn looks_like_character_log_dir(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if name.eq_ignore_ascii_case("whispers") || name.starts_with("device-") {
        return false;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let child = entry.path();
        (child.is_dir() && child.file_name().is_some_and(|n| n.eq_ignore_ascii_case("whispers")))
            || (child.is_file()
                && child
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("txt") || e.eq_ignore_ascii_case("log")))
    })
}

/// Lists all `.txt`/`.log` files for a character (including `whispers/`), or all shared/global logs when `character == SHARED_LOG_GROUP`.
pub fn list_log_files(base_dir: &Path, character: &str) -> Vec<PathBuf> {
    if character == SHARED_LOG_GROUP {
        let mut out = Vec::new();
        for wdir in all_whisper_dirs(base_dir) {
            if let Ok(entries) = fs::read_dir(wdir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file()
                        && path
                            .extension()
                            .and_then(|e| e.to_str())
                            .is_some_and(|e| {
                                e.eq_ignore_ascii_case("txt") || e.eq_ignore_ascii_case("log")
                            })
                    {
                        out.push(path);
                    }
                }
            }
        }
        let mut root_dirs = vec![base_dir.to_path_buf()];
        if let Ok(entries) = fs::read_dir(base_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("device-"))
                {
                    root_dirs.push(p);
                }
            }
        }
        for rdir in root_dirs {
            if let Ok(entries) = fs::read_dir(rdir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file()
                        && path
                            .extension()
                            .and_then(|e| e.to_str())
                            .is_some_and(|e| {
                                e.eq_ignore_ascii_case("txt") || e.eq_ignore_ascii_case("log")
                            })
                    {
                        out.push(path);
                    }
                }
            }
        }
        out.sort();
        out.dedup();
        out.reverse();
        return out;
    }

    let mut out = Vec::new();
    let mut stack = character_log_dirs(base_dir, character);
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("txt") || e.eq_ignore_ascii_case("log"))
            {
                out.push(path);
            }
        }
    }
    out.sort();
    out.reverse();
    out
}

fn character_log_dirs(base_dir: &Path, character: &str) -> Vec<PathBuf> {
    let name = sanitize_segment(character);
    let target_key = peer_shortname_key(character);
    let Ok(entries) = fs::read_dir(base_dir) else {
        return Vec::new();
    };
    let entries: Vec<_> = entries.flatten().collect();
    let mut dirs = Vec::new();
    let direct = base_dir.join(&name);
    if direct.is_dir() {
        dirs.push(direct);
    }
    for entry in entries.iter().filter(|e| e.path().is_dir()) {
        let p = entry.path();
        if p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| peer_shortname_key(n) == target_key)
            && !dirs.contains(&p)
        {
            dirs.push(p.clone());
        }
        let sub_direct = p.join(&name);
        if sub_direct.is_dir() && !dirs.contains(&sub_direct) {
            dirs.push(sub_direct);
        }
        if let Ok(sub_entries) = fs::read_dir(&p) {
            for sub in sub_entries.flatten() {
                let sp = sub.path();
                if sp.is_dir()
                    && sp
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| peer_shortname_key(n) == target_key)
                    && !dirs.contains(&sp)
                {
                    dirs.push(sp);
                }
            }
        }
    }
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_date_epoch() {
        assert_eq!(system_time_to_ymd_hms(UNIX_EPOCH), (1970, 1, 1, 0, 0, 0));
    }

    #[test]
    fn write_and_list_logs() {
        let tmp = std::env::temp_dir().join(format!(
            "furc_logs_test_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        append_chat_log(&tmp, "TestHero", "Hello world!").unwrap();
        append_whisper_log(&tmp, "TestHero", "TestFriend", "Hi there").unwrap();
        append_whisper_log(&tmp, "TestHero", "TestFriend", "Second line").unwrap();
        assert!(list_log_characters(&tmp).contains(&"TestHero".to_owned()));
        let files = list_log_files(&tmp, "TestHero");
        assert_eq!(files.len(), 2);
        let loaded = load_whisper_log(&tmp, "TestHero", "testfriend", 1);
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].ends_with("Second line"));

        // Shared whisper log test across characters (e.g. Amala Devi -> Sunni McCalister read by Fulkor)
        append_shared_whisper_log(&tmp, "Amala|Devi", "Sunni McCalister", "Shared whisper line").unwrap();
        let shared_loaded = load_shared_whisper_log(&tmp, "sunnimccalister", 10);
        assert!(shared_loaded.iter().any(|l| l.ends_with("Shared whisper line")));

        let _ = fs::remove_dir_all(&tmp);
    }
}
