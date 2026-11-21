//! Local user configuration persistence (`~/.config/furcadia-native/config.ini`)
//! and optional cloud-folder settings sync (`settings-<device-id>.ini`).
//!
//! Credentials (`email`, `password`, `remember_*`, `auto_login`) are stored only in
//! the local `config.ini` (created with `0o600` permissions on Unix) and are always
//! stripped before writing shared sync snapshots.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn new_sync_device_id() -> String {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("device-{stamp:x}-{:x}", std::process::id())
}

/// Persisted login, logging, notification, font, tray, and Pounce preferences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientConfig {
    pub email: String,
    pub character: String,
    pub password: String,
    pub remember_email: bool,
    pub remember_password: bool,
    pub auto_login: bool,
    pub watchlist: Vec<String>,
    pub logging_enabled: bool,
    pub log_whispers: bool,
    /// When `true` (default), logs use Furcadia-style shared logs (one shared whispers/chat pool across all characters).
    /// When `false`, logs are isolated strictly per individual character.
    pub shared_logs_mode: bool,
    pub log_retention_days: u32,
    pub notify_enabled: bool,
    pub notify_whisper: bool,
    pub notify_pounce: bool,
    pub notify_mention: bool,
    pub notify_only_unfocused: bool,
    pub flash_taskbar_on_message: bool,
    pub flash_taskbar_on_main_chat: bool,
    pub blink_tray_on_alert: bool,
    pub mention_nicknames: String,
    pub sound_enabled: bool,
    pub music_enabled: bool,
    pub minimize_to_tray: bool,
    pub close_to_tray: bool,
    pub font_family: String,
    pub font_size: u16,
    pub chat_font_size: u16,
    pub chat_text_color: u32,
    pub input_text_color: u32,
    pub my_speech_color: u32,
    pub bracket_text_color: u32,
    pub emote_text_color: u32,
    pub whisper_text_color: u32,
    pub emit_text_color: u32,
    pub shout_text_color: u32,
    pub roll_text_color: u32,
    pub success_text_color: u32,
    pub warning_text_color: u32,
    pub error_text_color: u32,
    pub tab_active_bg: u32,
    pub tab_active_text: u32,
    pub tab_unread_bg: u32,
    pub tab_unread_text: u32,
    pub tab_inactive_bg: u32,
    pub tab_inactive_text: u32,
    pub custom_ui_bg: bool,
    pub chat_bg_color: u32,
    pub panel_bg_color: u32,
    pub accent_color: u32,
    pub autocorrect: bool,
    pub anim_mode: String,
    pub theme: String,
    pub show_avatar_names: bool,
    pub pounce_visible: bool,
    pub pounce_detached: bool,
    pub pounce_show_offline: bool,
    pub show_debug_packets: bool,
    pub whisper_open_tab_on_incoming: bool,
    pub whisper_emote_in_whisper: bool,
    pub dream_bookmarks: Vec<String>,
    pub limit_view_range: bool,
    pub allow_dream_skins: bool,
    pub portrait_size: u16,
    pub check_updates_on_startup: bool,
    pub first_setup_done: bool,
    pub dice_roll: String,
    /// Local path to a folder managed by a cloud sync client; never copied into shared settings.
    pub sync_folder: String,
    /// Per-installation identifier used to keep log writes separate across devices.
    pub sync_device_id: String,
    /// Timestamp of this device's last synced preferences snapshot.
    pub sync_updated_at: u64,
    /// Last saved main window inner width in logical points (device-local).
    pub window_width: u16,
    /// Last saved main window inner height in logical points (device-local).
    pub window_height: u16,
    /// Whether the main window was maximized (device-local).
    pub window_maximized: bool,
    /// Saved custom portrait `pp<id>` per normalized character shortname.
    pub character_portraits: std::collections::BTreeMap<String, u32>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            email: String::new(),
            character: String::new(),
            password: String::new(),
            remember_email: true,
            remember_password: true,
            auto_login: false,
            watchlist: Vec::new(),
            logging_enabled: true,
            log_whispers: true,
            shared_logs_mode: true,
            log_retention_days: 0,
            notify_enabled: true,
            notify_whisper: true,
            notify_pounce: true,
            notify_mention: true,
            notify_only_unfocused: true,
            flash_taskbar_on_message: true,
            flash_taskbar_on_main_chat: false,
            blink_tray_on_alert: true,
            mention_nicknames: String::new(),
            sound_enabled: true,
            music_enabled: false,
            minimize_to_tray: false,
            close_to_tray: false,
            font_family: "Proportional".to_owned(),
            font_size: 13,
            chat_font_size: 13,
            chat_text_color: 0xf2f5fa,
            input_text_color: 0xffffff,
            my_speech_color: 0xfff8d7,
            bracket_text_color: 0x005c80,
            emote_text_color: 0xe8a0e4,
            whisper_text_color: 0xcd82eb,
            emit_text_color: 0x64d2e6,
            shout_text_color: 0x7daffa,
            roll_text_color: 0xc3e191,
            success_text_color: 0x64dc78,
            warning_text_color: 0xf0c35a,
            error_text_color: 0xf06464,
            tab_active_bg: 0x82c8ff,
            tab_active_text: 0x181006,
            tab_unread_bg: 0xfadc32,
            tab_unread_text: 0x140e06,
            tab_inactive_bg: 0x2a1e44,
            tab_inactive_text: 0xe6e0f0,
            custom_ui_bg: false,
            chat_bg_color: 0x170e2e,
            panel_bg_color: 0x221644,
            accent_color: 0xd4a94a,
            autocorrect: false,
            anim_mode: "Full".to_owned(),
            theme: "ClassicPurple".to_owned(),
            show_avatar_names: true,
            pounce_visible: true,
            pounce_detached: false,
            pounce_show_offline: true,
            show_debug_packets: false,
            whisper_open_tab_on_incoming: true,
            whisper_emote_in_whisper: true,
            dream_bookmarks: Vec::new(),
            limit_view_range: true,
            allow_dream_skins: true,
            portrait_size: 64,
            check_updates_on_startup: true,
            first_setup_done: false,
            dice_roll: "1d6".to_owned(),
            sync_folder: String::new(),
            sync_device_id: String::new(),
            sync_updated_at: 0,
            window_width: 1200,
            window_height: 800,
            window_maximized: false,
            character_portraits: std::collections::BTreeMap::new(),
        }
    }
}

impl ClientConfig {
    /// Returns `~/.config/furcadia-native/config.ini` (or Windows `%APPDATA%\furcadia-native\config.ini`).
    pub fn default_path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
            .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
            .or_else(|| {
                std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join("AppData/Roaming"))
            })?;
        Some(base.join("furcadia-native").join("config.ini"))
    }

    pub fn load_default() -> Self {
        let path = Self::default_path();
        let mut cfg = path
            .as_ref()
            .and_then(|p| Self::load(p).ok())
            .unwrap_or_default();
        if cfg.sync_device_id.is_empty() {
            cfg.sync_device_id = new_sync_device_id();
            if let Some(path) = path.as_ref() {
                let _ = cfg.save(path);
            }
        }
        if let Some(folder) = cfg.sync_folder_path() {
            let mut newest: Option<Self> = None;
            if let Ok(entries) = fs::read_dir(folder) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                        n.starts_with("settings-") && n.ends_with(".ini")
                    }) {
                        if let Ok(shared) = Self::load(&path) {
                            if shared.sync_updated_at > 0
                                && newest
                                    .as_ref()
                                    .is_none_or(|old| shared.sync_updated_at >= old.sync_updated_at)
                            {
                                newest = Some(shared);
                            }
                        }
                    }
                }
            }
            if let Some(shared) = newest {
                cfg.apply_shared_settings(&shared);
            }
        }
        cfg
    }

    pub fn save_default(&self) -> io::Result<()> {
        let Some(path) = Self::default_path() else {
            return Ok(());
        };
        self.save(&path)?;
        self.save_shared_settings()
    }

    pub fn save_local_default(&self) -> io::Result<()> {
        let Some(path) = Self::default_path() else {
            return Ok(());
        };
        self.save(&path)
    }

    pub fn shared_settings_path(&self) -> Option<PathBuf> {
        let folder = self.sync_folder_path()?;
        let id: String = self
            .sync_device_id
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        (!id.is_empty()).then(|| folder.join(format!("settings-{id}.ini")))
    }

    pub fn sync_folder_path(&self) -> Option<PathBuf> {
        let folder = self.sync_folder.trim();
        (!folder.is_empty()).then(|| PathBuf::from(folder))
    }

    pub fn has_shared_settings(&self) -> bool {
        self.sync_folder_path().is_some_and(|folder| {
            fs::read_dir(folder)
                .ok()
                .into_iter()
                .flatten()
                .flatten()
                .any(|entry| {
                    let path = entry.path();
                    let is_snapshot = entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("settings-")
                        && path
                            .extension()
                            .and_then(|ext| ext.to_str())
                            .is_some_and(|ext| ext.eq_ignore_ascii_case("ini"));
                    is_snapshot
                        && Self::load(&path)
                            .is_ok_and(|snapshot| snapshot.sync_updated_at > 0)
                })
        })
    }

    fn save_shared_settings(&self) -> io::Result<()> {
        let Some(path) = self.shared_settings_path() else {
            return Ok(());
        };
        let mut shared = self.clone();
        shared.email.clear();
        shared.password.clear();
        shared.remember_email = false;
        shared.remember_password = false;
        shared.auto_login = false;
        shared.sync_folder.clear();
        shared.sync_device_id.clear();
        shared.sync_updated_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        shared.save(&path)
    }

    fn apply_shared_settings(&mut self, shared: &Self) {
        self.character = shared.character.clone();
        self.watchlist = shared.watchlist.clone();
        self.logging_enabled = shared.logging_enabled;
        self.log_whispers = shared.log_whispers;
        self.shared_logs_mode = shared.shared_logs_mode;
        self.log_retention_days = shared.log_retention_days;
        self.notify_enabled = shared.notify_enabled;
        self.notify_whisper = shared.notify_whisper;
        self.notify_pounce = shared.notify_pounce;
        self.notify_mention = shared.notify_mention;
        self.notify_only_unfocused = shared.notify_only_unfocused;
        self.flash_taskbar_on_message = shared.flash_taskbar_on_message;
        self.flash_taskbar_on_main_chat = shared.flash_taskbar_on_main_chat;
        self.blink_tray_on_alert = shared.blink_tray_on_alert;
        self.mention_nicknames = shared.mention_nicknames.clone();
        self.sound_enabled = shared.sound_enabled;
        self.music_enabled = shared.music_enabled;
        self.minimize_to_tray = shared.minimize_to_tray;
        self.close_to_tray = shared.close_to_tray;
        self.font_family = shared.font_family.clone();
        self.font_size = shared.font_size;
        self.chat_font_size = shared.chat_font_size;
        self.chat_text_color = shared.chat_text_color;
        self.input_text_color = shared.input_text_color;
        self.my_speech_color = shared.my_speech_color;
        self.bracket_text_color = shared.bracket_text_color;
        self.emote_text_color = shared.emote_text_color;
        self.whisper_text_color = shared.whisper_text_color;
        self.emit_text_color = shared.emit_text_color;
        self.shout_text_color = shared.shout_text_color;
        self.roll_text_color = shared.roll_text_color;
        self.success_text_color = shared.success_text_color;
        self.warning_text_color = shared.warning_text_color;
        self.error_text_color = shared.error_text_color;
        self.tab_active_bg = shared.tab_active_bg;
        self.tab_active_text = shared.tab_active_text;
        self.tab_unread_bg = shared.tab_unread_bg;
        self.tab_unread_text = shared.tab_unread_text;
        self.tab_inactive_bg = shared.tab_inactive_bg;
        self.tab_inactive_text = shared.tab_inactive_text;
        self.custom_ui_bg = shared.custom_ui_bg;
        self.chat_bg_color = shared.chat_bg_color;
        self.panel_bg_color = shared.panel_bg_color;
        self.accent_color = shared.accent_color;
        self.autocorrect = shared.autocorrect;
        self.anim_mode = shared.anim_mode.clone();
        self.theme = shared.theme.clone();
        self.show_avatar_names = shared.show_avatar_names;
        self.pounce_visible = shared.pounce_visible;
        self.pounce_detached = shared.pounce_detached;
        self.pounce_show_offline = shared.pounce_show_offline;
        self.show_debug_packets = shared.show_debug_packets;
        self.whisper_open_tab_on_incoming = shared.whisper_open_tab_on_incoming;
        self.whisper_emote_in_whisper = shared.whisper_emote_in_whisper;
        self.dream_bookmarks = shared.dream_bookmarks.clone();
        self.limit_view_range = shared.limit_view_range;
        self.allow_dream_skins = shared.allow_dream_skins;
        self.portrait_size = shared.portrait_size;
        self.check_updates_on_startup = shared.check_updates_on_startup;
        self.first_setup_done = shared.first_setup_done;
        self.dice_roll = shared.dice_roll.clone();
        for (k, &v) in &shared.character_portraits {
            if !k.is_empty() && v > 0 {
                self.character_portraits.insert(k.clone(), v);
            }
        }
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        let text = fs::read_to_string(path)?;
        Ok(Self::parse(&text))
    }

    /// Writes configuration to `path`. On Unix, the file is created with `0o600`
    /// permissions atomically to avoid a world-readable window for saved credentials.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let data = self.serialize();
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(path)?;
            file.write_all(data.as_bytes())?;
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
        }
        #[cfg(not(unix))]
        {
            fs::write(path, data)?;
        }
        Ok(())
    }

    fn parse_hex_rgb(raw: &str, fallback: u32) -> u32 {
        u32::from_str_radix(raw.trim().trim_start_matches('#'), 16)
            .map(|v| v & 0xffffff)
            .unwrap_or(fallback)
    }

    pub fn parse(text: &str) -> Self {
        let mut cfg = Self::default();
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                match k.trim() {
                    "email" => cfg.email = v.trim().to_owned(),
                    "character" => cfg.character = v.trim().to_owned(),
                    // Preserve leading/trailing spaces in passwords; only strip trailing CR.
                    "password" => cfg.password = v.trim_end_matches('\r').to_owned(),
                    "remember_email" => cfg.remember_email = v.trim() != "false",
                    "remember_password" => cfg.remember_password = v.trim() != "false",
                    "remember_login" => {
                        let flag = v.trim() != "false";
                        cfg.remember_email = flag;
                        cfg.remember_password = flag;
                    }
                    "auto_login" => cfg.auto_login = v.trim() == "true",
                    "watch" => {
                        cfg.watchlist = v
                            .split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(ToOwned::to_owned)
                            .collect();
                    }
                    "logging_enabled" => cfg.logging_enabled = v.trim() != "false",
                    "log_whispers" => cfg.log_whispers = v.trim() != "false",
                    "shared_logs_mode" => cfg.shared_logs_mode = v.trim() != "false",
                    "log_retention_days" => {
                        if let Ok(days) = v.trim().parse::<u32>() {
                            cfg.log_retention_days = days;
                        }
                    }
                    "notify_enabled" => cfg.notify_enabled = v.trim() != "false",
                    "notify_whisper" => cfg.notify_whisper = v.trim() != "false",
                    "notify_pounce" => cfg.notify_pounce = v.trim() != "false",
                    "notify_mention" => cfg.notify_mention = v.trim() != "false",
                    "notify_only_unfocused" => cfg.notify_only_unfocused = v.trim() != "false",
                    "flash_taskbar_on_message" => cfg.flash_taskbar_on_message = v.trim() != "false",
                    "flash_taskbar_on_main_chat" => cfg.flash_taskbar_on_main_chat = v.trim() == "true",
                    "blink_tray_on_alert" => cfg.blink_tray_on_alert = v.trim() != "false",
                    "mention_nicknames" => cfg.mention_nicknames = v.trim().to_owned(),
                    "sound_enabled" => cfg.sound_enabled = v.trim() != "false",
                    "music_enabled" => cfg.music_enabled = v.trim() == "true",
                    "minimize_to_tray" => cfg.minimize_to_tray = v.trim() == "true",
                    "close_to_tray" => cfg.close_to_tray = v.trim() == "true",
                    "font_family" => {
                        let f = v.trim();
                        if !f.is_empty() {
                            cfg.font_family = f.to_owned();
                        }
                    }
                    "font_size" => {
                        if let Ok(sz) = v.trim().parse::<u16>() {
                            cfg.font_size = sz.clamp(9, 28);
                        }
                    }
                    "chat_font_size" => {
                        if let Ok(sz) = v.trim().parse::<u16>() {
                            cfg.chat_font_size = sz.clamp(9, 28);
                        }
                    }
                    "chat_text_color" => {
                        cfg.chat_text_color = Self::parse_hex_rgb(v, cfg.chat_text_color);
                    }
                    "input_text_color" => {
                        cfg.input_text_color = Self::parse_hex_rgb(v, cfg.input_text_color);
                    }
                    "my_speech_color" => {
                        cfg.my_speech_color = Self::parse_hex_rgb(v, cfg.my_speech_color);
                    }
                    "bracket_text_color" => {
                        cfg.bracket_text_color = Self::parse_hex_rgb(v, cfg.bracket_text_color);
                    }
                    "emote_text_color" => {
                        cfg.emote_text_color = Self::parse_hex_rgb(v, cfg.emote_text_color);
                    }
                    "whisper_text_color" => {
                        cfg.whisper_text_color = Self::parse_hex_rgb(v, cfg.whisper_text_color);
                    }
                    "emit_text_color" => {
                        cfg.emit_text_color = Self::parse_hex_rgb(v, cfg.emit_text_color);
                    }
                    "shout_text_color" => {
                        cfg.shout_text_color = Self::parse_hex_rgb(v, cfg.shout_text_color);
                    }
                    "roll_text_color" => {
                        cfg.roll_text_color = Self::parse_hex_rgb(v, cfg.roll_text_color);
                    }
                    "success_text_color" => {
                        cfg.success_text_color = Self::parse_hex_rgb(v, cfg.success_text_color);
                    }
                    "warning_text_color" => {
                        cfg.warning_text_color = Self::parse_hex_rgb(v, cfg.warning_text_color);
                    }
                    "error_text_color" => {
                        cfg.error_text_color = Self::parse_hex_rgb(v, cfg.error_text_color);
                    }
                    "tab_active_bg" => {
                        cfg.tab_active_bg = Self::parse_hex_rgb(v, cfg.tab_active_bg);
                    }
                    "tab_active_text" => {
                        cfg.tab_active_text = Self::parse_hex_rgb(v, cfg.tab_active_text);
                    }
                    "tab_unread_bg" => {
                        cfg.tab_unread_bg = Self::parse_hex_rgb(v, cfg.tab_unread_bg);
                    }
                    "tab_unread_text" => {
                        cfg.tab_unread_text = Self::parse_hex_rgb(v, cfg.tab_unread_text);
                    }
                    "tab_inactive_bg" => {
                        cfg.tab_inactive_bg = Self::parse_hex_rgb(v, cfg.tab_inactive_bg);
                    }
                    "tab_inactive_text" => {
                        cfg.tab_inactive_text = Self::parse_hex_rgb(v, cfg.tab_inactive_text);
                    }
                    "custom_ui_bg" => cfg.custom_ui_bg = v.trim() == "true",
                    "chat_bg_color" => {
                        cfg.chat_bg_color = Self::parse_hex_rgb(v, cfg.chat_bg_color);
                    }
                    "panel_bg_color" => {
                        cfg.panel_bg_color = Self::parse_hex_rgb(v, cfg.panel_bg_color);
                    }
                    "accent_color" => {
                        cfg.accent_color = Self::parse_hex_rgb(v, cfg.accent_color);
                    }
                    "autocorrect" => cfg.autocorrect = v.trim() == "true",
                    "anim_mode" => {
                        let m = v.trim();
                        if matches!(m, "Full" | "Half" | "Off") {
                            cfg.anim_mode = m.to_owned();
                        }
                    }
                    "theme" => {
                        let t = v.trim();
                        if !t.is_empty() {
                            cfg.theme = t.to_owned();
                        }
                    }
                    "show_avatar_names" => cfg.show_avatar_names = v.trim() != "false",
                    "pounce_visible" => cfg.pounce_visible = v.trim() != "false",
                    "pounce_detached" => cfg.pounce_detached = v.trim() == "true",
                    "pounce_show_offline" => cfg.pounce_show_offline = v.trim() != "false",
                    "show_debug_packets" => cfg.show_debug_packets = v.trim() == "true",
                    "whisper_open_tab_on_incoming" => {
                        cfg.whisper_open_tab_on_incoming = v.trim() != "false"
                    }
                    "whisper_emote_in_whisper" => {
                        cfg.whisper_emote_in_whisper = v.trim() != "false"
                    }
                    "dreams" => {
                        cfg.dream_bookmarks = v
                            .split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(ToOwned::to_owned)
                            .collect();
                    }
                    "limit_view_range" => cfg.limit_view_range = v.trim() != "false",
                    "allow_dream_skins" => cfg.allow_dream_skins = v.trim() != "false",
                    "portrait_size" => {
                        if let Ok(sz) = v.trim().parse::<u16>() {
                            cfg.portrait_size = sz.clamp(48, 160);
                        }
                    }
                    "check_updates_on_startup" => {
                        cfg.check_updates_on_startup = v.trim() != "false"
                    }
                    "first_setup_done" => cfg.first_setup_done = v.trim() == "true",
                    "dice_roll" => {
                        let d = v.trim();
                        if !d.is_empty() {
                            cfg.dice_roll = d.to_owned();
                        }
                    }
                    "sync_folder" => cfg.sync_folder = v.trim().to_owned(),
                    "sync_device_id" => cfg.sync_device_id = v.trim().to_owned(),
                    "sync_updated_at" => cfg.sync_updated_at = v.trim().parse().unwrap_or(0),
                    "window_width" => {
                        if let Ok(w) = v.trim().parse::<u16>() {
                            cfg.window_width = w.clamp(640, 7680);
                        }
                    }
                    "window_height" => {
                        if let Ok(h) = v.trim().parse::<u16>() {
                            cfg.window_height = h.clamp(420, 4320);
                        }
                    }
                    "window_maximized" => cfg.window_maximized = v.trim() == "true",
                    "character_portraits" => {
                        cfg.character_portraits = v
                            .split(',')
                            .filter_map(|entry| {
                                let (name, id_str) = entry.trim().split_once(':')?;
                                let key: String = name
                                    .chars()
                                    .filter(|c| c.is_ascii_alphanumeric())
                                    .map(|c| c.to_ascii_lowercase())
                                    .collect();
                                let pid = id_str.trim().parse::<u32>().ok()?;
                                (!key.is_empty() && pid > 0).then_some((key, pid))
                            })
                            .collect();
                    }
                    _ => {}
                }
            }
        }
        if !cfg.remember_email {
            cfg.email.clear();
        }
        if !cfg.remember_password {
            cfg.password.clear();
        }
        cfg
    }

    pub fn serialize(&self) -> String {
        let strip_nl = |s: &str| s.replace(['\r', '\n'], "");
        let saved_email = if self.remember_email {
            strip_nl(&self.email)
        } else {
            String::new()
        };
        let saved_pass = if self.remember_password {
            strip_nl(&self.password)
        } else {
            String::new()
        };
        let safe_character = strip_nl(&self.character);
        let safe_sync_folder = strip_nl(&self.sync_folder);
        let portraits_str: String = self
            .character_portraits
            .iter()
            .filter(|(k, &v)| !k.is_empty() && v > 0)
            .map(|(k, v)| format!("{k}:{v}"))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "remember_email={}\nremember_password={}\nauto_login={}\nemail={}\ncharacter={}\npassword={}\nwatch={}\nlogging_enabled={}\nlog_whispers={}\nshared_logs_mode={}\nlog_retention_days={}\nnotify_enabled={}\nnotify_whisper={}\nnotify_pounce={}\nnotify_mention={}\nnotify_only_unfocused={}\nflash_taskbar_on_message={}\nflash_taskbar_on_main_chat={}\nblink_tray_on_alert={}\nmention_nicknames={}\nsound_enabled={}\nmusic_enabled={}\nminimize_to_tray={}\nclose_to_tray={}\nfont_family={}\nfont_size={}\nchat_font_size={}\nchat_text_color={:06x}\ninput_text_color={:06x}\nmy_speech_color={:06x}\nbracket_text_color={:06x}\nemote_text_color={:06x}\nwhisper_text_color={:06x}\nemit_text_color={:06x}\nshout_text_color={:06x}\nroll_text_color={:06x}\nsuccess_text_color={:06x}\nwarning_text_color={:06x}\nerror_text_color={:06x}\ntab_active_bg={:06x}\ntab_active_text={:06x}\ntab_unread_bg={:06x}\ntab_unread_text={:06x}\ntab_inactive_bg={:06x}\ntab_inactive_text={:06x}\ncustom_ui_bg={}\nchat_bg_color={:06x}\npanel_bg_color={:06x}\naccent_color={:06x}\nautocorrect={}\nanim_mode={}\ntheme={}\nshow_avatar_names={}\npounce_visible={}\npounce_detached={}\npounce_show_offline={}\nshow_debug_packets={}\nwhisper_open_tab_on_incoming={}\nwhisper_emote_in_whisper={}\ndreams={}\nlimit_view_range={}\nallow_dream_skins={}\nportrait_size={}\ncheck_updates_on_startup={}\nfirst_setup_done={}\ndice_roll={}\nsync_folder={}\nsync_device_id={}\nsync_updated_at={}\nwindow_width={}\nwindow_height={}\nwindow_maximized={}\ncharacter_portraits={}\n",
            self.remember_email,
            self.remember_password,
            self.auto_login,
            saved_email,
            safe_character,
            saved_pass,
            self.watchlist.join(","),
            self.logging_enabled,
            self.log_whispers,
            self.shared_logs_mode,
            self.log_retention_days,
            self.notify_enabled,
            self.notify_whisper,
            self.notify_pounce,
            self.notify_mention,
            self.notify_only_unfocused,
            self.flash_taskbar_on_message,
            self.flash_taskbar_on_main_chat,
            self.blink_tray_on_alert,
            self.mention_nicknames.replace(['\r', '\n'], ","),
            self.sound_enabled,
            self.music_enabled,
            self.minimize_to_tray,
            self.close_to_tray,
            self.font_family,
            self.font_size,
            self.chat_font_size,
            self.chat_text_color,
            self.input_text_color,
            self.my_speech_color,
            self.bracket_text_color,
            self.emote_text_color,
            self.whisper_text_color,
            self.emit_text_color,
            self.shout_text_color,
            self.roll_text_color,
            self.success_text_color,
            self.warning_text_color,
            self.error_text_color,
            self.tab_active_bg,
            self.tab_active_text,
            self.tab_unread_bg,
            self.tab_unread_text,
            self.tab_inactive_bg,
            self.tab_inactive_text,
            self.custom_ui_bg,
            self.chat_bg_color,
            self.panel_bg_color,
            self.accent_color,
            self.autocorrect,
            self.anim_mode,
            self.theme,
            self.show_avatar_names,
            self.pounce_visible,
            self.pounce_detached,
            self.pounce_show_offline,
            self.show_debug_packets,
            self.whisper_open_tab_on_incoming,
            self.whisper_emote_in_whisper,
            self.dream_bookmarks.join(","),
            self.limit_view_range,
            self.allow_dream_skins,
            self.portrait_size,
            self.check_updates_on_startup,
            self.first_setup_done,
            self.dice_roll,
            safe_sync_folder,
            self.sync_device_id,
            self.sync_updated_at,
            self.window_width,
            self.window_height,
            self.window_maximized,
            portraits_str
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_roundtrip() {
        let cfg = ClientConfig {
            email: "user@example.com".into(),
            character: "Tactful".into(),
            password: "secret".into(),
            remember_email: true,
            remember_password: true,
            auto_login: false,
            watchlist: vec!["Alice".into(), "Bob".into()],
            logging_enabled: false,
            log_whispers: true,
            shared_logs_mode: false,
            log_retention_days: 30,
            notify_enabled: true,
            notify_whisper: true,
            notify_pounce: false,
            notify_mention: true,
            notify_only_unfocused: false,
            flash_taskbar_on_message: false,
            flash_taskbar_on_main_chat: true,
            blink_tray_on_alert: false,
            mention_nicknames: "Aur, Bear".into(),
            sound_enabled: false,
            music_enabled: true,
            minimize_to_tray: true,
            close_to_tray: true,
            font_family: "Fixedsys".into(),
            font_size: 16,
            chat_font_size: 15,
            chat_text_color: 0xaabbcc,
            input_text_color: 0xddeeff,
            my_speech_color: 0x112233,
            bracket_text_color: 0x223344,
            emote_text_color: 0x334455,
            whisper_text_color: 0x445566,
            emit_text_color: 0x556677,
            shout_text_color: 0x667788,
            roll_text_color: 0x778899,
            success_text_color: 0x8899aa,
            warning_text_color: 0x99aabb,
            error_text_color: 0xbbccdd,
            tab_active_bg: 0x102030,
            tab_active_text: 0x203040,
            tab_unread_bg: 0x304050,
            tab_unread_text: 0x405060,
            tab_inactive_bg: 0x506070,
            tab_inactive_text: 0x607080,
            custom_ui_bg: true,
            chat_bg_color: 0x050607,
            panel_bg_color: 0x08090a,
            accent_color: 0x0b0c0d,
            autocorrect: true,
            anim_mode: "Half".into(),
            theme: "DragonHoard".into(),
            show_avatar_names: false,
            pounce_visible: false,
            pounce_detached: true,
            pounce_show_offline: false,
            show_debug_packets: true,
            whisper_open_tab_on_incoming: false,
            whisper_emote_in_whisper: true,
            dream_bookmarks: vec!["furc://pmfurn".into()],
            limit_view_range: false,
            allow_dream_skins: false,
            portrait_size: 96,
            check_updates_on_startup: false,
            first_setup_done: true,
            dice_roll: "1d100".into(),
            sync_folder: String::new(),
            sync_device_id: String::new(),
            sync_updated_at: 0,
            window_width: 1200,
            window_height: 800,
            window_maximized: false,
            character_portraits: std::collections::BTreeMap::from([
                ("amaladevi".into(), 248815),
                ("aureusktharr".into(), 257927),
            ]),
        };
        let s = cfg.serialize();
        let parsed = ClientConfig::parse(&s);
        assert_eq!(cfg, parsed);
    }

    #[test]
    fn legacy_config_without_extended_colors_uses_defaults() {
        let parsed = ClientConfig::parse("chat_text_color=010203\ntheme=KitterMagic\n");
        let defaults = ClientConfig::default();
        assert_eq!(parsed.chat_text_color, 0x010203);
        assert_eq!(parsed.theme, "KitterMagic");
        assert_eq!(parsed.bracket_text_color, defaults.bracket_text_color);
        assert_eq!(parsed.tab_active_bg, defaults.tab_active_bg);
        assert_eq!(parsed.tab_unread_bg, defaults.tab_unread_bg);
        assert!(!parsed.custom_ui_bg);
        assert_eq!(parsed.chat_bg_color, defaults.chat_bg_color);
        assert!(parsed.character_portraits.is_empty());
    }

    #[test]
    fn character_portraits_parse_ignores_invalid_and_merges_shared() {
        let mut local = ClientConfig::parse(
            "character_portraits=Amala|Devi:248815,badentry,:10,zero:0,Fulkor:notnum\n",
        );
        assert_eq!(
            local.character_portraits,
            std::collections::BTreeMap::from([("amaladevi".into(), 248_815)])
        );

        let shared = ClientConfig {
            character_portraits: std::collections::BTreeMap::from([
                ("aureusktharr".into(), 257_927),
                ("amaladevi".into(), 999_001),
            ]),
            ..ClientConfig::default()
        };
        local.apply_shared_settings(&shared);
        assert_eq!(
            local.character_portraits,
            std::collections::BTreeMap::from([
                ("amaladevi".into(), 999_001),
                ("aureusktharr".into(), 257_927),
            ])
        );
    }
}
