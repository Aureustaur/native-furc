//! Local user configuration persistence (`~/.config/furcadia-native/config.ini`).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

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
    pub log_retention_days: u32,
    pub notify_enabled: bool,
    pub notify_whisper: bool,
    pub notify_pounce: bool,
    pub notify_mention: bool,
    pub notify_only_unfocused: bool,
    pub sound_enabled: bool,
    pub music_enabled: bool,
    pub minimize_to_tray: bool,
    pub close_to_tray: bool,
    pub font_family: String,
    pub font_size: u16,
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
            log_retention_days: 0,
            notify_enabled: true,
            notify_whisper: true,
            notify_pounce: true,
            notify_mention: true,
            notify_only_unfocused: true,
            sound_enabled: true,
            music_enabled: false,
            minimize_to_tray: false,
            close_to_tray: false,
            font_family: "Proportional".to_owned(),
            font_size: 13,
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
        }
    }
}

impl ClientConfig {
    /// Returns `~/.config/furcadia-native/config.ini` when `HOME` or `XDG_CONFIG_HOME` is set.
    pub fn default_path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("furcadia-native").join("config.ini"))
    }

    pub fn load_default() -> Self {
        Self::default_path()
            .and_then(|p| Self::load(&p).ok())
            .unwrap_or_default()
    }

    pub fn save_default(&self) -> io::Result<()> {
        let Some(path) = Self::default_path() else {
            return Ok(());
        };
        self.save(&path)
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        let text = fs::read_to_string(path)?;
        Ok(Self::parse(&text))
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let body = self.serialize();
        fs::write(path, body)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    pub fn parse(text: &str) -> Self {
        let mut cfg = Self::default();
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = trimmed.split_once('=') {
                match k.trim() {
                    "email" => cfg.email = v.trim().to_owned(),
                    "character" => cfg.character = v.trim().to_owned(),
                    "password" => cfg.password = v.trim().to_owned(),
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
        let saved_email = if self.remember_email {
            self.email.as_str()
        } else {
            ""
        };
        let saved_pass = if self.remember_password {
            self.password.as_str()
        } else {
            ""
        };
        format!(
            "remember_email={}\nremember_password={}\nauto_login={}\nemail={}\ncharacter={}\npassword={}\nwatch={}\nlogging_enabled={}\nlog_whispers={}\nlog_retention_days={}\nnotify_enabled={}\nnotify_whisper={}\nnotify_pounce={}\nnotify_mention={}\nnotify_only_unfocused={}\nsound_enabled={}\nmusic_enabled={}\nminimize_to_tray={}\nclose_to_tray={}\nfont_family={}\nfont_size={}\nanim_mode={}\ntheme={}\nshow_avatar_names={}\npounce_visible={}\npounce_detached={}\npounce_show_offline={}\nshow_debug_packets={}\nwhisper_open_tab_on_incoming={}\nwhisper_emote_in_whisper={}\ndreams={}\nlimit_view_range={}\nallow_dream_skins={}\nportrait_size={}\n",
            self.remember_email,
            self.remember_password,
            self.auto_login,
            saved_email,
            self.character,
            saved_pass,
            self.watchlist.join(","),
            self.logging_enabled,
            self.log_whispers,
            self.log_retention_days,
            self.notify_enabled,
            self.notify_whisper,
            self.notify_pounce,
            self.notify_mention,
            self.notify_only_unfocused,
            self.sound_enabled,
            self.music_enabled,
            self.minimize_to_tray,
            self.close_to_tray,
            self.font_family,
            self.font_size,
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
            self.portrait_size
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
            log_retention_days: 30,
            notify_enabled: true,
            notify_whisper: true,
            notify_pounce: false,
            notify_mention: true,
            notify_only_unfocused: false,
            sound_enabled: false,
            music_enabled: true,
            minimize_to_tray: true,
            close_to_tray: true,
            font_family: "Fixedsys".into(),
            font_size: 16,
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
        };
        let serialized = cfg.serialize();
        assert_eq!(ClientConfig::parse(&serialized), cfg);

        let no_pass = ClientConfig {
            remember_password: false,
            ..cfg
        };
        let parsed = ClientConfig::parse(&no_pass.serialize());
        assert!(parsed.password.is_empty());
        assert!(!parsed.remember_password);
    }
}
