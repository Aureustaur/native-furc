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

/// Converts the current local wall-clock time into `(year, month, day, hour, minute, second)`,
/// falling back to UTC if the platform local-time query fails.
pub fn local_now_ymd_hms() -> (i32, u32, u32, u32, u32, u32) {
    // Guard to 64-bit Unix where `time_t` is reliably `i64` (32-bit Unix with
    // `_TIME_BITS=64` has a 64-bit `time_t`, so passing `isize` would be UB).
    // 32-bit Unix falls back cleanly to `system_time_to_ymd_hms` below.
    #[cfg(all(unix, target_pointer_width = "64"))]
    {
        #[repr(C)]
        struct Tm {
            tm_sec: i32,
            tm_min: i32,
            tm_hour: i32,
            tm_mday: i32,
            tm_mon: i32,
            tm_year: i32,
            tm_wday: i32,
            tm_yday: i32,
            tm_isdst: i32,
            _pad: [u64; 8],
        }
        extern "C" {
            fn localtime_r(timep: *const i64, result: *mut Tm) -> *mut Tm;
        }
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let mut tm = Tm {
            tm_sec: 0,
            tm_min: 0,
            tm_hour: 0,
            tm_mday: 1,
            tm_mon: 0,
            tm_year: 70,
            tm_wday: 0,
            tm_yday: 0,
            tm_isdst: 0,
            _pad: [0; 8],
        };
        // SAFETY: `secs` and `tm` are valid stack locals; `_pad` covers platform-specific
        // trailing fields (`tm_gmtoff`, `tm_zone`) on glibc/BSD/macOS.
        let ptr = unsafe { localtime_r(&secs, &mut tm) };
        if !ptr.is_null() {
            return (
                tm.tm_year + 1900,
                (tm.tm_mon + 1).clamp(1, 12) as u32,
                tm.tm_mday.clamp(1, 31) as u32,
                tm.tm_hour.clamp(0, 23) as u32,
                tm.tm_min.clamp(0, 59) as u32,
                tm.tm_sec.clamp(0, 59) as u32,
            );
        }
    }
    #[cfg(windows)]
    {
        #[repr(C)]
        struct WinSystemTime {
            year: u16,
            month: u16,
            day_of_week: u16,
            day: u16,
            hour: u16,
            minute: u16,
            second: u16,
            milliseconds: u16,
        }
        #[link(name = "kernel32")]
        extern "system" {
            fn GetLocalTime(lp_system_time: *mut WinSystemTime);
        }
        let mut st = WinSystemTime {
            year: 1970,
            month: 1,
            day_of_week: 0,
            day: 1,
            hour: 0,
            minute: 0,
            second: 0,
            milliseconds: 0,
        };
        // SAFETY: `st` is a valid mutable stack struct matching Win32 `SYSTEMTIME`.
        unsafe { GetLocalTime(&mut st) };
        if st.year >= 1970 && (1..=12).contains(&st.month) && (1..=31).contains(&st.day) {
            return (
                i32::from(st.year),
                u32::from(st.month),
                u32::from(st.day),
                u32::from(st.hour.min(23)),
                u32::from(st.minute.min(59)),
                u32::from(st.second.min(59)),
            );
        }
    }
    system_time_to_ymd_hms(SystemTime::now())
}

fn append_line_to(path: &Path, line: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{line}")
}

fn format_furcadia_timestamp(ymd_hms: (i32, u32, u32, u32, u32, u32)) -> String {
    let (y, m, d, hh, mm, ss) = ymd_hms;
    format!("{m:02}/{d:02}/{:02} {hh:02}:{mm:02}:{ss:02}", y.rem_euclid(100))
}

fn is_whisper_colon_emote(body: &str) -> bool {
    let Some(rest) = body.strip_prefix(':') else {
        return false;
    };
    let rest = rest.trim_start();
    if rest.is_empty() {
        return false;
    }
    if rest.starts_with("'s ") || rest.starts_with("'S ") {
        return true;
    }
    let first_word = rest.split_whitespace().next().unwrap_or("");
    let first_char = first_word.chars().next().unwrap_or(' ');
    if !first_char.is_ascii_alphabetic() {
        return false;
    }
    let stripped = first_word.trim_end_matches(['!', '?', '.', ',']);
    let lower = stripped.to_ascii_lowercase();
    if !lower.is_empty() {
        let first_lower = lower.chars().next().unwrap();
        let all_same = lower.chars().all(|c| c == first_lower);
        if all_same && matches!(first_lower, 'd' | 'p' | 'o' | 's' | 'x' | 'b' | 'c' | 'v' | 'z') {
            return false;
        }
        if matches!(lower.as_str(), "xd" | "xp" | "xo" | "dd" | "pp") {
            return false;
        }
    }
    stripped.len() >= 2
}

/// Formats a plain Furcadia whisper server line into official Furcadia whisper-log style:
/// - Speech: `<Speaker>: <message>`
/// - Emote: `* <Speaker> <emote>`
///
/// If `text` does not match a Furcadia server whisper wrapper, it is returned unchanged.
pub fn format_whisper_log_text(character: &str, peer: &str, text: &str) -> String {
    let s = text.trim().trim_start_matches('[').trim_end_matches(']').trim();
    let (speaker, body) = if let Some(rest) = s.strip_prefix("You whisper ") {
        let Some(to_idx) = rest.rfind(" to ") else {
            return text.to_owned();
        };
        let quoted = rest[..to_idx].trim();
        let inner = quoted
            .strip_prefix('"')
            .and_then(|q| q.strip_suffix('"'))
            .unwrap_or(quoted);
        (character, inner)
    } else if let Some(w_idx) = s.find(" whispers,") {
        let after = s[w_idx + " whispers,".len()..].trim();
        if let Some(to_idx) = after.rfind(" to you") {
            let quoted = after[..to_idx].trim();
            let inner = quoted
                .strip_prefix('"')
                .and_then(|q| q.strip_suffix('"'))
                .unwrap_or(quoted);
            (peer, inner)
        } else if let Some(quoted) = after.strip_prefix('"') {
            let inner = quoted
                .rfind('"')
                .map(|end| &quoted[..end])
                .unwrap_or(quoted)
                .trim();
            (peer, inner)
        } else {
            return text.to_owned();
        }
    } else {
        return text.to_owned();
    };

    let trimmed = body.trim();
    let speaker_disp = speaker.trim().replace('|', " ");
    let speaker_space = speaker_disp.to_ascii_lowercase();
    let speaker_pipe = speaker.trim().replace(' ', "|").to_ascii_lowercase();
    let lower = trimmed.to_ascii_lowercase();
    let is_name_prefixed_emote = lower.starts_with(&format!("{speaker_space} "))
        || lower.starts_with(&format!("{speaker_pipe} "))
        || lower.starts_with(&format!("{speaker_space}'"))
        || lower.starts_with(&format!("{speaker_pipe}'"));

    if is_whisper_colon_emote(trimmed) {
        let emote_body = trimmed.strip_prefix(':').unwrap_or(trimmed).trim_start();
        let sep = if emote_body.starts_with('\'') { "" } else { " " };
        format!("* {speaker_disp}{sep}{emote_body}")
    } else if is_name_prefixed_emote {
        let prefix_len = if lower.starts_with(&speaker_pipe) {
            speaker_pipe.len()
        } else {
            speaker_space.len()
        };
        format!("* {speaker_disp}{}", &trimmed[prefix_len..])
    } else {
        format!("{speaker_disp}: {trimmed}")
    }
}

/// Appends a timestamped line to `<logs_dir>/<character>/YYYY-MM-DD.txt`.
pub fn append_chat_log(base_dir: &Path, character: &str, text: &str) -> io::Result<()> {
    let now = local_now_ymd_hms();
    let (y, m, d, _, _, _) = now;
    let char_dir = sanitize_segment(character);
    let file = base_dir
        .join(char_dir)
        .join(format!("{y:04}-{m:02}-{d:02}.txt"));
    append_line_to(&file, &format!("{} {text}", format_furcadia_timestamp(now)))
}

/// Appends a timestamped line to `<logs_dir>/YYYY-MM-DD.txt` (Furcadia-style shared chat log).
pub fn append_shared_chat_log(base_dir: &Path, _character: &str, text: &str) -> io::Result<()> {
    let now = local_now_ymd_hms();
    let (y, m, d, _, _, _) = now;
    let file = base_dir.join(format!("{y:04}-{m:02}-{d:02}.txt"));
    append_line_to(&file, &format!("{} {text}", format_furcadia_timestamp(now)))
}

/// Appends a timestamped line to `<logs_dir>/<character>/whispers/<peer>.txt`.
pub fn append_whisper_log(
    base_dir: &Path,
    character: &str,
    peer: &str,
    text: &str,
) -> io::Result<()> {
    let now = local_now_ymd_hms();
    let char_dir = sanitize_segment(character);
    let peer_file = sanitize_segment(peer);
    let file = base_dir
        .join(char_dir)
        .join("whispers")
        .join(format!("{peer_file}.txt"));
    let body = format_whisper_log_text(character, peer, text);
    append_line_to(
        &file,
        &format!("{} {body}", format_furcadia_timestamp(now)),
    )
}

/// Appends a timestamped whisper line to the Furcadia-style shared folder `<logs_dir>/whispers/<peer_shortname>.txt`
/// (without duplicating into per-character whisper folders).
pub fn append_shared_whisper_log(
    base_dir: &Path,
    character: &str,
    peer: &str,
    text: &str,
) -> io::Result<()> {
    let now = local_now_ymd_hms();
    let stamp = format_furcadia_timestamp(now);
    let body = format_whisper_log_text(character, peer, text);
    let formatted = format!("{stamp} {body}");
    let peer_key = peer_shortname_key(peer);
    let shared_file = base_dir.join("whispers").join(format!("{peer_key}.txt"));
    append_line_to(&shared_file, &formatted)
}

type LogTimestamp = (i32, u32, u32, u32, u32, u32);

/// Parses a leading log timestamp into a chronologically sortable `(year, month, day, hour, minute, second)`.
///
/// Supports:
/// - `MM/DD/YY HH:MM:SS` and `MM/DD/YY HH:MM:SS*` (Furcadia & native client format)
/// - `MM/DD/YYYY HH:MM:SS`
/// - `[YYYY-MM-DD HH:MM:SS]` (legacy native client whisper format)
/// - `YYYY-MM-DD HH:MM:SS`
pub fn parse_log_timestamp(line: &str) -> Option<LogTimestamp> {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix('[') {
        let close = rest.find(']')?;
        let inner = &rest[..close];
        return parse_iso_ymd_hms(inner);
    }
    let mut parts = trimmed.split_whitespace();
    let date_part = parts.next()?;
    let time_part = parts.next()?.trim_end_matches('*');
    if let Some(ts) = parse_mdy_hms(date_part, time_part) {
        return Some(ts);
    }
    parse_iso_ymd_hms(&format!("{date_part} {time_part}"))
}

fn parse_hms(time_part: &str) -> Option<(u32, u32, u32)> {
    let mut it = time_part.split(':');
    let hh: u32 = it.next()?.parse().ok()?;
    let mm: u32 = it.next()?.parse().ok()?;
    let ss: u32 = it.next()?.parse().ok()?;
    if it.next().is_some() || hh > 23 || mm > 59 || ss > 59 {
        return None;
    }
    Some((hh, mm, ss))
}

fn parse_mdy_hms(date_part: &str, time_part: &str) -> Option<LogTimestamp> {
    let mut it = date_part.split('/');
    let m: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.parse().ok()?;
    let y_raw = it.next()?;
    if it.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y_num: i32 = y_raw.parse().ok()?;
    let y = if y_raw.len() <= 2 {
        if y_num >= 70 { 1900 + y_num } else { 2000 + y_num }
    } else {
        y_num
    };
    let (hh, mm, ss) = parse_hms(time_part)?;
    Some((y, m, d, hh, mm, ss))
}

fn parse_iso_ymd_hms(s: &str) -> Option<LogTimestamp> {
    let mut parts = s.split_whitespace();
    let date_part = parts.next()?;
    let time_part = parts.next()?.trim_end_matches('*');
    let mut it = date_part.split('-');
    let y: i32 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.parse().ok()?;
    if it.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let (hh, mm, ss) = parse_hms(time_part)?;
    Some((y, m, d, hh, mm, ss))
}

struct TimestampedLogLine {
    ts: Option<LogTimestamp>,
    seq: usize,
    file_idx: usize,
    line: String,
}

fn read_matching_whisper_files_in_dir(
    whispers_dir: &Path,
    peer_file: &str,
    peer_key: &str,
    next_file_idx: &mut usize,
    next_seq: &mut usize,
    out: &mut Vec<TimestampedLogLine>,
) {
    let Ok(entries) = fs::read_dir(whispers_dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension().is_some_and(|e| {
                    e.eq_ignore_ascii_case("txt") || e.eq_ignore_ascii_case("log")
                })
                && p.file_stem().and_then(|s| s.to_str()).is_some_and(|s| {
                    s.eq_ignore_ascii_case(peer_file) || peer_shortname_key(s) == peer_key
                })
        })
        .collect();
    paths.sort();
    for path in paths {
        if let Ok(bytes) = fs::read(path) {
            let file_idx = *next_file_idx;
            *next_file_idx += 1;
            let mut last_ts: Option<LogTimestamp> = None;
            for raw in String::from_utf8_lossy(&bytes).lines() {
                let trimmed = raw.trim_end();
                if trimmed.is_empty() {
                    continue;
                }
                if let Some(ts) = parse_log_timestamp(trimmed) {
                    last_ts = Some(ts);
                }
                let seq = *next_seq;
                *next_seq += 1;
                out.push(TimestampedLogLine {
                    ts: last_ts,
                    seq,
                    file_idx,
                    line: trimmed.to_owned(),
                });
            }
        }
    }
}

fn finalize_whisper_lines(mut entries: Vec<TimestampedLogLine>, max_lines: usize) -> Vec<String> {
    entries.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.seq.cmp(&b.seq)));
    // Deduplicate identical lines copied across different whisper files while keeping
    // chronological order intact.
    let mut seen_file_by_line: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut lines = Vec::with_capacity(entries.len());
    for entry in entries {
        if let Some(&first_file) = seen_file_by_line.get(&entry.line) {
            if first_file != entry.file_idx {
                continue;
            }
        } else {
            seen_file_by_line.insert(entry.line.clone(), entry.file_idx);
        }
        lines.push(entry.line);
    }
    if lines.len() > max_lines {
        lines.drain(..lines.len() - max_lines);
    }
    lines
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
    let mut entries = Vec::new();
    let mut next_file_idx = 0usize;
    let mut next_seq = 0usize;
    for char_dir in character_log_dirs(base_dir, character) {
        let whispers_dir = char_dir.join("whispers");
        read_matching_whisper_files_in_dir(
            &whispers_dir,
            &peer_file,
            &peer_key,
            &mut next_file_idx,
            &mut next_seq,
            &mut entries,
        );
    }
    finalize_whisper_lines(entries, max_lines)
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
    let mut entries = Vec::new();
    let mut next_file_idx = 0usize;
    let mut next_seq = 0usize;

    for whispers_dir in all_whisper_dirs(base_dir) {
        read_matching_whisper_files_in_dir(
            &whispers_dir,
            &peer_file,
            &peer_key,
            &mut next_file_idx,
            &mut next_seq,
            &mut entries,
        );
    }

    finalize_whisper_lines(entries, max_lines)
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

fn has_log_file(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("txt") || e.eq_ignore_ascii_case("log"))
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
        if has_log_file(&p) {
            return true;
        }
        if p.is_dir()
            && p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("device-"))
        {
            if let Ok(sub_entries) = fs::read_dir(&p) {
                if sub_entries.flatten().any(|sub| has_log_file(&sub.path())) {
                    return true;
                }
            }
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
        let mut chat_files = Vec::new();
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
                    if has_log_file(&path) {
                        chat_files.push(path);
                    }
                }
            }
        }
        chat_files.sort_by(|a, b| {
            b.file_name()
                .cmp(&a.file_name())
                .then_with(|| b.cmp(a))
        });
        chat_files.dedup();

        // Deduplicate whisper logs by peer shortname key, picking the most recently
        // modified file as the representative path.
        let mut whisper_by_peer: std::collections::HashMap<String, (SystemTime, PathBuf)> =
            std::collections::HashMap::new();
        for wdir in all_whisper_dirs(base_dir) {
            if let Ok(entries) = fs::read_dir(wdir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if !has_log_file(&path) {
                        continue;
                    }
                    let key = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .map(peer_shortname_key)
                        .unwrap_or_else(|| "unknown".to_owned());
                    let mtime = entry
                        .metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(UNIX_EPOCH);
                    match whisper_by_peer.get(&key) {
                        Some((old_mtime, _)) if *old_mtime > mtime => {}
                        _ => {
                            whisper_by_peer.insert(key, (mtime, path));
                        }
                    }
                }
            }
        }
        let mut whisper_entries: Vec<(String, PathBuf)> = whisper_by_peer
            .into_iter()
            .map(|(k, (_, p))| (k, p))
            .collect();
        whisper_entries.sort_by(|a, b| a.0.cmp(&b.0));

        let mut out = chat_files;
        out.extend(whisper_entries.into_iter().map(|(_, p)| p));
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

    #[test]
    fn shared_logs_write_single_bucket_without_character_dirs() {
        let base = std::env::temp_dir().join(format!(
            "furc_shared_logs_test_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&base);

        append_shared_chat_log(&base, "Amala|Devi", "Shared chat line").unwrap();
        append_shared_whisper_log(&base, "Amala|Devi", "Sunni McCalister", "Shared whisper").unwrap();

        assert_eq!(list_log_characters(&base), vec![SHARED_LOG_GROUP.to_owned()]);
        assert!(!base.join("Amala_Devi").exists());
        assert!(!base.join("Amala|Devi").exists());

        let files = list_log_files(&base, SHARED_LOG_GROUP);
        assert_eq!(files.len(), 2);

        let loaded = load_shared_whisper_log(&base, "sunnimccalister", 10);
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].ends_with("Shared whisper"));

        let synced = base.join("synced_only");
        append_line_to(&synced.join("device-test").join("2026-11-19.txt"), "Synced chat").unwrap();
        assert_eq!(list_log_characters(&synced), vec![SHARED_LOG_GROUP.to_owned()]);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn whisper_log_chronological_sort_keeps_latest_across_years_and_formats() {
        let base = std::env::temp_dir().join(format!(
            "furc_chrono_logs_test_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&base);

        let wfile = base.join("whispers").join("aeryx.txt");
        append_line_to(&wfile, "12/14/25 22:45:24 * Aeryx waves from 2025").unwrap();
        append_line_to(&wfile, "[2026-11-15 10:00:00] Aeryx: legacy bracket timestamp").unwrap();
        append_line_to(&wfile, "11/10/26 21:27:35 Aeryx: earlier in Nov 2026").unwrap();
        append_line_to(&wfile, "11/19/26 15:55:00 Amala Devi: latest message today").unwrap();

        let all = load_shared_whisper_log(&base, "Aeryx", 10);
        assert_eq!(all.len(), 4);
        assert!(all[0].contains("2025"));
        assert!(all[1].contains("earlier in Nov 2026"));
        assert!(all[2].contains("legacy bracket timestamp"));
        assert!(all[3].contains("latest message today"));

        // Trimming to the latest 2 lines must keep the 2 newest chronologically,
        // NOT lexicographically larger 12/14/25 or [2026-...] lines.
        let latest_two = load_shared_whisper_log(&base, "Aeryx", 2);
        assert_eq!(latest_two.len(), 2);
        assert!(latest_two[0].contains("legacy bracket timestamp"));
        assert!(latest_two[1].contains("latest message today"));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn furcadia_style_whisper_formatting() {
        assert_eq!(
            format_whisper_log_text(
                "Amala|Devi",
                "Aeryx",
                "[ You whisper \"How do you feel about tacos?\" to Aeryx. ]"
            ),
            "Amala Devi: How do you feel about tacos?"
        );
        assert_eq!(
            format_whisper_log_text(
                "Amala|Devi",
                "Aeryx",
                "[ Aeryx whispers, \"Great!\" to you. ]"
            ),
            "Aeryx: Great!"
        );
        assert_eq!(
            format_whisper_log_text(
                "Amala|Devi",
                "Aeryx",
                "[ You whisper \":licks her lips slowly.\" to Aeryx. ]"
            ),
            "* Amala Devi licks her lips slowly."
        );
        assert_eq!(
            format_whisper_log_text(
                "Amala|Devi",
                "Aeryx",
                "[ Aeryx whispers, \":D\" to you. ]"
            ),
            "Aeryx: :D"
        );
        assert_eq!(
            format_whisper_log_text(
                "Amala|Devi",
                "Aeryx",
                "[ Aeryx whispers, \"Aeryx growls playfully.\" to you. ]"
            ),
            "* Aeryx growls playfully."
        );
    }

    #[test]
    fn shared_list_log_files_deduplicates_whispers_by_peer() {
        let base = std::env::temp_dir().join(format!(
            "furc_dedup_list_test_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&base);

        append_line_to(&base.join("Aureus").join("whispers").join("Aeryx.txt"), "12/14/25 10:00:00 Aeryx: old").unwrap();
        append_line_to(&base.join("whispers").join("aeryx.txt"), "11/19/26 15:00:00 Aeryx: new").unwrap();

        let files = list_log_files(&base, SHARED_LOG_GROUP);
        assert_eq!(files.len(), 1);

        let _ = fs::remove_dir_all(&base);
    }
}
