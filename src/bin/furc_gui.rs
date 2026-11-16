//! Native cross-platform GUI client for Furcadia using `eframe` / `egui`.
#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]

#[path = "../logo.rs"]
mod logo;

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Color32, ColorImage, FontId, Pos2, Rect, RichText, Sense, Stroke, TextureHandle,
    TextureOptions, Vec2,
};
use furcadia_client::{
    decode_fsh_sprites, extract_fr01_to_dir, fetch_account_characters, fetch_news, is_fr01,
    logs::{
        append_chat_log, append_whisper_log, default_logs_dir, list_log_characters,
        list_log_files, load_whisper_log, prune_old_logs,
    },
    parse_chat_line, unescape_html,
    win_import::{discover_default_windows_furcadia_dirs, import_windows_furcadia_dir},
    AccountInfo, AssetLocator, Avatar, AvatarAppearance, ChatLine, ChatSpan, CharacterSummary,
    ClientCommand, ClientConfig, ConnectionConfig, FoxArchive, FoxAvatarShape, FoxFrame, FoxLayer,
    FurcadiaClient, FurcadiaMap, LookedPlayerInfo, NewsEntry, OnlineStatus, Palette, PounceEvent,
    PounceMonitor, RegionLayer, RemapLuts, ServerMessage, SessionState, WorldState, DEFAULT_HOST,
    DEFAULT_PORT,
};

const BG_PURPLE: Color32 = Color32::from_rgb(23, 14, 46);
const CARD_PURPLE: Color32 = Color32::from_rgb(34, 22, 68);
const INPUT_PURPLE: Color32 = Color32::from_rgb(20, 12, 40);
const GOLD: Color32 = Color32::from_rgb(212, 169, 74);
const GOLD_DIM: Color32 = Color32::from_rgb(165, 130, 62);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppScreen {
    Login,
    CharacterSelect,
    Connecting,
    InGame,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharSort {
    Recent,
    Alphabetical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SidebarTab {
    Move,
    GoTo,
    Edit,
    Digo,
    Emotes,
    Misc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DreamTheme {
    ClassicPurple,
    DragonHoard,
    KitterMagic,
    FerianWylde,
}

impl DreamTheme {
    fn label(self) -> &'static str {
        match self {
            Self::ClassicPurple => "Classic Purple",
            Self::DragonHoard => "DragonHoard",
            Self::KitterMagic => "KitterMagic",
            Self::FerianWylde => "FerianWylde",
        }
    }

    fn bg(self) -> Color32 {
        match self {
            Self::ClassicPurple => BG_PURPLE,
            Self::DragonHoard => Color32::from_rgb(38, 16, 16),
            Self::KitterMagic => Color32::from_rgb(20, 26, 48),
            Self::FerianWylde => Color32::from_rgb(18, 34, 24),
        }
    }

    fn panel(self) -> Color32 {
        match self {
            Self::ClassicPurple => CARD_PURPLE,
            Self::DragonHoard => Color32::from_rgb(56, 24, 22),
            Self::KitterMagic => Color32::from_rgb(30, 40, 72),
            Self::FerianWylde => Color32::from_rgb(28, 50, 36),
        }
    }

    fn accent(self) -> Color32 {
        match self {
            Self::ClassicPurple | Self::DragonHoard => GOLD,
            Self::KitterMagic => Color32::from_rgb(135, 205, 250),
            Self::FerianWylde => Color32::from_rgb(155, 220, 120),
        }
    }
}

#[derive(Debug, Clone)]
struct UpdateReleaseInfo {
    tag: String,
    version: String,
    html_url: String,
    asset_url: Option<String>,
}

enum UpdateCheckMsg {
    UpToDate(String),
    UpdateAvailable(UpdateReleaseInfo),
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum UpdateInstallState {
    Idle,
    Downloading,
    Installing,
    Installed,
    Failed(String),
}

enum UpdateInstallMsg {
    #[cfg(not(target_os = "windows"))]
    Installing,
    #[cfg(not(target_os = "windows"))]
    Installed,
    #[cfg(target_os = "windows")]
    LaunchInstallerAndExit(std::path::PathBuf),
    Failed(String),
}

fn update_download_filename(url: &str) -> String {
    let base = url.split('?').next().unwrap_or(url);
    let name = base.rsplit('/').next().unwrap_or("").trim();
    if name.is_empty() {
        "furcadia-native-update".to_owned()
    } else {
        name.to_owned()
    }
}

fn extract_json_string_field(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let idx = json.find(&needle)?;
    let rest = &json[idx + needle.len()..];
    let colon = rest.find(':')?;
    let after_colon = rest[colon + 1..].trim_start();
    let content = after_colon.strip_prefix('"')?;
    let mut out = String::new();
    let mut escaped = false;
    for ch in content.chars() {
        if escaped {
            out.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            return Some(out);
        } else {
            out.push(ch);
        }
    }
    None
}

fn parse_version_tuple(raw: &str) -> Option<(u64, u64, u64)> {
    let trimmed = raw.trim().trim_start_matches(['v', 'V']);
    let core = trimmed.split(['-', '+']).next().unwrap_or(trimmed);
    let mut parts = core.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next().unwrap_or("0").parse::<u64>().ok()?;
    let patch = parts.next().unwrap_or("0").parse::<u64>().ok()?;
    Some((major, minor, patch))
}

fn is_newer_version(current: &str, latest: &str) -> bool {
    match (parse_version_tuple(current), parse_version_tuple(latest)) {
        (Some(c), Some(l)) => l > c,
        _ => false,
    }
}

fn parse_github_release_json(json: &str) -> Option<UpdateReleaseInfo> {
    let tag = extract_json_string_field(json, "tag_name")?;
    let version = tag.trim().trim_start_matches(['v', 'V']).to_owned();
    let html_url = extract_json_string_field(json, "html_url")
        .unwrap_or_else(|| "https://github.com/morganlowe/native-furcadia/releases".to_owned());

    let mut urls = Vec::new();
    let mut search = json;
    while let Some(pos) = search.find("\"browser_download_url\"") {
        let slice = &search[pos..];
        if let Some(url) = extract_json_string_field(slice, "browser_download_url") {
            urls.push(url);
        }
        search = &search[pos + "\"browser_download_url\"".len()..];
    }

    #[cfg(target_os = "windows")]
    let asset_url = urls
        .iter()
        .find(|u| u.to_ascii_lowercase().ends_with(".exe"))
        .cloned();
    #[cfg(not(target_os = "windows"))]
    let asset_url = if std::path::Path::new("/etc/arch-release").exists() {
        urls.iter()
            .find(|u| u.ends_with(".pkg.tar.zst") && !u.contains("-debug-"))
            .cloned()
    } else {
        urls.iter().find(|u| u.ends_with(".deb")).cloned()
    };

    Some(UpdateReleaseInfo {
        tag,
        version,
        html_url,
        asset_url,
    })
}

struct WhisperSession {
    peer: String,
    history_lines: Vec<String>,
    lines: Vec<ChatLine>,
    input: String,
    unread: usize,
    detached: bool,
}

impl WhisperSession {
    fn new(peer: String) -> Self {
        Self {
            peer,
            history_lines: Vec::new(),
            lines: Vec::new(),
            input: String::new(),
            unread: 0,
            detached: false,
        }
    }
}

fn render_whisper_history_lines(
    ui: &mut egui::Ui,
    history_lines: &[String],
    has_live_lines: bool,
) {
    if history_lines.is_empty() {
        return;
    }
    let log_color = Color32::from_rgb(142, 145, 160);
    for line in history_lines {
        ui.add(
            egui::Label::new(RichText::new(line).size(12.5).color(log_color)).wrap(),
        );
    }
    if has_live_lines {
        ui.label(
            RichText::new("─── Current Session ───")
                .size(11.0)
                .color(Color32::from_rgb(108, 110, 126)),
        );
    }
}

#[derive(Clone)]
struct CachedSprite {
    tex: TextureHandle,
    offset: (i16, i16),
    elev: (i16, i16),
    is_one_by_one: bool,
}

#[derive(Clone)]
struct ResolvedLayerTex {
    tex: TextureHandle,
    offset: (i16, i16),
    is_one_by_one: bool,
    category: u16,
    slot: u8,
}

#[derive(Clone)]
struct ResolvedShapeLayers {
    layers: Vec<ResolvedLayerTex>,
    elev: (i16, i16),
    effect_pass: u8,
    alpha: u8,
    is_animated: bool,
}

#[derive(Clone)]
struct AvatarHitBox {
    uid: u32,
    display_name: String,
    short_name: String,
    is_self: bool,
    sprite_rect: Option<Rect>,
    badge_rect: Option<Rect>,
    fallback_center: Option<Pos2>,
    fallback_radius: f32,
}

impl AvatarHitBox {
    fn contains_badge(&self, pos: Pos2) -> bool {
        self.badge_rect.is_some_and(|r| r.contains(pos))
    }

    fn contains_body(&self, pos: Pos2) -> bool {
        self.sprite_rect.is_some_and(|r| r.contains(pos))
            || self
                .fallback_center
                .is_some_and(|c| (pos - c).length() <= self.fallback_radius)
    }
}

#[derive(Clone)]
struct NameBadge {
    uid: u32,
    pos: Pos2,
    name: String,
    is_self: bool,
}

#[derive(Clone)]
struct ContextMenuTarget {
    uid: u32,
    display_name: String,
    short_name: String,
    is_self: bool,
    anchor_pos: Pos2,
    opened_this_frame: bool,
}

#[derive(Clone)]
struct LookedPlayerCard {
    info: LookedPlayerInfo,
    short_name: String,
    portrait_id: Option<u32>,
    portrait_tex: Option<TextureHandle>,
    badge_tex: Option<TextureHandle>,
    resolved_for: Option<(u16, u8, [u8; 12])>,
}

#[derive(Clone, Copy)]
enum PlayerMenuAction {
    Look,
    Whisper,
    Join,
    Summon,
    Follow,
    Lead,
    Cuddle,
    TogglePounce,
    ToggleIgnore,
    ShareEdit,
    UnshareEdit,
    Raw(&'static str),
}

fn render_player_menu_buttons(
    ui: &mut egui::Ui,
    is_self: bool,
    in_pounce: bool,
    pounce_only: bool,
) -> Option<PlayerMenuAction> {
    let mut chosen = None;
    let mut item = |ui: &mut egui::Ui, label: &str, act: PlayerMenuAction| {
        if ui
            .add(egui::Button::new(label).min_size(Vec2::new(146.0, 22.0)))
            .clicked()
        {
            chosen = Some(act);
        }
    };
    if is_self {
        item(ui, "Look (Self)", PlayerMenuAction::Look);
        ui.separator();
        for (label, cmd) in [
            ("Stand", "stand"),
            ("Sit", "sit"),
            ("Lay Down", "lay"),
            ("Turn Left", "<"),
            ("Turn Right", ">"),
            ("Get / Drop", "get"),
            ("Use", "use"),
            ("Wings", "wings"),
            ("Costume", "costume"),
        ] {
            item(ui, label, PlayerMenuAction::Raw(cmd));
        }
    } else if pounce_only {
        item(ui, "Whisper", PlayerMenuAction::Whisper);
        item(ui, "Look", PlayerMenuAction::Look);
        ui.separator();
        item(ui, "Join", PlayerMenuAction::Join);
        item(ui, "Summon", PlayerMenuAction::Summon);
        item(ui, "Follow", PlayerMenuAction::Follow);
        item(ui, "Cuddle", PlayerMenuAction::Cuddle);
        ui.separator();
        item(ui, "Ignore / Unignore", PlayerMenuAction::ToggleIgnore);
        item(ui, "Remove", PlayerMenuAction::TogglePounce);
    } else {
        item(ui, "Look", PlayerMenuAction::Look);
        item(ui, "Whisper", PlayerMenuAction::Whisper);
        ui.separator();
        item(ui, "Join", PlayerMenuAction::Join);
        item(ui, "Summon", PlayerMenuAction::Summon);
        item(ui, "Follow", PlayerMenuAction::Follow);
        item(ui, "Lead", PlayerMenuAction::Lead);
        item(ui, "Cuddle", PlayerMenuAction::Cuddle);
        ui.separator();
        item(
            ui,
            if in_pounce {
                "Remove from Pounce"
            } else {
                "Add to Pounce"
            },
            PlayerMenuAction::TogglePounce,
        );
        item(ui, "Ignore / Unignore", PlayerMenuAction::ToggleIgnore);
        item(ui, "Share Edit", PlayerMenuAction::ShareEdit);
        item(ui, "Unshare Edit", PlayerMenuAction::UnshareEdit);
    }
    chosen
}

fn wire_player_name(name: &str) -> String {
    name.trim().replace(' ', "|")
}

fn shortname_key(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Maps wire species ID to `(fox_file_id, object_id)` matching web client `Eu()` (`L2293`).
fn resolve_species_fox_spec(sp_id: usize) -> (usize, u16) {
    match sp_id {
        97 => (104, 1),
        98 => (106, 1),
        99 => (119, 1),
        id if id > 2000 => (id % 2000, (id / 2000) as u16),
        id => (id.max(1), 0),
    }
}

/// Cache-key offset for dynamic avatars (`DPlayer<id>.fox`) so they never collide with `species<id>.fox`.
const DYNAMIC_AVATAR_CACHE_BASE: usize = 100_000;

const ARCHIVE_DEFAULT: u8 = 0;
const ARCHIVE_DREAM: u8 = 1;
const ARCHIVE_LEGACY: u8 = 2;

struct FoxSpriteCache {
    default_archive: Option<FoxArchive>,
    legacy_archive: Option<FoxArchive>,
    dream_archive: Option<FoxArchive>,
    shape_textures: HashMap<(u16, u8, bool), Option<CachedSprite>>,
    image_textures: HashMap<(u8, usize), (TextureHandle, bool)>,
    avatar_textures: HashMap<(bool, usize, [u8; 12]), (TextureHandle, bool)>,
    prefix: String,
}

impl FoxSpriteCache {
    fn load(assets: &AssetLocator, filename: &str, prefix: &str) -> Self {
        Self::load_with_legacy(assets, filename, None, prefix)
    }

    fn load_with_legacy(
        assets: &AssetLocator,
        default_filename: &str,
        legacy_filename: Option<&str>,
        prefix: &str,
    ) -> Self {
        let default_archive = assets
            .default_patch_path(default_filename)
            .and_then(|p| FoxArchive::load(&p).ok());
        let legacy_archive = legacy_filename
            .and_then(|f| assets.legacy_patch_path(f))
            .and_then(|p| FoxArchive::load(&p).ok());
        Self {
            default_archive,
            legacy_archive,
            dream_archive: None,
            shape_textures: HashMap::new(),
            image_textures: HashMap::new(),
            avatar_textures: HashMap::new(),
            prefix: prefix.to_owned(),
        }
    }

    fn load_legacy_only(assets: &AssetLocator, legacy_filename: &str, prefix: &str) -> Self {
        let legacy_archive = assets
            .legacy_patch_path(legacy_filename)
            .and_then(|p| FoxArchive::load(&p).ok());
        Self {
            default_archive: None,
            legacy_archive,
            dream_archive: None,
            shape_textures: HashMap::new(),
            image_textures: HashMap::new(),
            avatar_textures: HashMap::new(),
            prefix: prefix.to_owned(),
        }
    }

    fn load_local_species(assets: &AssetLocator, local_idx: usize, prefix: &str) -> Self {
        let default_archive = assets
            .local_species_patch_path(local_idx)
            .and_then(|p| FoxArchive::load(&p).ok());
        Self {
            default_archive,
            legacy_archive: None,
            dream_archive: None,
            shape_textures: HashMap::new(),
            image_textures: HashMap::new(),
            avatar_textures: HashMap::new(),
            prefix: prefix.to_owned(),
        }
    }

    fn from_dream_only(path: &std::path::Path, prefix: &str) -> Self {
        Self {
            default_archive: None,
            legacy_archive: None,
            dream_archive: FoxArchive::load(path).ok(),
            shape_textures: HashMap::new(),
            image_textures: HashMap::new(),
            avatar_textures: HashMap::new(),
            prefix: prefix.to_owned(),
        }
    }

    fn set_dream_patch(&mut self, path: &std::path::Path) -> Result<(), String> {
        match FoxArchive::load(path) {
            Ok(arc) => {
                self.dream_archive = Some(arc);
                self.shape_textures.clear();
                self.image_textures.clear();
                self.avatar_textures.clear();
                Ok(())
            }
            Err(e) => {
                self.dream_archive = None;
                self.shape_textures.clear();
                self.image_textures.clear();
                Err(format!(
                    "{}: {e}",
                    path.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("patch.fox")
                ))
            }
        }
    }

    fn clear_dream_patch(&mut self) {
        if self.dream_archive.take().is_some() {
            self.shape_textures.clear();
            self.image_textures.clear();
            self.avatar_textures.clear();
        }
    }

    fn is_loaded(&self) -> bool {
        self.dream_archive.is_some()
            || self.default_archive.is_some()
            || self.legacy_archive.is_some()
    }

    fn has_dream(&self) -> bool {
        self.dream_archive.is_some()
    }

    fn is_loaded_for_mode(&self, use_legacy: bool) -> bool {
        self.dream_archive.is_some()
            || if use_legacy {
                self.legacy_archive.is_some()
            } else {
                self.default_archive.is_some()
            }
    }

    fn active_is_fshx_or_fsh(&self, use_legacy: bool) -> bool {
        let base = if use_legacy {
            self.legacy_archive.as_ref()
        } else {
            self.default_archive.as_ref()
        };
        self.dream_archive
            .as_ref()
            .or(base)
            .is_some_and(|arc| arc.is_fshx_or_fsh)
    }

    fn dream_is_fshx_or_fsh(&self) -> bool {
        self.dream_archive
            .as_ref()
            .is_some_and(|arc| arc.is_fshx_or_fsh)
    }

    fn decode_or_get_image(
        fox: &mut FoxArchive,
        archive_tag: u8,
        prefix: &str,
        image_textures: &mut HashMap<(u8, usize), (TextureHandle, bool)>,
        ctx: &egui::Context,
        palette: &Palette,
        image_idx: usize,
    ) -> Option<(TextureHandle, bool)> {
        if let Some(entry) = image_textures.get(&(archive_tag, image_idx)) {
            return Some(entry.clone());
        }
        let sprite = fox.decode_image(image_idx, palette).ok()?;
        if sprite.width == 0 || sprite.height == 0 {
            return None;
        }
        let is_one_by_one = sprite.width <= 1 && sprite.height <= 1;
        let color_img = ColorImage::from_rgba_unmultiplied(
            [sprite.width as usize, sprite.height as usize],
            &sprite.rgba,
        );
        let tag = match archive_tag {
            ARCHIVE_DREAM => "d",
            ARCHIVE_LEGACY => "l",
            _ => "b",
        };
        let tex = ctx.load_texture(
            format!("{prefix}_{tag}_img_{image_idx}"),
            color_img,
            TextureOptions::NEAREST,
        );
        let pair = (tex, is_one_by_one);
        image_textures.insert((archive_tag, image_idx), pair.clone());
        Some(pair)
    }

    fn resolve_frame_layers(
        fox: &mut FoxArchive,
        archive_tag: u8,
        prefix: &str,
        image_textures: &mut HashMap<(u8, usize), (TextureHandle, bool)>,
        ctx: &egui::Context,
        palette: &Palette,
        frame: &FoxFrame,
        slot: u8,
        slot_offset: (i16, i16),
        out: &mut Vec<ResolvedLayerTex>,
    ) {
        let mut layers: Vec<FoxLayer> = frame.layers.clone();
        layers.sort_by_key(|l| if (l.category & 0x40) != 0 { 0 } else { 1 });
        for layer in layers {
            let Some((tex, is_one_by_one)) = Self::decode_or_get_image(
                fox,
                archive_tag,
                prefix,
                image_textures,
                ctx,
                palette,
                layer.image_idx,
            ) else {
                continue;
            };
            if is_one_by_one {
                continue;
            }
            out.push(ResolvedLayerTex {
                tex,
                offset: (
                    slot_offset.0.saturating_add(layer.layer_offset.0),
                    slot_offset.1.saturating_add(layer.layer_offset.1),
                ),
                is_one_by_one,
                category: layer.category,
                slot,
            });
        }
    }

    fn resolve_shape_slots(
        fox: &mut FoxArchive,
        archive_tag: u8,
        prefix: &str,
        image_textures: &mut HashMap<(u8, usize), (TextureHandle, bool)>,
        ctx: &egui::Context,
        palette: &Palette,
        shape: &FoxAvatarShape,
        anim_ms: u64,
    ) -> Option<ResolvedShapeLayers> {
        let resolved = shape.slots_at(anim_ms)?;
        let is_animated = shape.frames.len() > 1 && !shape.anim_steps.is_empty();
        let mut layers = Vec::new();
        for active in resolved.slots.iter().flatten() {
            let Some(frame) = shape.frames.get(active.frame_idx) else {
                continue;
            };
            Self::resolve_frame_layers(
                fox,
                archive_tag,
                prefix,
                image_textures,
                ctx,
                palette,
                frame,
                active.slot,
                active.offset,
                &mut layers,
            );
        }
        Some(ResolvedShapeLayers {
            layers,
            elev: resolved.elev,
            effect_pass: resolved.effect_pass,
            alpha: resolved.alpha,
            is_animated,
        })
    }

    fn get_or_load_shape_mode(
        &mut self,
        ctx: &egui::Context,
        palette: &Palette,
        id: u16,
        dir: u8,
        use_legacy: bool,
    ) -> Option<CachedSprite> {
        if let Some(cached) = self.shape_textures.get(&(id, dir, use_legacy)) {
            return cached.clone();
        }

        let candidates = if use_legacy {
            [
                (ARCHIVE_DREAM, self.dream_archive.as_mut()),
                (ARCHIVE_LEGACY, self.legacy_archive.as_mut()),
                (ARCHIVE_DEFAULT, self.default_archive.as_mut()),
            ]
        } else {
            [
                (ARCHIVE_DREAM, self.dream_archive.as_mut()),
                (ARCHIVE_DEFAULT, self.default_archive.as_mut()),
                (ARCHIVE_LEGACY, self.legacy_archive.as_mut()),
            ]
        };
        let mut loaded: Option<CachedSprite> = None;
        for (archive_tag, maybe_fox) in candidates {
            if let Some(fox) = maybe_fox {
                if let Some(shape) = fox.resolve_shape(id, dir) {
                    if let Some((tex, is_one_by_one)) = Self::decode_or_get_image(
                        fox,
                        archive_tag,
                        &self.prefix,
                        &mut self.image_textures,
                        ctx,
                        palette,
                        shape.image_idx,
                    ) {
                        loaded = Some(CachedSprite {
                            tex,
                            offset: shape.offset,
                            elev: shape.elev,
                            is_one_by_one,
                        });
                        break;
                    }
                }
            }
        }

        self.shape_textures.insert((id, dir, use_legacy), loaded.clone());
        loaded
    }

    fn get_or_load_shape_layers(
        &mut self,
        ctx: &egui::Context,
        palette: &Palette,
        id: u16,
        dir: u8,
        anim_ms: u64,
    ) -> Option<ResolvedShapeLayers> {
        self.get_or_load_layers_inner(ctx, palette, id, None, None, dir, anim_ms, false)
    }

    fn get_or_load_shape_layers_mode(
        &mut self,
        ctx: &egui::Context,
        palette: &Palette,
        id: u16,
        dir: u8,
        anim_ms: u64,
        use_legacy: bool,
    ) -> Option<ResolvedShapeLayers> {
        self.get_or_load_layers_inner(ctx, palette, id, None, None, dir, anim_ms, use_legacy)
    }

    fn get_or_load_dream_shape_layers(
        &mut self,
        ctx: &egui::Context,
        palette: &Palette,
        id: u16,
        dir: u8,
        anim_ms: u64,
    ) -> Option<ResolvedShapeLayers> {
        let fox = self.dream_archive.as_mut()?;
        let shape = fox.resolve_object_shape(id, dir).cloned()?;
        Self::resolve_shape_slots(
            fox,
            ARCHIVE_DREAM,
            &self.prefix,
            &mut self.image_textures,
            ctx,
            palette,
            &shape,
            anim_ms,
        )
    }

    fn get_or_load_default_shape_layers(
        &mut self,
        ctx: &egui::Context,
        palette: &Palette,
        id: u16,
        dir: u8,
        anim_ms: u64,
    ) -> Option<ResolvedShapeLayers> {
        let fox = self.default_archive.as_mut()?;
        let shape = fox.resolve_object_shape(id, dir).cloned()?;
        Self::resolve_shape_slots(
            fox,
            ARCHIVE_DEFAULT,
            &self.prefix,
            &mut self.image_textures,
            ctx,
            palette,
            &shape,
            anim_ms,
        )
    }

    fn get_or_load_ps_layers(
        &mut self,
        ctx: &egui::Context,
        palette: &Palette,
        id: u16,
        purpose: u8,
        state: Option<u8>,
        dir: u8,
        anim_ms: u64,
    ) -> Option<ResolvedShapeLayers> {
        self.get_or_load_layers_inner(
            ctx,
            palette,
            id,
            Some(purpose),
            Some(state),
            dir,
            anim_ms,
            false,
        )
    }

    fn get_or_load_layers_inner(
        &mut self,
        ctx: &egui::Context,
        palette: &Palette,
        id: u16,
        purpose: Option<u8>,
        state: Option<Option<u8>>,
        dir: u8,
        anim_ms: u64,
        use_legacy: bool,
    ) -> Option<ResolvedShapeLayers> {
        let prefix = self.prefix.clone();
        let candidates = if use_legacy {
            [
                (ARCHIVE_DREAM, self.dream_archive.as_mut()),
                (ARCHIVE_LEGACY, self.legacy_archive.as_mut()),
                (ARCHIVE_DEFAULT, self.default_archive.as_mut()),
            ]
        } else {
            [
                (ARCHIVE_DREAM, self.dream_archive.as_mut()),
                (ARCHIVE_DEFAULT, self.default_archive.as_mut()),
                (ARCHIVE_LEGACY, self.legacy_archive.as_mut()),
            ]
        };
        for (archive_tag, maybe_fox) in candidates {
            let Some(fox) = maybe_fox else {
                continue;
            };
            let shape_opt = match (purpose, state) {
                (Some(p), Some(s)) => fox.resolve_object_shape_ps(id, p, s, dir),
                _ => fox.resolve_object_shape(id, dir),
            };
            let Some(shape) = shape_opt.cloned() else {
                continue;
            };
            if let Some(resolved) = Self::resolve_shape_slots(
                fox,
                archive_tag,
                &prefix,
                &mut self.image_textures,
                ctx,
                palette,
                &shape,
                anim_ms,
            ) {
                return Some(resolved);
            }
        }
        None
    }

    fn decode_or_get_avatar_image(
        fox: &mut FoxArchive,
        is_dream: bool,
        prefix: &str,
        avatar_textures: &mut HashMap<(bool, usize, [u8; 12]), (TextureHandle, bool)>,
        ctx: &egui::Context,
        palette: &Palette,
        appearance: &AvatarAppearance,
        image_idx: usize,
        remap_color: bool,
    ) -> Option<(TextureHandle, bool)> {
        let cache_color = if remap_color {
            appearance.color_key
        } else {
            [0xff; 12]
        };
        if let Some(cached) = avatar_textures.get(&(is_dream, image_idx, cache_color)) {
            return Some(cached.clone());
        }
        let sprite = fox.decode_image(image_idx, palette).ok()?;
        if sprite.width == 0 || sprite.height == 0 {
            return None;
        }
        let is_one_by_one = sprite.width <= 1 && sprite.height <= 1;
        let pixels = if remap_color {
            RemapLuts::from_appearance(appearance).apply_rgba(&sprite.rgba)
        } else {
            sprite.rgba.clone()
        };
        let color_img = ColorImage::from_rgba_unmultiplied(
            [sprite.width as usize, sprite.height as usize],
            &pixels,
        );
        let tag = if is_dream { "d" } else { "b" };
        let handle = ctx.load_texture(
            format!("{prefix}_{tag}_av_{image_idx}_{cache_color:?}"),
            color_img,
            TextureOptions::NEAREST,
        );
        let pair = (handle, is_one_by_one);
        avatar_textures.insert((is_dream, image_idx, cache_color), pair.clone());
        Some(pair)
    }

    /// Resolves, colorizes (via Furcadia `B == 0` green-channel LUTs only when `category & 0x20 != 0`),
    /// and caches avatar layers, supporting `[dream_archive, default_archive]` fallback and KitterSpeak
    /// frame animations. Returns `(layers, is_animated)` sorted shadow-first.
    #[allow(clippy::too_many_arguments)]
    fn get_or_load_avatar_layers(
        &mut self,
        ctx: &egui::Context,
        palette: &Palette,
        appearance: &AvatarAppearance,
        obj_id: u16,
        gender: u8,
        dir: u8,
        pose: u8,
        anim_ms: u64,
    ) -> Option<(Vec<(TextureHandle, (i16, i16), u16)>, bool)> {
        let mut found: Option<(bool, FoxAvatarShape)> = None;
        let pass_flags: &[bool] = if obj_id == 0 {
            &[true]
        } else {
            &[false, true]
        };
        for &allow_obj0_fallback in pass_flags {
            for (is_dream, maybe_fox) in [
                (true, self.dream_archive.as_ref()),
                (false, self.default_archive.as_ref()),
            ] {
                let Some(fox) = maybe_fox else {
                    continue;
                };
                let eff_pose = if fox.object_pose_clamp(obj_id) && (1..=4).contains(&pose) {
                    2
                } else {
                    pose
                };
                if let Some(sh) = fox.resolve_avatar_shape_opt(
                    obj_id,
                    gender,
                    dir,
                    eff_pose,
                    allow_obj0_fallback,
                ) {
                    found = Some((is_dream, sh.clone()));
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }

        let (is_dream, shape) = found?;
        let is_animated = shape.frames.len() > 1 && !shape.anim_steps.is_empty();
        let fox = if is_dream {
            self.dream_archive.as_mut()?
        } else {
            self.default_archive.as_mut()?
        };

        let mut layer_batches: Vec<((i16, i16), Vec<FoxLayer>)> = Vec::new();
        if let Some(resolved) = shape.slots_at(anim_ms) {
            for active in resolved.slots.iter().flatten() {
                if let Some(frame) = shape.frames.get(active.frame_idx) {
                    if !frame.layers.is_empty() {
                        layer_batches.push((active.offset, frame.layers.clone()));
                    }
                }
            }
        }
        if layer_batches.is_empty() && !shape.layers.is_empty() {
            layer_batches.push((shape.offset, shape.layers.clone()));
        }

        let mut out = Vec::new();
        for (slot_off, mut layers) in layer_batches {
            // Draw shadows (category bit 0x40) underneath body/wings layers.
            layers.sort_by_key(|l| if (l.category & 0x40) != 0 { 0 } else { 1 });
            for layer in layers {
                let is_shadow = (layer.category & 0x40) != 0;
                let remap_color = !is_shadow && (layer.category & 0x20) != 0;
                let Some((tex, is_one_by_one)) = Self::decode_or_get_avatar_image(
                    fox,
                    is_dream,
                    &self.prefix,
                    &mut self.avatar_textures,
                    ctx,
                    palette,
                    appearance,
                    layer.image_idx,
                    remap_color,
                ) else {
                    continue;
                };
                if is_one_by_one {
                    continue;
                }
                let total_off = (
                    slot_off.0.saturating_add(layer.layer_offset.0),
                    slot_off.1.saturating_add(layer.layer_offset.1),
                );
                out.push((tex, total_off, layer.category));
            }
        }

        if out.is_empty() {
            None
        } else {
            Some((out, is_animated))
        }
    }

    /// Resolves a species portrait (`purpose = 4`) or gender/species icon (`purpose = 35`).
    fn get_or_load_portrait_tex(
        &mut self,
        ctx: &egui::Context,
        palette: &Palette,
        appearance: &AvatarAppearance,
        obj_id: u16,
        gender: u8,
        purpose: u8,
    ) -> Option<TextureHandle> {
        const GENDER_NIBBLE: [u8; 3] = [1, 2, 4];
        let nibble = GENDER_NIBBLE[(gender as usize).min(2)];
        let try_ids: &[u16] = if obj_id == 0 { &[0] } else { &[obj_id, 0] };

        for &try_id in try_ids {
            for (is_dream, maybe_fox) in [
                (true, self.dream_archive.as_mut()),
                (false, self.default_archive.as_mut()),
            ] {
                let Some(fox) = maybe_fox else {
                    continue;
                };
                let Some(shape) = fox
                    .resolve_object_shape_ps(try_id, purpose, Some(nibble), 0)
                    .cloned()
                else {
                    continue;
                };
                let layers = shape
                    .frames
                    .first()
                    .filter(|f| !f.layers.is_empty())
                    .map(|f| f.layers.as_slice())
                    .unwrap_or(shape.layers.as_slice());
                let candidate = layers
                    .iter()
                    .find(|l| (l.category & 0x40) == 0 && (l.category & 0x80) == 0)
                    .or_else(|| layers.iter().find(|l| (l.category & 0x80) == 0))
                    .or_else(|| layers.first());
                let Some(layer) = candidate else {
                    continue;
                };
                let remap_color = (layer.category & 0x40) == 0 && (layer.category & 0x20) != 0;
                if let Some((tex, is_one_by_one)) = Self::decode_or_get_avatar_image(
                    fox,
                    is_dream,
                    &self.prefix,
                    &mut self.avatar_textures,
                    ctx,
                    palette,
                    appearance,
                    layer.image_idx,
                    remap_color,
                ) {
                    if !is_one_by_one {
                        return Some(tex);
                    }
                }
            }
        }
        None
    }
}

enum UiToNet {
    AuthenticateAccount {
        email: String,
        password: String,
    },
    ConnectCharacter {
        seq: u64,
        email: String,
        character: String,
        password: String,
        updated_desc: Option<String>,
        colors: Option<String>,
    },
    Send(ClientCommand),
    DownloadDream {
        url: String,
        map_name: String,
        checksum: Option<String>,
    },
    DownloadSpecies {
        file_id: usize,
        server_code: Option<char>,
        if_newer_than: Option<std::path::PathBuf>,
    },
    DownloadPortrait {
        portrait_id: u32,
    },
    DownloadChatImage {
        url: String,
    },
    SpellCheck {
        seq: u64,
        text: String,
        custom_dict: std::collections::HashSet<String>,
    },
    Disconnect,
}

#[derive(Clone, Debug)]
struct MisspelledWord {
    word: String,
    start: usize,
    end: usize,
    suggestions: Vec<String>,
}

enum NetToUi {
    Status(String),
    AuthError(String),
    Authenticated(AccountInfo),
    News(Vec<NewsEntry>),
    ConnectStage {
        seq: u64,
        stage: String,
    },
    ConnectFailed {
        seq: u64,
        error: String,
    },
    Connected {
        seq: u64,
        connected: bool,
    },
    Message(ServerMessage),
    DreamDownloaded {
        map_name: String,
        path: std::path::PathBuf,
        dream_dir: Option<std::path::PathBuf>,
    },
    DreamError(String),
    SpeciesDownloaded {
        file_id: usize,
        path: Option<std::path::PathBuf>,
    },
    PortraitDownloaded {
        portrait_id: u32,
        rgba: Option<(usize, usize, Vec<u8>)>,
    },
    ChatImageDownloaded {
        url: String,
        rgba: Option<(usize, usize, Vec<u8>)>,
    },
    SpellCheckResult {
        seq: u64,
        misspelled: Vec<MisspelledWord>,
    },
}

struct BackgroundSession {
    char_idx: usize,
    char_name: String,
    is_connected: bool,
    status_text: String,
    world: WorldState,
    chat_log: Vec<ChatLine>,
    whispers: Vec<WhisperSession>,
    active_chat_tab: Option<String>,
    current_dream_dir: Option<std::path::PathBuf>,
    cam_offset: Vec2,
    unread_count: usize,
    to_net: Sender<UiToNet>,
    from_net: Receiver<NetToUi>,
}

struct FurcadiaApp {
    screen: AppScreen,

    email: String,
    password: String,
    remember_email: bool,
    remember_password: bool,
    auto_login: bool,
    auth_busy: bool,
    auth_error: Option<String>,

    account_info: Option<AccountInfo>,
    selected_char_idx: usize,
    char_sort: CharSort,
    editing_char: bool,
    edit_desc_buf: String,
    pending_desc: Option<String>,
    news: Vec<NewsEntry>,
    char_portraits: HashMap<String, Option<TextureHandle>>,

    status_text: String,
    is_connected: bool,
    connect_seq: u64,
    connecting_stage: String,
    connecting_error: Option<String>,
    connected_at: Option<Instant>,

    chat_input: String,
    spell_seq: u64,
    spell_dirty_at: Option<Instant>,
    spell_misspelled: Vec<MisspelledWord>,
    custom_dict_words: std::collections::HashSet<String>,
    chat_log: Vec<ChatLine>,
    main_chat_unread: usize,
    whispers: Vec<WhisperSession>,
    active_chat_tab: Option<String>,
    focus_chat_input: bool,
    dragging_whisper_tab: Option<usize>,
    new_whisper_input: String,
    active_char_tab_pos: usize,
    dragging_char_tab: Option<usize>,

    sidebar_tab: SidebarTab,
    theme: DreamTheme,
    dream_url_input: String,
    target_player_input: String,
    live_desc_input: String,
    afk_active: bool,
    show_debug_packets: bool,
    show_avatar_names: bool,
    import_path_input: String,
    import_source_detected: bool,

    watch_input: String,
    pounce: PounceMonitor,
    pounce_visible: bool,
    pounce_detached: bool,
    pounce_show_offline: bool,

    assets: AssetLocator,
    palette: Palette,
    floors_cache: FoxSpriteCache,
    floors2_cache: FoxSpriteCache,
    floore_cache: FoxSpriteCache,
    items_cache: FoxSpriteCache,
    iteme_cache: FoxSpriteCache,
    portals_cache: FoxSpriteCache,
    portalse_cache: FoxSpriteCache,
    effects_cache: FoxSpriteCache,
    effectse_cache: FoxSpriteCache,
    lighting_cache: FoxSpriteCache,
    lightinge_cache: FoxSpriteCache,
    ambience_cache: FoxSpriteCache,
    ambiencee_cache: FoxSpriteCache,
    dream_item_splits: Vec<(u16, u16, u16, FoxSpriteCache)>,
    legacy_item_splits: Vec<(u16, u16, u16, FoxSpriteCache)>,
    junction_cache: FoxSpriteCache,
    wall_caches: HashMap<usize, FoxSpriteCache>,
    species_caches: HashMap<usize, FoxSpriteCache>,
    pending_species_downloads: std::collections::HashSet<usize>,
    dynamic_avatars: HashMap<u16, (usize, u16)>,
    dynamic_avatar_server: Option<char>,
    custom_portrait_rgba: HashMap<u32, Option<(usize, usize, Vec<u8>)>>,
    custom_portrait_textures: HashMap<u32, TextureHandle>,
    pending_portrait_downloads: std::collections::HashSet<u32>,
    player_portrait_ids: HashMap<String, u32>,
    smiley_textures: Vec<Option<TextureHandle>>,
    desctag_textures: Vec<Option<TextureHandle>>,
    chat_img_textures: HashMap<String, Option<TextureHandle>>,
    pending_chat_img_downloads: std::collections::HashSet<String>,
    player_accent_colors: HashMap<String, Color32>,
    current_dream_dir: Option<std::path::PathBuf>,
    current_dream_url: Option<String>,
    world: WorldState,
    pending_vascodagama: bool,
    login_ack_at: Option<Instant>,
    login_trace_remaining: usize,
    pending_dream_modern: Option<bool>,
    pending_dream_name: Option<String>,
    looked_player: Option<LookedPlayerCard>,
    hovered_avatar_uid: Option<u32>,
    context_menu_avatar: Option<ContextMenuTarget>,

    cam_offset: Vec2,
    zoom: f32,

    to_net: Sender<UiToNet>,
    from_net: Receiver<NetToUi>,
    bg_sessions: Vec<BackgroundSession>,

    // Settings, logging, notifications, tray, fonts & importer state
    logging_enabled: bool,
    log_whispers: bool,
    log_retention_days: u32,
    notify_enabled: bool,
    notify_whisper: bool,
    notify_pounce: bool,
    notify_mention: bool,
    notify_only_unfocused: bool,
    sound_enabled: bool,
    music_enabled: bool,
    minimize_to_tray: bool,
    close_to_tray: bool,
    font_family: String,
    font_size: u16,
    fonts_configured: bool,
    applied_font_family: String,
    applied_font_size: u16,
    settings_open: bool,
    log_viewer_open: bool,
    log_viewer_char: String,
    log_viewer_files: Vec<std::path::PathBuf>,
    log_viewer_selected: Option<std::path::PathBuf>,
    log_viewer_content: String,
    smiley_picker_open: bool,
    link_builder_open: bool,
    link_url_input: String,
    link_label_input: String,
    last_visible_avatar_count: usize,
    music_child: Option<std::process::Child>,
    help_open: bool,
    about_open: bool,
    bear_logo: Option<TextureHandle>,
    chat_scroll_delta: f32,
    font_popup_open: bool,
    import_report: Option<String>,
    window_focused: bool,
    posture: u8,
    anim_mode: String,
    last_sent_msg: String,
    last_whisper_target: String,
    last_received_whisper_from: String,
    saved_character: String,
    whisper_open_tab_on_incoming: bool,
    whisper_emote_in_whisper: bool,
    settings_tab: usize,
    dream_bookmarks: Vec<String>,
    limit_view_range: bool,
    allow_dream_skins: bool,
    portrait_size: f32,
    custom_dream_skin: Option<(Color32, Color32, Color32)>,
    check_updates_on_startup: bool,
    first_setup_done: bool,
    first_setup_open: bool,
    update_checking: bool,
    update_status: Option<String>,
    update_available: Option<UpdateReleaseInfo>,
    update_popup_open: bool,
    update_rx: Option<mpsc::Receiver<UpdateCheckMsg>>,
    update_install_state: UpdateInstallState,
    update_install_rx: Option<mpsc::Receiver<UpdateInstallMsg>>,
    cookies_collected: Option<u32>,
    cookies_total: Option<u32>,
    dice_roll: String,
}

impl FurcadiaApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let (to_net, net_rx) = mpsc::channel::<UiToNet>();
        let (net_tx, from_net) = mpsc::channel::<NetToUi>();
        let ctx = cc.egui_ctx.clone();
        let net_ctx = ctx.clone();

        thread::spawn(move || network_worker(net_rx, net_tx, net_ctx));

        let cfg = ClientConfig::load_default();
        let assets = AssetLocator::discover();
        let discovered_import_dirs = discover_default_windows_furcadia_dirs();
        let import_source_detected = !discovered_import_dirs.is_empty();
        let import_path_input = discovered_import_dirs
            .first()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "Furcadia Windows/program Files (x86)/Furcadia".to_owned());
        let palette = assets.load_default_palette().unwrap_or_default();
        let floors_cache =
            FoxSpriteCache::load_with_legacy(&assets, "floors.fox", Some("floors.fox"), "floor");
        let floors2_cache = FoxSpriteCache::load_legacy_only(&assets, "floors2.fox", "floors2");
        let floore_cache =
            FoxSpriteCache::load_with_legacy(&assets, "floore.fox", Some("floore.fox"), "floore");
        let items_cache =
            FoxSpriteCache::load_with_legacy(&assets, "items.fox", Some("item.fox"), "item");
        let iteme_cache =
            FoxSpriteCache::load_with_legacy(&assets, "iteme.fox", Some("iteme.fox"), "iteme");
        let portals_cache = FoxSpriteCache::load(&assets, "portals.fox", "portal");
        let portalse_cache = FoxSpriteCache::load(&assets, "portalse.fox", "portalse");
        let effects_cache = FoxSpriteCache::load_with_legacy(
            &assets,
            "effects.fox",
            Some("effects.fox"),
            "effect",
        );
        let effectse_cache = FoxSpriteCache::load_with_legacy(
            &assets,
            "effectse.fox",
            Some("effectse.fox"),
            "effectse",
        );
        let lighting_cache = FoxSpriteCache::load(&assets, "lighting.fox", "lighting");
        let lightinge_cache = FoxSpriteCache::load(&assets, "lightinge.fox", "lightinge");
        let ambience_cache = FoxSpriteCache::load(&assets, "ambience.fox", "ambience");
        let ambiencee_cache = FoxSpriteCache::load(&assets, "ambiencee.fox", "ambiencee");
        let junction_cache = FoxSpriteCache::load_with_legacy(
            &assets,
            "junction.fox",
            Some("junction.fsh"),
            "junction",
        );
        let mut legacy_item_splits = Vec::new();
        for (fname, min_id, max_id, offset) in [
            ("item2.fox", 305u16, 999u16, 305u16),
            ("item3.fox", 1000, 1348, 1000),
            ("item4.fox", 1349, 1748, 1349),
        ] {
            let cache = FoxSpriteCache::load_legacy_only(&assets, fname, fname);
            if cache.is_loaded() {
                legacy_item_splits.push((min_id, max_id, offset, cache));
            }
        }
        let mut wall_caches = HashMap::new();
        wall_caches.insert(
            0,
            FoxSpriteCache::load_with_legacy(&assets, "wall.fox", Some("wall.fox"), "wall_0"),
        );
        for g in 1..=20 {
            let fname = format!("wall_{g}.fox");
            if assets.default_patch_path(&fname).is_some()
                || assets.legacy_patch_path(&fname).is_some()
            {
                wall_caches.insert(
                    g,
                    FoxSpriteCache::load_with_legacy(
                        &assets,
                        &fname,
                        Some(&fname),
                        &format!("wall_{g}"),
                    ),
                );
            }
        }
        let mut smiley_textures: Vec<Option<TextureHandle>> = Vec::with_capacity(54);
        for (fsh_name, slot_prefix) in [("smileys.fsh", "sm0"), ("smileys2.fsh", "sm1")] {
            if let Some(raw) = assets
                .default_patch_path(fsh_name)
                .and_then(|p| std::fs::read(p).ok())
            {
                for (i, sp) in decode_fsh_sprites(&raw, &palette).into_iter().enumerate() {
                    if sp.width == 0 || sp.height == 0 {
                        smiley_textures.push(None);
                        continue;
                    }
                    let img = ColorImage::from_rgba_unmultiplied(
                        [sp.width as usize, sp.height as usize],
                        &sp.rgba,
                    );
                    let tex = ctx.load_texture(
                        format!("{slot_prefix}_{i}"),
                        img,
                        TextureOptions::NEAREST,
                    );
                    smiley_textures.push(Some(tex));
                }
            }
        }

        let mut desctag_textures: Vec<Option<TextureHandle>> = Vec::new();
        if let Some(p) = assets.default_patch_path("desctags.fox") {
            if let Ok(mut archive) = FoxArchive::load(&p) {
                for idx in 0..archive.images.len() {
                    if let Ok(sp) = archive.decode_image(idx, &palette) {
                        if sp.width > 0 && sp.height > 0 {
                            let img = ColorImage::from_rgba_unmultiplied(
                                [sp.width as usize, sp.height as usize],
                                &sp.rgba,
                            );
                            let tex = ctx.load_texture(
                                format!("desctag_{idx}"),
                                img,
                                TextureOptions::NEAREST,
                            );
                            desctag_textures.push(Some(tex));
                            continue;
                        }
                    }
                    desctag_textures.push(None);
                }
            }
        }

        let mut pounce = PounceMonitor::default();
        for w in &cfg.watchlist {
            pounce.add_watch(w);
        }

        let world = WorldState::default();

        let mut auth_busy = false;
        if cfg.auto_login && !cfg.email.is_empty() && !cfg.password.is_empty() {
            auth_busy = true;
            let _ = to_net.send(UiToNet::AuthenticateAccount {
                email: cfg.email.clone(),
                password: cfg.password.clone(),
            });
        }

        if cfg.log_retention_days > 0 {
            if let Some(dir) = default_logs_dir() {
                let _ = prune_old_logs(&dir, cfg.log_retention_days);
            }
        }

        let mut app = Self {
            screen: AppScreen::Login,
            email: cfg.email,
            password: cfg.password,
            remember_email: cfg.remember_email,
            remember_password: cfg.remember_password,
            auto_login: cfg.auto_login,
            auth_busy,
            auth_error: None,
            account_info: None,
            selected_char_idx: 0,
            char_sort: CharSort::Recent,
            editing_char: false,
            edit_desc_buf: String::new(),
            pending_desc: None,
            news: Vec::new(),
            char_portraits: HashMap::new(),
            status_text: "Disconnected".to_owned(),
            is_connected: false,
            connect_seq: 0,
            connecting_stage: String::new(),
            connecting_error: None,
            connected_at: None,
            chat_input: String::new(),
            spell_seq: 0,
            spell_dirty_at: None,
            spell_misspelled: Vec::new(),
            custom_dict_words: load_custom_dictionary(),
            chat_log: vec![parse_chat_line(
                "<font color='success'>Welcome to the Native Linux Furcadia Client.</font>",
            )],
            main_chat_unread: 0,
            whispers: Vec::new(),
            active_chat_tab: None,
            focus_chat_input: false,
            dragging_whisper_tab: None,
            new_whisper_input: String::new(),
            active_char_tab_pos: 0,
            dragging_char_tab: None,
            sidebar_tab: SidebarTab::Move,
            theme: match cfg.theme.as_str() {
                "DragonHoard" => DreamTheme::DragonHoard,
                "KitterMagic" => DreamTheme::KitterMagic,
                "FerianWylde" => DreamTheme::FerianWylde,
                _ => DreamTheme::ClassicPurple,
            },
            dream_url_input: "furc://".to_owned(),
            target_player_input: String::new(),
            live_desc_input: String::new(),
            afk_active: false,
            show_debug_packets: cfg.show_debug_packets,
            show_avatar_names: cfg.show_avatar_names,
            import_path_input,
            import_source_detected,
            watch_input: String::new(),
            pounce,
            pounce_visible: cfg.pounce_visible,
            pounce_detached: cfg.pounce_detached,
            pounce_show_offline: cfg.pounce_show_offline,
            assets,
            palette,
            floors_cache,
            floors2_cache,
            floore_cache,
            items_cache,
            iteme_cache,
            portals_cache,
            portalse_cache,
            effects_cache,
            effectse_cache,
            lighting_cache,
            lightinge_cache,
            ambience_cache,
            ambiencee_cache,
            dream_item_splits: Vec::new(),
            legacy_item_splits,
            junction_cache,
            wall_caches,
            species_caches: HashMap::new(),
            pending_species_downloads: std::collections::HashSet::new(),
            dynamic_avatars: HashMap::from([(164u16, (29usize, 0u16))]),
            dynamic_avatar_server: None,
            custom_portrait_rgba: HashMap::new(),
            custom_portrait_textures: HashMap::new(),
            pending_portrait_downloads: std::collections::HashSet::new(),
            player_portrait_ids: HashMap::new(),
            smiley_textures,
            desctag_textures,
            chat_img_textures: HashMap::new(),
            pending_chat_img_downloads: std::collections::HashSet::new(),
            player_accent_colors: HashMap::new(),
            current_dream_dir: None,
            current_dream_url: None,
            world,
            pending_vascodagama: false,
            login_ack_at: None,
            login_trace_remaining: 0,
            pending_dream_modern: None,
            pending_dream_name: None,
            looked_player: None,
            hovered_avatar_uid: None,
            context_menu_avatar: None,
            cam_offset: Vec2::ZERO,
            zoom: 1.25,
            to_net,
            from_net,
            bg_sessions: Vec::new(),
            logging_enabled: cfg.logging_enabled,
            log_whispers: cfg.log_whispers,
            log_retention_days: cfg.log_retention_days,
            notify_enabled: cfg.notify_enabled,
            notify_whisper: cfg.notify_whisper,
            notify_pounce: cfg.notify_pounce,
            notify_mention: cfg.notify_mention,
            notify_only_unfocused: cfg.notify_only_unfocused,
            sound_enabled: cfg.sound_enabled,
            music_enabled: cfg.music_enabled,
            minimize_to_tray: cfg.minimize_to_tray,
            close_to_tray: cfg.close_to_tray,
            font_family: cfg.font_family.clone(),
            font_size: cfg.font_size,
            fonts_configured: false,
            applied_font_family: String::new(),
            applied_font_size: 0,
            settings_open: false,
            log_viewer_open: false,
            log_viewer_char: String::new(),
            log_viewer_files: Vec::new(),
            log_viewer_selected: None,
            log_viewer_content: String::new(),
            smiley_picker_open: false,
            link_builder_open: false,
            link_url_input: String::new(),
            link_label_input: String::new(),
            last_visible_avatar_count: 0,
            music_child: None,
            help_open: false,
            about_open: false,
            bear_logo: None,
            chat_scroll_delta: 0.0,
            font_popup_open: false,
            import_report: None,
            window_focused: true,
            posture: 0,
            anim_mode: cfg.anim_mode.clone(),
            last_sent_msg: String::new(),
            last_whisper_target: String::new(),
            last_received_whisper_from: String::new(),
            saved_character: cfg.character.clone(),
            whisper_open_tab_on_incoming: cfg.whisper_open_tab_on_incoming,
            whisper_emote_in_whisper: cfg.whisper_emote_in_whisper,
            settings_tab: 0,
            dream_bookmarks: cfg.dream_bookmarks.clone(),
            limit_view_range: cfg.limit_view_range,
            allow_dream_skins: cfg.allow_dream_skins,
            portrait_size: cfg.portrait_size as f32,
            custom_dream_skin: None,
            check_updates_on_startup: cfg.check_updates_on_startup,
            first_setup_done: cfg.first_setup_done,
            first_setup_open: !cfg.first_setup_done,
            update_checking: false,
            update_status: None,
            update_available: None,
            update_popup_open: false,
            update_rx: None,
            update_install_state: UpdateInstallState::Idle,
            update_install_rx: None,
            cookies_collected: None,
            cookies_total: None,
            dice_roll: cfg.dice_roll.clone(),
        };
        app.apply_font_settings(&ctx);
        if app.first_setup_done && app.check_updates_on_startup {
            app.trigger_update_check(&ctx);
        }
        app
    }

    fn selected_character(&self) -> Option<&CharacterSummary> {
        let info = self.account_info.as_ref()?;
        info.characters.get(self.selected_char_idx)
    }

    fn save_config(&self) {
        let character = self
            .selected_character()
            .map(|c| c.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| self.saved_character.clone());
        let theme_name = match self.theme {
            DreamTheme::ClassicPurple => "ClassicPurple",
            DreamTheme::DragonHoard => "DragonHoard",
            DreamTheme::KitterMagic => "KitterMagic",
            DreamTheme::FerianWylde => "FerianWylde",
        };
        let cfg = ClientConfig {
            email: self.email.clone(),
            character,
            password: self.password.clone(),
            remember_email: self.remember_email,
            remember_password: self.remember_password,
            auto_login: self.auto_login,
            watchlist: self.pounce.entries().map(|e| e.name.clone()).collect(),
            logging_enabled: self.logging_enabled,
            log_whispers: self.log_whispers,
            log_retention_days: self.log_retention_days,
            notify_enabled: self.notify_enabled,
            notify_whisper: self.notify_whisper,
            notify_pounce: self.notify_pounce,
            notify_mention: self.notify_mention,
            notify_only_unfocused: self.notify_only_unfocused,
            sound_enabled: self.sound_enabled,
            music_enabled: self.music_enabled,
            minimize_to_tray: self.minimize_to_tray,
            close_to_tray: self.close_to_tray,
            font_family: self.font_family.clone(),
            font_size: self.font_size,
            anim_mode: self.anim_mode.clone(),
            theme: theme_name.to_owned(),
            show_avatar_names: self.show_avatar_names,
            pounce_visible: self.pounce_visible,
            pounce_detached: self.pounce_detached,
            pounce_show_offline: self.pounce_show_offline,
            show_debug_packets: self.show_debug_packets,
            whisper_open_tab_on_incoming: self.whisper_open_tab_on_incoming,
            whisper_emote_in_whisper: self.whisper_emote_in_whisper,
            dream_bookmarks: self.dream_bookmarks.clone(),
            limit_view_range: self.limit_view_range,
            allow_dream_skins: self.allow_dream_skins,
            portrait_size: self.portrait_size.round().clamp(48.0, 160.0) as u16,
            check_updates_on_startup: self.check_updates_on_startup,
            first_setup_done: self.first_setup_done,
            dice_roll: if self.dice_roll.trim().is_empty() {
                "1d6".to_owned()
            } else {
                self.dice_roll.trim().to_owned()
            },
        };
        let _ = cfg.save_default();
    }

    fn find_extra_asset(&self, rel: &str) -> Option<std::path::PathBuf> {
        let mut candidates = Vec::new();
        if let Some(dir) = AssetLocator::user_assets_dir() {
            candidates.push(dir.join(rel));
            if let Some(parent) = dir.parent() {
                candidates.push(parent.join(rel));
            }
        }
        if let Some(root) = &self.assets.root {
            candidates.push(root.join(rel));
        }
        candidates.push(std::path::PathBuf::from("Furcadia Windows/program Files (x86)/Furcadia").join(rel));
        candidates.push(std::path::PathBuf::from(self.import_path_input.trim()).join(rel));
        candidates.into_iter().find(|p| p.is_file())
    }

    fn apply_font_settings(&mut self, ctx: &egui::Context) {
        if !self.fonts_configured {
            self.fonts_configured = true;
            if let Some(ttf_path) = self.find_extra_asset("fsex2p00_public.ttf") {
                if let Ok(bytes) = std::fs::read(&ttf_path) {
                    let mut defs = egui::FontDefinitions::default();
                    defs.font_data
                        .insert("fixedsys".to_owned(), egui::FontData::from_owned(bytes));
                    defs.families.insert(
                        egui::FontFamily::Name("fixedsys".into()),
                        vec!["fixedsys".to_owned()],
                    );
                    ctx.set_fonts(defs);
                }
            }
        }
        if self.applied_font_family == self.font_family && self.applied_font_size == self.font_size {
            return;
        }
        self.applied_font_family = self.font_family.clone();
        self.applied_font_size = self.font_size;

        let family = match self.font_family.as_str() {
            "Monospace" => egui::FontFamily::Monospace,
            "Fixedsys" => {
                if self.find_extra_asset("fsex2p00_public.ttf").is_some() {
                    egui::FontFamily::Name("fixedsys".into())
                } else {
                    egui::FontFamily::Monospace
                }
            }
            _ => egui::FontFamily::Proportional,
        };
        let sz = (self.font_size as f32).clamp(9.0, 28.0);
        ctx.style_mut(|style| {
            style.text_styles.insert(
                egui::TextStyle::Body,
                FontId::new(sz, family.clone()),
            );
            style.text_styles.insert(
                egui::TextStyle::Button,
                FontId::new((sz - 0.5).max(9.0), family.clone()),
            );
            style.text_styles.insert(
                egui::TextStyle::Small,
                FontId::new((sz - 2.0).max(8.5), family.clone()),
            );
            style.text_styles.insert(
                egui::TextStyle::Monospace,
                FontId::new(sz, egui::FontFamily::Monospace),
            );
        });
    }

    fn play_sound(&self, wav_name: &str) {
        if !self.sound_enabled {
            return;
        }
        let rel = format!("online/{wav_name}");
        let path_opt = self
            .current_dream_dir
            .as_deref()
            .and_then(|d| Self::find_file_case_insensitive(d, wav_name))
            .or_else(|| self.find_extra_asset(&rel))
            .or_else(|| self.find_extra_asset(wav_name));
        let Some(path) = path_opt else {
            return;
        };
        thread::spawn(move || {
            #[cfg(target_os = "windows")]
            {
                let script = format!(
                    "(New-Object System.Media.SoundPlayer '{}').PlaySync()",
                    path.display().to_string().replace('\'', "''")
                );
                let _ = furcadia_client::sys::command("powershell")
                    .args(["-NoProfile", "-NonInteractive", "-Command", &script])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
            }
            #[cfg(not(target_os = "windows"))]
            for (bin, args) in [
                ("pw-play", Vec::<&str>::new()),
                ("paplay", vec![]),
                ("aplay", vec![]),
                ("ffplay", vec!["-nodisp", "-autoexit", "-loglevel", "quiet"]),
                ("mpv", vec!["--no-video", "--really-quiet"]),
            ] {
                if std::process::Command::new(bin)
                    .args(&args)
                    .arg(&path)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .is_ok_and(|s| s.success())
                {
                    break;
                }
            }
        });
    }

    fn stop_music(&mut self) {
        if let Some(mut child) = self.music_child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn play_dream_music(&mut self, dir: &std::path::Path) {
        self.stop_music();
        if !self.music_enabled {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut music_file: Option<(std::path::PathBuf, bool)> = None;
        for ent in entries.flatten() {
            let p = ent.path();
            if !p.is_file() {
                continue;
            }
            let ext = p
                .extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_default();
            if matches!(ext.as_str(), "mid" | "midi") {
                music_file = Some((p, true));
                break;
            } else if matches!(ext.as_str(), "ogg" | "mp3" | "wav") {
                music_file = Some((p, false));
                break;
            }
        }
        let Some((path, is_midi)) = music_file else {
            return;
        };
        let candidates: Vec<(&str, Vec<&str>)> = if is_midi {
            vec![
                ("timidity", vec!["-id", "-Ol"]),
                ("fluidsynth", vec!["-i", "-n", "-a", "pulseaudio"]),
            ]
        } else {
            vec![
                ("ffplay", vec!["-nodisp", "-autoexit", "-loop", "0", "-loglevel", "quiet"]),
                ("mpv", vec!["--no-video", "--loop=inf", "--really-quiet"]),
                ("pw-play", vec![]),
            ]
        };
        for (bin, args) in candidates {
            if let Ok(child) = std::process::Command::new(bin)
                .args(&args)
                .arg(&path)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                self.music_child = Some(child);
                break;
            }
        }
    }

    fn effective_colors(&self) -> (Color32, Color32, Color32) {
        if self.allow_dream_skins {
            if let Some((bg, panel, accent)) = self.custom_dream_skin {
                return (bg, panel, accent);
            }
        }
        (self.theme.bg(), self.theme.panel(), self.theme.accent())
    }

    fn parse_skin_color(raw: &str) -> Option<Color32> {
        let s = raw.trim().trim_start_matches('#');
        if s.len() == 6 && s.chars().all(|c| c.is_ascii_hexdigit()) {
            let r = u8::from_str_radix(&s[0..2], 16).ok()?;
            let g = u8::from_str_radix(&s[2..4], 16).ok()?;
            let b = u8::from_str_radix(&s[4..6], 16).ok()?;
            return Some(Color32::from_rgb(r, g, b));
        }
        let parts: Vec<u8> = s
            .split([',', ' '])
            .filter(|p| !p.is_empty())
            .filter_map(|p| p.parse::<u8>().ok())
            .collect();
        if parts.len() >= 3 {
            return Some(Color32::from_rgb(parts[0], parts[1], parts[2]));
        }
        None
    }

    fn parse_dream_skin_ini(dir: &std::path::Path) -> Option<(Color32, Color32, Color32)> {
        let path = Self::find_file_case_insensitive(dir, "skin.ini")
            .or_else(|| Self::find_file_case_insensitive(dir, "furcadia.ini"))?;
        let text = std::fs::read_to_string(path).ok()?;
        let mut bg = None;
        let mut panel = None;
        let mut accent = None;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with([';', '#', '[']) {
                continue;
            }
            if let Some((k, v)) = trimmed.split_once('=') {
                let key = k.trim().to_ascii_lowercase();
                if let Some(c) = Self::parse_skin_color(v) {
                    if key == "bg" || key == "background" || key == "bgcolor" {
                        bg = Some(c);
                    } else if key == "panel" || key == "panelcolor" || key == "frame" {
                        panel = Some(c);
                    } else if key == "accent" || key == "border" || key == "highlight" {
                        accent = Some(c);
                    }
                }
            }
        }
        if bg.is_some() || panel.is_some() || accent.is_some() {
            Some((
                bg.unwrap_or(Color32::from_rgb(24, 18, 42)),
                panel.unwrap_or(Color32::from_rgb(36, 26, 62)),
                accent.unwrap_or(GOLD),
            ))
        } else {
            None
        }
    }

    fn send_desktop_notification(&self, title: &str, body: &str) {
        if !self.notify_enabled || (self.notify_only_unfocused && self.window_focused) {
            return;
        }
        let title = title.to_owned();
        let body = body.to_owned();
        thread::spawn(move || {
            #[cfg(target_os = "windows")]
            {
                let esc = |s: &str| s.replace('\'', "''");
                let script = format!(
                    "Add-Type -AssemblyName System.Windows.Forms; Add-Type -AssemblyName System.Drawing; \
                     $n = New-Object System.Windows.Forms.NotifyIcon; \
                     $n.Icon = [System.Drawing.SystemIcons]::Information; $n.Visible = $true; \
                     $n.ShowBalloonTip(5000, '{}', '{}', [System.Windows.Forms.ToolTipIcon]::None); \
                     Start-Sleep -Seconds 6; $n.Dispose()",
                    esc(&title),
                    esc(&body)
                );
                let _ = furcadia_client::sys::command("powershell")
                    .args(["-NoProfile", "-NonInteractive", "-Command", &script])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
            }
            #[cfg(not(target_os = "windows"))]
            let _ = std::process::Command::new("notify-send")
                .args(["-a", "Furcadia", &title, &body])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        });
    }

    fn active_character_name(&self) -> String {
        self.world
            .self_name
            .as_deref()
            .or_else(|| self.selected_character().map(|c| c.name.as_str()))
            .or_else(|| {
                (!self.saved_character.trim().is_empty()).then_some(self.saved_character.as_str())
            })
            .unwrap_or("Unknown")
            .to_owned()
    }

    fn log_chat_line(&self, text: &str) {
        if !self.logging_enabled {
            return;
        }
        if let Some(dir) = default_logs_dir() {
            let _ = append_chat_log(&dir, &self.active_character_name(), text);
        }
    }

    fn log_whisper_line(&self, peer: &str, text: &str) {
        if !self.logging_enabled || !self.log_whispers {
            return;
        }
        if let Some(dir) = default_logs_dir() {
            let _ = append_whisper_log(&dir, &self.active_character_name(), peer, text);
        }
    }

    fn launch_new_window() {
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::process::Command::new(exe)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
    }

    fn run_windows_import(&mut self) {
        let src = std::path::PathBuf::from(self.import_path_input.trim());
        let existing: Vec<String> = self.pounce.entries().map(|e| e.name.clone()).collect();
        let summary = import_windows_furcadia_dir(&src, &existing, &self.active_character_name());
        if summary.assets_copied > 0 {
            self.assets = AssetLocator::discover();
            self.fonts_configured = false;
        }
        for name in &summary.pounce_added {
            let wire = wire_player_name(name);
            self.pounce.add_watch(name.clone());
            if self.is_connected {
                self.send_raw_cmd(&format!("pounce adduser {wire}"));
            }
        }
        if !summary.pounce_added.is_empty() {
            self.save_config();
        }
        let mut msg = format!(
            "Imported {} asset(s), {} Pounce user(s), {} character INI(s), {} log file(s).",
            summary.assets_copied,
            summary.pounce_added.len(),
            summary.characters_found.len(),
            summary.logs_copied
        );
        if !summary.warnings.is_empty() {
            msg.push_str("\nWarnings: ");
            msg.push_str(&summary.warnings.join("; "));
        }
        self.import_report = Some(msg.clone());
        self.push_system_line("success", &msg);
    }

    fn browse_import_directory(&mut self) {
        #[cfg(target_os = "windows")]
        let out = furcadia_client::sys::command("powershell")
            .args([
                "-NoProfile",
                "-STA",
                "-Command",
                "Add-Type -AssemblyName System.Windows.Forms; \
                 $d = New-Object System.Windows.Forms.FolderBrowserDialog; \
                 $d.Description = 'Select Furcadia Folder'; \
                 if ($d.ShowDialog() -eq 'OK') { Write-Output $d.SelectedPath }",
            ])
            .output();
        #[cfg(not(target_os = "windows"))]
        let out = furcadia_client::sys::command("zenity")
            .args(["--file-selection", "--directory", "--title=Select Furcadia Windows Folder"])
            .output()
            .or_else(|_| {
                furcadia_client::sys::command("kdialog")
                    .args(["--getexistingdirectory", "."])
                    .output()
            });
        if let Ok(output) = out {
            if output.status.success() {
                let picked = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                if !picked.is_empty() {
                    self.import_path_input = picked;
                }
            }
        }
    }

    fn open_log_viewer(&mut self) {
        self.log_viewer_open = true;
        if self.log_viewer_char.is_empty() {
            self.log_viewer_char = self.active_character_name();
        }
        self.refresh_log_viewer_files();
    }

    fn refresh_log_viewer_files(&mut self) {
        self.log_viewer_files.clear();
        if let Some(dir) = default_logs_dir() {
            let chars = list_log_characters(&dir);
            if !chars.is_empty() && !chars.iter().any(|c| c == &self.log_viewer_char) {
                self.log_viewer_char = chars[0].clone();
            }
            if !self.log_viewer_char.is_empty() {
                self.log_viewer_files = list_log_files(&dir, &self.log_viewer_char);
            }
        }
        if let Some(first) = self.log_viewer_files.first().cloned() {
            self.load_log_file_into_viewer(first);
        } else {
            self.log_viewer_selected = None;
            self.log_viewer_content.clear();
        }
    }

    fn load_log_file_into_viewer(&mut self, path: std::path::PathBuf) {
        self.log_viewer_content = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| format!("Failed to read {}: {e}", path.display()));
        self.log_viewer_selected = Some(path);
    }

    fn suspend_active_session_to_bg(&mut self, ctx: &egui::Context) {
        let (new_to_net, net_rx) = mpsc::channel::<UiToNet>();
        let (net_tx, new_from_net) = mpsc::channel::<NetToUi>();
        let net_ctx = ctx.clone();
        thread::spawn(move || network_worker(net_rx, net_tx, net_ctx));

        let old_to_net = std::mem::replace(&mut self.to_net, new_to_net);
        let old_from_net = std::mem::replace(&mut self.from_net, new_from_net);
        let old_world = std::mem::take(&mut self.world);
        let old_chat = std::mem::take(&mut self.chat_log);
        let old_whispers = std::mem::take(&mut self.whispers);
        let old_tab = self.active_chat_tab.take();
        let old_dream_dir = self.current_dream_dir.take();
        let old_status = std::mem::replace(&mut self.status_text, "Disconnected".to_owned());
        let old_connected = std::mem::replace(&mut self.is_connected, false);
        let old_cam = std::mem::replace(&mut self.cam_offset, Vec2::ZERO);
        let char_name = old_world
            .self_name
            .clone()
            .or_else(|| self.selected_character().map(|c| c.name.clone()))
            .unwrap_or_else(|| "Character".to_owned());

        let insert_at = self.active_char_tab_pos.min(self.bg_sessions.len());
        self.bg_sessions.insert(
            insert_at,
            BackgroundSession {
                char_idx: self.selected_char_idx,
                char_name,
                is_connected: old_connected,
                status_text: old_status,
                world: old_world,
                chat_log: old_chat,
                whispers: old_whispers,
                active_chat_tab: old_tab,
                current_dream_dir: old_dream_dir,
                cam_offset: old_cam,
                unread_count: 0,
                to_net: old_to_net,
                from_net: old_from_net,
            },
        );
    }

    fn swap_char_tabs(&mut self, a: usize, b: usize) {
        let total = self.bg_sessions.len() + 1;
        if a == b || a >= total || b >= total {
            return;
        }
        let pos = self.active_char_tab_pos.min(self.bg_sessions.len());
        if a == pos {
            if a < b {
                for step in a..b {
                    let bg_idx = step;
                    if bg_idx < self.bg_sessions.len() && step + 1 != pos {
                        // Moving active right past bg[step] keeps relative order of bg_sessions.
                        let _ = bg_idx;
                    }
                }
            }
            // Move active from `pos` to `b`: remove nothing, shift bg elements between `pos` and `b`
            // Actually, if `pos == 0, bg=[B, C]` and `a=0, b=1`: `B` moves to 0, `active` moves to 1 -> `bg=[B, C]`, `pos=1`!
            // If `pos == 0, bg=[B, C]` and `a=0, b=2`: `B`->0, `C`->1, `active`->2 -> `bg=[B, C]`, `pos=2`!
            // Only `active_char_tab_pos = b` is needed when moving `active`!
            // Wait: if we SWAP `a` and `b` (not slide): `[A, B, C]` swap(0, 2) -> `[C, B, A]`, which means `C` moves to 0 and `A` moves to 2!
            let bg_b = if b < pos { b } else { b - 1 };
            let target = self.bg_sessions.remove(bg_b);
            let insert_at = if b < pos { pos - 1 } else { pos };
            self.bg_sessions.insert(insert_at.min(self.bg_sessions.len()), target);
            self.active_char_tab_pos = b;
        } else if b == pos {
            self.swap_char_tabs(b, a);
        } else {
            let bg_a = if a < pos { a } else { a - 1 };
            let bg_b = if b < pos { b } else { b - 1 };
            self.bg_sessions.swap(bg_a, bg_b);
        }
    }

    fn switch_to_bg_session(&mut self, idx: usize, ctx: &egui::Context) {
        if idx >= self.bg_sessions.len() {
            return;
        }
        let old_pos = self.active_char_tab_pos.min(self.bg_sessions.len());
        let target_vis_pos = if idx < old_pos { idx } else { idx + 1 };
        let target = self.bg_sessions.remove(idx);
        if self.is_connected {
            let insert_idx = if idx < old_pos {
                old_pos.saturating_sub(1)
            } else {
                old_pos
            };
            self.active_char_tab_pos = insert_idx;
            self.suspend_active_session_to_bg(ctx);
            self.active_char_tab_pos = target_vis_pos;
        } else {
            self.active_char_tab_pos = idx.min(self.bg_sessions.len());
        }
        self.selected_char_idx = target.char_idx;
        self.is_connected = target.is_connected;
        self.status_text = target.status_text;
        self.world = target.world;
        self.chat_log = target.chat_log;
        self.whispers = target.whispers;
        self.active_chat_tab = target.active_chat_tab;
        self.cookies_collected = None;
        self.cookies_total = None;
        if self.current_dream_dir != target.current_dream_dir {
            self.clear_dream_patches();
            if let Some(dir) = target.current_dream_dir {
                self.load_dream_dir(&dir);
            }
        }
        self.cam_offset = target.cam_offset;
        self.to_net = target.to_net;
        self.from_net = target.from_net;
        self.looked_player = None;
        self.context_menu_avatar = None;
        self.screen = AppScreen::InGame;
    }

    fn poll_bg_sessions(&mut self) {
        let mut events_for_notify: Vec<(String, String, bool)> = Vec::new();
        for sess in &mut self.bg_sessions {
            while let Ok(ev) = sess.from_net.try_recv() {
                match ev {
                    NetToUi::Connected { connected, .. } => {
                        sess.is_connected = connected;
                        sess.status_text = if connected {
                            "Connected".to_owned()
                        } else {
                            "Disconnected".to_owned()
                        };
                    }
                    NetToUi::ConnectFailed { error, .. } => {
                        sess.is_connected = false;
                        sess.status_text = format!("Disconnected: {error}");
                    }
                    NetToUi::Message(ServerMessage::Text(t)) => {
                        let line = parse_chat_line(&t);
                        let plain = line.to_plain_text();
                        if let Some(peer) = extract_whisper_peer(&plain) {
                            sess.unread_count += 1;
                            let formatted =
                                format_whisper_tab_line(&sess.char_name, &peer, &plain, &line);
                            if let Some(w) = sess
                                .whispers
                                .iter_mut()
                                .find(|w| w.peer.eq_ignore_ascii_case(&peer))
                            {
                                w.unread += 1;
                                w.lines.push(formatted);
                            } else {
                                let mut w = WhisperSession::new(peer.clone());
                                w.unread = 1;
                                w.lines.push(formatted);
                                sess.whispers.push(w);
                            }
                            events_for_notify.push((
                                format!("[{}] Whisper from {peer}", sess.char_name),
                                plain.clone(),
                                true,
                            ));
                        } else {
                            sess.chat_log.push(line);
                        }
                    }
                    NetToUi::Message(ServerMessage::WorldPacket { opcode, data }) => {
                        sess.world.apply_world_packet(opcode, &data);
                    }
                    NetToUi::Message(ServerMessage::RegionDefaults(data)) => {
                        sess.world.apply_region_defaults(&data);
                    }
                    NetToUi::Message(ServerMessage::CharacterIdentity { uid, name }) => {
                        sess.world.self_uid = uid.parse().ok();
                        sess.world.self_name = Some(name.clone());
                        sess.char_name = name;
                    }
                    _ => {}
                }
            }
        }
        for (title, body, is_whisper) in events_for_notify {
            if is_whisper && self.notify_whisper {
                self.play_sound("whisper.wav");
                self.send_desktop_notification(&title, &body);
            }
        }
    }

    fn trigger_login(&mut self) {
        if self.email.trim().is_empty() || self.password.is_empty() {
            self.auth_error = Some("Please enter your email and password.".into());
            return;
        }
        self.save_config();
        self.auth_busy = true;
        self.auth_error = None;
        let _ = self.to_net.send(UiToNet::AuthenticateAccount {
            email: self.email.trim().to_owned(),
            password: self.password.clone(),
        });
    }

    fn trigger_play(&mut self) {
        let Some(ch) = self.selected_character().cloned() else {
            return;
        };
        self.saved_character = ch.name.clone();
        self.save_config();
        self.world = WorldState::default();
        self.clear_dream_patches();
        self.pending_vascodagama = false;
        self.login_ack_at = None;
        self.login_trace_remaining = 0;
        self.pending_dream_modern = None;
        self.looked_player = None;
        self.hovered_avatar_uid = None;
        self.context_menu_avatar = None;
        self.cam_offset = Vec2::ZERO;
        self.live_desc_input = unescape_html(&ch.desc);
        self.chat_log.clear();
        self.is_connected = false;
        self.connected_at = None;
        self.connect_seq = self.connect_seq.wrapping_add(1);
        self.connecting_error = None;
        self.connecting_stage = format!("Connecting as {}...", ch.name);
        self.push_system_line("emit", &format!("Connecting as {}...", ch.name));
        self.screen = AppScreen::Connecting;
        let login_desc = self
            .pending_desc
            .take()
            .or_else(|| {
                let d = self.live_desc_input.trim();
                (!d.is_empty()).then(|| d.to_owned())
            })
            .map(|d| d.replace(['\r', '\n'], " "));
        let _ = self.to_net.send(UiToNet::ConnectCharacter {
            seq: self.connect_seq,
            email: self.email.trim().to_owned(),
            character: ch.name,
            password: self.password.clone(),
            updated_desc: login_desc,
            colors: (!ch.colors.is_empty()).then_some(ch.colors),
        });
    }

    fn push_system_line(&mut self, color: &str, text: &str) {
        self.chat_log
            .push(parse_chat_line(&format!("<font color='{color}'>{text}</font>")));
    }

    fn sanitize_dream_id(name: &str) -> String {
        let base = name.trim().trim_end_matches(".map");
        base.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect()
    }

    fn clear_dream_patches(&mut self) {
        self.floors_cache.clear_dream_patch();
        self.floors2_cache.clear_dream_patch();
        self.floore_cache.clear_dream_patch();
        self.items_cache.clear_dream_patch();
        self.iteme_cache.clear_dream_patch();
        self.portals_cache.clear_dream_patch();
        self.portalse_cache.clear_dream_patch();
        self.effects_cache.clear_dream_patch();
        self.effectse_cache.clear_dream_patch();
        self.lighting_cache.clear_dream_patch();
        self.lightinge_cache.clear_dream_patch();
        self.ambience_cache.clear_dream_patch();
        self.ambiencee_cache.clear_dream_patch();
        self.dream_item_splits.clear();
        self.junction_cache.clear_dream_patch();
        for cache in self.wall_caches.values_mut() {
            cache.clear_dream_patch();
        }
        self.species_caches.clear();
        self.char_portraits.clear();
        self.current_dream_dir = None;
        self.custom_dream_skin = None;
        self.stop_music();
    }

    fn bookmark_dream(&mut self, raw_url: &str) {
        let trimmed = raw_url.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("furc://") {
            return;
        }
        let normalized = if trimmed.starts_with("furc://") {
            trimmed.to_owned()
        } else {
            format!("furc://{}", trimmed.trim_end_matches(".map"))
        };
        if !self
            .dream_bookmarks
            .iter()
            .any(|d| d.eq_ignore_ascii_case(&normalized))
        {
            self.dream_bookmarks.push(normalized);
            self.save_config();
        }
    }

    fn find_file_case_insensitive(dir: &std::path::Path, target: &str) -> Option<std::path::PathBuf> {
        let exact = dir.join(target);
        if exact.is_file() {
            return Some(exact);
        }
        let entries: Vec<_> = std::fs::read_dir(dir).ok()?.flatten().collect();
        for entry in &entries {
            let path = entry.path();
            if path.is_file()
                && path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.eq_ignore_ascii_case(target))
            {
                return Some(path);
            }
        }
        // Check one subdirectory level in case an archive wrapped files inside a folder.
        for entry in entries {
            let sub = entry.path();
            if sub.is_dir() {
                if let Ok(sub_entries) = std::fs::read_dir(&sub) {
                    for sub_entry in sub_entries.flatten() {
                        let path = sub_entry.path();
                        if path.is_file()
                            && path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .is_some_and(|n| n.eq_ignore_ascii_case(target))
                        {
                            return Some(path);
                        }
                    }
                }
            }
        }
        None
    }

    fn load_dream_dir(&mut self, dream_dir: &std::path::Path) {
        self.current_dream_dir = Some(dream_dir.to_path_buf());
        self.custom_dream_skin = Self::parse_dream_skin_ini(dream_dir);
        self.play_dream_music(dream_dir);
        let mut patch_warnings = Vec::new();
        fn collect_err(warnings: &mut Vec<String>, res: Result<(), String>) {
            if let Err(err) = res {
                warnings.push(err);
            }
        }

        fn find_patch_file(dir: &std::path::Path, stem: &str) -> Option<std::path::PathBuf> {
            FurcadiaApp::find_file_case_insensitive(dir, &format!("{stem}.fox"))
                .or_else(|| FurcadiaApp::find_file_case_insensitive(dir, &format!("{stem}.fsh")))
        }

        if let Some(p) =
            find_patch_file(dream_dir, "floors").or_else(|| find_patch_file(dream_dir, "floor"))
        {
            collect_err(&mut patch_warnings, self.floors_cache.set_dream_patch(&p));
        }
        for (stem, target) in [
            ("floors2", &mut self.floors2_cache),
            ("floore", &mut self.floore_cache),
            ("items", &mut self.items_cache),
            ("iteme", &mut self.iteme_cache),
            ("portals", &mut self.portals_cache),
            ("portalse", &mut self.portalse_cache),
            ("effects", &mut self.effects_cache),
            ("effectse", &mut self.effectse_cache),
            ("lighting", &mut self.lighting_cache),
            ("lightinge", &mut self.lightinge_cache),
            ("ambience", &mut self.ambience_cache),
            ("ambiencee", &mut self.ambiencee_cache),
            ("junction", &mut self.junction_cache),
        ] {
            if let Some(p) = find_patch_file(dream_dir, stem) {
                collect_err(&mut patch_warnings, target.set_dream_patch(&p));
            }
        }
        for (stem, min_id, max_id, offset) in [
            ("item", 1u16, 304u16, 0u16),
            ("item2", 305, 999, 305),
            ("item3", 1000, 1348, 1000),
            ("item4", 1349, 1748, 1349),
        ] {
            if let Some(p) = find_patch_file(dream_dir, stem) {
                let cache = FoxSpriteCache::from_dream_only(&p, stem);
                if !cache.is_loaded() {
                    patch_warnings.push(format!("{stem}: failed to parse patch archive"));
                }
                self.dream_item_splits.push((min_id, max_id, offset, cache));
            }
        }
        if let Some(wall0) =
            find_patch_file(dream_dir, "wall").or_else(|| find_patch_file(dream_dir, "wall_0"))
        {
            if let Some(c) = self.wall_caches.get_mut(&0) {
                collect_err(&mut patch_warnings, c.set_dream_patch(&wall0));
            }
        }
        for g in 1..=20 {
            let stem = format!("wall_{g}");
            if let Some(p) = find_patch_file(dream_dir, &stem) {
                if let Some(c) = self.wall_caches.get_mut(&g) {
                    collect_err(&mut patch_warnings, c.set_dream_patch(&p));
                } else {
                    let cache = FoxSpriteCache::from_dream_only(&p, &stem);
                    if !cache.is_loaded() {
                        patch_warnings.push(format!("{stem}: failed to parse patch archive"));
                    }
                    self.wall_caches.insert(g, cache);
                }
            }
        }
        for warn in patch_warnings {
            self.push_system_line("warning", &format!("Dream patch decode warning ({warn})"));
        }
    }

    fn find_map_in_dir(dir: &std::path::Path) -> Option<std::path::PathBuf> {
        if let Some(default_map) = Self::find_file_case_insensitive(dir, "default.map") {
            return Some(default_map);
        }
        let mut candidates: Vec<std::path::PathBuf> = Vec::new();
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_file()
                && path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("map"))
            {
                candidates.push(path);
            } else if path.is_dir() {
                if let Ok(sub) = std::fs::read_dir(&path) {
                    for se in sub.flatten() {
                        let sp = se.path();
                        if sp.is_file()
                            && sp
                                .extension()
                                .and_then(|e| e.to_str())
                                .is_some_and(|e| e.eq_ignore_ascii_case("map"))
                        {
                            candidates.push(sp);
                        }
                    }
                }
            }
        }
        candidates.sort();
        candidates.into_iter().next()
    }

    fn try_load_cached_dream(
        &mut self,
        name: &str,
        expected_checksum: Option<&str>,
    ) -> Option<(FurcadiaMap, std::path::PathBuf)> {
        let dreams_dir = AssetLocator::dreams_cache_dir()?;
        let dir = dreams_dir.join(Self::sanitize_dream_id(name));
        if let Some(expected) = expected_checksum {
            let cached_hash = std::fs::read_to_string(dir.join(".hash")).ok()?;
            if cached_hash.trim() != expected.trim() {
                return None;
            }
        }
        let map_path = Self::find_map_in_dir(&dir)?;
        let map = FurcadiaMap::load(&map_path).ok()?;
        Some((map, dir))
    }

    fn send_pending_vascodagama(&mut self) {
        self.login_ack_at = None;
        if std::mem::take(&mut self.pending_vascodagama) && self.is_connected {
            let _ = self
                .to_net
                .send(UiToNet::Send(ClientCommand::Raw("vascodagama".to_owned())));
        }
    }

    fn handle_extended_dream(&mut self, payload: &str) {
        let tokens: Vec<&str> = payload.split_whitespace().collect();
        let is_modern = tokens.iter().any(|t| *t == "modern");
        let explicit_url = tokens
            .iter()
            .copied()
            .find(|t| t.starts_with("http://") || t.starts_with("https://"));
        self.pending_vascodagama = true;
        if let Some(url) = explicit_url {
            let dream_id = tokens
                .first()
                .copied()
                .filter(|t| {
                    !t.starts_with("http://") && !t.starts_with("https://") && *t != "modern"
                })
                .map(str::to_owned)
                .or_else(|| self.pending_dream_name.clone())
                .unwrap_or_else(|| "custom_dream".to_owned());
            let checksum = tokens.iter().copied().find(|t| {
                !t.starts_with("http://")
                    && !t.starts_with("https://")
                    && *t != dream_id
                    && *t != "modern"
            });
            self.enter_dream(&dream_id, checksum, Some(url), false, Some(is_modern));
        } else if let Some(&dream_id) = tokens.first().filter(|t| **t != "modern") {
            let checksum = tokens.get(1).copied().filter(|t| *t != "modern");
            self.enter_dream(dream_id, checksum, None, false, Some(is_modern));
        } else {
            self.world.is_modern = is_modern;
            self.send_pending_vascodagama();
        }
    }

    fn handle_use_map(&mut self, name: &str) {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            // The server sends `]J` with an empty payload right after `]q <dream_id>` when
            // entering a player dream (indicating no legacy main-map override). Do not wipe
            // the dream that `]q` just loaded or started downloading.
            return;
        }
        let lower = trimmed.trim_end_matches(".map").to_ascii_lowercase();
        let normalized = normalize_main_map_id(&lower).unwrap_or(trimmed);
        let already_pending = self.pending_dream_name.as_deref().is_some_and(|p| {
            p.trim()
                .trim_end_matches(".map")
                .eq_ignore_ascii_case(normalized.trim_end_matches(".map"))
        });
        if already_pending {
            return;
        }
        self.enter_dream(normalized, None, None, false, None);
    }

    /// Returns (and caches) a species portrait (`purpose = 4`) colorized for `app`.
    fn portrait_for_appearance(
        &mut self,
        ctx: &egui::Context,
        app: &AvatarAppearance,
        key: String,
    ) -> Option<TextureHandle> {
        if let Some(t) = self.char_portraits.get(&key) {
            return t.clone();
        }
        let sp = if app.species_id == 0 { 1 } else { app.species_id };
        let (file_id, obj_id) = self.resolve_species_spec(sp as usize);
        let ck = self.ensure_species_cache(file_id);
        let palette = &self.palette;
        let tex = self.species_caches.get_mut(&ck).and_then(|c| {
            c.get_or_load_portrait_tex(ctx, palette, app, obj_id, app.gender, 4)
        });
        self.char_portraits.insert(key, tex.clone());
        tex
    }

    /// Portrait for an account character from its `clogin.php` color code.
    fn character_portrait(
        &mut self,
        ctx: &egui::Context,
        id: &str,
        colors: &str,
    ) -> Option<TextureHandle> {
        if colors.is_empty() {
            return None;
        }
        let app = furcadia_client::parse_color_code(colors);
        self.portrait_for_appearance(ctx, &app, format!("char:{id}:{colors}"))
    }

    /// Portrait for the logged-in character (live appearance, falling back to account colors).
    fn custom_portrait_texture(&mut self, ctx: &egui::Context, pid: u32) -> Option<TextureHandle> {
        if let Some(tex) = self.custom_portrait_textures.get(&pid) {
            return Some(tex.clone());
        }
        if let Some(Some((w, h, pixels))) = self.custom_portrait_rgba.get(&pid) {
            let img = ColorImage::from_rgba_unmultiplied([*w, *h], pixels);
            let tex = ctx.load_texture(
                format!("custom_portrait_{pid}"),
                img,
                TextureOptions::LINEAR,
            );
            self.custom_portrait_textures.insert(pid, tex.clone());
            return Some(tex);
        }
        None
    }

    fn self_custom_portrait_id(&self) -> Option<u32> {
        let self_name = self.world.self_name.as_deref().unwrap_or("");
        let active_name = self.active_character_name();
        let target_key = if !self_name.is_empty() {
            shortname_key(self_name)
        } else {
            shortname_key(&active_name)
        };
        if target_key.is_empty() {
            return None;
        }
        self.player_portrait_ids
            .iter()
            .find(|(name, _)| shortname_key(name) == target_key)
            .map(|(_, &pid)| pid)
    }

    fn self_portrait(&mut self, ctx: &egui::Context) -> Option<TextureHandle> {
        if let Some(pid) = self.self_custom_portrait_id() {
            if let Some(tex) = self.custom_portrait_texture(ctx, pid) {
                return Some(tex);
            }
        }
        let live = self
            .world
            .self_uid
            .and_then(|uid| self.world.avatars.get(&uid))
            .map(|a| a.appearance.clone());
        if let Some(app) = live {
            let key = format!("self:{}:{}:{:?}", app.species_id, app.gender, app.color_key);
            return self.portrait_for_appearance(ctx, &app, key);
        }
        let (id, colors) = self
            .selected_character()
            .map(|c| (c.id.clone(), c.colors.clone()))?;
        self.character_portrait(ctx, &id, &colors)
    }

    fn resolve_species_spec(&self, sp_id: usize) -> (usize, u16) {
        if (1000..=2000).contains(&sp_id) {
            return (sp_id, 0);
        }
        if let Ok(sp_u16) = u16::try_from(sp_id) {
            if let Some(&(dplayer_id, obj_id)) = self.dynamic_avatars.get(&sp_u16) {
                return (DYNAMIC_AVATAR_CACHE_BASE + dplayer_id, obj_id);
            }
        }
        resolve_species_fox_spec(sp_id)
    }

    /// Parses `]M%<server_code><8-byte entries>` (`DynamicAvatarDetails`) from `Furcadia.exe`.
    fn apply_dynamic_avatar_details(&mut self, data: &[u8]) {
        if data.first() != Some(&b'%') || data.len() < 2 {
            return;
        }
        let srv = data[1];
        self.dynamic_avatar_server =
            (srv != b' ' && srv.is_ascii_alphanumeric()).then_some(srv as char);
        let mut changed = false;
        for (idx, chunk) in data[2..].chunks_exact(8).enumerate() {
            let lo = chunk[2].saturating_sub(35) as u16;
            let hi_and_flag = chunk[3].saturating_sub(35) as u16;
            let species_id = (hi_and_flag >> 1) * 220 + lo;
            let obj_flag = hi_and_flag & 1;
            let dplayer_id = idx + 1;
            if species_id > 0 {
                let prev = self.dynamic_avatars.insert(species_id, (dplayer_id, obj_flag));
                if prev != Some((dplayer_id, obj_flag)) {
                    changed = true;
                    self.species_caches.remove(&(species_id as usize));
                }
            }
        }
        if changed {
            self.char_portraits.clear();
            if let Some(card) = self.looked_player.as_mut() {
                card.resolved_for = None;
            }
        }
    }

    /// Parses `]&` (`PortraitSolveRequest`) which may contain `<id>`, `<id> pp<id>`, `<id> <name>`, or `<name> <id>`.
    fn handle_portrait_directive(&mut self, raw: &str) {
        let mut pid: Option<u32> = None;
        let mut name_parts: Vec<&str> = Vec::new();
        for tok in raw.split_whitespace() {
            if pid.is_none() {
                if let Ok(id) = tok.parse::<u32>() {
                    if id > 0 {
                        pid = Some(id);
                        continue;
                    }
                }
            }
            if tok.len() > 2
                && tok[..2].eq_ignore_ascii_case("pp")
                && tok[2..].bytes().all(|b| b.is_ascii_digit())
            {
                if pid.is_none() {
                    if let Ok(id) = tok[2..].parse::<u32>() {
                        if id > 0 {
                            pid = Some(id);
                        }
                    }
                }
                continue;
            }
            name_parts.push(tok);
        }
        let Some(pid) = pid else {
            return;
        };
        let target_name = if name_parts.is_empty() {
            self.looked_player.as_ref().map(|c| c.short_name.clone())
        } else {
            Some(wire_player_name(&name_parts.join(" ")))
        };
        if let Some(ref name) = target_name {
            self.player_portrait_ids.insert(name.clone(), pid);
        }
        if let Some(card) = self.looked_player.as_mut() {
            if target_name.as_deref().is_none_or(|n| n.eq_ignore_ascii_case(&card.short_name)) {
                card.portrait_id = Some(pid);
            }
        }
        if !self.custom_portrait_rgba.contains_key(&pid)
            && self.pending_portrait_downloads.insert(pid)
        {
            let _ = self.to_net.send(UiToNet::DownloadPortrait { portrait_id: pid });
        }
    }

    fn ensure_species_cache(&mut self, file_id: usize) -> usize {
        if !self.species_caches.contains_key(&file_id) {
            let assets = &self.assets;
            let dream_dir = self.current_dream_dir.as_deref();
            let mut cache = if file_id >= DYNAMIC_AVATAR_CACHE_BASE {
                let dplayer_id = file_id - DYNAMIC_AVATAR_CACHE_BASE;
                let cached = AssetLocator::species_cache_dir()
                    .map(|d| d.join(format!("dplayer{dplayer_id}.fox")))
                    .filter(|p| p.is_file());
                if let Some(cached_p) = cached {
                    let mut tmp =
                        FoxSpriteCache::from_dream_only(&cached_p, &format!("dplayer_{dplayer_id}"));
                    tmp.default_archive = tmp.dream_archive.take();
                    tmp
                } else {
                    FoxSpriteCache::from_dream_only(
                        std::path::Path::new(""),
                        &format!("dplayer_{dplayer_id}"),
                    )
                }
            } else if file_id > 1000 {
                let sp_name = format!("species{file_id}.fox");
                let c = FoxSpriteCache::load(assets, &sp_name, &format!("species_{file_id}"));
                if c.is_loaded() {
                    c
                } else {
                    FoxSpriteCache::load_local_species(
                        assets,
                        file_id - 1000,
                        &format!("local_{}", file_id - 1000),
                    )
                }
            } else {
                let mut c = if let Some(cached_p) = assets.species_patch_path(file_id) {
                    let mut tmp =
                        FoxSpriteCache::from_dream_only(&cached_p, &format!("species_{file_id}"));
                    tmp.default_archive = tmp.dream_archive.take();
                    tmp
                } else {
                    FoxSpriteCache::load(
                        assets,
                        &format!("species{file_id}.fox"),
                        &format!("species_{file_id}"),
                    )
                };
                if !c.is_loaded() {
                    c = FoxSpriteCache::load(
                        assets,
                        &format!("species{file_id}.fox"),
                        &format!("species_{file_id}"),
                    );
                }
                c
            };
            if let Some(dir) = dream_dir {
                let patch_path = if file_id > 1000 {
                    Self::find_file_case_insensitive(dir, &format!("species{file_id}.fox"))
                        .or_else(|| {
                            Self::find_file_case_insensitive(
                                dir,
                                &format!("local{}.fox", file_id - 1000),
                            )
                        })
                } else {
                    Self::find_file_case_insensitive(dir, &format!("species{file_id}.fox"))
                };
                if let Some(p) = patch_path {
                    let _ = cache.set_dream_patch(&p);
                }
            }
            self.species_caches.insert(file_id, cache);
        }
        let is_loaded = self
            .species_caches
            .get(&file_id)
            .is_some_and(FoxSpriteCache::is_loaded);
        if file_id >= DYNAMIC_AVATAR_CACHE_BASE {
            if self.pending_species_downloads.insert(file_id) {
                let dplayer_id = file_id - DYNAMIC_AVATAR_CACHE_BASE;
                let cached = AssetLocator::species_cache_dir()
                    .map(|d| d.join(format!("dplayer{dplayer_id}.fox")))
                    .filter(|p| p.is_file());
                let _ = self.to_net.send(UiToNet::DownloadSpecies {
                    file_id,
                    server_code: self.dynamic_avatar_server,
                    if_newer_than: cached,
                });
            }
        } else if ((100..1000).contains(&file_id) || (!is_loaded && (1..1000).contains(&file_id)))
            && self.pending_species_downloads.insert(file_id)
        {
            let _ = self.to_net.send(UiToNet::DownloadSpecies {
                file_id,
                server_code: self.dynamic_avatar_server,
                if_newer_than: self.assets.species_patch_path(file_id),
            });
        }
        if is_loaded {
            file_id
        } else {
            if !self.species_caches.contains_key(&1) {
                let mut c1 = FoxSpriteCache::load(&self.assets, "species1.fox", "species_1");
                if let Some(p) = self
                    .current_dream_dir
                    .as_deref()
                    .and_then(|d| Self::find_file_case_insensitive(d, "species1.fox"))
                {
                    let _ = c1.set_dream_patch(&p);
                }
                self.species_caches.insert(1, c1);
            }
            1
        }
    }

    fn enter_dream(
        &mut self,
        name: &str,
        checksum: Option<&str>,
        explicit_url: Option<&str>,
        clear_avatars: bool,
        explicit_modern: Option<bool>,
    ) {
        let prev_name = self.world.map_name.as_deref().unwrap_or("");
        let switching = prev_name != name;
        if switching || clear_avatars {
            let preserved_avatars = if !clear_avatars {
                Some(std::mem::take(&mut self.world.avatars))
            } else {
                None
            };
            self.world.begin_dream(name.to_owned());
            if let Some(av) = preserved_avatars {
                self.world.avatars = av;
            }
        } else {
            self.world.map_name = Some(name.to_owned());
        }
        if let Some(m) = explicit_modern {
            self.world.is_modern = m;
        }
        self.pending_dream_modern = explicit_modern;
        self.pending_dream_name = Some(name.to_owned());
        if clear_avatars || switching {
            self.cam_offset = Vec2::ZERO;
        }
        self.clear_dream_patches();

        if explicit_url.is_none() {
            if let Some((map, dir)) = self.try_load_cached_dream(name, checksum) {
                self.pending_dream_name = None;
                self.load_dream_dir(&dir);
                let prev_modern = self
                    .pending_dream_modern
                    .take()
                    .unwrap_or(self.world.is_modern);
                self.world.attach_map(map);
                if prev_modern {
                    self.world.is_modern = true;
                }
                self.send_pending_vascodagama();
                return;
            }
        }

        // Fall back to any bundled local map temporarily while the live dream downloads.
        if switching || self.world.map.is_none() {
            self.world.map = self
                .assets
                .resolve_map_path(name)
                .and_then(|p| FurcadiaMap::load(&p).ok());
        }

        if self.screen == AppScreen::Connecting {
            self.connecting_stage = format!("Downloading dream '{name}'...");
        }
        let encoded_name = percent_encode_query(name);
        let url = explicit_url.map(str::to_owned).unwrap_or_else(|| {
            format!("http://apollo.furcadia.com/dream/get.php?file={encoded_name}&server=l")
        });
        let _ = self.to_net.send(UiToNet::DownloadDream {
            url,
            map_name: name.to_owned(),
            checksum: checksum.map(str::to_owned),
        });
    }

    fn resolve_floor_sprite(
        &mut self,
        ctx: &egui::Context,
        floor_id: u16,
        x: usize,
        y: usize,
    ) -> Option<CachedSprite> {
        let dream_overrides_floor = floor_id < 500
            && self
                .floors_cache
                .dream_archive
                .as_ref()
                .is_some_and(|f| f.resolve_shape(floor_id, 0).is_some());
        let fid = if dream_overrides_floor {
            floor_id
        } else {
            resolve_floor_variant(floor_id, x, y)
        };
        let use_legacy = !self.world.is_modern;
        if (209..=308).contains(&fid) && (use_legacy || self.floors2_cache.has_dream()) {
            if let Some(sp) = self
                .floors2_cache
                .get_or_load_shape_mode(ctx, &self.palette, fid - 209, 0, use_legacy)
                .or_else(|| {
                    self.floors2_cache
                        .get_or_load_shape_mode(ctx, &self.palette, fid, 0, use_legacy)
                })
            {
                return Some(sp);
            }
        }
        if fid >= 500 {
            return self
                .floore_cache
                .get_or_load_shape_mode(ctx, &self.palette, fid - 500, 0, use_legacy)
                .or_else(|| {
                    self.floors_cache
                        .get_or_load_shape_mode(ctx, &self.palette, fid, 0, use_legacy)
                });
        }
        self.floors_cache
            .get_or_load_shape_mode(ctx, &self.palette, fid, 0, use_legacy)
    }

    fn resolve_item_layers(
        &mut self,
        ctx: &egui::Context,
        item_id: u16,
        portal_state: Option<i32>,
        anim_ms: u64,
    ) -> Option<ResolvedShapeLayers> {
        if item_id == 0 {
            if self.world.is_modern && portal_state.is_some() {
                return resolve_portal_layers_from(
                    &mut self.portals_cache,
                    ctx,
                    &self.palette,
                    0,
                    portal_state,
                    anim_ms,
                );
            }
            return None;
        }
        if (1800..=1899).contains(&item_id) {
            let pid = item_id - 1800;
            return resolve_portal_layers_from(
                &mut self.portals_cache,
                ctx,
                &self.palette,
                pid,
                portal_state,
                anim_ms,
            )
            .or_else(|| {
                if portal_state.is_some_and(|s| s >= 0) && pid != 0 {
                    resolve_portal_layers_from(
                        &mut self.portals_cache,
                        ctx,
                        &self.palette,
                        0,
                        portal_state,
                        anim_ms,
                    )
                } else {
                    None
                }
            });
        }
        if (2000..=2099).contains(&item_id) {
            let pid = item_id - 2000;
            return resolve_portal_layers_from(
                &mut self.portalse_cache,
                ctx,
                &self.palette,
                pid,
                portal_state,
                anim_ms,
            )
            .or_else(|| {
                resolve_portal_layers_from(
                    &mut self.portals_cache,
                    ctx,
                    &self.palette,
                    pid,
                    portal_state,
                    anim_ms,
                )
            })
            .or_else(|| {
                if portal_state.is_some_and(|s| s >= 0) && pid != 0 {
                    resolve_portal_layers_from(
                        &mut self.portals_cache,
                        ctx,
                        &self.palette,
                        0,
                        portal_state,
                        anim_ms,
                    )
                } else {
                    None
                }
            });
        }
        let use_legacy = !self.world.is_modern;
        if item_id >= 2400 {
            let oid = item_id - 2400;
            if self.world.is_modern && portal_state.is_some_and(|s| s >= 0) {
                if cache_has_portal_shapes(&self.iteme_cache, oid) {
                    if let res @ Some(_) = resolve_portal_layers_from(
                        &mut self.iteme_cache,
                        ctx,
                        &self.palette,
                        oid,
                        portal_state,
                        anim_ms,
                    ) {
                        return res;
                    }
                    return resolve_portal_layers_from(
                        &mut self.portals_cache,
                        ctx,
                        &self.palette,
                        0,
                        portal_state,
                        anim_ms,
                    );
                }
            }
            if let res @ Some(_) = self.iteme_cache.get_or_load_shape_layers_mode(
                ctx,
                &self.palette,
                oid,
                0,
                anim_ms,
                use_legacy,
            ) {
                return res;
            }
        }
        if !self.items_cache.has_dream() || item_id >= 1750 {
            for (min_id, max_id, offset, cache) in &mut self.dream_item_splits {
                if item_id >= *min_id && item_id <= *max_id {
                    let oid = item_id - *offset;
                    if self.world.is_modern && portal_state.is_some_and(|s| s >= 0) {
                        if cache_has_portal_shapes(cache, oid) {
                            if let res @ Some(_) = resolve_portal_layers_from(
                                cache,
                                ctx,
                                &self.palette,
                                oid,
                                portal_state,
                                anim_ms,
                            ) {
                                return res;
                            }
                            return resolve_portal_layers_from(
                                &mut self.portals_cache,
                                ctx,
                                &self.palette,
                                0,
                                portal_state,
                                anim_ms,
                            );
                        }
                    }
                    if let res @ Some(_) =
                        cache.get_or_load_shape_layers(ctx, &self.palette, oid, 0, anim_ms)
                    {
                        return res;
                    }
                }
            }
        }
        if self.items_cache.has_dream() {
            let dream_oid = if self.items_cache.dream_is_fshx_or_fsh() {
                item_id
            } else {
                item_id - 1
            };
            if self.world.is_modern && portal_state.is_some_and(|s| s >= 0) {
                if cache_has_portal_shapes(&self.items_cache, dream_oid) {
                    if let res @ Some(_) = resolve_portal_layers_from(
                        &mut self.items_cache,
                        ctx,
                        &self.palette,
                        dream_oid,
                        portal_state,
                        anim_ms,
                    ) {
                        return res;
                    }
                    return resolve_portal_layers_from(
                        &mut self.portals_cache,
                        ctx,
                        &self.palette,
                        0,
                        portal_state,
                        anim_ms,
                    );
                }
            }
            if let res @ Some(_) = self.items_cache.get_or_load_dream_shape_layers(
                ctx,
                &self.palette,
                dream_oid,
                0,
                anim_ms,
            ) {
                return res;
            }
        }
        let oid = item_id - 1;
        if self.world.is_modern && portal_state.is_some_and(|s| s >= 0) {
            if cache_has_portal_shapes(&self.items_cache, oid) {
                if let res @ Some(_) = resolve_portal_layers_from(
                    &mut self.items_cache,
                    ctx,
                    &self.palette,
                    oid,
                    portal_state,
                    anim_ms,
                ) {
                    return res;
                }
                return resolve_portal_layers_from(
                    &mut self.portals_cache,
                    ctx,
                    &self.palette,
                    0,
                    portal_state,
                    anim_ms,
                );
            }
        }
        if let res @ Some(_) = self.items_cache.get_or_load_default_shape_layers(
            ctx,
            &self.palette,
            oid,
            0,
            anim_ms,
        ) {
            return res;
        }
        if item_id >= 305 {
            for (min_id, max_id, offset, cache) in &mut self.legacy_item_splits {
                if item_id >= *min_id && item_id <= *max_id {
                    let legacy_oid = item_id - *offset;
                    if let res @ Some(_) = cache.get_or_load_shape_layers_mode(
                        ctx,
                        &self.palette,
                        legacy_oid,
                        0,
                        anim_ms,
                        true,
                    ) {
                        return res;
                    }
                }
            }
        }
        if let res @ Some(_) = self.items_cache.get_or_load_shape_layers_mode(
            ctx,
            &self.palette,
            item_id,
            0,
            anim_ms,
            true,
        ) {
            return res;
        }
        if self.world.is_modern && portal_state.is_some_and(|s| s >= 0) {
            return resolve_portal_layers_from(
                &mut self.portals_cache,
                ctx,
                &self.palette,
                0,
                portal_state,
                anim_ms,
            );
        }
        None
    }

    fn resolve_effect_layers(
        &mut self,
        ctx: &egui::Context,
        effect_id: u16,
        anim_ms: u64,
    ) -> Option<ResolvedShapeLayers> {
        if effect_id == 0 {
            return None;
        }
        let use_legacy = !self.world.is_modern;
        let resolve_effects_base = |app: &mut Self, id: u16| -> Option<ResolvedShapeLayers> {
            if app.effects_cache.has_dream() && app.effects_cache.dream_is_fshx_or_fsh() {
                if let res @ Some(_) = app.effects_cache.get_or_load_dream_shape_layers(
                    ctx,
                    &app.palette,
                    id,
                    0,
                    anim_ms,
                ) {
                    return res;
                }
            }
            app.effects_cache.get_or_load_shape_layers_mode(
                ctx,
                &app.palette,
                id - 1,
                0,
                anim_ms,
                use_legacy,
            )
        };
        if effect_id >= 200 {
            if effect_id <= 249 && !self.effectse_cache.has_dream() {
                if let res @ Some(_) = resolve_effects_base(self, effect_id) {
                    return res;
                }
            }
            if let res @ Some(_) = self.effectse_cache.get_or_load_shape_layers_mode(
                ctx,
                &self.palette,
                effect_id - 200,
                0,
                anim_ms,
                use_legacy,
            ) {
                return res;
            }
            if effect_id <= 249 {
                return resolve_effects_base(self, effect_id);
            }
            return None;
        }
        resolve_effects_base(self, effect_id)
    }

    fn resolve_junction_sprite(
        &mut self,
        ctx: &egui::Context,
        group_or: usize,
        mask: u8,
    ) -> Option<CachedSprite> {
        if group_or > 1 || mask == 0 || mask == 6 || mask == 9 || mask > 15 {
            return None;
        }
        let use_legacy = !self.world.is_modern;
        // Dream wall_1.fox / wall_2.fox suppresses junction.fox for that group (`ps.iv`).
        if self
            .wall_caches
            .get(&(group_or + 1))
            .is_some_and(|c| c.has_dream() || c.default_archive.is_some())
        {
            return None;
        }
        let obj_id = (15 * group_or + mask as usize) as u16;
        let is_fshx = self.junction_cache.active_is_fshx_or_fsh(use_legacy);
        let dir = if is_fshx { 0 } else { 7 };
        self.junction_cache
            .get_or_load_shape_mode(ctx, &self.palette, obj_id, dir, use_legacy)
    }

    fn resolve_wall_edge_sprite(
        &mut self,
        ctx: &egui::Context,
        wall_byte: u16,
        dir: u8,
    ) -> Option<CachedSprite> {
        let tex = (wall_byte / 12) as usize;
        let v = wall_byte % 12;
        if wall_byte == 0 || tex >= 18 {
            return None;
        }
        let obj: u16 = if v >= 4 {
            v - 4
        } else if v == 1 || v == 3 {
            8
        } else {
            return None;
        };

        let use_legacy = !self.world.is_modern;
        let has_tex_file = self
            .wall_caches
            .get(&(tex + 1))
            .is_some_and(|c| c.has_dream() || c.default_archive.is_some());

        if tex < 2 && !has_tex_file {
            if obj == 8 {
                let junc_obj = if dir == 7 {
                    1 + 15 * (tex as u16) + 8
                } else {
                    1 + 15 * (tex as u16) + 5
                };
                let jdir = if self.junction_cache.active_is_fshx_or_fsh(use_legacy) {
                    0
                } else {
                    7
                };
                return self
                    .junction_cache
                    .get_or_load_shape_mode(ctx, &self.palette, junc_obj, jdir, use_legacy);
            }
            let cache = self.wall_caches.get_mut(&0)?;
            let is_fshx = cache.active_is_fshx_or_fsh(use_legacy);
            let side_off = if dir == 9 { 1 } else { 0 };
            let (obj_id, lookup_dir) = if is_fshx {
                (16 * tex as u16 + 2 * obj + side_off, 0)
            } else {
                (8 * tex as u16 + obj, dir)
            };
            return cache.get_or_load_shape_mode(
                ctx,
                &self.palette,
                obj_id,
                lookup_dir,
                use_legacy,
            );
        }

        let file_key = tex + 1;
        let cache = self.wall_caches.get_mut(&file_key)?;
        let is_fshx = cache.active_is_fshx_or_fsh(use_legacy);
        let side_off = if dir == 9 { 1 } else { 0 };
        let (obj_id, lookup_dir) = if is_fshx {
            (2 * obj + side_off, 0)
        } else {
            (obj, dir)
        };
        cache.get_or_load_shape_mode(ctx, &self.palette, obj_id, lookup_dir, use_legacy)
    }

    fn handle_net_events(&mut self, ctx: &egui::Context) {
        while let Ok(ev) = self.from_net.try_recv() {
            match ev {
                NetToUi::Status(s) => self.status_text = s,
                NetToUi::AuthError(err) => {
                    self.auth_busy = false;
                    self.auth_error = Some(err);
                }
                NetToUi::Authenticated(info) => {
                    self.auth_busy = false;
                    self.auth_error = None;
                    let saved_norm = self.saved_character.trim().replace('|', " ");
                    self.selected_char_idx = if saved_norm.is_empty() {
                        0
                    } else {
                        info.characters
                            .iter()
                            .position(|c| {
                                c.name.eq_ignore_ascii_case(&self.saved_character)
                                    || c.name.replace('|', " ").eq_ignore_ascii_case(&saved_norm)
                            })
                            .unwrap_or(0)
                    };
                    if let Some(ch) = info.characters.get(self.selected_char_idx) {
                        self.edit_desc_buf = ch.desc.clone();
                    }
                    self.account_info = Some(info);
                    self.screen = AppScreen::CharacterSelect;
                }
                NetToUi::News(entries) => {
                    self.news = entries;
                }
                NetToUi::ConnectStage { seq, stage } => {
                    if seq == self.connect_seq {
                        self.connecting_stage = stage;
                    }
                }
                NetToUi::ConnectFailed { seq, error } => {
                    if seq == self.connect_seq {
                        self.is_connected = false;
                        self.connected_at = None;
                        self.connecting_error = Some(error.clone());
                        self.connecting_stage = "Connection failed".to_owned();
                        self.status_text = error.clone();
                        self.push_system_line("error", &error);
                    }
                }
                NetToUi::Connected { seq, connected } => {
                    if seq == self.connect_seq {
                        self.is_connected = connected;
                        if connected {
                            self.connecting_error = None;
                            self.connected_at = Some(Instant::now());
                            self.connecting_stage = "Logged in! Entering dream...".to_owned();
                        } else {
                            self.connected_at = None;
                            if self.screen == AppScreen::Connecting && self.connecting_error.is_none()
                            {
                                self.connecting_error =
                                    Some("Disconnected by server.".to_owned());
                            } else if self.screen == AppScreen::InGame {
                                self.push_system_line("warning", "Disconnected from server.");
                            }
                        }
                    }
                }
                NetToUi::Message(msg) => self.process_server_message(msg),
                NetToUi::DreamDownloaded {
                    map_name,
                    path,
                    dream_dir,
                } => {
                    if let Some(expected) = self.pending_dream_name.as_deref() {
                        let e = expected.trim().trim_end_matches(".map");
                        let m = map_name.trim().trim_end_matches(".map");
                        if !e.is_empty() && !e.eq_ignore_ascii_case(m) {
                            continue;
                        }
                    }
                    self.pending_dream_name = None;
                    self.clear_dream_patches();
                    if let Some(dir) = dream_dir {
                        self.load_dream_dir(&dir);
                    }
                    match FurcadiaMap::load(&path) {
                        Ok(map) => {
                            self.world.map_name = Some(map_name.clone());
                            let prev_modern = self
                                .pending_dream_modern
                                .take()
                                .unwrap_or(self.world.is_modern);
                            self.world.attach_map(map);
                            if prev_modern {
                                self.world.is_modern = true;
                            }
                            self.cam_offset = Vec2::ZERO;
                            self.push_system_line(
                                "success",
                                &format!("Loaded dream package '{map_name}'."),
                            );
                            self.send_pending_vascodagama();
                        }
                        Err(e) => {
                            self.push_system_line(
                                "warning",
                                &format!("Failed to parse dream map '{map_name}': {e}"),
                            );
                            self.send_pending_vascodagama();
                        }
                    }
                }
                NetToUi::DreamError(msg) => {
                    self.push_system_line("warning", &msg);
                    if self.screen == AppScreen::Connecting && self.is_connected {
                        self.screen = AppScreen::InGame;
                    }
                    if !self.world.map_ready {
                        if let Some(fallback_map) = self.world.map.take() {
                            let prev_modern = self
                                .pending_dream_modern
                                .take()
                                .unwrap_or(self.world.is_modern);
                            self.world.attach_map(fallback_map);
                            if prev_modern {
                                self.world.is_modern = true;
                            }
                        }
                    }
                    self.send_pending_vascodagama();
                }
                NetToUi::SpeciesDownloaded { file_id, path } => {
                    if let Some(p) = path {
                        let mut cache = FoxSpriteCache::from_dream_only(&p, &format!("species_{file_id}"));
                        // Promote the downloaded file to default_archive so dream patches can override it.
                        cache.default_archive = cache.dream_archive.take();
                        if let Some(patch_p) = self
                            .current_dream_dir
                            .as_deref()
                            .and_then(|d| Self::find_file_case_insensitive(d, &format!("species{file_id}.fox")))
                        {
                            let _ = cache.set_dream_patch(&patch_p);
                        }
                        if cache.is_loaded() {
                            self.species_caches.insert(file_id, cache);
                            self.char_portraits.clear();
                            if let Some(card) = self.looked_player.as_mut() {
                                card.resolved_for = None;
                            }
                        }
                    }
                }
                NetToUi::PortraitDownloaded { portrait_id, rgba } => {
                    self.pending_portrait_downloads.remove(&portrait_id);
                    self.custom_portrait_rgba.insert(portrait_id, rgba);
                }
                NetToUi::ChatImageDownloaded { url, rgba } => {
                    self.pending_chat_img_downloads.remove(&url);
                    let tex = rgba.map(|(w, h, bytes)| {
                        let img = ColorImage::from_rgba_unmultiplied([w, h], &bytes);
                        ctx.load_texture(format!("chat_img_{url}"), img, TextureOptions::NEAREST)
                    });
                    self.chat_img_textures.insert(url, tex);
                }
                NetToUi::SpellCheckResult { seq, misspelled } => {
                    if seq == self.spell_seq {
                        self.spell_misspelled = misspelled
                            .into_iter()
                            .filter(|m| !self.custom_dict_words.contains(&m.word.to_ascii_lowercase()))
                            .collect();
                    }
                }
            }
        }
    }

    fn open_whisper_tab(&mut self, peer: &str, focus: bool) -> usize {
        let trimmed = peer.trim();
        if let Some(idx) = self
            .whispers
            .iter()
            .position(|w| w.peer.eq_ignore_ascii_case(trimmed))
        {
            if focus {
                self.whispers[idx].unread = 0;
                self.active_chat_tab = Some(self.whispers[idx].peer.clone());
                self.focus_chat_input = true;
            }
            return idx;
        }
        let mut session = WhisperSession::new(trimmed.to_owned());
        if let Some(dir) = default_logs_dir() {
            session.history_lines =
                load_whisper_log(&dir, &self.active_character_name(), trimmed, 200);
        }
        self.whispers.push(session);
        let idx = self.whispers.len() - 1;
        if focus {
            self.active_chat_tab = Some(self.whispers[idx].peer.clone());
            self.focus_chat_input = true;
        }
        idx
    }

    fn process_server_message(&mut self, msg: ServerMessage) {
        let is_map_directive = matches!(
            &msg,
            ServerMessage::UseMap(_)
                | ServerMessage::MapName(_)
                | ServerMessage::Extended {
                    code: Some('q' | 'r' | 'J' | 'M' | '&'),
                    ..
                }
                | ServerMessage::ExtendedBytes {
                    code: b'q' | b'r' | b'J' | b'M' | b'&' | b'f',
                    ..
                }
        );
        if (self.login_trace_remaining > 0 && !matches!(msg, ServerMessage::WorldPacket { .. }))
            || is_map_directive
        {
            if self.login_trace_remaining > 0 && !matches!(msg, ServerMessage::WorldPacket { .. }) {
                self.login_trace_remaining -= 1;
            }
            if let Some(dir) = default_logs_dir() {
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(dir.join("login_trace.log"))
                {
                    use std::io::Write;
                    let _ = writeln!(f, "{msg:?}");
                }
            }
        }
        match msg {
            ServerMessage::Text(t) => {
                let line = rewrite_pounce_add_message(parse_chat_line(&t));
                for span in &line.spans {
                    if let furcadia_client::ChatSpan::Image { src, .. } = span {
                        let url = src.trim();
                        let lower = url.to_ascii_lowercase();
                        if (lower.starts_with("http://") || lower.starts_with("https://"))
                            && !self.chat_img_textures.contains_key(url)
                            && self.pending_chat_img_downloads.insert(url.to_owned())
                        {
                            let _ = self
                                .to_net
                                .send(UiToNet::DownloadChatImage { url: url.to_owned() });
                        }
                    }
                }
                let plain = line.to_plain_text();
                let (collected, total) = parse_cookie_counts(&plain);
                if collected.is_some() {
                    self.cookies_collected = collected;
                }
                if total.is_some() {
                    self.cookies_total = total;
                }
                self.log_chat_line(&plain);
                if let Some(peer) = extract_whisper_peer(&plain) {
                    self.log_whisper_line(&peer, &plain);
                    let self_name = self.active_character_name();
                    let formatted = format_whisper_tab_line(&self_name, &peer, &plain, &line);
                    let idx = self.open_whisper_tab(&peer, false);
                    let is_focused = !self.whispers[idx].detached
                        && self
                            .active_chat_tab
                            .as_deref()
                            .is_some_and(|a| a.eq_ignore_ascii_case(&peer));
                    if !is_focused && !self.whispers[idx].detached {
                        self.whispers[idx].unread += 1;
                    }
                    self.whispers[idx].lines.push(formatted);
                    if self.notify_whisper && !plain.starts_with("[ You whisper ") && !plain.starts_with("You whisper ") {
                        self.play_sound("whisper.wav");
                        self.send_desktop_notification(&format!("Whisper from {peer}"), &plain);
                    }
                    self.last_received_whisper_from = peer;
                } else {
                    if self.notify_mention {
                        let me = self.active_character_name();
                        if !me.is_empty()
                            && me != "Unknown"
                            && plain.to_ascii_lowercase().contains(&me.to_ascii_lowercase())
                            && !plain.starts_with(&format!("{me}:"))
                        {
                            self.play_sound("talk.wav");
                            self.send_desktop_notification(&format!("Mentioned ({me})"), &plain);
                        }
                    }
                    if self.active_chat_tab.is_some() {
                        self.main_chat_unread += 1;
                    }
                    self.chat_log.push(line);
                }
            }
            ServerMessage::LoginAck => {
                self.connected_at.get_or_insert_with(Instant::now);
                self.connecting_stage = "Logged in! Loading dream...".to_owned();
                self.push_system_line("success", "Login acknowledged. Entering world...");
                self.pending_vascodagama = true;
                self.login_ack_at = Some(Instant::now());
                self.login_trace_remaining = 60;
                if let Some(dir) = default_logs_dir() {
                    let _ = std::fs::create_dir_all(&dir);
                    let _ = std::fs::write(dir.join("login_trace.log"), "");
                }
            }
            ServerMessage::CharacterIdentity { uid, name } => {
                self.world.self_uid = uid.parse().ok();
                self.world.self_name = Some(name.clone());
                self.push_system_line("success", &format!("Logged in as {name} (UID {uid})"));
                let _ = self
                    .to_net
                    .send(UiToNet::Send(ClientCommand::Raw("pounceInit".to_owned())));
            }
            ServerMessage::MapName(_name) => {
                // `]m<name>` is the server's legacy area label (e.g. "arco 99"), not a dream/map
                // filename; `;` (UseMap) and `]q`/`]r` carry the actual map/dream package to load.
            }
            ServerMessage::WorldPacket { opcode, data } => {
                self.world.apply_world_packet(opcode, &data);
                let now = Instant::now();
                let mut visible_names: Vec<String> = Vec::new();
                for av in self.world.avatars.values() {
                    let key = shortname_key(&av.name);
                    if !key.is_empty() {
                        if let Some(color) = accent_color_for_appearance(&av.appearance) {
                            self.player_accent_colors.insert(key, color);
                        }
                    }
                    if Some(av.uid) != self.world.self_uid && self.pounce.contains(&av.name) {
                        visible_names.push(av.name.clone());
                    }
                }
                for name in visible_names {
                    let already_online = self
                        .pounce
                        .entries()
                        .any(|e| e.name.eq_ignore_ascii_case(&name) && e.status == OnlineStatus::Online);
                    if !already_online {
                        if let Some(PounceEvent::CameOnline(n)) =
                            self.pounce.handle_online_status(&name, true, now)
                        {
                            self.push_system_line("success", &format!("[Pounce] {n} came ONLINE!"));
                        }
                    }
                }
            }
            ServerMessage::RegionDefaults(data) => {
                self.world.apply_region_defaults(&data);
            }
            ServerMessage::OnlineStatus { online, name } => {
                match self
                    .pounce
                    .handle_online_status(&name, online, Instant::now())
                {
                    Some(PounceEvent::CameOnline(n)) => {
                        self.push_system_line("success", &format!("[Pounce] {n} came ONLINE!"));
                        if self.notify_pounce {
                            self.play_sound("online.wav");
                            self.send_desktop_notification("Pounce", &format!("{n} came online!"));
                        }
                    }
                    Some(PounceEvent::WentOffline(n)) => {
                        self.push_system_line("warning", &format!("[Pounce] {n} went OFFLINE."));
                        if self.notify_pounce {
                            self.play_sound("offline.wav");
                        }
                    }
                    Some(PounceEvent::Checked { .. }) => {}
                    None => {
                        let state = if online { "ONLINE" } else { "offline" };
                        self.push_system_line("emit", &format!("{name} is {state}."));
                    }
                }
            }
            ServerMessage::Extended { code, payload } => {
                if matches!(code, Some('q' | 'r')) {
                    self.handle_extended_dream(&payload);
                } else if code == Some('J') {
                    self.handle_use_map(&payload);
                } else if code == Some('s') {
                    self.world.apply_show_dream_name(&payload);
                } else if code == Some('t') {
                    self.world.apply_remove_dream_name(&payload);
                } else if code == Some('&') {
                    self.handle_portrait_directive(&payload);
                } else if code == Some('M') {
                    self.apply_dynamic_avatar_details(payload.as_bytes());
                } else if code == Some('o') {
                    let slug = payload.trim().trim_start_matches("furc://").trim_matches('/');
                    if !slug.is_empty() {
                        self.current_dream_url = Some(format!("furc://{slug}"));
                    }
                }
                if self.show_debug_packets {
                    let c = code.unwrap_or('?');
                    self.push_system_line("emit", &format!("]{c} {payload}"));
                }
            }
            ServerMessage::ExtendedBytes { code, data } => {
                match code {
                    b's' => self.world.apply_show_dream_name_bytes(&data),
                    b't' => self.world.apply_remove_dream_name_bytes(&data),
                    b'q' | b'r' => {
                        let payload = furcadia_client::encoding::decode_win1252(&data);
                        self.handle_extended_dream(&payload);
                    }
                    b'J' => {
                        let payload = furcadia_client::encoding::decode_win1252(&data);
                        self.handle_use_map(&payload);
                    }
                    b'_' => self.world.apply_avatar_scale(&data),
                    b'o' => {
                        let payload = furcadia_client::encoding::decode_win1252(&data);
                        let slug = payload.trim().trim_start_matches("furc://").trim_matches('/');
                        if !slug.is_empty() {
                            self.current_dream_url = Some(format!("furc://{slug}"));
                        }
                    }
                    b'H' => self.world.apply_avatar_offset(&data),
                    b'O' => self.world.apply_avatar_gloaming(&data),
                    b'}' => {
                        self.world.apply_self_color_bytes(&data);
                        if let Some(uid) = self.world.self_uid {
                            if let Some(av) = self.world.avatars.get(&uid) {
                                let key = shortname_key(&av.name);
                                if !key.is_empty() {
                                    if let Some(color) = accent_color_for_appearance(&av.appearance) {
                                        self.player_accent_colors.insert(key, color);
                                    }
                                }
                            }
                        }
                    }
                    b'M' => self.apply_dynamic_avatar_details(&data),
                    b'f' => {
                        if let Some(info) = WorldState::parse_looked_player_bytes(&data) {
                            let short_name = wire_player_name(&info.name);
                            let portrait_id = self.player_portrait_ids.get(&short_name).copied();
                            self.looked_player = Some(LookedPlayerCard {
                                info,
                                short_name,
                                portrait_id,
                                portrait_tex: None,
                                badge_tex: None,
                                resolved_for: None,
                            });
                        }
                    }
                    b'&' => {
                        let raw = furcadia_client::encoding::decode_win1252(&data);
                        self.handle_portrait_directive(&raw);
                    }
                    b'?' => {
                        for ev in self.pounce.apply_server_pounce(&data, Instant::now()) {
                            match ev {
                                PounceEvent::CameOnline(n) => {
                                    self.push_system_line(
                                        "success",
                                        &format!("[Pounce] {n} is now online!"),
                                    );
                                    if self.notify_pounce {
                                        self.play_sound("online.wav");
                                        self.send_desktop_notification(
                                            "Pounce",
                                            &format!("{n} is now online!"),
                                        );
                                    }
                                }
                                PounceEvent::WentOffline(n) => {
                                    self.push_system_line(
                                        "warning",
                                        &format!("[Pounce] {n} went offline."),
                                    );
                                    if self.notify_pounce {
                                        self.play_sound("offline.wav");
                                    }
                                }
                                PounceEvent::Checked { .. } => {}
                            }
                        }
                    }
                    _ => {}
                }
                if self.show_debug_packets {
                    let c = code as char;
                    self.push_system_line("emit", &format!("]{c} ({} bytes)", data.len()));
                }
            }
            ServerMessage::UseMap(name) => {
                self.handle_use_map(&name);
            }
            ServerMessage::Dragonroar { .. } => {}
            ServerMessage::Raw(raw) => {
                if self.show_debug_packets {
                    self.push_system_line("emit", &format!("[raw] {raw}"));
                }
            }
        }
    }

    fn spellcheck_dict_with_names(&self) -> std::collections::HashSet<String> {
        let mut dict = self.custom_dict_words.clone();
        let mut add_name = |name: &str| {
            for part in name.split(|c: char| !c.is_ascii_alphabetic() && c != '\'') {
                let w = part.trim_matches('\'').to_ascii_lowercase();
                if w.len() >= 2 {
                    dict.insert(w);
                }
            }
        };
        for av in self.world.avatars.values() {
            add_name(&av.name);
        }
        for entry in self.pounce.entries() {
            add_name(&entry.name);
        }
        for w in &self.whispers {
            add_name(&w.peer);
        }
        if let Some(lp) = &self.looked_player {
            add_name(&lp.info.name);
        }
        if let Some(info) = &self.account_info {
            for ch in &info.characters {
                add_name(&ch.name);
            }
        }
        dict
    }

    fn add_word_to_dictionary(&mut self, word: &str) {
        let trimmed = word.trim();
        if trimmed.is_empty() {
            return;
        }
        let lower = trimmed.to_ascii_lowercase();
        if self.custom_dict_words.insert(lower.clone()) {
            if let Some(path) = custom_dictionary_path() {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                {
                    use std::io::Write;
                    let _ = writeln!(f, "{trimmed}");
                }
            }
        }
        self.spell_misspelled
            .retain(|m| !m.word.eq_ignore_ascii_case(trimmed));
    }

    fn send_chat_input(&mut self) {
        let raw = self.chat_input.trim().to_owned();
        if raw.is_empty() || !self.is_connected {
            return;
        }
        self.last_sent_msg = raw.clone();
        self.chat_input.clear();
        self.spell_misspelled.clear();
        self.spell_dirty_at = None;

        let cmd = if let Some(raw_cmd) = raw.strip_prefix('`') {
            ClientCommand::Raw(raw_cmd.to_owned())
        } else if let Some(after_slash) = raw.strip_prefix('/') {
            if after_slash.starts_with("%%") || after_slash.starts_with('%') {
                // Offline message (/%%name msg) or exact-name whisper (/%name msg).
                ClientCommand::Raw(raw)
            } else if let Some(rest) = after_slash.strip_prefix('"') {
                // Quoted whisper: /"furre name" message
                if let Some((target, msg)) = rest.split_once('"') {
                    let msg = msg.trim_start();
                    let disp = target.replace('|', " ");
                    self.last_whisper_target = disp.clone();
                    self.open_whisper_tab(&disp, true);
                    ClientCommand::Whisper {
                        target: wire_player_name(target),
                        message: msg.to_owned(),
                    }
                } else {
                    ClientCommand::Raw(raw)
                }
            } else if let Some(rest) = after_slash
                .strip_prefix("wh ")
                .or_else(|| after_slash.strip_prefix("w "))
            {
                if let Some((target, msg)) = rest.split_once(' ') {
                    let disp = target.replace('|', " ");
                    self.last_whisper_target = disp.clone();
                    self.open_whisper_tab(&disp, true);
                    ClientCommand::Whisper {
                        target: wire_player_name(target),
                        message: msg.to_owned(),
                    }
                } else {
                    ClientCommand::Raw(raw)
                }
            } else if let Some(name) = after_slash.strip_prefix("onln ") {
                ClientCommand::OnlineCheck(wire_player_name(name))
            } else if let Some(name) = after_slash.strip_prefix("glook ") {
                ClientCommand::Glook(wire_player_name(name))
            } else if after_slash.eq_ignore_ascii_case("who") {
                ClientCommand::Who
            } else if let Some(line) = after_slash.strip_prefix("raw ") {
                ClientCommand::Raw(line.to_owned())
            } else if let Some((target, msg)) = after_slash.split_once(' ') {
                let first_lower = target.to_ascii_lowercase();
                if matches!(
                    first_lower.as_str(),
                    "help" | "look" | "ignore" | "banish" | "tempbanish" | "summon" | "join"
                        | "lead" | "follow" | "desc" | "chdesc" | "roll" | "m" | "jg" | "chat"
                        | "afk" | "fdl"
                ) {
                    ClientCommand::Raw(after_slash.to_owned())
                } else {
                    // Furcadia `/furrename message` shorthand whisper
                    let disp = target.replace('|', " ");
                    self.last_whisper_target = disp.clone();
                    self.open_whisper_tab(&disp, true);
                    ClientCommand::Whisper {
                        target: wire_player_name(target),
                        message: msg.to_owned(),
                    }
                }
            } else {
                // Single-word `/command` such as `/info`, `/time`, `/sit`, `/dreamurl`, etc.
                ClientCommand::Raw(after_slash.to_owned())
            }
        } else if raw.starts_with('=') {
            // Channel broadcasts (=talz, =news, =fun, etc.)
            ClientCommand::Raw(raw)
        } else if raw.starts_with(':') {
            if let Some(peer) = self.active_chat_tab.clone().filter(|_| self.whisper_emote_in_whisper) {
                self.last_whisper_target = peer.clone();
                ClientCommand::Whisper {
                    target: wire_player_name(&peer),
                    message: raw,
                }
            } else {
                ClientCommand::Raw(raw)
            }
        } else if Self::is_bare_furcadia_command(&raw) {
            ClientCommand::Raw(raw)
        } else if let Some(peer) = self.active_chat_tab.clone() {
            self.last_whisper_target = peer.clone();
            ClientCommand::Whisper {
                target: wire_player_name(&peer),
                message: raw,
            }
        } else {
            ClientCommand::Say(raw)
        };

        let _ = self.to_net.send(UiToNet::Send(cmd));
    }

    fn is_bare_furcadia_command(raw: &str) -> bool {
        let lower = raw.to_ascii_lowercase();
        let word = lower.split_whitespace().next().unwrap_or("");
        matches!(
            word,
            "whispersoff"
                | "whisperson"
                | "unafk"
                | "desctags"
                | "dreamurl"
                | "buy-cookie"
                | "liedown"
                | "listgroups"
                | "listf"
                | "banish-off"
                | "banish-list"
                | "glook"
                | "chdesc"
                | "tempbanish"
                | "<"
                | ">"
        ) || matches!(lower.as_str(), "ignore off" | "list ignores" | "m 1" | "m 3" | "m 7" | "m 9")
    }
}

impl eframe::App for FurcadiaApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.apply_font_settings(ctx);
        self.window_focused = ctx.input(|i| i.focused);
        if self.close_to_tray && ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }

        self.handle_net_events(ctx);
        self.poll_bg_sessions();

        if self.is_connected {
            if self.pending_vascodagama
                && self
                    .login_ack_at
                    .is_some_and(|t| t.elapsed() >= Duration::from_millis(500))
            {
                self.send_pending_vascodagama();
            }
            if let Some(target) = self.pounce.next_due_check(Instant::now()) {
                let _ = self
                    .to_net
                    .send(UiToNet::Send(ClientCommand::OnlineCheck(wire_player_name(
                        &target,
                    ))));
            }
            if self.pounce.has_unknown() {
                ctx.request_repaint_after(Duration::from_millis(45));
            }
        }

        if self.screen == AppScreen::Connecting {
            let ready_for_world = self.is_connected
                && (self.world.map.is_some()
                    || self
                        .connected_at
                        .is_some_and(|t| t.elapsed() >= Duration::from_secs(4)));
            if ready_for_world {
                self.screen = AppScreen::InGame;
            } else if self.connecting_error.is_none() {
                ctx.request_repaint_after(Duration::from_millis(50));
            }
        }

        if self.bear_logo.is_none() {
            self.bear_logo = Some(logo::load_logo(ctx));
        }

        match self.screen {
            AppScreen::Login => self.render_login_screen(ctx),
            AppScreen::CharacterSelect => self.render_character_select_screen(ctx),
            AppScreen::Connecting => self.render_connecting_screen(ctx),
            AppScreen::InGame => self.render_in_game_screen(ctx),
        }

        if self.settings_open {
            self.render_settings_window(ctx);
        }
        if self.log_viewer_open {
            self.render_log_viewer_window(ctx);
        }
        if self.help_open {
            self.render_help_window(ctx);
        }
        if self.about_open {
            self.render_about_window(ctx);
        }
        self.poll_update_check();
        self.poll_update_install(ctx);
        if self.first_setup_open {
            self.render_first_setup_window(ctx);
        }
        if self.update_popup_open {
            self.render_update_window(ctx);
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.stop_music();
        self.save_config();
    }
}

impl FurcadiaApp {
    fn trigger_update_check(&mut self, ctx: &egui::Context) {
        if self.update_checking {
            return;
        }
        self.update_checking = true;
        self.update_status = Some("Checking GitHub for updates...".to_owned());
        let (tx, rx) = mpsc::channel();
        self.update_rx = Some(rx);
        let ctx_clone = ctx.clone();
        let current_ver = env!("CARGO_PKG_VERSION").to_owned();
        thread::spawn(move || {
            let out = furcadia_client::sys::command("curl")
                .args([
                    "-fsSL",
                    "-H",
                    "Accept: application/vnd.github+json",
                    "-H",
                    "User-Agent: furcadia-native",
                    "--max-time",
                    "10",
                    "https://api.github.com/repos/morganlowe/native-furcadia/releases/latest",
                ])
                .output();
            let msg = match out {
                Ok(o) if o.status.success() => {
                    let body = String::from_utf8_lossy(&o.stdout);
                    if let Some(info) = parse_github_release_json(&body) {
                        if is_newer_version(&current_ver, &info.version) {
                            UpdateCheckMsg::UpdateAvailable(info)
                        } else {
                            UpdateCheckMsg::UpToDate(info.version)
                        }
                    } else {
                        UpdateCheckMsg::Error("Could not parse GitHub release metadata.".to_owned())
                    }
                }
                Ok(o) => {
                    let err = String::from_utf8_lossy(&o.stderr).trim().to_owned();
                    UpdateCheckMsg::Error(if err.is_empty() {
                        format!("HTTP/curl exited {}", o.status)
                    } else {
                        err
                    })
                }
                Err(e) => UpdateCheckMsg::Error(format!("Failed to run curl: {e}")),
            };
            let _ = tx.send(msg);
            ctx_clone.request_repaint();
        });
    }

    fn poll_update_check(&mut self) {
        let msg = match &self.update_rx {
            Some(rx) => match rx.try_recv() {
                Ok(m) => Some(m),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.update_checking = false;
                    self.update_rx = None;
                    None
                }
            },
            None => None,
        };
        if let Some(m) = msg {
            self.update_checking = false;
            self.update_rx = None;
            match m {
                UpdateCheckMsg::UpToDate(_latest) => {
                    self.update_available = None;
                    self.update_status = Some(format!(
                        "Up to date (v{}).",
                        env!("CARGO_PKG_VERSION")
                    ));
                }
                UpdateCheckMsg::UpdateAvailable(info) => {
                    self.update_status = Some(format!("Update available: {}", info.tag));
                    self.update_available = Some(info);
                    self.update_popup_open = true;
                }
                UpdateCheckMsg::Error(err) => {
                    self.update_status = Some(format!("Update check failed: {err}"));
                }
            }
        }
    }

    fn trigger_update_install(&mut self, ctx: &egui::Context) {
        if matches!(
            self.update_install_state,
            UpdateInstallState::Downloading | UpdateInstallState::Installing
        ) {
            return;
        }
        let Some(asset_url) = self
            .update_available
            .as_ref()
            .and_then(|info| info.asset_url.clone())
        else {
            self.update_install_state = UpdateInstallState::Failed(
                "No matching installer asset found for this platform.".to_owned(),
            );
            return;
        };

        self.update_install_state = UpdateInstallState::Downloading;
        let (tx, rx) = mpsc::channel();
        self.update_install_rx = Some(rx);
        let ctx_clone = ctx.clone();

        thread::spawn(move || {
            let filename = update_download_filename(&asset_url);
            let dest_path = std::env::temp_dir().join(&filename);
            let dest_str = dest_path.to_string_lossy().into_owned();

            let dl = furcadia_client::sys::command("curl")
                .args(["-fsSL", "--max-time", "180", "-o", &dest_str, &asset_url])
                .output();
            match dl {
                Ok(o) if o.status.success() => {}
                Ok(o) => {
                    let _ = std::fs::remove_file(&dest_path);
                    let err = String::from_utf8_lossy(&o.stderr).trim().to_owned();
                    let _ = tx.send(UpdateInstallMsg::Failed(if err.is_empty() {
                        format!("Download failed (curl exited {}).", o.status)
                    } else {
                        format!("Download failed: {err}")
                    }));
                    ctx_clone.request_repaint();
                    return;
                }
                Err(e) => {
                    let _ = tx.send(UpdateInstallMsg::Failed(format!(
                        "Failed to run curl: {e}"
                    )));
                    ctx_clone.request_repaint();
                    return;
                }
            }

            #[cfg(target_os = "windows")]
            {
                let _ = tx.send(UpdateInstallMsg::LaunchInstallerAndExit(dest_path));
                ctx_clone.request_repaint();
                return;
            }

            #[cfg(not(target_os = "windows"))]
            {
                let _ = tx.send(UpdateInstallMsg::Installing);
                ctx_clone.request_repaint();

                let is_arch = filename.ends_with(".pkg.tar.zst");
                let is_deb = filename.ends_with(".deb");
                let install_res = if is_arch {
                    std::process::Command::new("pkexec")
                        .args(["pacman", "-U", "--noconfirm", &dest_str])
                        .output()
                } else if is_deb {
                    std::process::Command::new("pkexec")
                        .args(["apt-get", "install", "-y", &dest_str])
                        .output()
                } else {
                    let _ = std::fs::remove_file(&dest_path);
                    let _ = tx.send(UpdateInstallMsg::Failed(format!(
                        "Unsupported package format: {filename}"
                    )));
                    ctx_clone.request_repaint();
                    return;
                };

                let _ = std::fs::remove_file(&dest_path);
                match install_res {
                    Ok(o) if o.status.success() => {
                        let _ = tx.send(UpdateInstallMsg::Installed);
                    }
                    Ok(o) => {
                        let err = String::from_utf8_lossy(&o.stderr).trim().to_owned();
                        let msg = if o.status.code() == Some(126) || o.status.code() == Some(127) {
                            "Authorization cancelled or pkexec unavailable.".to_owned()
                        } else if err.is_empty() {
                            format!("Installer exited with {}.", o.status)
                        } else {
                            format!("Install failed: {err}")
                        };
                        let _ = tx.send(UpdateInstallMsg::Failed(msg));
                    }
                    Err(e) => {
                        let _ = tx.send(UpdateInstallMsg::Failed(format!(
                            "Failed to launch pkexec: {e}"
                        )));
                    }
                }
                ctx_clone.request_repaint();
            }
        });
    }

    fn poll_update_install(&mut self, #[allow(unused_variables)] ctx: &egui::Context) {
        let msg = match &self.update_install_rx {
            Some(rx) => match rx.try_recv() {
                Ok(m) => Some(m),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if matches!(
                        self.update_install_state,
                        UpdateInstallState::Downloading | UpdateInstallState::Installing
                    ) {
                        self.update_install_state = UpdateInstallState::Failed(
                            "Update worker stopped unexpectedly.".to_owned(),
                        );
                    }
                    self.update_install_rx = None;
                    None
                }
            },
            None => None,
        };
        if let Some(m) = msg {
            match m {
                #[cfg(not(target_os = "windows"))]
                UpdateInstallMsg::Installing => {
                    self.update_install_state = UpdateInstallState::Installing;
                }
                #[cfg(not(target_os = "windows"))]
                UpdateInstallMsg::Installed => {
                    self.update_install_rx = None;
                    self.update_install_state = UpdateInstallState::Installed;
                    self.update_status = Some("Update installed! Restart to apply.".to_owned());
                }
                #[cfg(target_os = "windows")]
                UpdateInstallMsg::LaunchInstallerAndExit(path) => {
                    self.update_install_rx = None;
                    match furcadia_client::sys::command(&path)
                        .arg("/CLOSEAPPLICATIONS")
                        .spawn()
                    {
                        Ok(_) => {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        Err(e) => {
                            self.update_install_state = UpdateInstallState::Failed(format!(
                                "Could not launch installer: {e}"
                            ));
                        }
                    }
                }
                UpdateInstallMsg::Failed(err) => {
                    self.update_install_rx = None;
                    self.update_install_state = UpdateInstallState::Failed(err);
                }
            }
        }
    }

    fn render_first_setup_window(&mut self, ctx: &egui::Context) {
        let accent = self.theme.accent();
        let mut open = self.first_setup_open;
        egui::Window::new("Welcome — First-Time Setup")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .default_width(440.0)
            .show(ctx, |ui| {
                ui.heading(RichText::new("Furcadia Native Client").color(accent));
                ui.label(
                    RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION")))
                        .size(12.0)
                        .color(Color32::LIGHT_GRAY),
                );
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(6.0);

                ui.label(RichText::new("Automatic Updates").strong().color(accent));
                ui.checkbox(
                    &mut self.check_updates_on_startup,
                    "Automatically check GitHub for updates on startup",
                );
                ui.label(
                    RichText::new(
                        "You can change this anytime in Settings → Pounce, Tray & Updates.",
                    )
                    .size(11.0)
                    .color(Color32::GRAY),
                );

                ui.add_space(10.0);
                ui.label(RichText::new("Import Existing Furcadia Data").strong().color(accent));
                ui.label(
                    RichText::new(
                        "This imports available patches/assets, your Pounce watchlist, and chat and whisper logs. It also scans character INI files. The app checks Documents/Furcadia, My Documents/Furcadia, OneDrive/Documents/Furcadia, common Furcadia install folders, and Wine Documents/AppData folders.",
                    )
                    .size(11.5)
                    .color(Color32::LIGHT_GRAY),
                );
                if let Some(root) = &self.assets.root {
                    ui.colored_label(
                        Color32::from_rgb(130, 225, 145),
                        format!("Local assets found: {}", root.display()),
                    );
                }
                if self.import_source_detected {
                    ui.colored_label(
                        Color32::from_rgb(130, 225, 145),
                        format!("Existing Furcadia data found: {}", self.import_path_input),
                    );
                } else {
                    ui.label(
                        RichText::new(
                            "No Furcadia data found in the usual Documents or Wine folders. Choose your Windows Furcadia folder to import existing files.",
                        )
                        .size(11.5)
                        .color(Color32::LIGHT_GRAY),
                    );
                }
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.import_path_input)
                            .desired_width(270.0),
                    );
                    if ui.button("Browse...").clicked() {
                        self.browse_import_directory();
                    }
                });
                ui.horizontal(|ui| {
                    let import_path_exists = std::path::Path::new(self.import_path_input.trim()).is_dir();
                    if ui
                        .add_enabled(import_path_exists, egui::Button::new("Yes, import now"))
                        .clicked()
                    {
                        self.run_windows_import();
                    }
                    if ui.button("Not now").clicked() {
                        self.import_report = Some(
                            "You can import later from Settings → Import Windows Furcadia & Pounce Data.".to_owned(),
                        );
                    }
                });
                if let Some(report) = &self.import_report {
                    ui.colored_label(Color32::from_rgb(130, 225, 145), report);
                }

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Finish Setup")
                                    .strong()
                                    .color(Color32::from_rgb(20, 14, 6)),
                            )
                            .fill(accent),
                        )
                        .clicked()
                    {
                        self.first_setup_done = true;
                        self.first_setup_open = false;
                        self.save_config();
                        if self.check_updates_on_startup {
                            self.trigger_update_check(ctx);
                        }
                    }
                });
            });
        if !open && self.first_setup_open {
            self.first_setup_done = true;
            self.first_setup_open = false;
            self.save_config();
        }
    }

    fn render_update_window(&mut self, ctx: &egui::Context) {
        let Some(info) = self.update_available.clone() else {
            self.update_popup_open = false;
            return;
        };
        let accent = self.theme.accent();
        let mut open = self.update_popup_open;
        egui::Window::new("Update Available")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .default_width(380.0)
            .show(ctx, |ui| {
                ui.heading(RichText::new(format!("New version {} is available!", info.tag)).color(accent));
                ui.label(
                    RichText::new(format!(
                        "You are currently running v{}.",
                        env!("CARGO_PKG_VERSION")
                    ))
                    .color(Color32::LIGHT_GRAY),
                );
                ui.add_space(10.0);
                match &self.update_install_state {
                    UpdateInstallState::Idle => {
                        ui.horizontal_wrapped(|ui| {
                            if info.asset_url.is_some()
                                && ui
                                    .add(
                                        egui::Button::new(
                                            RichText::new("Download & Install")
                                                .strong()
                                                .color(Color32::from_rgb(20, 14, 6)),
                                        )
                                        .fill(accent),
                                    )
                                    .clicked()
                            {
                                self.trigger_update_install(ctx);
                            }
                            if ui.button("Open Release Page").clicked() {
                                ctx.open_url(egui::OpenUrl::new_tab(&info.html_url));
                            }
                            if ui.button("Later").clicked() {
                                self.update_popup_open = false;
                            }
                        });
                    }
                    UpdateInstallState::Downloading => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Downloading update package...");
                        });
                    }
                    UpdateInstallState::Installing => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Installing update (waiting for authorization)...");
                        });
                    }
                    UpdateInstallState::Installed => {
                        ui.label(
                            RichText::new(
                                "Update installed! Restart the client to run the new version.",
                            )
                            .color(Color32::from_rgb(120, 220, 140)),
                        );
                        ui.add_space(8.0);
                        ui.horizontal_wrapped(|ui| {
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("Restart Now")
                                            .strong()
                                            .color(Color32::from_rgb(20, 14, 6)),
                                    )
                                    .fill(accent),
                                )
                                .clicked()
                            {
                                if let Ok(exe) = std::env::current_exe() {
                                    let _ = std::process::Command::new(exe).spawn();
                                }
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                            if ui.button("Close").clicked() {
                                self.update_popup_open = false;
                            }
                        });
                    }
                    UpdateInstallState::Failed(err) => {
                        ui.label(
                            RichText::new(err)
                                .size(12.0)
                                .color(Color32::from_rgb(240, 110, 110)),
                        );
                        ui.add_space(8.0);
                        ui.horizontal_wrapped(|ui| {
                            if info.asset_url.is_some()
                                && ui
                                    .add(
                                        egui::Button::new(
                                            RichText::new("Retry Install")
                                                .strong()
                                                .color(Color32::from_rgb(20, 14, 6)),
                                        )
                                        .fill(accent),
                                    )
                                    .clicked()
                            {
                                self.trigger_update_install(ctx);
                            }
                            if ui.button("Open Release Page").clicked() {
                                ctx.open_url(egui::OpenUrl::new_tab(&info.html_url));
                            }
                            if ui.button("Later").clicked() {
                                self.update_popup_open = false;
                            }
                        });
                    }
                }
            });
        if !open {
            self.update_popup_open = false;
        }
    }

    fn render_settings_window(&mut self, ctx: &egui::Context) {
        let mut open = self.settings_open;
        let accent = self.theme.accent();
        egui::Window::new("Native Client Settings")
            .open(&mut open)
            .resizable(true)
            .default_size(Vec2::new(560.0, 440.0))
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (idx, label) in [
                        (0usize, "Appearance & Fonts"),
                        (1, "Animations & Map"),
                        (2, "Whispers & Chat"),
                        (3, "Sounds & Notify"),
                        (4, "Pounce, Tray & Updates"),
                        (5, "Logs"),
                        (6, "Windows Importer"),
                    ] {
                        if ui.selectable_label(self.settings_tab == idx, label).clicked() {
                            self.settings_tab = idx;
                        }
                    }
                });
                ui.separator();

                egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| match self.settings_tab {
                    0 => {
                        ui.label(RichText::new("Dream Window Skin").strong().color(accent));
                        ui.horizontal_wrapped(|ui| {
                            for t in [
                                DreamTheme::ClassicPurple,
                                DreamTheme::DragonHoard,
                                DreamTheme::KitterMagic,
                                DreamTheme::FerianWylde,
                            ] {
                                if ui.radio(self.theme == t, t.label()).clicked() {
                                    self.theme = t;
                                    self.save_config();
                                }
                            }
                        });
                        ui.separator();
                        ui.label(RichText::new("Fonts").strong().color(accent));
                        ui.horizontal(|ui| {
                            ui.label("Font Family:");
                            for fam in ["Proportional", "Monospace", "Fixedsys"] {
                                if ui.radio(self.font_family == fam, fam).clicked() {
                                    self.font_family = fam.to_owned();
                                    self.save_config();
                                }
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Font Size:");
                            if ui
                                .add(egui::Slider::new(&mut self.font_size, 10..=22).suffix(" px"))
                                .changed()
                            {
                                self.save_config();
                            }
                        });
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label("Left-panel portrait size:");
                            if ui
                                .add(egui::Slider::new(&mut self.portrait_size, 48.0..=160.0).suffix(" px"))
                                .changed()
                            {
                                self.save_config();
                            }
                            if ui.small_button("Reset").clicked() {
                                self.portrait_size = 64.0;
                                self.save_config();
                            }
                        });
                    }
                    1 => {
                        ui.label(RichText::new("Animations (F11)").strong().color(accent));
                        ui.horizontal(|ui| {
                            for mode in ["Full", "Half", "Off"] {
                                if ui.radio(self.anim_mode == mode, mode).clicked() {
                                    self.anim_mode = mode.to_owned();
                                    self.save_config();
                                }
                            }
                        });
                        ui.separator();
                        ui.label(RichText::new("Map & Avatar Display").strong().color(accent));
                        if ui
                            .checkbox(&mut self.show_avatar_names, "Show names above avatars (Tab)")
                            .changed()
                        {
                            self.save_config();
                        }
                        if ui
                            .checkbox(
                                &mut self.limit_view_range,
                                "Limit map view to Furcadia speech/sight range",
                            )
                            .changed()
                        {
                            self.save_config();
                        }
                        if ui
                            .checkbox(&mut self.allow_dream_skins, "Allow dream custom window skins")
                            .changed()
                        {
                            self.save_config();
                        }
                        if ui
                            .checkbox(&mut self.show_debug_packets, "Log raw/extended packets to chat")
                            .changed()
                        {
                            self.save_config();
                        }
                    }
                    2 => {
                        ui.label(RichText::new("Whispers & Chat Behavior").strong().color(accent));
                        if ui
                            .checkbox(
                                &mut self.whisper_open_tab_on_incoming,
                                "Automatically open whisper tab on incoming whisper",
                            )
                            .changed()
                        {
                            self.save_config();
                        }
                        if ui
                            .checkbox(
                                &mut self.whisper_emote_in_whisper,
                                "Send :emotes inside active whisper tab as whispers",
                            )
                            .changed()
                        {
                            self.save_config();
                        }
                        ui.separator();
                        ui.label(RichText::new("Dice Roll Button").strong().color(accent));
                        ui.horizontal(|ui| {
                            ui.label("Roll expression:");
                            if ui
                                .add(
                                    egui::TextEdit::singleline(&mut self.dice_roll)
                                        .hint_text("1d6")
                                        .desired_width(90.0),
                                )
                                .changed()
                            {
                                self.save_config();
                            }
                            for preset in ["1d6", "1d20", "1d100", "2d6"] {
                                if ui
                                    .selectable_label(self.dice_roll.trim() == preset, preset)
                                    .clicked()
                                {
                                    self.dice_roll = preset.to_owned();
                                    self.save_config();
                                }
                            }
                        });
                    }
                    3 => {
                        ui.label(RichText::new("Audio & Desktop Notifications").strong().color(accent));
                        if ui
                            .checkbox(&mut self.sound_enabled, "Sound effects (.wav) [Ctrl+P]")
                            .changed()
                        {
                            self.save_config();
                        }
                        if ui
                            .checkbox(&mut self.music_enabled, "Background music [Ctrl+O]")
                            .changed()
                        {
                            self.save_config();
                        }
                        ui.separator();
                        if ui
                            .checkbox(&mut self.notify_enabled, "Enable desktop notifications")
                            .changed()
                        {
                            self.save_config();
                        }
                        if ui
                            .checkbox(&mut self.notify_whisper, "Notify on incoming whispers")
                            .changed()
                        {
                            self.save_config();
                        }
                        if ui
                            .checkbox(&mut self.notify_pounce, "Notify when Pounce friend comes online")
                            .changed()
                        {
                            self.save_config();
                        }
                        if ui
                            .checkbox(&mut self.notify_mention, "Notify when your name is mentioned")
                            .changed()
                        {
                            self.save_config();
                        }
                        if ui
                            .checkbox(&mut self.notify_only_unfocused, "Only notify when window is unfocused")
                            .changed()
                        {
                            self.save_config();
                        }
                    }
                    4 => {
                        ui.label(RichText::new("Pounce & Window Behavior").strong().color(accent));
                        if ui
                            .checkbox(&mut self.pounce_visible, "Show Pounce panel")
                            .changed()
                        {
                            self.save_config();
                        }
                        if ui
                            .checkbox(&mut self.pounce_detached, "Detach Pounce into separate window")
                            .changed()
                        {
                            self.save_config();
                        }
                        ui.separator();
                        if ui
                            .checkbox(&mut self.minimize_to_tray, "Minimize to tray/panel")
                            .changed()
                        {
                            self.save_config();
                        }
                        if ui
                            .checkbox(&mut self.close_to_tray, "Close button minimizes instead of quitting")
                            .changed()
                        {
                            self.save_config();
                        }
                        ui.separator();
                        ui.label(RichText::new("GitHub Updates").strong().color(accent));
                        if ui
                            .checkbox(
                                &mut self.check_updates_on_startup,
                                "Check GitHub for updates on startup",
                            )
                            .changed()
                        {
                            self.save_config();
                        }
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    !self.update_checking,
                                    egui::Button::new("Check for Updates Now"),
                                )
                                .clicked()
                            {
                                self.trigger_update_check(ctx);
                            }
                            if self.update_available.is_some()
                                && ui.button("Install Update...").clicked()
                            {
                                self.update_popup_open = true;
                            }
                        });
                        if let Some(status) = &self.update_status {
                            ui.label(RichText::new(status).size(11.5).color(Color32::LIGHT_GRAY));
                        }
                    }
                    5 => {
                        ui.label(RichText::new("Chat & Whisper Logs").strong().color(accent));
                        if ui
                            .checkbox(&mut self.logging_enabled, "Save daily chat logs per character")
                            .changed()
                        {
                            self.save_config();
                        }
                        if ui
                            .checkbox(&mut self.log_whispers, "Save separate per-player whisper logs")
                            .changed()
                        {
                            self.save_config();
                        }
                        ui.horizontal(|ui| {
                            ui.label("Keep logs:");
                            for (days, label) in [
                                (0u32, "Forever"),
                                (7, "7 days"),
                                (30, "30 days"),
                                (90, "90 days"),
                                (365, "1 year"),
                            ] {
                                if ui.radio(self.log_retention_days == days, label).clicked() {
                                    self.log_retention_days = days;
                                    self.save_config();
                                }
                            }
                        });
                        if let Some(dir) = default_logs_dir() {
                            ui.label(
                                RichText::new(format!("Log directory: {}", dir.display()))
                                    .size(11.0)
                                    .color(Color32::GRAY),
                            );
                        }
                        if ui.button("Open Log Viewer").clicked() {
                            self.open_log_viewer();
                        }
                    }
                    _ => {
                        ui.label(
                            RichText::new("Import Windows Furcadia & Pounce Data")
                                .strong()
                                .color(accent),
                        );
                        ui.label(
                            RichText::new(
                                "Point to your Windows/Wine Furcadia install or Documents/Furcadia folder to import patches, sounds, fonts, Pounce watchlists, character INIs, and logs.",
                            )
                            .size(11.5)
                            .color(Color32::LIGHT_GRAY),
                        );
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.import_path_input)
                                    .desired_width((ui.available_width() - 88.0).max(160.0)),
                            );
                            if ui.button("Browse...").clicked() {
                                self.browse_import_directory();
                            }
                        });
                        if ui.button("Import Windows Furcadia Data").clicked() {
                            self.run_windows_import();
                        }
                        if let Some(report) = &self.import_report {
                            ui.colored_label(Color32::from_rgb(130, 225, 145), report);
                        }
                    }
                });
            });
        self.settings_open = open;
    }

    fn render_log_viewer_window(&mut self, ctx: &egui::Context) {
        let mut open = self.log_viewer_open;
        let accent = self.theme.accent();
        egui::Window::new("Furcadia Chat & Whisper Logs")
            .open(&mut open)
            .resizable(true)
            .default_size(Vec2::new(660.0, 440.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .checkbox(&mut self.logging_enabled, "Logging enabled")
                        .changed()
                    {
                        self.save_config();
                    }
                    if ui.button("Refresh").clicked() {
                        self.refresh_log_viewer_files();
                    }
                    if ui.button("Open Logs Folder").clicked() {
                        if let Some(dir) = default_logs_dir() {
                            let _ = std::fs::create_dir_all(&dir);
                            #[cfg(target_os = "windows")]
                            let _ = furcadia_client::sys::command("explorer").arg(&dir).spawn();
                            #[cfg(not(target_os = "windows"))]
                            let _ = std::process::Command::new("xdg-open").arg(&dir).spawn();
                        }
                    }
                });
                ui.separator();

                let chars = default_logs_dir()
                    .map(|d| list_log_characters(&d))
                    .unwrap_or_default();
                if chars.is_empty() {
                    ui.label(
                        RichText::new("No saved logs yet. Chat or whisper while logging is enabled.")
                            .italics()
                            .color(Color32::GRAY),
                    );
                    return;
                }

                ui.horizontal(|ui| {
                    ui.label("Character:");
                    for ch in &chars {
                        if ui.selectable_label(&self.log_viewer_char == ch, ch).clicked() {
                            self.log_viewer_char = ch.clone();
                            self.refresh_log_viewer_files();
                        }
                    }
                });
                ui.separator();

                let avail_h = ui.available_height().max(180.0);
                ui.columns(2, |cols| {
                    cols[0].set_max_width(210.0);
                    egui::ScrollArea::vertical()
                        .id_salt("log_files_list")
                        .max_height(avail_h)
                        .show(&mut cols[0], |ui| {
                            let mut picked = None;
                            for path in &self.log_viewer_files {
                                let label = path
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("log.txt");
                                let is_whisper = path
                                    .parent()
                                    .and_then(|p| p.file_name())
                                    .and_then(|n| n.to_str())
                                    .is_some_and(|n| n == "whispers");
                                let display = if is_whisper {
                                    format!("💬 {label}")
                                } else {
                                    format!("📄 {label}")
                                };
                                let sel = self.log_viewer_selected.as_ref() == Some(path);
                                if ui.selectable_label(sel, display).clicked() {
                                    picked = Some(path.clone());
                                }
                            }
                            if let Some(p) = picked {
                                self.load_log_file_into_viewer(p);
                            }
                        });

                    egui::ScrollArea::vertical()
                        .id_salt("log_file_body")
                        .max_height(avail_h)
                        .show(&mut cols[1], |ui| {
                            ui.label(
                                RichText::new(
                                    self.log_viewer_selected
                                        .as_ref()
                                        .and_then(|p| p.file_name())
                                        .and_then(|n| n.to_str())
                                        .unwrap_or("Select a log file"),
                                )
                                .strong()
                                .color(accent),
                            );
                            ui.add(
                                egui::TextEdit::multiline(&mut self.log_viewer_content.as_str())
                                    .desired_width(f32::INFINITY)
                                    .font(egui::TextStyle::Monospace),
                            );
                        });
                });
            });
        self.log_viewer_open = open;
    }

    fn render_bg_session_tab_bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, current_tab_label: &str) {
        if self.bg_sessions.is_empty() {
            return;
        }
        ui.horizontal_wrapped(|ui| {
            let mut switch_to: Option<usize> = None;
            for (idx, bg) in self.bg_sessions.iter().enumerate() {
                let lbl = if bg.unread_count > 0 {
                    format!("{} ({})", bg.char_name, bg.unread_count)
                } else {
                    bg.char_name.clone()
                };
                let clicked = if bg.unread_count > 0 {
                    ui.add(
                        egui::Button::new(
                            RichText::new(lbl)
                                .strong()
                                .color(Color32::from_rgb(20, 14, 6)),
                        )
                        .fill(Color32::from_rgb(235, 180, 60))
                        .stroke(Stroke::new(1.5_f32, Color32::from_rgb(255, 225, 130))),
                    )
                    .clicked()
                } else {
                    ui.selectable_label(false, RichText::new(lbl).size(12.0)).clicked()
                };
                if clicked {
                    switch_to = Some(idx);
                }
            }
            let _ = ui.selectable_label(true, RichText::new(current_tab_label).size(12.0).strong());
            if ui
                .small_button("×")
                .on_hover_text("Close this tab and return to your logged-in character")
                .clicked()
            {
                switch_to = Some(0);
            }
            if let Some(idx) = switch_to {
                self.switch_to_bg_session(idx, ctx);
            }
        });
        ui.separator();
    }

    fn render_login_screen(&mut self, ctx: &egui::Context) {
        if !self.bg_sessions.is_empty() {
            egui::TopBottomPanel::top("login_tab_bar")
                .frame(egui::Frame::none().fill(BG_PURPLE).inner_margin(8.0))
                .show(ctx, |ui| {
                    self.render_bg_session_tab_bar(ui, ctx, "Login (+Char Tab)");
                });
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(BG_PURPLE))
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(40.0);
                    ui.label(
                        RichText::new("FURCADIA")
                            .size(44.0)
                            .strong()
                            .color(GOLD),
                    );
                    ui.label(
                        RichText::new("The Second Dreaming  •  Native Furcadia Client")
                            .size(16.0)
                            .italics()
                            .color(Color32::from_rgb(225, 200, 145)),
                    );
                    ui.add_space(28.0);

                    egui::Frame::none()
                        .fill(CARD_PURPLE)
                        .stroke(Stroke::new(2.0_f32, GOLD_DIM))
                        .rounding(8.0)
                        .inner_margin(24.0)
                        .show(ui, |ui| {
                            ui.set_width(360.0);
                            ui.label(RichText::new("Email Address").color(GOLD));
                            let email_resp = ui.add(
                                egui::TextEdit::singleline(&mut self.email)
                                    .hint_text("you@example.com")
                                    .desired_width(350.0),
                            );
                            ui.add_space(10.0);

                            ui.label(RichText::new("Password").color(GOLD));
                            let pass_resp = ui.add(
                                egui::TextEdit::singleline(&mut self.password)
                                    .password(true)
                                    .desired_width(350.0),
                            );
                            let enter_pressed = (email_resp.lost_focus() || pass_resp.lost_focus())
                                && ui.input(|i| i.key_pressed(egui::Key::Enter));

                            ui.add_space(16.0);
                            let login_btn = egui::Button::new(
                                RichText::new(if self.auth_busy {
                                    "LOGGING IN..."
                                } else {
                                    "LOGIN"
                                })
                                .size(18.0)
                                .strong()
                                .color(Color32::WHITE),
                            )
                            .fill(Color32::from_rgb(56, 38, 110))
                            .stroke(Stroke::new(1.5_f32, GOLD))
                            .min_size(Vec2::new(350.0, 40.0));

                            if ui.add_enabled(!self.auth_busy, login_btn).clicked()
                                || (enter_pressed && !self.auth_busy)
                            {
                                self.trigger_login();
                            }

                            if let Some(err) = &self.auth_error {
                                ui.add_space(8.0);
                                ui.colored_label(Color32::from_rgb(245, 110, 110), err);
                            }

                            ui.add_space(12.0);
                            let mut changed = false;
                            changed |= ui
                                .checkbox(&mut self.remember_email, "Remember my email address")
                                .changed();
                            changed |= ui
                                .checkbox(&mut self.remember_password, "Remember my password")
                                .changed();
                            changed |= ui
                                .checkbox(&mut self.auto_login, "Log in automatically")
                                .changed();
                            if changed {
                                self.save_config();
                            }

                            ui.add_space(12.0);
                            ui.separator();
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                ui.hyperlink_to(
                                    RichText::new("Create Account").color(GOLD),
                                    "https://www.furcadia.com/create/",
                                );
                                ui.label("•");
                                ui.hyperlink_to(
                                    RichText::new("Forgot Password?").color(GOLD),
                                    "https://www.furcadia.com/accounts/forgot.php",
                                );
                                ui.label("•");
                                ui.hyperlink_to(
                                    RichText::new("User Agreement").color(GOLD),
                                    "https://www.furcadia.com/about/tos.html",
                                );
                            });
                        });

                    ui.add_space(20.0);
                    ui.label(
                        RichText::new("Let your imagination soar!  •  \"Teen+\" Environment")
                            .size(12.0)
                            .color(Color32::from_rgb(175, 165, 200)),
                    );
                    ui.add_space(12.0);
                    if let Some(tex) = &self.bear_logo {
                        ui.add(
                            egui::Image::new(tex)
                                .max_size(Vec2::splat(72.0))
                                .rounding(6.0),
                        );
                    }
                    ui.add_space(4.0);
                    if ui
                        .small_button("About & Open Source Credits")
                        .clicked()
                    {
                        self.about_open = !self.about_open;
                    }
                });
            });
    }

    fn render_character_select_screen(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("char_select_top")
            .frame(egui::Frame::none().fill(BG_PURPLE).inner_margin(8.0))
            .show(ctx, |ui| {
                self.render_bg_session_tab_bar(ui, ctx, "New Char (+Char Tab)");
                ui.horizontal(|ui| {
                    if ui.button("← Switch Account").clicked() {
                        self.screen = AppScreen::Login;
                    }
                    ui.separator();
                    ui.label(
                        RichText::new(format!("Account: {}", self.email))
                            .color(GOLD)
                            .strong(),
                    );
                });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(BG_PURPLE).inner_margin(12.0))
            .show(ctx, |ui| {
                ui.columns(2, |cols| {
                    // Left Column: Announcements
                    egui::Frame::none()
                        .fill(CARD_PURPLE)
                        .stroke(Stroke::new(1.5_f32, GOLD_DIM))
                        .rounding(8.0)
                        .inner_margin(12.0)
                        .show(&mut cols[0], |ui| {
                            ui.heading(RichText::new("Announcements").color(GOLD));
                            ui.separator();
                            egui::ScrollArea::vertical()
                                .id_salt("announcements_scroll")
                                .show(ui, |ui| {
                                    if self.news.is_empty() {
                                        ui.label("Loading Furcadia announcements...");
                                    } else {
                                        for entry in &self.news {
                                            ui.horizontal(|ui| {
                                                ui.label(
                                                    RichText::new(&entry.title)
                                                        .strong()
                                                        .color(Color32::from_rgb(230, 225, 245)),
                                                );
                                                ui.with_layout(
                                                    egui::Layout::right_to_left(egui::Align::Center),
                                                    |ui| {
                                                        ui.label(
                                                            RichText::new(&entry.date)
                                                                .size(11.0)
                                                                .color(Color32::LIGHT_GRAY),
                                                        );
                                                    },
                                                );
                                            });
                                            ui.label(
                                                RichText::new(&entry.body)
                                                    .color(Color32::from_rgb(200, 195, 220)),
                                            );
                                            if let Some(url) = &entry.url {
                                                ui.hyperlink_to(
                                                    RichText::new("READ MORE").color(GOLD).strong(),
                                                    url,
                                                );
                                            }
                                            ui.add_space(6.0);
                                            ui.separator();
                                            ui.add_space(4.0);
                                        }
                                    }
                                });
                        });

                    // Right Column: Character Selection & Edit
                    egui::Frame::none()
                        .fill(CARD_PURPLE)
                        .stroke(Stroke::new(1.5_f32, GOLD_DIM))
                        .rounding(8.0)
                        .inner_margin(12.0)
                        .show(&mut cols[1], |ui| {
                            ui.horizontal(|ui| {
                                ui.heading(RichText::new("Select Character").color(GOLD));
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.radio_value(&mut self.char_sort, CharSort::Alphabetical, "A-Z");
                                        ui.radio_value(&mut self.char_sort, CharSort::Recent, "Recent");
                                    },
                                );
                            });
                            ui.separator();

                            let mut sorted_indices: Vec<usize> = self
                                .account_info
                                .as_ref()
                                .map(|i| (0..i.characters.len()).collect())
                                .unwrap_or_default();

                            if let Some(info) = &self.account_info {
                                let saved_norm = self.saved_character.trim().replace('|', " ");
                                match self.char_sort {
                                    CharSort::Recent => sorted_indices.sort_by(|&a, &b| {
                                        let a_saved = !saved_norm.is_empty()
                                            && info.characters[a]
                                                .name
                                                .replace('|', " ")
                                                .eq_ignore_ascii_case(&saved_norm);
                                        let b_saved = !saved_norm.is_empty()
                                            && info.characters[b]
                                                .name
                                                .replace('|', " ")
                                                .eq_ignore_ascii_case(&saved_norm);
                                        b_saved.cmp(&a_saved).then_with(|| {
                                            info.characters[b]
                                                .last_login
                                                .cmp(&info.characters[a].last_login)
                                        })
                                    }),
                                    CharSort::Alphabetical => sorted_indices.sort_by(|&a, &b| {
                                        info.characters[a]
                                            .name
                                            .to_ascii_lowercase()
                                            .cmp(&info.characters[b].name.to_ascii_lowercase())
                                    }),
                                }
                            }

                            let tiles: Vec<(usize, String, String, String)> = self
                                .account_info
                                .as_ref()
                                .map(|info| {
                                    sorted_indices
                                        .iter()
                                        .map(|&i| {
                                            let c = &info.characters[i];
                                            (i, c.id.clone(), c.name.replace('|', " "), c.colors.clone())
                                        })
                                        .collect()
                                })
                                .unwrap_or_default();
                            let mut clicked_idx: Option<usize> = None;
                            let mut play_idx: Option<usize> = None;
                            egui::Frame::none()
                                .fill(INPUT_PURPLE)
                                .stroke(Stroke::new(1.0_f32, GOLD_DIM))
                                .rounding(6.0)
                                .inner_margin(6.0)
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    egui::ScrollArea::vertical()
                                        .id_salt("char_list_scroll")
                                        .max_height(300.0)
                                        .min_scrolled_height(300.0)
                                        .show(ui, |ui| {
                                            if self.account_info.is_none() {
                                                ui.label("Loading characters...");
                                                return;
                                            }
                                            if tiles.is_empty() {
                                                ui.label("No characters found on this account.");
                                                return;
                                            }
                                            ui.horizontal_wrapped(|ui| {
                                                ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                                                for (idx, id, name, colors) in &tiles {
                                                    let selected = *idx == self.selected_char_idx;
                                                    let portrait = self.character_portrait(ctx, id, colors);
                                                    let tile_size = Vec2::new(112.0, 134.0);
                                                    let (rect, resp) =
                                                        ui.allocate_exact_size(tile_size, Sense::click());
                                                    let painter = ui.painter_at(rect);
                                                    let bg = if selected {
                                                        Color32::from_rgb(78, 62, 140)
                                                    } else if resp.hovered() {
                                                        Color32::from_rgb(48, 36, 92)
                                                    } else {
                                                        Color32::TRANSPARENT
                                                    };
                                                    painter.rect_filled(rect, 4.0, bg);
                                                    let img_rect = Rect::from_min_size(
                                                        rect.min + Vec2::new(8.0, 4.0),
                                                        Vec2::new(96.0, 96.0),
                                                    );
                                                    if let Some(tex) = portrait {
                                                        painter.image(
                                                            tex.id(),
                                                            img_rect,
                                                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                                                            Color32::WHITE,
                                                        );
                                                    } else {
                                                        painter.rect_filled(
                                                            img_rect,
                                                            4.0,
                                                            Color32::from_rgb(40, 30, 76),
                                                        );
                                                        painter.text(
                                                            img_rect.center(),
                                                            egui::Align2::CENTER_CENTER,
                                                            name.chars().next().unwrap_or('?'),
                                                            FontId::proportional(36.0),
                                                            GOLD_DIM,
                                                        );
                                                    }
                                                    if selected {
                                                        painter.rect_stroke(
                                                            img_rect,
                                                            2.0,
                                                            Stroke::new(1.5_f32, GOLD),
                                                        );
                                                    }
                                                    let mut shown = name.clone();
                                                    if shown.chars().count() > 13 {
                                                        shown = shown.chars().take(12).collect::<String>() + "…";
                                                    }
                                                    painter.text(
                                                        Pos2::new(rect.center().x, rect.max.y - 16.0),
                                                        egui::Align2::CENTER_CENTER,
                                                        shown,
                                                        FontId::proportional(14.0),
                                                        if selected {
                                                            Color32::WHITE
                                                        } else {
                                                            Color32::from_rgb(200, 190, 235)
                                                        },
                                                    );
                                                    let resp = resp.on_hover_text(name.as_str());
                                                    if resp.double_clicked() {
                                                        play_idx = Some(*idx);
                                                    } else if resp.clicked() {
                                                        clicked_idx = Some(*idx);
                                                    }
                                                }
                                            });
                                        });
                                });
                            if let Some(idx) = clicked_idx.or(play_idx) {
                                self.selected_char_idx = idx;
                                if let Some(ch) =
                                    self.account_info.as_ref().and_then(|i| i.characters.get(idx))
                                {
                                    self.edit_desc_buf = ch.desc.clone();
                                }
                                self.editing_char = false;
                            }
                            if play_idx.is_some() {
                                self.trigger_play();
                            }

                            ui.add_space(6.0);
                            ui.columns(3, |c| {
                                let mk = |t: &str| {
                                    egui::Button::new(RichText::new(t).strong().size(15.0).color(Color32::WHITE))
                                        .fill(Color32::from_rgb(44, 30, 90))
                                        .stroke(Stroke::new(1.0_f32, GOLD_DIM))
                                        .min_size(Vec2::new(0.0, 32.0))
                                };
                                if c[0]
                                    .add_sized([c[0].available_width(), 32.0], mk("NEW"))
                                    .on_hover_text("Create a new character (web editor)")
                                    .clicked()
                                {
                                    ctx.open_url(egui::OpenUrl::new_tab("https://www.furcadia.com/fured"));
                                }
                                if c[1]
                                    .add_sized([c[1].available_width(), 32.0], mk("MY DEN"))
                                    .on_hover_text("View your characters on My Den")
                                    .clicked()
                                {
                                    ctx.open_url(egui::OpenUrl::new_tab("https://cms.furcadia.com/myden"));
                                }
                                if c[2]
                                    .add_sized([c[2].available_width(), 32.0], mk("EDIT"))
                                    .on_hover_text("Edit the selected character's description")
                                    .clicked()
                                {
                                    self.editing_char = !self.editing_char;
                                }
                            });

                            if self.editing_char {
                                ui.add_space(6.0);
                                egui::Frame::none()
                                    .fill(INPUT_PURPLE)
                                    .stroke(Stroke::new(1.0_f32, GOLD_DIM))
                                    .rounding(6.0)
                                    .inner_margin(8.0)
                                    .show(ui, |ui| {
                                        ui.label(RichText::new("Edit Character Description").color(GOLD));
                                        ui.add(
                                            egui::TextEdit::multiline(&mut self.edit_desc_buf)
                                                .desired_rows(3)
                                                .desired_width(f32::INFINITY),
                                        );
                                        ui.horizontal(|ui| {
                                            if ui.button("Apply on Login").clicked() {
                                                let new_desc = self.edit_desc_buf.trim().to_owned();
                                                self.pending_desc = Some(new_desc.clone());
                                                if let Some(info) = self.account_info.as_mut() {
                                                    if let Some(ch) =
                                                        info.characters.get_mut(self.selected_char_idx)
                                                    {
                                                        ch.desc = new_desc;
                                                    }
                                                }
                                                self.editing_char = false;
                                            }
                                            if ui.button("Cancel").clicked() {
                                                self.editing_char = false;
                                            }
                                        });
                                    });
                            }

                            ui.add_space(12.0);
                            let play_btn = egui::Button::new(
                                RichText::new("PLAY")
                                    .size(22.0)
                                    .strong()
                                    .color(Color32::WHITE),
                            )
                            .fill(Color32::from_rgb(56, 38, 110))
                            .stroke(Stroke::new(1.5_f32, GOLD))
                            .min_size(Vec2::new(ui.available_width(), 44.0));

                            if ui
                                .add_enabled(self.selected_character().is_some(), play_btn)
                                .clicked()
                            {
                                self.trigger_play();
                            }

                            ui.add_space(12.0);
                            ui.label(RichText::new("Select Costume").color(GOLD).size(15.0));
                            egui::Frame::none()
                                .fill(INPUT_PURPLE)
                                .stroke(Stroke::new(1.0_f32, GOLD_DIM))
                                .rounding(6.0)
                                .inner_margin(8.0)
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.label("Last appearance");
                                    if let Some(ch) = self.selected_character() {
                                        ui.label(
                                            RichText::new(format!("Colors: {}", ch.colors))
                                                .size(11.0)
                                                .color(Color32::LIGHT_GRAY),
                                        );
                                    }
                                });
                        });
                });
            });
    }

    fn render_connecting_screen(&mut self, ctx: &egui::Context) {
        let char_name = self
            .selected_character()
            .map(|c| c.name.clone())
            .unwrap_or_else(|| "Character".to_owned());

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(BG_PURPLE))
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(96.0);
                    egui::Frame::none()
                        .fill(CARD_PURPLE)
                        .stroke(Stroke::new(2.0_f32, GOLD_DIM))
                        .rounding(10.0)
                        .inner_margin(28.0)
                        .show(ui, |ui| {
                            ui.set_width(460.0);
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    RichText::new("FURCADIA")
                                        .size(42.0)
                                        .strong()
                                        .color(GOLD),
                                );
                                ui.label(
                                    RichText::new("The Second Dreaming")
                                        .size(14.0)
                                        .italics()
                                        .color(Color32::from_rgb(225, 200, 145)),
                                );
                                ui.add_space(10.0);
                                ui.separator();
                                ui.add_space(10.0);
                                ui.label(
                                    RichText::new(format!("Joining as {char_name}"))
                                        .size(16.0)
                                        .strong()
                                        .color(Color32::from_rgb(230, 225, 245)),
                                );
                                ui.add_space(16.0);

                                if let Some(err) = self.connecting_error.clone() {
                                    egui::Frame::none()
                                        .fill(INPUT_PURPLE)
                                        .stroke(Stroke::new(
                                            1.0_f32,
                                            Color32::from_rgb(220, 90, 90),
                                        ))
                                        .rounding(6.0)
                                        .inner_margin(12.0)
                                        .show(ui, |ui| {
                                            ui.set_width(ui.available_width());
                                            ui.colored_label(
                                                Color32::from_rgb(245, 115, 115),
                                                err,
                                            );
                                        });
                                    ui.add_space(16.0);
                                    ui.horizontal(|ui| {
                                        if ui
                                            .add(
                                                egui::Button::new(
                                                    RichText::new("Retry Connection")
                                                        .strong()
                                                        .color(Color32::WHITE),
                                                )
                                                .fill(Color32::from_rgb(56, 38, 110))
                                                .stroke(Stroke::new(1.5_f32, GOLD))
                                                .min_size(Vec2::new(170.0, 36.0)),
                                            )
                                            .clicked()
                                        {
                                            self.trigger_play();
                                        }
                                        if ui
                                            .add(
                                                egui::Button::new("← Back to Characters")
                                                    .min_size(Vec2::new(170.0, 36.0)),
                                            )
                                            .clicked()
                                        {
                                            self.connecting_error = None;
                                            self.screen = AppScreen::CharacterSelect;
                                        }
                                    });
                                } else {
                                    let stage_text = if self.connecting_stage.is_empty() {
                                        "Connecting to Furcadia...".to_owned()
                                    } else {
                                        self.connecting_stage.clone()
                                    };
                                    ui.label(
                                        RichText::new(stage_text)
                                            .size(13.5)
                                            .color(Color32::LIGHT_GRAY),
                                    );
                                    ui.add_space(10.0);

                                    let t = ui.input(|i| i.time) as f32;
                                    let bar_w = 380.0_f32;
                                    let (bar_rect, _) = ui.allocate_exact_size(
                                        Vec2::new(bar_w, 12.0),
                                        Sense::hover(),
                                    );
                                    let painter = ui.painter();
                                    painter.rect_filled(bar_rect, 4.0, INPUT_PURPLE);
                                    painter.rect_stroke(
                                        bar_rect,
                                        4.0,
                                        Stroke::new(1.0_f32, GOLD_DIM),
                                    );
                                    let seg_w = if self.is_connected {
                                        bar_rect.width() * 0.85
                                    } else {
                                        bar_rect.width() * 0.28
                                    };
                                    let travel = (bar_rect.width() - seg_w - 4.0).max(0.0);
                                    let phase = ((t * 0.9).sin() * 0.5) + 0.5;
                                    let x0 = bar_rect.min.x + 2.0 + travel * phase;
                                    let fill_rect = Rect::from_min_size(
                                        Pos2::new(x0, bar_rect.min.y + 2.0),
                                        Vec2::new(seg_w, bar_rect.height() - 4.0),
                                    );
                                    painter.rect_filled(fill_rect, 3.0, GOLD);

                                    ui.add_space(16.0);
                                    if ui
                                        .add(
                                            egui::Button::new("Cancel")
                                                .min_size(Vec2::new(140.0, 32.0)),
                                        )
                                        .clicked()
                                    {
                                        self.connect_seq = self.connect_seq.wrapping_add(1);
                                        self.is_connected = false;
                                        self.connected_at = None;
                                        let _ = self.to_net.send(UiToNet::Disconnect);
                                        self.screen = AppScreen::CharacterSelect;
                                    }
                                }
                            });
                        });
                });
            });
    }

    fn send_raw_cmd(&self, cmd: &str) {
        if self.is_connected {
            let _ = self.to_net.send(UiToNet::Send(ClientCommand::Raw(cmd.to_owned())));
        }
    }

    fn render_pounce_contents(&mut self, ui: &mut egui::Ui, is_detached: bool) {
        let accent = self.theme.accent();
        ui.horizontal(|ui| {
            ui.heading(RichText::new("Pounce").color(accent));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if is_detached {
                    if ui.small_button("Dock").clicked() {
                        self.pounce_detached = false;
                        self.pounce_visible = true;
                    }
                } else {
                    if ui.small_button("×").on_hover_text("Hide Pounce").clicked() {
                        self.pounce_visible = false;
                    }
                    if ui
                        .small_button("↗ Detach")
                        .on_hover_text("Pop out Pounce window")
                        .clicked()
                    {
                        self.pounce_detached = true;
                    }
                }
            });
        });

        ui.horizontal(|ui| {
            let enter = ui
                .add(
                    egui::TextEdit::singleline(&mut self.watch_input)
                        .hint_text("Character name")
                        .desired_width((ui.available_width() - 62.0).max(80.0)),
                )
                .lost_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.button("Watch").clicked() || enter {
                let name = self.watch_input.trim().replace('|', " ");
                if !name.is_empty() {
                    let wire = wire_player_name(&name);
                    self.pounce.add_watch(name);
                    self.watch_input.clear();
                    self.save_config();
                    if self.is_connected {
                        self.send_raw_cmd(&format!("pounce adduser {wire}"));
                    }
                }
            }
        });

        let rows: Vec<(String, OnlineStatus)> = self
            .pounce
            .sorted_entries()
            .into_iter()
            .map(|e| (e.name.clone(), e.status))
            .collect();
        let online_count = rows
            .iter()
            .filter(|(_, st)| *st == OnlineStatus::Online)
            .count();
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("{online_count}/{} online", rows.len()))
                    .size(11.5)
                    .color(Color32::LIGHT_GRAY),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .checkbox(&mut self.pounce_show_offline, RichText::new("Show Offline").size(11.0))
                    .on_hover_text("Show or hide offline furres in Pounce")
                    .changed()
                {
                    self.save_config();
                }
            });
        });
        ui.separator();

        let mut to_remove = None;
        let mut to_glook = None;
        let mut to_whisper = None;
        let mut ctx_action: Option<(String, String, PlayerMenuAction)> = None;
        egui::ScrollArea::vertical()
            .id_salt("pounce_scroll")
            .show(ui, |ui| {
                for (name, status) in rows {
                    if !self.pounce_show_offline && status != OnlineStatus::Online {
                        continue;
                    }
                    ui.horizontal(|ui| {
                        let (dot_color, status_tip) = match status {
                            OnlineStatus::Online => (Color32::from_rgb(80, 220, 110), "Online"),
                            OnlineStatus::Offline => (Color32::from_rgb(130, 130, 135), "Offline"),
                            OnlineStatus::Unknown => {
                                (Color32::from_rgb(220, 190, 80), "Checking status")
                            }
                        };
                        ui.colored_label(dot_color, "●").on_hover_text(status_tip);
                        let name_color = if status == OnlineStatus::Online {
                            Color32::WHITE
                        } else {
                            Color32::LIGHT_GRAY
                        };
                        let name_w = (ui.available_width() - 98.0).max(48.0);
                        let name_resp = ui
                            .allocate_ui(Vec2::new(name_w, 18.0), |ui| {
                                ui.add(
                                    egui::Label::new(RichText::new(&name).color(name_color))
                                        .truncate()
                                        .sense(Sense::click()),
                                )
                            })
                            .inner
                            .on_hover_text(format!("{name}\nRight-click for player actions"));
                        if name_resp.double_clicked() {
                            to_whisper = Some(name.clone());
                        }
                        let row_name = name.clone();
                        name_resp.context_menu(|ui| {
                            if let Some(act) = render_player_menu_buttons(ui, false, true, true) {
                                ctx_action =
                                    Some((row_name.clone(), wire_player_name(&row_name), act));
                                ui.close_menu();
                            }
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("×").clicked() {
                                to_remove = Some(name.clone());
                            }
                            if ui.small_button("💬").on_hover_text("Whisper").clicked() {
                                to_whisper = Some(name.clone());
                            }
                            if self.is_connected && ui.small_button("Look").clicked() {
                                to_glook = Some(wire_player_name(&name));
                            }
                        });
                    });
                }
            });
        if let Some((disp, short, act)) = ctx_action {
            self.execute_player_action(&disp, &short, act);
        }
        if let Some(name) = to_remove {
            let wire = wire_player_name(&name);
            self.pounce.remove_watch(&name);
            self.save_config();
            if self.is_connected {
                self.send_raw_cmd(&format!("pounce remuser {wire}"));
            }
        }
        if let Some(name) = to_glook {
            let _ = self.to_net.send(UiToNet::Send(ClientCommand::Glook(name)));
        }
        if let Some(name) = to_whisper {
            self.open_whisper_tab(&name, true);
        }

        ui.add_space(6.0);
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Saved Dreams").strong().color(accent));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("+ Current")
                    .on_hover_text("Save current dream to Pounce list")
                    .clicked()
                {
                    if let Some(cur) = self
                        .current_dream_url
                        .clone()
                        .or_else(|| self.world.map_name.clone())
                    {
                        self.bookmark_dream(&cur);
                    }
                }
            });
        });
        let mut go_dream: Option<String> = None;
        let mut del_dream: Option<usize> = None;
        for (i, d) in self.dream_bookmarks.iter().enumerate() {
            ui.horizontal(|ui| {
                let label_w = (ui.available_width() - 28.0).max(60.0);
                if ui
                    .add_sized([label_w, 20.0], egui::Button::new(d.as_str()))
                    .on_hover_text(format!("Click to enter {d}"))
                    .clicked()
                {
                    go_dream = Some(d.clone());
                }
                if ui.small_button("×").on_hover_text("Remove dream").clicked() {
                    del_dream = Some(i);
                }
            });
        }
        if let Some(d) = go_dream {
            self.send_raw_cmd(&format!("fdl {d}"));
        }
        if let Some(idx) = del_dream {
            if idx < self.dream_bookmarks.len() {
                self.dream_bookmarks.remove(idx);
                self.save_config();
            }
        }
    }

    fn render_detached_viewports(&mut self, ctx: &egui::Context) {
        if self.pounce_detached {
            let pounce_id = egui::ViewportId::from_hash_of("furc_pounce_viewport");
            let builder = egui::ViewportBuilder::default()
                .with_title("Furcadia - Pounce")
                .with_inner_size([280.0, 420.0]);
            ctx.show_viewport_immediate(pounce_id, builder, |v_ctx, _class| {
                if v_ctx.input(|i| i.viewport().close_requested()) {
                    self.pounce_detached = false;
                }
                egui::CentralPanel::default()
                    .frame(egui::Frame::none().fill(self.theme.panel()).inner_margin(10.0))
                    .show(v_ctx, |ui| {
                        self.render_pounce_contents(ui, true);
                    });
            });
        }

        let detached_peers: Vec<String> = self
            .whispers
            .iter()
            .filter(|w| w.detached)
            .map(|w| w.peer.clone())
            .collect();

        for peer in detached_peers {
            let vp_id = egui::ViewportId::from_hash_of(("furc_whisper", &peer));
            let builder = egui::ViewportBuilder::default()
                .with_title(format!("Whisper: {peer}"))
                .with_inner_size([420.0, 300.0]);
            let mut dock_back = false;
            let mut close_win = false;
            let mut send_msg: Option<String> = None;
            let mut chat_action: Option<ChatClickAction> = None;

            ctx.show_viewport_immediate(vp_id, builder, |v_ctx, _class| {
                if v_ctx.input(|i| i.viewport().close_requested()) {
                    dock_back = true;
                }
                if let Some(w) = self
                    .whispers
                    .iter_mut()
                    .find(|w| w.peer.eq_ignore_ascii_case(&peer))
                {
                    w.unread = 0;
                    egui::CentralPanel::default()
                        .frame(egui::Frame::none().fill(self.theme.bg()).inner_margin(8.0))
                        .show(v_ctx, |ui| {
                            ui.horizontal(|ui| {
                                ui.heading(
                                    RichText::new(format!("Whisper: {}", w.peer))
                                        .color(self.theme.accent()),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.small_button("× Close").clicked() {
                                            close_win = true;
                                        }
                                        if ui.small_button("↙ Dock").clicked() {
                                            dock_back = true;
                                        }
                                    },
                                );
                            });
                            ui.separator();
                            let h = (ui.available_height() - 34.0).max(80.0);
                            egui::ScrollArea::vertical()
                                .id_salt(("whisper_det_scroll", &w.peer))
                                .max_height(h)
                                .stick_to_bottom(true)
                                .show(ui, |ui| {
                                    render_whisper_history_lines(
                                        ui,
                                        &w.history_lines,
                                        !w.lines.is_empty(),
                                    );
                                    for line in &w.lines {
                                        render_chat_line(
                                            ui,
                                            line,
                                            &mut chat_action,
                                            &self.smiley_textures,
                                            &self.desctag_textures,
                                            &self.chat_img_textures,
                                            &self.player_accent_colors,
                                        );
                                    }
                                });
                            ui.separator();
                            ui.horizontal(|ui| {
                                let enter = ui
                                    .add(
                                        egui::TextEdit::singleline(&mut w.input)
                                            .hint_text(format!("Whisper {}...", w.peer))
                                            .desired_width((ui.available_width() - 62.0).max(100.0)),
                                    )
                                    .lost_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                                if (ui.button("Send").clicked() || enter)
                                    && !w.input.trim().is_empty()
                                {
                                    send_msg = Some(w.input.trim().to_owned());
                                    w.input.clear();
                                }
                            });
                        });
                }
            });

            if let Some(act) = chat_action {
                self.handle_chat_click_action(act);
            }
            if let Some(msg) = send_msg {
                if self.is_connected {
                    let _ = self.to_net.send(UiToNet::Send(ClientCommand::Whisper {
                        target: wire_player_name(&peer),
                        message: msg,
                    }));
                }
            }
            if dock_back {
                if let Some(w) = self
                    .whispers
                    .iter_mut()
                    .find(|w| w.peer.eq_ignore_ascii_case(&peer))
                {
                    w.detached = false;
                    self.active_chat_tab = Some(w.peer.clone());
                }
            } else if close_win {
                self.whispers
                    .retain(|w| !w.peer.eq_ignore_ascii_case(&peer));
            }
        }
    }

    fn render_sidebar(&mut self, ctx: &egui::Context) {
        let panel_bg = self.theme.panel();
        let accent = self.theme.accent();

        let sidebar_w = (256.0 + (self.portrait_size - 64.0).max(0.0)).min(360.0);
        egui::SidePanel::left("furc_sidebar")
            .exact_width(sidebar_w)
            .resizable(false)
            .frame(
                egui::Frame::none()
                    .fill(panel_bg)
                    .stroke(Stroke::new(1.5_f32, GOLD_DIM))
                    .inner_margin(8.0),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("FURCADIA").size(18.0).strong().color(accent));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("About").on_hover_text("About & Credits").clicked() {
                            self.about_open = !self.about_open;
                        }
                        if ui.small_button("Help").on_hover_text("F1 Help").clicked() {
                            self.help_open = !self.help_open;
                        }
                        if ui
                            .small_button("Chars")
                            .on_hover_text("Disconnect and return to Character Select")
                            .clicked()
                        {
                            let _ = self.to_net.send(UiToNet::Disconnect);
                            self.is_connected = false;
                            self.screen = AppScreen::CharacterSelect;
                        }
                    });
                });

                ui.add_space(3.0);
                ui.horizontal(|ui| {
                    if ui
                        .small_button("+Char Window")
                        .on_hover_text("Launch a new Furcadia window for another character")
                        .clicked()
                    {
                        Self::launch_new_window();
                    }
                    if ui
                        .small_button("+Char Tab")
                        .on_hover_text("Keep this character logged in and log in another character in a tab")
                        .clicked()
                    {
                        self.suspend_active_session_to_bg(ctx);
                        self.active_char_tab_pos = self.bg_sessions.len();
                        self.screen = AppScreen::CharacterSelect;
                    }
                });

                // Multi-character in-window session tabs (fixed order on click; drag to reorder)
                if !self.bg_sessions.is_empty() {
                    ui.add_space(3.0);
                    let total_tabs = self.bg_sessions.len() + 1;
                    let active_pos = self.active_char_tab_pos.min(self.bg_sessions.len());
                    let active_name = self.active_character_name();
                    let mut switch_to: Option<usize> = None;
                    let mut close_bg: Option<usize> = None;
                    let mut close_active = false;
                    let mut tab_rects: Vec<(usize, Rect)> = Vec::with_capacity(total_tabs);

                    ui.horizontal_wrapped(|ui| {
                        for vis in 0..total_tabs {
                            if vis == active_pos {
                                let resp = ui.add(
                                    egui::Button::new(
                                        RichText::new(&active_name)
                                            .size(11.0)
                                            .strong()
                                            .color(Color32::from_rgb(24, 16, 6)),
                                    )
                                    .fill(accent)
                                    .sense(Sense::click_and_drag()),
                                );
                                if resp.drag_started() {
                                    self.dragging_char_tab = Some(vis);
                                }
                                tab_rects.push((vis, resp.rect));
                                if ui
                                    .small_button("×")
                                    .on_hover_text("Disconnect & close this character tab")
                                    .clicked()
                                {
                                    close_active = true;
                                }
                            } else {
                                let bg_idx = if vis < active_pos { vis } else { vis - 1 };
                                let bg = &self.bg_sessions[bg_idx];
                                let lbl = if bg.unread_count > 0 {
                                    format!("{} ({})", bg.char_name, bg.unread_count)
                                } else {
                                    bg.char_name.clone()
                                };
                                let resp = if bg.unread_count > 0 {
                                    ui.add(
                                        egui::Button::new(
                                            RichText::new(lbl)
                                                .size(11.0)
                                                .strong()
                                                .color(Color32::from_rgb(20, 14, 6)),
                                        )
                                        .fill(Color32::from_rgb(235, 180, 60))
                                        .sense(Sense::click_and_drag()),
                                    )
                                } else {
                                    ui.add(
                                        egui::Button::new(
                                            RichText::new(lbl)
                                                .size(11.0)
                                                .color(Color32::from_rgb(230, 224, 240)),
                                        )
                                        .fill(Color32::from_rgb(42, 30, 68))
                                        .sense(Sense::click_and_drag()),
                                    )
                                };
                                if resp.drag_started() {
                                    self.dragging_char_tab = Some(vis);
                                } else if resp.clicked() {
                                    switch_to = Some(bg_idx);
                                }
                                tab_rects.push((vis, resp.rect));
                                if ui
                                    .small_button("×")
                                    .on_hover_text("Disconnect & close this character tab")
                                    .clicked()
                                {
                                    close_bg = Some(bg_idx);
                                }
                            }
                        }
                    });

                    if !ui.input(|i| i.pointer.any_down()) {
                        self.dragging_char_tab = None;
                    } else if let (Some(from_vis), Some(pos)) =
                        (self.dragging_char_tab, ui.input(|i| i.pointer.interact_pos()))
                    {
                        if let Some(&(to_vis, _)) = tab_rects
                            .iter()
                            .find(|(v, r)| *v != from_vis && r.contains(pos))
                        {
                            self.swap_char_tabs(from_vis, to_vis);
                            self.dragging_char_tab = Some(to_vis);
                        }
                    }

                    if close_active {
                        let _ = self.to_net.send(UiToNet::Disconnect);
                        self.is_connected = false;
                        if !self.bg_sessions.is_empty() {
                            let next_idx = active_pos.min(self.bg_sessions.len() - 1);
                            self.switch_to_bg_session(next_idx, ctx);
                        } else {
                            self.screen = AppScreen::CharacterSelect;
                        }
                    } else if let Some(idx) = close_bg {
                        let sess = self.bg_sessions.remove(idx);
                        let _ = sess.to_net.send(UiToNet::Disconnect);
                        let removed_vis = if idx < active_pos { idx } else { idx + 1 };
                        if removed_vis < self.active_char_tab_pos {
                            self.active_char_tab_pos = self.active_char_tab_pos.saturating_sub(1);
                        }
                        self.active_char_tab_pos =
                            self.active_char_tab_pos.min(self.bg_sessions.len());
                    } else if let Some(idx) = switch_to {
                        self.switch_to_bg_session(idx, ctx);
                    }
                }

                ui.add_space(4.0);
                let self_port = self.self_portrait(ctx);
                egui::Frame::none()
                    .fill(self.theme.bg())
                    .stroke(Stroke::new(1.2_f32, GOLD_DIM))
                    .rounding(6.0)
                    .inner_margin(6.0)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            let port_frame = egui::Frame::none()
                                .fill(Color32::from_rgb(18, 12, 34))
                                .stroke(Stroke::new(1.5_f32, GOLD))
                                .rounding(4.0)
                                .inner_margin(2.0);
                            port_frame.show(ui, |ui| {
                                let psize = Vec2::splat(self.portrait_size);
                                if let Some(tex) = &self_port {
                                    ui.image((tex.id(), psize));
                                } else {
                                    ui.allocate_ui(psize, |ui| {
                                        ui.centered_and_justified(|ui| {
                                            ui.label(RichText::new("🐾").size(24.0).color(GOLD_DIM));
                                        });
                                    });
                                }
                            });

                            ui.vertical(|ui| {
                                let char_name = self.active_character_name();
                                let status_color = if self.is_connected {
                                    Color32::from_rgb(90, 210, 120)
                                } else {
                                    Color32::from_rgb(200, 180, 90)
                                };
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(&char_name).strong().color(accent));
                                    ui.colored_label(
                                        status_color,
                                        if self.is_connected { "• Online" } else { "• Offline" },
                                    );
                                });
                                let dream_label = self
                                    .current_dream_url
                                    .as_deref()
                                    .map(|u| u.trim_start_matches("furc://").trim_matches('/'))
                                    .filter(|s| !s.is_empty())
                                    .or_else(|| self.world.map_name.as_deref());
                                if let Some(dream) = dream_label {
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(format!("Dream: {dream}"))
                                                .size(11.0)
                                                .color(Color32::LIGHT_GRAY),
                                        )
                                        .truncate(),
                                    );
                                }
                                if let Some((x, y)) = self.world.self_pos {
                                    ui.label(
                                        RichText::new(format!("Pos: ({x}, {y})"))
                                            .size(11.0)
                                            .color(Color32::GRAY),
                                    );
                                }
                                ui.horizontal(|ui| {
                                    if ui
                                        .selectable_label(self.afk_active, "AFK")
                                        .clicked()
                                    {
                                        self.afk_active = !self.afk_active;
                                        self.send_raw_cmd("afk");
                                    }
                                    if self.is_connected {
                                        if ui
                                            .small_button("Disconnect")
                                            .on_hover_text("Disconnect from server")
                                            .clicked()
                                        {
                                            let _ = self.to_net.send(UiToNet::Disconnect);
                                        }
                                    } else if ui.small_button("Reconn").clicked() {
                                        self.trigger_play();
                                    }
                                });
                                if self.cookies_collected.is_some() || self.cookies_total.is_some() {
                                    let mut txt = String::from("🍪");
                                    if let Some(c) = self.cookies_collected {
                                        txt.push_str(&format!(" {c} held"));
                                    }
                                    if let Some(t) = self.cookies_total {
                                        txt.push_str(&format!(" · {t} all-time"));
                                    }
                                    ui.label(
                                        RichText::new(txt)
                                            .size(11.0)
                                            .color(Color32::from_rgb(235, 190, 120)),
                                    )
                                    .on_hover_text("Cookies reported by the server (updates when the server sends cookie info)");
                                }
                            });
                        });

                        ui.add_space(4.0);
                        let name = self
                            .world
                            .self_name
                            .as_deref()
                            .or_else(|| self.selected_character().map(|c| c.name.as_str()))
                            .unwrap_or("Adventurer");
                        egui::Frame::none()
                            .fill(Color32::from_rgb(56, 42, 102))
                            .stroke(Stroke::new(1.0_f32, GOLD_DIM))
                            .rounding(10.0)
                            .inner_margin(egui::Margin::symmetric(8.0, 3.0))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.vertical_centered(|ui| {
                                    ui.label(
                                        RichText::new(name)
                                            .size(13.5)
                                            .strong()
                                            .color(Color32::WHITE),
                                    );
                                });
                            });
                    });

                ui.add_space(6.0);
                self.render_looked_player_card(ui, ctx);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for (tab, letter, sub, tip) in [
                        (SidebarTab::Move, "A", "MOVE", "Move & Actions"),
                        (SidebarTab::GoTo, "B", "GO TO", "Go To / Maps"),
                        (SidebarTab::Edit, "C", "EDIT", "Edit Character & Dream"),
                        (SidebarTab::Digo, "D", "DIGO", "Digo & Costumes"),
                        (SidebarTab::Emotes, "E", "DIGO", "Emotes & Digo #2"),
                        (SidebarTab::Misc, "F", "MISC", "Misc, Logs, Settings & Skin"),
                    ] {
                        let selected = self.sidebar_tab == tab;
                        let (rect, resp) = ui.allocate_exact_size(Vec2::new(37.0, 34.0), Sense::click());
                        let fill = if selected {
                            accent
                        } else if resp.hovered() {
                            Color32::from_rgb(64, 46, 102)
                        } else {
                            Color32::from_rgb(42, 30, 68)
                        };
                        let border = if selected {
                            Color32::from_rgb(255, 232, 150)
                        } else if resp.hovered() {
                            accent
                        } else {
                            GOLD_DIM
                        };
                        let p = ui.painter();
                        p.rect_filled(rect, 5.0, fill);
                        p.rect_stroke(
                            rect,
                            5.0,
                            Stroke::new(if selected { 1.5_f32 } else { 1.0_f32 }, border),
                        );
                        p.text(
                            egui::pos2(rect.center().x, rect.top() + 10.5),
                            egui::Align2::CENTER_CENTER,
                            letter,
                            egui::FontId::proportional(11.0),
                            if selected { Color32::from_rgb(28, 18, 8) } else { accent },
                        );
                        p.text(
                            egui::pos2(rect.center().x, rect.bottom() - 9.0),
                            egui::Align2::CENTER_CENTER,
                            sub,
                            egui::FontId::proportional(8.5),
                            if selected {
                                Color32::from_rgb(28, 18, 8)
                            } else {
                                Color32::from_rgb(235, 230, 245)
                            },
                        );
                        if resp.on_hover_text(tip).clicked() {
                            self.sidebar_tab = tab;
                        }
                    }
                });
                ui.separator();

                let btn_fill = Color32::from_rgb(44, 32, 72);
                let btn_hover = Color32::from_rgb(64, 46, 102);
                let btn_active = Color32::from_rgb(80, 58, 122);
                {
                    let visuals = ui.visuals_mut();
                    visuals.widgets.inactive.weak_bg_fill = btn_fill;
                    visuals.widgets.inactive.bg_fill = btn_fill;
                    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, GOLD_DIM);
                    visuals.widgets.inactive.rounding = egui::Rounding::same(5.0);
                    visuals.widgets.hovered.weak_bg_fill = btn_hover;
                    visuals.widgets.hovered.bg_fill = btn_hover;
                    visuals.widgets.hovered.bg_stroke = Stroke::new(1.2_f32, accent);
                    visuals.widgets.hovered.rounding = egui::Rounding::same(5.0);
                    visuals.widgets.active.weak_bg_fill = btn_active;
                    visuals.widgets.active.bg_fill = btn_active;
                    visuals.widgets.active.bg_stroke = Stroke::new(1.5_f32, accent);
                    visuals.widgets.active.rounding = egui::Rounding::same(5.0);
                }

                egui::ScrollArea::vertical()
                    .id_salt("sidebar_tab_scroll")
                    .show(ui, |ui| match self.sidebar_tab {
                        SidebarTab::Move => {
                            ui.label(RichText::new("✦ MOVE & POSTURE").strong().color(accent));
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    for (idx, lbl, cmd) in [
                                        (0u8, "🧍 Stand", "stand"),
                                        (1u8, "🪑 Sit", "sit"),
                                        (2u8, "🛌 Lie Down", "lie"),
                                    ] {
                                        let sel = self.posture == idx;
                                        if ui
                                            .add(
                                                egui::Button::new(
                                                    RichText::new(lbl)
                                                        .strong()
                                                        .color(if sel {
                                                            Color32::from_rgb(24, 16, 6)
                                                        } else {
                                                            Color32::from_rgb(240, 236, 250)
                                                        }),
                                                )
                                                .fill(if sel { accent } else { btn_fill })
                                                .stroke(Stroke::new(
                                                    if sel { 1.5_f32 } else { 1.0_f32 },
                                                    if sel { Color32::from_rgb(255, 232, 150) } else { GOLD_DIM },
                                                ))
                                                .rounding(5.0)
                                                .min_size(Vec2::new(86.0, 28.0)),
                                            )
                                            .clicked()
                                        {
                                            self.posture = idx;
                                            self.send_raw_cmd(cmd);
                                        }
                                    }
                                });
                                egui::Frame::none()
                                    .fill(Color32::from_rgb(26, 18, 46))
                                    .stroke(Stroke::new(1.0_f32, GOLD_DIM))
                                    .rounding(6.0)
                                    .inner_margin(4.0)
                                    .show(ui, |ui| {
                                        ui.vertical(|ui| {
                                            ui.horizontal(|ui| {
                                                if ui
                                                    .add(egui::Button::new(RichText::new("7 ↖").strong()).min_size(Vec2::new(62.0, 36.0)))
                                                    .on_hover_text("Move NW (7 / Home)")
                                                    .clicked()
                                                    && self.is_connected
                                                {
                                                    let _ = self.to_net.send(UiToNet::Send(ClientCommand::Move(7)));
                                                }
                                                if ui
                                                    .add(egui::Button::new(RichText::new("9 ↗").strong()).min_size(Vec2::new(62.0, 36.0)))
                                                    .on_hover_text("Move NE (9 / PageUp)")
                                                    .clicked()
                                                    && self.is_connected
                                                {
                                                    let _ = self.to_net.send(UiToNet::Send(ClientCommand::Move(9)));
                                                }
                                            });
                                            ui.horizontal(|ui| {
                                                if ui
                                                    .add(egui::Button::new(RichText::new("1 ↙").strong()).min_size(Vec2::new(62.0, 36.0)))
                                                    .on_hover_text("Move SW (1 / End)")
                                                    .clicked()
                                                    && self.is_connected
                                                {
                                                    let _ = self.to_net.send(UiToNet::Send(ClientCommand::Move(1)));
                                                }
                                                if ui
                                                    .add(egui::Button::new(RichText::new("3 ↘").strong()).min_size(Vec2::new(62.0, 36.0)))
                                                    .on_hover_text("Move SE (3 / PageDown)")
                                                    .clicked()
                                                    && self.is_connected
                                                {
                                                    let _ = self.to_net.send(UiToNet::Send(ClientCommand::Move(3)));
                                                }
                                            });
                                        });
                                    });
                            });
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                if ui
                                    .add(egui::Button::new("↺ Turn L").min_size(Vec2::new(112.0, 28.0)))
                                    .clicked()
                                {
                                    self.send_raw_cmd("<");
                                }
                                if ui
                                    .add(egui::Button::new("Turn R ↻").min_size(Vec2::new(112.0, 28.0)))
                                    .clicked()
                                {
                                    self.send_raw_cmd(">");
                                }
                            });
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                if ui
                                    .add(
                                        egui::Button::new(RichText::new("✨ Use (Ctrl-U)").strong().color(Color32::from_rgb(220, 250, 225)))
                                            .fill(Color32::from_rgb(36, 68, 52))
                                            .stroke(Stroke::new(1.0_f32, Color32::from_rgb(90, 175, 120)))
                                            .min_size(Vec2::new(112.0, 34.0)),
                                    )
                                    .clicked()
                                {
                                    self.send_raw_cmd("use");
                                }
                                if ui
                                    .add(
                                        egui::Button::new(RichText::new("✋ Get (Ctrl-G)").strong().color(Color32::from_rgb(255, 240, 205)))
                                            .fill(Color32::from_rgb(72, 54, 34))
                                            .stroke(Stroke::new(1.0_f32, GOLD_DIM))
                                            .min_size(Vec2::new(112.0, 34.0)),
                                    )
                                    .clicked()
                                {
                                    self.send_raw_cmd("get");
                                }
                            });
                            ui.add_space(8.0);
                            if ui
                                .add(
                                    egui::Button::new(RichText::new("🚪 EXIT").strong().color(Color32::WHITE))
                                        .fill(Color32::from_rgb(120, 40, 54))
                                        .stroke(Stroke::new(1.0_f32, Color32::from_rgb(210, 95, 110)))
                                        .min_size(Vec2::new(98.0, 28.0)),
                                )
                                .on_hover_text("Disconnect and return to Character Select")
                                .clicked()
                            {
                                let _ = self.to_net.send(UiToNet::Disconnect);
                                self.is_connected = false;
                                self.screen = AppScreen::CharacterSelect;
                            }
                        }
                        SidebarTab::GoTo => {
                            ui.label(RichText::new("🗺 GO TO / MAPS").strong().color(accent));
                            ui.add_space(4.0);
                            ui.horizontal_wrapped(|ui| {
                                for (label, cmd) in [
                                    ("✨ Vinca", "fdl furc://vinca"),
                                    ("🎭 Imaginarium", "fdl furc://imaginarium"),
                                    ("🌊 Naia", "fdl furc://naia"),
                                    ("🏛 Acropolis", "fdl furc://acropolis"),
                                    ("⛵ Allegria", "fdl furc://allegria"),
                                    ("🌲 Wylde", "fdl furc://wylde"),
                                    ("🏘 Olde Towne", "fdl furc://oldetowne"),
                                    ("🌿 Hawthorn", "fdl furc://hawthorn"),
                                    ("🐾 FurN", "fdl furc://furn"),
                                    ("🐱 Meovanni", "fdl furc://meovannivillage/"),
                                ] {
                                    if ui
                                        .add(
                                            egui::Button::new(label)
                                                .min_size(Vec2::new(108.0, 26.0)),
                                        )
                                        .clicked()
                                    {
                                        self.send_raw_cmd(cmd);
                                    }
                                }
                            });
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                if ui.button("↩ BACK").clicked() {
                                    self.send_raw_cmd("goback");
                                }
                                if ui.button("👥 WHO").clicked() && self.is_connected {
                                    let _ = self.to_net.send(UiToNet::Send(ClientCommand::Who));
                                }
                            });
                            ui.add_space(6.0);
                            ui.label(RichText::new("✦ Player Join / Summon").color(accent));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.target_player_input)
                                    .hint_text("Player name")
                                    .desired_width(f32::INFINITY),
                            );
                            ui.horizontal(|ui| {
                                let wire = wire_player_name(&self.target_player_input);
                                if ui.button("➡ JOIN").clicked() && !wire.is_empty() {
                                    self.send_raw_cmd(&format!("join {wire}"));
                                }
                                if ui.button("⬅ SUMMON").clicked() && !wire.is_empty() {
                                    self.send_raw_cmd(&format!("summon {wire}"));
                                }
                                if ui.button("👁 LOOK").clicked()
                                    && !wire.is_empty()
                                    && self.is_connected
                                {
                                    let _ =
                                        self.to_net.send(UiToNet::Send(ClientCommand::Glook(wire)));
                                }
                            });
                            ui.add_space(6.0);
                            ui.label(RichText::new("✦ Dream URL").color(accent));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.dream_url_input)
                                    .desired_width(f32::INFINITY),
                            );
                            ui.horizontal(|ui| {
                                if ui.button("🌀 Enter Dream").clicked() {
                                    let url = self.dream_url_input.trim().to_owned();
                                    if !url.is_empty() {
                                        self.send_raw_cmd(&format!("fdl {url}"));
                                    }
                                }
                                if ui
                                    .button("⭐ + Save Dream")
                                    .on_hover_text("Add this Dream URL to your Pounce Saved Dreams list")
                                    .clicked()
                                {
                                    let url = self.dream_url_input.trim().to_owned();
                                    if !url.is_empty() && !url.eq_ignore_ascii_case("furc://") {
                                        self.bookmark_dream(&url);
                                    } else if let Some(cur) = self
                                        .current_dream_url
                                        .clone()
                                        .or_else(|| self.world.map_name.clone())
                                    {
                                        self.bookmark_dream(&cur);
                                    }
                                }
                            });
                        }
                        SidebarTab::Edit => {
                            ui.label(RichText::new("✏ EDIT").strong().color(accent));
                            ui.add_space(4.0);
                            ui.horizontal_wrapped(|ui| {
                                if ui
                                    .add(egui::Button::new("⚙ Edit My Settings").min_size(Vec2::new(112.0, 28.0)))
                                    .clicked()
                                {
                                    self.settings_open = true;
                                }
                                if ui
                                    .add(egui::Button::new("💤 AFK Settings").min_size(Vec2::new(112.0, 28.0)))
                                    .clicked()
                                {
                                    self.afk_active = !self.afk_active;
                                    self.send_raw_cmd("afk");
                                }
                                if ui
                                    .add(egui::Button::new("🦊 Edit My Furre").min_size(Vec2::new(112.0, 28.0)))
                                    .clicked()
                                {
                                    ctx.open_url(egui::OpenUrl::new_tab("https://www.furcadia.com/fured"));
                                }
                                if ui
                                    .add(egui::Button::new("✨ PLACE DREAM").min_size(Vec2::new(112.0, 28.0)))
                                    .clicked()
                                {
                                    self.send_raw_cmd("dream");
                                }
                            });
                            ui.add_space(6.0);
                            ui.label(RichText::new("✦ Live Description").color(accent));
                            ui.add(
                                egui::TextEdit::multiline(&mut self.live_desc_input)
                                    .desired_rows(4)
                                    .desired_width(f32::INFINITY),
                            );
                            if ui.button("📝 Update Description").clicked() && self.is_connected {
                                let clean_desc = self
                                    .live_desc_input
                                    .trim()
                                    .replace(['\r', '\n'], " ");
                                let _ = self.to_net.send(UiToNet::Send(ClientCommand::Desc(
                                    clean_desc,
                                )));
                            }
                            ui.add_space(6.0);
                            if let Some(map) = &self.world.map {
                                ui.label(
                                    RichText::new(format!(
                                        "Loaded Map: {}x{}",
                                        map.width, map.height
                                    ))
                                    .size(11.5)
                                    .color(Color32::LIGHT_GRAY),
                                );
                            }
                        }
                        SidebarTab::Digo => {
                            ui.label(RichText::new("💎 DIGO TOGGLES & WINGS").strong().color(accent));
                            if let Some(ch) = self.selected_character() {
                                ui.label(
                                    RichText::new(format!("Colors: {}", ch.colors))
                                        .size(11.0)
                                        .color(Color32::LIGHT_GRAY),
                                );
                            }
                            ui.horizontal_wrapped(|ui| {
                                if ui.button("🪽 Cycle Wings").on_hover_text("Cycle owned wings (`wings)").clicked() {
                                    self.send_raw_cmd("wings");
                                }
                                if ui.button("🏷 Desctags").on_hover_text("List/toggle desctags (`desctags)").clicked() {
                                    self.send_raw_cmd("desctags");
                                }
                                if ui.button("🖼 Cycle Portrait").on_hover_text("Cycle custom portraits (`portrchng)").clicked() {
                                    self.send_raw_cmd("portrchng");
                                }
                                if ui.button("💎 Digo Market").on_hover_text("Open Digo Market in browser").clicked() {
                                    ctx.open_url(egui::OpenUrl::new_tab(
                                        "https://www.furcadia.com/digo/",
                                    ));
                                }
                            });

                            ui.add_space(6.0);
                            ui.label(RichText::new("Wings (Toggle On/Off)").size(11.5).color(accent));
                            ui.horizontal_wrapped(|ui| {
                                for (label, cmd) in [
                                    ("Classic", "clsscwngs"),
                                    ("Tricolor", "triwngs"),
                                    ("Butterfly", "bflywngs"),
                                    ("Bat", "batwngs"),
                                    ("Prime", "primewngs"),
                                    ("Dragonfly", "dfwings"),
                                ] {
                                    if ui
                                        .add(egui::Button::new(label).min_size(Vec2::new(68.0, 24.0)))
                                        .on_hover_text(format!("Toggle {label} wings (`{cmd})"))
                                        .clicked()
                                    {
                                        self.send_raw_cmd(cmd);
                                    }
                                }
                            });

                            ui.add_space(6.0);
                            ui.label(RichText::new("Mythical & Special Digos (Toggle)").size(11.5).color(accent));
                            ui.horizontal_wrapped(|ui| {
                                for (label, cmd) in [
                                    ("Dragon", "dragon"),
                                    ("Phoenix", "phoenix"),
                                    ("Gryffe", "gryffe"),
                                    ("Unicorn", "unicorn"),
                                    ("Naga", "naga"),
                                    ("Kirin", "kirin"),
                                    ("Gargoyle", "gargoyle"),
                                    ("Angelcat", "angelcat"),
                                    ("Purrwing", "purrwing"),
                                    ("Kitter Sm", "kitw1"),
                                    ("Kitter Lg", "kitw2"),
                                    ("Kiwi", "kiwi"),
                                    ("Werewolf", "werewolf"),
                                    ("Furling", "furling"),
                                    ("Noble (Cycle)", "noble"),
                                ] {
                                    if ui
                                        .add(egui::Button::new(label).min_size(Vec2::new(72.0, 24.0)))
                                        .on_hover_text(format!("Toggle {label} (`{cmd})"))
                                        .clicked()
                                    {
                                        self.send_raw_cmd(cmd);
                                    }
                                }
                            });

                            ui.add_space(6.0);
                            ui.label(RichText::new("Ferians (Toggle)").size(11.5).color(accent));
                            ui.horizontal_wrapped(|ui| {
                                for (label, cmd) in [
                                    ("Wolven", "wolven"),
                                    ("Foxen", "foxen"),
                                    ("Catten", "catten"),
                                    ("Owlen", "owlen"),
                                    ("Ferian Dragon", "mfdragon"),
                                    ("Ferian Unicorn", "mfunicorn"),
                                ] {
                                    if ui
                                        .add(egui::Button::new(label).min_size(Vec2::new(72.0, 24.0)))
                                        .on_hover_text(format!("Toggle {label} (`{cmd})"))
                                        .clicked()
                                    {
                                        self.send_raw_cmd(cmd);
                                    }
                                }
                            });

                            ui.add_space(6.0);
                            ui.label(RichText::new("Nobles (Toggle)").size(11.5).color(accent));
                            ui.horizontal_wrapped(|ui| {
                                for (label, cmd) in [
                                    ("Canine", "noblecanine"),
                                    ("Feline", "noblefeline"),
                                    ("Equine", "nobleequine"),
                                    ("Lapine", "noblelapine"),
                                    ("Musteline", "noblemusteline"),
                                    ("Rodent", "noblerodent"),
                                    ("Squirrel", "noblesquirrel"),
                                    ("Hyooman", "noblehyooman"),
                                ] {
                                    if ui
                                        .add(egui::Button::new(label).min_size(Vec2::new(72.0, 24.0)))
                                        .on_hover_text(format!("Toggle Noble {label} (`{cmd})"))
                                        .clicked()
                                    {
                                        self.send_raw_cmd(cmd);
                                    }
                                }
                            });
                        }
                        SidebarTab::Emotes => {
                            ui.label(RichText::new("✨ DIGO ABILITIES & ACTIONS").strong().color(accent));
                            ui.add_space(2.0);
                            ui.label(RichText::new("Digo Magic & Effects").size(11.5).color(accent));
                            ui.horizontal_wrapped(|ui| {
                                for (label, cmd) in [
                                    ("✨ Magic", "magic"),
                                    ("🪽 Fly", "fly"),
                                    ("🔥 Flame", "flame"),
                                    ("💨 Breath", "breath"),
                                    ("🌟 Glamour", "glamour"),
                                    ("💦 Splash", "splash"),
                                    ("🪨 Stone", "stone"),
                                    ("☁ Kirin Cloud", "kirincloud"),
                                    ("🧹 Purify", "purify"),
                                ] {
                                    if ui
                                        .add(egui::Button::new(label).min_size(Vec2::new(72.0, 24.0)))
                                        .on_hover_text(format!("Send `{cmd}"))
                                        .clicked()
                                    {
                                        self.send_raw_cmd(cmd);
                                    }
                                }
                            });
                            ui.add_space(4.0);
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new("Size:").size(11.5).color(Color32::LIGHT_GRAY));
                                for (label, cmd) in [
                                    ("50%", "kittersize 50"),
                                    ("100%", "kittersize 100"),
                                    ("150%", "kittersize 150"),
                                ] {
                                    if ui
                                        .small_button(label)
                                        .on_hover_text(format!("Send `{cmd}"))
                                        .clicked()
                                    {
                                        self.send_raw_cmd(cmd);
                                    }
                                }
                            });

                            ui.add_space(6.0);
                            ui.label(RichText::new("Social & Actions").size(11.5).color(accent));
                            ui.horizontal_wrapped(|ui| {
                                for (label, cmd) in [
                                    ("🤝 Lead", "lead"),
                                    ("👣 Follow", "follow"),
                                    ("✋ Stop", "stop"),
                                    ("✋ Get", "get"),
                                    ("✨ Use", "use"),
                                    ("ℹ Info", "info"),
                                    ("⬅ Summon", "summon"),
                                    ("➡ Join", "join"),
                                    ("🚫 Decline", "decline"),
                                    ("🍪 Cookie", "buy-cookie"),
                                ] {
                                    if ui
                                        .add(egui::Button::new(label).min_size(Vec2::new(68.0, 24.0)))
                                        .clicked()
                                    {
                                        self.send_raw_cmd(cmd);
                                    }
                                }
                            });

                            ui.add_space(6.0);
                            ui.label(RichText::new("Emotes").size(11.5).color(accent));
                            ui.horizontal_wrapped(|ui| {
                                for (label, emote) in [
                                    ("Wave", ":waves."),
                                    ("Smile", ":smiles."),
                                    ("Nod", ":nods."),
                                    ("Laugh", ":laughs!"),
                                    ("Bow", ":bows."),
                                    ("Cheer", ":cheers!"),
                                    ("Dance", ":dances!"),
                                    ("Purr", ":purrs."),
                                    ("Howl", ":howls!"),
                                    ("Hug", ":hugs!"),
                                    ("Blush", ":blushes."),
                                    ("Wink", ":winks."),
                                ] {
                                    if ui
                                        .add(egui::Button::new(label).min_size(Vec2::new(68.0, 24.0)))
                                        .clicked()
                                    {
                                        self.send_raw_cmd(emote);
                                    }
                                }
                            });
                        }
                        SidebarTab::Misc => {
                            ui.label(RichText::new("✦ MISC & TOOLS").strong().color(accent));
                            ui.add_space(4.0);
                            ui.horizontal_wrapped(|ui| {
                                if ui
                                    .add(egui::Button::new("🖼 Change Portrait").min_size(Vec2::new(112.0, 26.0)))
                                    .clicked()
                                {
                                    ctx.open_url(egui::OpenUrl::new_tab("https://www.furcadia.com/fured"));
                                }
                                if ui
                                    .add(egui::Button::new("📜 View Log").min_size(Vec2::new(112.0, 26.0)))
                                    .clicked()
                                {
                                    self.open_log_viewer();
                                }
                                if ui
                                    .add(egui::Button::new("💬 Forums").min_size(Vec2::new(112.0, 26.0)))
                                    .clicked()
                                {
                                    ctx.open_url(egui::OpenUrl::new_tab("https://forums.furcadia.com/"));
                                }
                                if ui
                                    .add(egui::Button::new("📰 News").min_size(Vec2::new(112.0, 26.0)))
                                    .clicked()
                                {
                                    ctx.open_url(egui::OpenUrl::new_tab("https://www.furcadia.com/news/"));
                                }
                                if ui
                                    .add(egui::Button::new("💎 Digo Market").min_size(Vec2::new(112.0, 26.0)))
                                    .clicked()
                                {
                                    ctx.open_url(egui::OpenUrl::new_tab("https://www.furcadia.com/digo/"));
                                }
                                if ui
                                    .add(egui::Button::new("📸 Screenshot").min_size(Vec2::new(112.0, 26.0)))
                                    .clicked()
                                {
                                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
                                    self.push_system_line("emit", "Screenshot requested.");
                                }
                                if ui
                                    .add(
                                        egui::Button::new(if self.sound_enabled {
                                            "🔊 Sound: ON"
                                        } else {
                                            "🔇 Sound: OFF"
                                        })
                                        .min_size(Vec2::new(112.0, 26.0)),
                                    )
                                    .clicked()
                                {
                                    self.sound_enabled = !self.sound_enabled;
                                    self.save_config();
                                    if self.sound_enabled {
                                        self.play_sound("online.wav");
                                    }
                                }
                                if ui
                                    .add(
                                        egui::Button::new(if self.music_enabled {
                                            "🎵 Music: ON"
                                        } else {
                                            "🎵 Music: OFF"
                                        })
                                        .min_size(Vec2::new(112.0, 26.0)),
                                    )
                                    .clicked()
                                {
                                    self.music_enabled = !self.music_enabled;
                                    self.save_config();
                                    if !self.music_enabled {
                                        self.stop_music();
                                    } else if let Some(dir) = self.current_dream_dir.clone() {
                                        self.play_dream_music(&dir);
                                    }
                                }
                            });

                            ui.add_space(6.0);
                            if ui
                                .add(
                                    egui::Button::new(RichText::new("⚙ Settings & Windows Importer").strong())
                                        .min_size(Vec2::new(ui.available_width(), 28.0)),
                                )
                                .clicked()
                            {
                                self.settings_open = true;
                            }

                            ui.add_space(8.0);
                            ui.label(RichText::new("Animations (F11)").strong().color(accent));
                            ui.horizontal(|ui| {
                                for mode in ["Full", "Half", "Off"] {
                                    if ui.radio(self.anim_mode == mode, mode).clicked() {
                                        self.anim_mode = mode.to_owned();
                                        self.save_config();
                                    }
                                }
                            });

                            ui.add_space(8.0);
                            ui.label(RichText::new("POUNCE & WINDOWS").strong().color(accent));
                            ui.horizontal(|ui| {
                                if ui
                                    .button(if self.pounce_visible {
                                        "Hide Pounce"
                                    } else {
                                        "Show Pounce"
                                    })
                                    .clicked()
                                {
                                    self.pounce_visible = !self.pounce_visible;
                                    self.save_config();
                                }
                                if ui
                                    .button(if self.pounce_detached {
                                        "Dock Pounce"
                                    } else {
                                        "Detach Pounce"
                                    })
                                    .clicked()
                                {
                                    self.pounce_detached = !self.pounce_detached;
                                    self.save_config();
                                }
                            });
                            ui.add_space(8.0);
                            ui.label(RichText::new("Dream Window Skin").strong().color(accent));
                            for t in [
                                DreamTheme::ClassicPurple,
                                DreamTheme::DragonHoard,
                                DreamTheme::KitterMagic,
                                DreamTheme::FerianWylde,
                            ] {
                                if ui.radio(self.theme == t, t.label()).clicked() {
                                    self.theme = t;
                                    self.save_config();
                                }
                            }
                            ui.add_space(6.0);
                            if ui
                                .checkbox(&mut self.show_debug_packets, "Log raw/extended packets")
                                .changed()
                            {
                                self.save_config();
                            }
                        }
                    });
            });
    }

    fn render_help_window(&mut self, ctx: &egui::Context) {
        let mut open = self.help_open;
        let (_, _, accent) = self.effective_colors();
        egui::Window::new("Furcadia Help & Hotkeys (F1)")
            .open(&mut open)
            .resizable(true)
            .default_size(Vec2::new(500.0, 400.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Quick Reference").strong().color(accent));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Ask Live Beekin Help (`help)").clicked() {
                            self.send_raw_cmd("help");
                        }
                    });
                });
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (cat, items) in [
                        (
                            "Movement & Posture",
                            "Arrows / Num 1,3,7,9: Move  |  Shift+Left/Right or < / >: Rotate\nCtrl+L / `lie / `sit / `stand: Posture  |  F2 / Ctrl+G / `get: Pick up/Drop\nF3 / Ctrl+U: Use held item  |  Tab: Toggle avatar names",
                        ),
                        (
                            "Chat & Formatting",
                            ":action: Emote  |  /\"name\" msg: Whisper  |  /%%name msg: Offline msg\nShift+Ctrl+B/I/U: Bold/Italic/Underline  |  Ctrl+1..0 / Shift+Ctrl+1..5: Emoticons\nCtrl+I/J/K: Recall last sent / whisper target / received whisper\nF5 / F6: Scroll chat up/down  |  F8 / Ctrl+R: Open last posted link  |  Esc: Clear input",
                        ),
                        (
                            "Audio, Views & System",
                            "Ctrl+P: Toggle sound  |  Ctrl+O: Toggle music  |  Ctrl+Y: Test/re-init sound\nCtrl+N: Reload dream patches  |  F11: Cycle animation speed (Full/Half/Off)\nCtrl+D: Switch chat/whisper tab  |  Ctrl+F: Font picker  |  F4 / Ctrl+W: Who",
                        ),
                    ] {
                        ui.label(RichText::new(cat).strong().color(accent));
                        ui.label(RichText::new(items).size(11.5).color(Color32::LIGHT_GRAY));
                        ui.add_space(6.0);
                    }
                });
            });
        self.help_open = open;
    }

    fn render_about_window(&mut self, ctx: &egui::Context) {
        let mut open = self.about_open;
        let (_, _, accent) = self.effective_colors();
        egui::Window::new("About Furcadia Native Client")
            .open(&mut open)
            .resizable(false)
            .default_width(420.0)
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.heading(RichText::new("Furcadia Native Client").color(accent));
                    ui.label(
                        RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION")))
                            .size(13.0)
                            .color(Color32::LIGHT_GRAY),
                    );
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                !self.update_checking,
                                egui::Button::new("Check for Updates"),
                            )
                            .clicked()
                        {
                            self.trigger_update_check(ctx);
                        }
                        if let Some(status) = &self.update_status {
                            ui.label(RichText::new(status).size(11.5).color(Color32::LIGHT_GRAY));
                        }
                    });
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("Native cross-platform Furcadia client for Linux and Windows.")
                            .size(12.0),
                    );
                });

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(6.0);

                ui.label(RichText::new("Open Source Resources & Credits").strong().color(accent));
                ui.add_space(4.0);
                for (name, desc) in [
                    ("eframe / egui (MIT / Apache-2.0)", "Immediate-mode Rust GUI framework"),
                    ("image (MIT / Apache-2.0)", "PNG and image decoding"),
                    ("lzma-rs (MIT)", "LZMA decompression for Fox5/FR01 archives"),
                    ("Rust Standard Library (MIT / Apache-2.0)", "Networking, filesystem, and concurrency"),
                    ("Furcadia", "Protocol & game world by Dragon's Eye Productions, Inc. Uses user-supplied local Furcadia assets."),
                ] {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(name).strong().size(11.5).color(Color32::WHITE));
                        ui.label(RichText::new(format!("- {desc}")).size(11.5).color(Color32::LIGHT_GRAY));
                    });
                }

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);

                ui.vertical_centered(|ui| {
                    if let Some(tex) = &self.bear_logo {
                        ui.add(
                            egui::Image::new(tex)
                                .max_size(Vec2::splat(96.0))
                                .rounding(8.0),
                        );
                    }
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("Bear Built  •  CC BY-NC-SA 4.0 (Non-Commercial)")
                            .size(11.5)
                            .color(Color32::GRAY),
                    );
                });
            });
        self.about_open = open;
    }

    fn handle_in_game_hotkeys(&mut self, ctx: &egui::Context) {
        let mut raw_cmd: Option<&'static str> = None;
        let mut send_who = false;
        let mut toggle_names = false;
        let mut toggle_sound = false;
        let mut toggle_music = false;
        let mut test_sound = false;
        let mut reload_patches = false;
        let mut open_last_link = false;
        let mut cycle_anim = false;
        let mut cycle_posture = false;
        let mut switch_chat_tab = false;
        let mut toggle_font = false;
        let mut toggle_help = false;
        let mut clear_input = false;
        let mut insert_tag: Option<&'static str> = None;
        let mut recall_mode: u8 = 0; // 1=sent, 2=wh_target, 3=wh_recv
        let mut move_dir: Option<u8> = None;
        let wants_text = ctx.wants_keyboard_input();
        let single_line_input = !self.chat_input.contains('\n');
        let empty_input = self.chat_input.is_empty();

        ctx.input_mut(|i| {
            let ctrl = i.modifiers.ctrl || i.modifiers.command;
            let shift = i.modifiers.shift;
            if i.key_pressed(egui::Key::Escape) {
                clear_input = true;
            } else if i.key_pressed(egui::Key::F1) || (ctrl && i.key_pressed(egui::Key::Z)) {
                toggle_help = true;
            } else if i.consume_key(egui::Modifiers::NONE, egui::Key::Tab) {
                toggle_names = true;
            } else if i.key_pressed(egui::Key::F2) || (ctrl && i.key_pressed(egui::Key::G)) {
                raw_cmd = Some("get");
            } else if i.key_pressed(egui::Key::F3) || (ctrl && i.key_pressed(egui::Key::U)) {
                raw_cmd = Some("use");
            } else if i.key_pressed(egui::Key::F4) || (ctrl && i.key_pressed(egui::Key::W)) {
                send_who = true;
            } else if i.key_pressed(egui::Key::F5) {
                self.chat_scroll_delta += 140.0;
            } else if i.key_pressed(egui::Key::F6) {
                self.chat_scroll_delta -= 140.0;
            } else if i.key_pressed(egui::Key::F8) || (ctrl && i.key_pressed(egui::Key::R)) {
                open_last_link = true;
            } else if i.key_pressed(egui::Key::F11) {
                cycle_anim = true;
            } else if ctrl && i.key_pressed(egui::Key::P) {
                toggle_sound = true;
            } else if ctrl && i.key_pressed(egui::Key::O) {
                toggle_music = true;
            } else if ctrl && i.key_pressed(egui::Key::Y) {
                test_sound = true;
            } else if ctrl && i.key_pressed(egui::Key::N) {
                reload_patches = true;
            } else if ctrl && i.key_pressed(egui::Key::L) {
                cycle_posture = true;
            } else if ctrl && i.key_pressed(egui::Key::D) {
                switch_chat_tab = true;
            } else if ctrl && i.key_pressed(egui::Key::F) {
                toggle_font = true;
            } else if ctrl && shift && i.key_pressed(egui::Key::B) {
                insert_tag = Some("<b></b>");
            } else if ctrl && shift && i.key_pressed(egui::Key::I) {
                insert_tag = Some("<i></i>");
            } else if ctrl && shift && i.key_pressed(egui::Key::U) {
                insert_tag = Some("<u></u>");
            } else if ctrl && !shift && i.key_pressed(egui::Key::I) {
                recall_mode = 1;
            } else if ctrl && !shift && i.key_pressed(egui::Key::J) {
                recall_mode = 2;
            } else if ctrl && !shift && i.key_pressed(egui::Key::K) {
                recall_mode = 3;
            } else if ctrl && !shift && i.key_pressed(egui::Key::B) {
                raw_cmd = Some("goback");
            } else if ctrl && shift && i.key_pressed(egui::Key::Num1) {
                insert_tag = Some("#SK");
            } else if ctrl && shift && i.key_pressed(egui::Key::Num2) {
                insert_tag = Some("#SL");
            } else if ctrl && shift && i.key_pressed(egui::Key::Num3) {
                insert_tag = Some("#SM");
            } else if ctrl && shift && i.key_pressed(egui::Key::Num4) {
                insert_tag = Some("#SN");
            } else if ctrl && shift && i.key_pressed(egui::Key::Num5) {
                insert_tag = Some("#SO");
            } else if ctrl && !shift && i.key_pressed(egui::Key::Num1) {
                insert_tag = Some("#SA");
            } else if ctrl && !shift && i.key_pressed(egui::Key::Num2) {
                insert_tag = Some("#SB");
            } else if ctrl && !shift && i.key_pressed(egui::Key::Num3) {
                insert_tag = Some("#SC");
            } else if ctrl && !shift && i.key_pressed(egui::Key::Num4) {
                insert_tag = Some("#SD");
            } else if ctrl && !shift && i.key_pressed(egui::Key::Num5) {
                insert_tag = Some("#SE");
            } else if ctrl && !shift && i.key_pressed(egui::Key::Num6) {
                insert_tag = Some("#SF");
            } else if ctrl && !shift && i.key_pressed(egui::Key::Num7) {
                insert_tag = Some("#SG");
            } else if ctrl && !shift && i.key_pressed(egui::Key::Num8) {
                insert_tag = Some("#SH");
            } else if ctrl && !shift && i.key_pressed(egui::Key::Num9) {
                insert_tag = Some("#SI");
            } else if ctrl && !shift && i.key_pressed(egui::Key::Num0) {
                insert_tag = Some("#SJ");
            } else if (shift && i.key_pressed(egui::Key::ArrowLeft))
                || (!wants_text && i.key_pressed(egui::Key::Insert))
                || (ctrl && i.key_pressed(egui::Key::ArrowLeft))
            {
                raw_cmd = Some("<");
            } else if (shift && i.key_pressed(egui::Key::ArrowRight))
                || (!wants_text && i.key_pressed(egui::Key::Delete))
                || (ctrl && i.key_pressed(egui::Key::ArrowRight))
            {
                raw_cmd = Some(">");
            }
            if self.is_connected && move_dir.is_none() {
                if !shift && i.consume_key(egui::Modifiers::NONE, egui::Key::Home) {
                    move_dir = Some(7);
                } else if !shift && i.consume_key(egui::Modifiers::NONE, egui::Key::PageUp) {
                    move_dir = Some(9);
                } else if !shift && i.consume_key(egui::Modifiers::NONE, egui::Key::End) {
                    move_dir = Some(1);
                } else if !shift && i.consume_key(egui::Modifiers::NONE, egui::Key::PageDown) {
                    move_dir = Some(3);
                } else if !shift && !ctrl && single_line_input && i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                    move_dir = Some(9);
                } else if !shift && !ctrl && single_line_input && i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                    move_dir = Some(1);
                } else if !shift && !ctrl && empty_input && i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft) {
                    move_dir = Some(7);
                } else if !shift && !ctrl && empty_input && i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight) {
                    move_dir = Some(3);
                }
            }
        });

        if let Some(dir) = move_dir {
            let _ = self.to_net.send(UiToNet::Send(ClientCommand::Move(dir)));
        }

        if clear_input {
            self.chat_input.clear();
        }
        if toggle_help {
            self.help_open = !self.help_open;
        }
        if let Some(tag) = insert_tag {
            self.chat_input.push_str(tag);
        }
        match recall_mode {
            1 if !self.last_sent_msg.is_empty() => self.chat_input = self.last_sent_msg.clone(),
            2 if !self.last_whisper_target.is_empty() => {
                self.chat_input = format!("/\"{}\" ", self.last_whisper_target);
            }
            3 if !self.last_received_whisper_from.is_empty() => {
                self.chat_input = format!("/\"{}\" ", self.last_received_whisper_from);
            }
            _ => {}
        }
        if toggle_names {
            self.show_avatar_names = !self.show_avatar_names;
            self.save_config();
        }
        if toggle_sound {
            self.sound_enabled = !self.sound_enabled;
            self.save_config();
        }
        if toggle_music {
            self.music_enabled = !self.music_enabled;
            self.save_config();
            if !self.music_enabled {
                self.stop_music();
            } else if let Some(dir) = self.current_dream_dir.clone() {
                self.play_dream_music(&dir);
            }
        }
        if test_sound {
            self.sound_enabled = true;
            self.play_sound("online.wav");
            self.push_system_line("emit", "Sound system initialized.");
        }
        if reload_patches {
            if let Some(dir) = self.current_dream_dir.clone() {
                self.load_dream_dir(&dir);
                self.push_system_line("emit", "Reloaded dream patches.");
            }
        }
        if open_last_link {
            let last_url = self.chat_log.iter().rev().find_map(|line| {
                line.spans.iter().rev().find_map(|s| match s {
                    furcadia_client::ChatSpan::Text { style, .. } => style.link.clone(),
                    _ => None,
                })
            });
            if let Some(url) = last_url {
                if url.starts_with("furc://") {
                    self.send_raw_cmd(&format!("fdl {url}"));
                } else {
                    ctx.open_url(egui::OpenUrl::new_tab(url));
                }
            }
        }
        if cycle_anim {
            self.anim_mode = match self.anim_mode.as_str() {
                "Full" => "Half".to_owned(),
                "Half" => "Off".to_owned(),
                _ => "Full".to_owned(),
            };
            self.save_config();
            self.push_system_line("emit", &format!("Animations set to: {}", self.anim_mode));
        }
        if cycle_posture {
            self.posture = (self.posture + 1) % 3;
            let cmd = match self.posture {
                1 => "sit",
                2 => "lie",
                _ => "stand",
            };
            self.send_raw_cmd(cmd);
        }
        if switch_chat_tab {
            if self.active_chat_tab.is_some() {
                self.active_chat_tab = None;
            } else if let Some(w) = self.whispers.iter().find(|w| !w.detached) {
                self.active_chat_tab = Some(w.peer.clone());
            }
        }
        if toggle_font {
            self.font_popup_open = !self.font_popup_open;
        }
        if let Some(cmd) = raw_cmd {
            self.send_raw_cmd(cmd);
        }
        if send_who && self.is_connected {
            let _ = self.to_net.send(UiToNet::Send(ClientCommand::Who));
        }
    }

    fn render_in_game_screen(&mut self, ctx: &egui::Context) {
        self.handle_in_game_hotkeys(ctx);
        self.render_detached_viewports(ctx);
        self.render_sidebar(ctx);

        if self.pounce_visible && !self.pounce_detached {
            egui::SidePanel::right("pounce_panel")
                .min_width(220.0)
                .frame(
                    egui::Frame::none()
                        .fill(self.theme.panel())
                        .stroke(Stroke::new(1.5_f32, GOLD_DIM))
                        .inner_margin(8.0),
                )
                .show(ctx, |ui| {
                    self.render_pounce_contents(ui, false);
                });
        }

        let (eff_bg, eff_panel, accent) = self.effective_colors();
        egui::TopBottomPanel::top("map_options_bar")
            .frame(
                egui::Frame::none()
                    .fill(eff_panel)
                    .stroke(Stroke::new(1.0_f32, GOLD_DIM))
                    .inner_margin(egui::Margin::symmetric(8.0, 4.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui.small_button("↖").on_hover_text("Move NW (7)").clicked() && self.is_connected {
                        let _ = self.to_net.send(UiToNet::Send(ClientCommand::Move(7)));
                    }
                    if ui.small_button("↗").on_hover_text("Move NE (9)").clicked() && self.is_connected {
                        let _ = self.to_net.send(UiToNet::Send(ClientCommand::Move(9)));
                    }
                    if ui.small_button("↙").on_hover_text("Move SW (1)").clicked() && self.is_connected {
                        let _ = self.to_net.send(UiToNet::Send(ClientCommand::Move(1)));
                    }
                    if ui.small_button("↘").on_hover_text("Move SE (3)").clicked() && self.is_connected {
                        let _ = self.to_net.send(UiToNet::Send(ClientCommand::Move(3)));
                    }
                    if ui.small_button("Center").clicked() {
                        self.cam_offset = Vec2::ZERO;
                    }
                    if ui
                        .selectable_label(self.show_avatar_names, "Names")
                        .on_hover_text("Toggle names (Tab)")
                        .clicked()
                    {
                        self.show_avatar_names = !self.show_avatar_names;
                        self.save_config();
                    }
                    if ui
                        .selectable_label(self.limit_view_range, "Chat View")
                        .on_hover_text("Limit map to Furcadia chat/sight distance (624x352)")
                        .clicked()
                    {
                        self.limit_view_range = !self.limit_view_range;
                        if self.limit_view_range && self.zoom < 0.95 {
                            self.zoom = 0.95;
                        }
                        self.save_config();
                    }
                    ui.label(
                        RichText::new(format!("{} Visible", self.last_visible_avatar_count))
                            .size(11.0)
                            .color(Color32::LIGHT_GRAY),
                    )
                    .on_hover_text("Visible avatars on the map");
                    ui.separator();
                    if ui.small_button("Who").on_hover_text("Who is online (F4)").clicked() && self.is_connected {
                        let _ = self.to_net.send(UiToNet::Send(ClientCommand::Who));
                    }
                    if ui.small_button("Time").on_hover_text("Furcadia Standard Time").clicked() && self.is_connected {
                        let _ = self.to_net.send(UiToNet::Send(ClientCommand::Raw("time".into())));
                    }
                    let anim_label = format!("Anim: {}", self.anim_mode);
                    if ui.small_button(anim_label).on_hover_text("Cycle animation speed (Full / Half / Off)").clicked() {
                        self.anim_mode = match self.anim_mode.as_str() {
                            "Full" => "Half".to_owned(),
                            "Half" => "Off".to_owned(),
                            _ => "Full".to_owned(),
                        };
                        self.save_config();
                    }
                    if ui.small_button("Settings").on_hover_text("Open Settings").clicked() {
                        self.settings_open = !self.settings_open;
                    }
                });
            });

        let chat_frame = egui::Frame::none()
            .fill(eff_bg)
            .stroke(Stroke::new(1.5_f32, GOLD_DIM))
            .inner_margin(8.0);

        if self.limit_view_range {
            let map_h = (352.0 * self.zoom)
                .clamp(240.0, (ctx.screen_rect().height() * 0.58).max(260.0));
            egui::TopBottomPanel::top("map_viewport_panel")
                .exact_height(map_h)
                .frame(
                    egui::Frame::none()
                        .fill(Color32::BLACK)
                        .stroke(Stroke::new(1.5_f32, accent)),
                )
                .show(ctx, |ui| {
                    self.render_isometric_world(ui);
                });
            egui::CentralPanel::default()
                .frame(chat_frame)
                .show(ctx, |ui| {
                    self.render_chat_panel_contents(ui);
                });
        } else {
            egui::TopBottomPanel::bottom("chat_panel")
                .min_height(210.0)
                .resizable(true)
                .frame(chat_frame)
                .show(ctx, |ui| {
                    self.render_chat_panel_contents(ui);
                });
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::none()
                        .fill(self.theme.bg())
                        .stroke(Stroke::new(2.0_f32, accent)),
                )
                .show(ctx, |ui| {
                    self.render_isometric_world(ui);
                });
        }
    }

    fn render_whisper_pounce_toolbar(&mut self, ui: &mut egui::Ui, peer: &str) {
        let wire = wire_player_name(peer);
        let watched = self
            .pounce
            .entries()
            .any(|e| e.name.eq_ignore_ascii_case(peer) || wire_player_name(&e.name).eq_ignore_ascii_case(&wire));
        ui.horizontal_wrapped(|ui| {
            if ui.small_button("👁 Look").on_hover_text(format!("Look at {peer}")).clicked() {
                self.send_raw_cmd(&format!("glook {wire}"));
                self.focus_chat_input = true;
            }
            if ui.small_button("➡ Join").on_hover_text(format!("Join {peer}")).clicked() {
                self.send_raw_cmd(&format!("join {wire}"));
                self.focus_chat_input = true;
            }
            if ui.small_button("⬅ Summon").on_hover_text(format!("Summon {peer}")).clicked() {
                self.send_raw_cmd(&format!("summon {wire}"));
                self.focus_chat_input = true;
            }
            if ui.small_button("👣 Follow").on_hover_text(format!("Follow {peer}")).clicked() {
                self.send_raw_cmd(&format!("follow {wire}"));
                self.focus_chat_input = true;
            }
            if watched {
                if ui.small_button("-Pounce").on_hover_text(format!("Remove {peer} from Pounce")).clicked() {
                    self.pounce.remove_watch(peer);
                    self.save_config();
                    if self.is_connected {
                        self.send_raw_cmd(&format!("pounce deluser {wire}"));
                    }
                    self.focus_chat_input = true;
                }
            } else if ui.small_button("+Pounce").on_hover_text(format!("Add {peer} to Pounce")).clicked() {
                self.pounce.add_watch(peer.to_owned());
                self.save_config();
                if self.is_connected {
                    self.send_raw_cmd(&format!("pounce adduser {wire}"));
                }
                self.focus_chat_input = true;
            }
            if ui.small_button("🚫 Ignore").on_hover_text(format!("Ignore / Unignore {peer}")).clicked() {
                self.send_raw_cmd(&format!("ignore {wire}"));
                self.focus_chat_input = true;
            }
            if ui.small_button("📜 History").on_hover_text(format!("View whisper logs with {peer}")).clicked() {
                self.open_log_viewer();
                if let Some(p) = self
                    .log_viewer_files
                    .iter()
                    .find(|p| {
                        p.file_stem()
                            .and_then(|s| s.to_str())
                            .is_some_and(|s| s.eq_ignore_ascii_case(&wire))
                    })
                    .cloned()
                {
                    self.load_log_file_into_viewer(p);
                }
            }
            if ui
                .small_button("↗ Pop Out")
                .on_hover_text(format!("Detach {peer} whisper into its own window"))
                .clicked()
            {
                if let Some(w) = self
                    .whispers
                    .iter_mut()
                    .find(|w| w.peer.eq_ignore_ascii_case(peer))
                {
                    w.detached = true;
                    w.unread = 0;
                }
                if self
                    .active_chat_tab
                    .as_deref()
                    .is_some_and(|a| a.eq_ignore_ascii_case(peer))
                {
                    self.main_chat_unread = 0;
                    self.active_chat_tab = None;
                    self.focus_chat_input = true;
                }
            }
        });
    }

    fn render_chat_panel_contents(&mut self, ui: &mut egui::Ui) {
        let accent = self.theme.accent();
                // Chat + Whisper Tabs bar (horizontally scrollable when many whispers are open)
                let mut close_peer: Option<String> = None;
                ui.horizontal(|ui| {
                    let tabs_max_w = (ui.available_width() - 178.0).max(120.0);
                    ui.allocate_ui(Vec2::new(tabs_max_w, 26.0), |ui| {
                        egui::ScrollArea::horizontal()
                            .id_salt("whisper_tabs_scroll")
                            .scroll_bar_visibility(
                                egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded,
                            )
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    let main_sel = self.active_chat_tab.is_none();
                                    let main_label = if self.main_chat_unread > 0 && !main_sel {
                                        format!("Main Chat ({})", self.main_chat_unread)
                                    } else {
                                        "Main Chat".to_owned()
                                    };
                                    let main_clicked = if !main_sel && self.main_chat_unread > 0 {
                                        ui.add(
                                            egui::Button::new(
                                                RichText::new(main_label)
                                                    .strong()
                                                    .color(Color32::from_rgb(20, 14, 6)),
                                            )
                                            .fill(Color32::from_rgb(235, 180, 60))
                                            .stroke(Stroke::new(
                                                1.5_f32,
                                                Color32::from_rgb(255, 225, 130),
                                            )),
                                        )
                                        .clicked()
                                    } else {
                                        ui.selectable_label(main_sel, main_label).clicked()
                                    };
                                    if main_clicked {
                                        self.main_chat_unread = 0;
                                        self.active_chat_tab = None;
                                        self.focus_chat_input = true;
                                    }

                                    let mut whisper_tab_rects: Vec<(usize, Rect)> = Vec::new();
                                    for (idx, w) in self.whispers.iter_mut().enumerate() {
                                        if w.detached {
                                            continue;
                                        }
                                        let sel = self
                                            .active_chat_tab
                                            .as_deref()
                                            .is_some_and(|a| a.eq_ignore_ascii_case(&w.peer));
                                        let label = if w.unread > 0 && !sel {
                                            format!("💬 {} ({})", w.peer, w.unread)
                                        } else {
                                            format!("💬 {}", w.peer)
                                        };
                                        ui.horizontal(|ui| {
                                            ui.spacing_mut().item_spacing.x = 2.0;
                                            let resp = if !sel && w.unread > 0 {
                                                ui.add(
                                                    egui::Button::new(
                                                        RichText::new(label)
                                                            .strong()
                                                            .color(Color32::from_rgb(20, 14, 6)),
                                                    )
                                                    .fill(Color32::from_rgb(235, 180, 60))
                                                    .stroke(Stroke::new(
                                                        1.5_f32,
                                                        Color32::from_rgb(255, 225, 130),
                                                    ))
                                                    .sense(Sense::click_and_drag()),
                                                )
                                            } else if sel {
                                                ui.add(
                                                    egui::Button::new(
                                                        RichText::new(label)
                                                            .strong()
                                                            .color(Color32::from_rgb(24, 16, 6)),
                                                    )
                                                    .fill(accent)
                                                    .sense(Sense::click_and_drag()),
                                                )
                                            } else {
                                                ui.add(
                                                    egui::Button::new(label)
                                                        .fill(Color32::from_rgb(42, 30, 68))
                                                        .sense(Sense::click_and_drag()),
                                                )
                                            };
                                            if resp.drag_started() {
                                                self.dragging_whisper_tab = Some(idx);
                                            } else if resp.clicked() {
                                                w.unread = 0;
                                                self.active_chat_tab = Some(w.peer.clone());
                                                self.focus_chat_input = true;
                                            }
                                            whisper_tab_rects.push((idx, resp.rect));
                                            if ui
                                                .small_button("×")
                                                .on_hover_text("Close tab")
                                                .clicked()
                                            {
                                                close_peer = Some(w.peer.clone());
                                            }
                                        });
                                    }
                                    if !ui.input(|i| i.pointer.any_down()) {
                                        self.dragging_whisper_tab = None;
                                    } else if let (Some(from_idx), Some(pos)) = (
                                        self.dragging_whisper_tab,
                                        ui.input(|i| i.pointer.interact_pos()),
                                    ) {
                                        if let Some(&(to_idx, _)) = whisper_tab_rects
                                            .iter()
                                            .find(|(i, r)| *i != from_idx && r.contains(pos))
                                        {
                                            if from_idx < self.whispers.len()
                                                && to_idx < self.whispers.len()
                                            {
                                                self.whispers.swap(from_idx, to_idx);
                                                self.dragging_whisper_tab = Some(to_idx);
                                            }
                                        }
                                    }
                                });
                            });
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("+ Whisper").clicked()
                            && !self.new_whisper_input.trim().is_empty()
                        {
                            let p = self.new_whisper_input.trim().to_owned();
                            self.new_whisper_input.clear();
                            self.open_whisper_tab(&p, true);
                        }
                        let wh_resp = ui.add(
                            egui::TextEdit::singleline(&mut self.new_whisper_input)
                                .hint_text("Player...")
                                .desired_width(95.0),
                        );
                        if wh_resp.lost_focus()
                            && ui.input(|i| i.key_pressed(egui::Key::Enter))
                            && !self.new_whisper_input.trim().is_empty()
                        {
                            let p = self.new_whisper_input.trim().to_owned();
                            self.new_whisper_input.clear();
                            self.open_whisper_tab(&p, true);
                        }
                    });
                });

                if let Some(peer) = close_peer {
                    self.whispers
                        .retain(|w| !w.peer.eq_ignore_ascii_case(&peer));
                    if self
                        .active_chat_tab
                        .as_deref()
                        .is_some_and(|a| a.eq_ignore_ascii_case(&peer))
                    {
                        self.main_chat_unread = 0;
                        self.active_chat_tab = None;
                        self.focus_chat_input = true;
                    }
                }

                ui.separator();
                // Chat Formatting Toolbar (AFK, B, I, U, Smileys, Font)
                ui.horizontal(|ui| {
                    if ui
                        .selectable_label(self.afk_active, "AFK")
                        .on_hover_text("Toggle AFK")
                        .clicked()
                    {
                        self.afk_active = !self.afk_active;
                        self.send_raw_cmd("afk");
                    }
                    ui.separator();
                    if ui.small_button(RichText::new("B").strong()).on_hover_text("Bold <b>...</b>").clicked() {
                        self.chat_input.push_str("<b></b>");
                    }
                    if ui.small_button(RichText::new("I").italics()).on_hover_text("Italic <i>...</i>").clicked() {
                        self.chat_input.push_str("<i></i>");
                    }
                    if ui.small_button(RichText::new("U").underline()).on_hover_text("Underline <u>...</u>").clicked() {
                        self.chat_input.push_str("<u></u>");
                    }
                    if ui
                        .selectable_label(self.smiley_picker_open, "☺")
                        .on_hover_text("Insert Smiley")
                        .clicked()
                    {
                        self.smiley_picker_open = !self.smiley_picker_open;
                    }
                    if ui
                        .selectable_label(self.link_builder_open, "🔗 Link")
                        .on_hover_text("Create a clickable link for chat")
                        .clicked()
                    {
                        self.link_builder_open = !self.link_builder_open;
                    }
                    let dice = if self.dice_roll.trim().is_empty() {
                        "1d6"
                    } else {
                        self.dice_roll.trim()
                    };
                    if ui
                        .small_button(format!("🎲 {dice}"))
                        .on_hover_text(format!("Roll {dice} (configurable in Settings → Whispers & Chat)"))
                        .clicked()
                        && self.is_connected
                    {
                        let _ = self.to_net.send(UiToNet::Send(ClientCommand::Say(format!(
                            "roll {dice}"
                        ))));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .selectable_label(self.font_popup_open, "Font")
                            .on_hover_text("Select chat font & size")
                            .clicked()
                        {
                            self.font_popup_open = !self.font_popup_open;
                        }
                        if ui.small_button("Logs").on_hover_text("View saved logs").clicked() {
                            self.open_log_viewer();
                        }
                    });
                });
                if self.link_builder_open {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("URL:").size(11.5));
                        ui.add(
                            egui::TextEdit::singleline(&mut self.link_url_input)
                                .hint_text("https://... or furc://...")
                                .desired_width(180.0),
                        );
                        ui.label(RichText::new("Text:").size(11.5));
                        ui.add(
                            egui::TextEdit::singleline(&mut self.link_label_input)
                                .hint_text("optional label")
                                .desired_width(120.0),
                        );
                        if ui.small_button("Insert").clicked() {
                            let url = self.link_url_input.trim();
                            if !url.is_empty() {
                                let label = self.link_label_input.trim();
                                let formatted = if label.is_empty() {
                                    url.to_owned()
                                } else {
                                    format!("<a href='{url}'>{label}</a>")
                                };
                                if !self.chat_input.is_empty() && !self.chat_input.ends_with(' ') {
                                    self.chat_input.push(' ');
                                }
                                self.chat_input.push_str(&formatted);
                                self.link_url_input.clear();
                                self.link_label_input.clear();
                                self.link_builder_open = false;
                            }
                        }
                    });
                }
                if self.smiley_picker_open {
                    ui.horizontal_wrapped(|ui| {
                        let fallback_emoji = [
                            ('A', "😊"), ('B', "😁"), ('C', "😉"), ('D', "😢"),
                            ('E', "😛"), ('F', "😮"), ('G', "😠"), ('H', "😎"),
                            ('I', "😳"), ('J', "😇"), ('K', "😈"), ('L', "❤️"),
                            ('M', "💔"), ('N', "🌹"), ('O', "⭐"), ('P', "🎵"),
                            ('Q', "🎉"), ('R', "💤"), ('S', "🤔"), ('T', "😂"),
                            ('U', "😘"), ('V', "🙄"), ('W', "😬"), ('X', "😴"),
                            ('Y', "👍"), ('Z', "👎"),
                        ];
                        for (ch, emoji) in fallback_emoji {
                            let code = format!("#S{ch}");
                            let tex_opt = smiley_index_for_code(ch)
                                .and_then(|idx| self.smiley_textures.get(idx))
                                .and_then(|t| t.as_ref());
                            let resp = if let Some(tex) = tex_opt {
                                let (rect, resp) = ui.allocate_exact_size(Vec2::new(26.0, 26.0), Sense::click());
                                let bg = if resp.hovered() {
                                    Color32::from_rgb(245, 240, 255)
                                } else {
                                    Color32::from_rgb(218, 212, 232)
                                };
                                let border = if resp.hovered() { accent } else { GOLD_DIM };
                                let p = ui.painter();
                                p.rect_filled(rect, 4.0, bg);
                                p.rect_stroke(rect, 4.0, Stroke::new(1.0_f32, border));
                                let img_rect = rect.shrink(3.0);
                                p.image(
                                    tex.id(),
                                    img_rect,
                                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                    Color32::WHITE,
                                );
                                resp
                            } else {
                                ui.add(
                                    egui::Button::new(emoji)
                                        .fill(Color32::from_rgb(218, 212, 232))
                                        .min_size(Vec2::new(26.0, 26.0)),
                                )
                            };
                            if resp.on_hover_text(&code).clicked() {
                                self.chat_input.push_str(&code);
                                self.smiley_picker_open = false;
                            }
                        }
                    });
                }
                if self.font_popup_open {
                    ui.horizontal(|ui| {
                        for fam in ["Proportional", "Monospace", "Fixedsys"] {
                            if ui.radio(self.font_family == fam, fam).clicked() {
                                self.font_family = fam.to_owned();
                                self.save_config();
                            }
                        }
                        if ui.small_button("-").clicked() && self.font_size > 9 {
                            self.font_size -= 1;
                            self.save_config();
                        }
                        ui.label(format!("{}px", self.font_size));
                        if ui.small_button("+").clicked() && self.font_size < 24 {
                            self.font_size += 1;
                            self.save_config();
                        }
                    });
                }

                if let Some(peer) = self.active_chat_tab.clone() {
                    ui.separator();
                    self.render_whisper_pounce_toolbar(ui, &peer);
                }
                ui.separator();
                let total_avail_h = ui.available_height();
                let line_h = ui.text_style_height(&egui::TextStyle::Body);
                let input_w = (ui.available_width() - 70.0).max(120.0);
                let spelling_h = if self.spell_misspelled.is_empty() { 0.0 } else { 24.0 };
                let min_chat_h = (line_h * 4.0 + 16.0).min((total_avail_h * 0.5).max(48.0));
                let max_input_h = (total_avail_h - min_chat_h - spelling_h - 18.0).max(line_h + 12.0);
                let typed_h = if self.chat_input.is_empty() {
                    line_h
                } else {
                    let job = build_spellcheck_layout_job(
                        ui,
                        &self.chat_input,
                        (input_w - 12.0).max(60.0),
                        &self.spell_misspelled,
                    );
                    ui.fonts(|f| f.layout_job(job)).size().y
                };
                let input_h = (typed_h + 12.0).clamp(line_h + 12.0, max_input_h);
                let available_h = (total_avail_h - input_h - spelling_h - 18.0).max(min_chat_h);
                let active_peer = self.active_chat_tab.clone();
                let mut chat_action: Option<ChatClickAction> = None;
                egui::ScrollArea::vertical()
                    .id_salt(("chat_scroll", active_peer.as_deref().unwrap_or("main")))
                    .auto_shrink([false, false])
                    .min_scrolled_height(available_h)
                    .max_height(available_h)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        ui.set_min_height(available_h);
                        let scroll_d = std::mem::take(&mut self.chat_scroll_delta);
                        if scroll_d.abs() > 0.5 {
                            ui.scroll_with_delta(Vec2::new(0.0, scroll_d));
                        }
                        if let Some(peer) = &active_peer {
                            if let Some(w) = self
                                .whispers
                                .iter()
                                .find(|w| w.peer.eq_ignore_ascii_case(peer))
                            {
                                if w.history_lines.is_empty() && w.lines.is_empty() {
                                    ui.label(
                                        RichText::new(format!("Whispering with {}...", w.peer))
                                            .italics()
                                            .color(Color32::GRAY),
                                    );
                                } else {
                                    render_whisper_history_lines(
                                        ui,
                                        &w.history_lines,
                                        !w.lines.is_empty(),
                                    );
                                    for line in &w.lines {
                                        render_chat_line(
                                            ui,
                                            line,
                                            &mut chat_action,
                                            &self.smiley_textures,
                                            &self.desctag_textures,
                                            &self.chat_img_textures,
                                            &self.player_accent_colors,
                                        );
                                    }
                                }
                            }
                        } else {
                            for line in &self.chat_log {
                                render_chat_line(
                                    ui,
                                    line,
                                    &mut chat_action,
                                    &self.smiley_textures,
                                    &self.desctag_textures,
                                    &self.chat_img_textures,
                                    &self.player_accent_colors,
                                );
                            }
                        }
                    });
                if let Some(act) = chat_action {
                    self.handle_chat_click_action(act);
                }

                if let Some(dirty_at) = self.spell_dirty_at {
                    if dirty_at.elapsed() >= std::time::Duration::from_millis(200) {
                        self.spell_dirty_at = None;
                        self.spell_seq = self.spell_seq.wrapping_add(1);
                        let _ = self.to_net.send(UiToNet::SpellCheck {
                            seq: self.spell_seq,
                            text: self.chat_input.clone(),
                            custom_dict: self.spellcheck_dict_with_names(),
                        });
                    } else {
                        ui.ctx().request_repaint_after(std::time::Duration::from_millis(60));
                    }
                }
                if !self.spell_misspelled.is_empty() {
                    let mut replace_action: Option<(usize, usize, String)> = None;
                    let mut add_dict_word: Option<String> = None;
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("Spelling:")
                                .size(11.0)
                                .color(Color32::from_rgb(245, 130, 130)),
                        );
                        for m in self.spell_misspelled.iter().take(2) {
                            ui.label(
                                RichText::new(format!("'{}'", m.word))
                                    .size(11.0)
                                    .color(Color32::from_rgb(245, 190, 190)),
                            );
                            for sug in m.suggestions.iter().take(3) {
                                if ui.small_button(sug).clicked() {
                                    replace_action = Some((m.start, m.end, sug.clone()));
                                }
                            }
                            if ui
                                .small_button("+Dict")
                                .on_hover_text(format!("Add '{}' to dictionary", m.word))
                                .clicked()
                            {
                                add_dict_word = Some(m.word.clone());
                            }
                        }
                    });
                    if let Some((start, end, rep)) = replace_action {
                        if start <= end && end <= self.chat_input.len()
                            && self.chat_input.is_char_boundary(start)
                            && self.chat_input.is_char_boundary(end)
                        {
                            self.chat_input.replace_range(start..end, &rep);
                            self.spell_dirty_at = Some(Instant::now());
                        }
                    }
                    if let Some(w) = add_dict_word {
                        self.add_word_to_dictionary(&w);
                    }
                }
                ui.separator();
                ui.horizontal(|ui| {
                    let hint = if let Some(peer) = &self.active_chat_tab {
                        format!("Whisper to {peer}... (or `cmd)")
                    } else {
                        "Say something, :emote, `cmd, /wh <name> <msg>, /onln <name>, /glook <name>, /who".to_owned()
                    };
                    let chat_input_id = ui.make_persistent_id("main_chat_input");
                    if std::mem::take(&mut self.focus_chat_input) {
                        ui.memory_mut(|m| m.request_focus(chat_input_id));
                    }
                    let enter = ui.memory(|m| m.has_focus(chat_input_id))
                        && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
                    let misspelled_snapshot = self.spell_misspelled.clone();
                    let mut layouter = move |ui: &egui::Ui, text: &str, wrap_width: f32| {
                        let job = build_spellcheck_layout_job(
                            ui,
                            text,
                            wrap_width,
                            &misspelled_snapshot,
                        );
                        ui.fonts(|f| f.layout_job(job))
                    };
                    let resp = egui::ScrollArea::vertical()
                        .id_salt("main_chat_input_scroll")
                        .max_height(input_h)
                        .min_scrolled_height(input_h)
                        .show(ui, |ui| {
                            ui.add(
                                egui::TextEdit::multiline(&mut self.chat_input)
                                    .id(chat_input_id)
                                    .desired_rows(1)
                                    .hint_text(hint)
                                    .desired_width(input_w)
                                    .layouter(&mut layouter),
                            )
                        })
                        .inner;
                    if resp.changed() {
                        if self.chat_input.trim().is_empty() {
                            self.spell_misspelled.clear();
                            self.spell_dirty_at = None;
                        } else {
                            self.spell_dirty_at = Some(Instant::now());
                        }
                    }
                    let mut ctx_replace: Option<(usize, usize, String)> = None;
                    let mut ctx_add_dict: Option<String> = None;
                    let ctx_misspelled = self.spell_misspelled.clone();
                    resp.context_menu(|ui| {
                        let state = egui::TextEdit::load_state(ui.ctx(), chat_input_id);
                        let sel_range = state.as_ref().and_then(|s| s.cursor.char_range());
                        let selected_text = sel_range.and_then(|r| {
                            let (a, b) = (r.primary.index.min(r.secondary.index), r.primary.index.max(r.secondary.index));
                            if a < b {
                                Some(self.chat_input.chars().skip(a).take(b - a).collect::<String>())
                            } else {
                                None
                            }
                        });
                        if ui
                            .add_enabled(selected_text.is_some(), egui::Button::new("Cut"))
                            .clicked()
                        {
                            if let (Some(txt), Some(r)) = (&selected_text, sel_range) {
                                ui.ctx().copy_text(txt.clone());
                                let a = r.primary.index.min(r.secondary.index);
                                let b = r.primary.index.max(r.secondary.index);
                                let byte_a = self.chat_input.char_indices().nth(a).map(|(i, _)| i).unwrap_or(self.chat_input.len());
                                let byte_b = self.chat_input.char_indices().nth(b).map(|(i, _)| i).unwrap_or(self.chat_input.len());
                                self.chat_input.replace_range(byte_a..byte_b, "");
                                if let Some(mut st) = state.clone() {
                                    let c = egui::text::CCursor::new(a);
                                    st.cursor.set_char_range(Some(egui::text::CCursorRange::one(c)));
                                    st.store(ui.ctx(), chat_input_id);
                                }
                                self.spell_dirty_at = Some(Instant::now());
                            }
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(selected_text.is_some(), egui::Button::new("Copy"))
                            .clicked()
                        {
                            if let Some(txt) = &selected_text {
                                ui.ctx().copy_text(txt.clone());
                            }
                            ui.close_menu();
                        }
                        if ui.button("Paste").clicked() {
                            if let Some(clip) = read_system_clipboard() {
                                let cleaned = clip.replace(['\r', '\n'], " ");
                                if let Some(r) = sel_range {
                                    let a = r.primary.index.min(r.secondary.index);
                                    let b = r.primary.index.max(r.secondary.index);
                                    let byte_a = self.chat_input.char_indices().nth(a).map(|(i, _)| i).unwrap_or(self.chat_input.len());
                                    let byte_b = self.chat_input.char_indices().nth(b).map(|(i, _)| i).unwrap_or(self.chat_input.len());
                                    self.chat_input.replace_range(byte_a..byte_b, &cleaned);
                                    if let Some(mut st) = state.clone() {
                                        let new_pos = a + cleaned.chars().count();
                                        let c = egui::text::CCursor::new(new_pos);
                                        st.cursor.set_char_range(Some(egui::text::CCursorRange::one(c)));
                                        st.store(ui.ctx(), chat_input_id);
                                    }
                                } else {
                                    self.chat_input.push_str(&cleaned);
                                }
                                self.spell_dirty_at = Some(Instant::now());
                            }
                            ui.close_menu();
                        }
                        if ui.button("Select All").clicked() {
                            let len_chars = self.chat_input.chars().count();
                            let mut st = state.unwrap_or_default();
                            st.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                                egui::text::CCursor::new(0),
                                egui::text::CCursor::new(len_chars),
                            )));
                            st.store(ui.ctx(), chat_input_id);
                            ui.close_menu();
                        }
                        if !ctx_misspelled.is_empty() {
                            ui.separator();
                            for m in ctx_misspelled.iter().take(3) {
                                ui.label(
                                    RichText::new(format!("Spelling: {}", m.word))
                                        .small()
                                        .color(Color32::from_rgb(245, 130, 130)),
                                );
                                for sug in m.suggestions.iter().take(4) {
                                    if ui.button(format!("  -> {sug}")).clicked() {
                                        ctx_replace = Some((m.start, m.end, sug.clone()));
                                        ui.close_menu();
                                    }
                                }
                                if ui.button(format!("  + Add \"{}\" to dictionary", m.word)).clicked() {
                                    ctx_add_dict = Some(m.word.clone());
                                    ui.close_menu();
                                }
                            }
                        }
                    });
                    if let Some((start, end, rep)) = ctx_replace {
                        if start <= end && end <= self.chat_input.len()
                            && self.chat_input.is_char_boundary(start)
                            && self.chat_input.is_char_boundary(end)
                        {
                            self.chat_input.replace_range(start..end, &rep);
                            self.spell_dirty_at = Some(Instant::now());
                        }
                    }
                    if let Some(w) = ctx_add_dict {
                        self.add_word_to_dictionary(&w);
                    }
                    let send_btn = egui::Button::new(RichText::new("Send").color(Color32::WHITE))
                        .fill(self.theme.panel())
                        .stroke(Stroke::new(1.0_f32, accent));
                    if ui.add(send_btn).clicked() || enter {
                        self.send_chat_input();
                        resp.request_focus();
                    }
                });
    }

    fn render_isometric_world(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if !ctx.wants_keyboard_input() && self.is_connected {
            let mut move_dir = None;
            ctx.input(|i| {
                if i.key_pressed(egui::Key::Num7) {
                    move_dir = Some(7);
                } else if i.key_pressed(egui::Key::Num9) {
                    move_dir = Some(9);
                } else if i.key_pressed(egui::Key::Num1) {
                    move_dir = Some(1);
                } else if i.key_pressed(egui::Key::Num3) {
                    move_dir = Some(3);
                }
            });
            if let Some(dir) = move_dir {
                let _ = self.to_net.send(UiToNet::Send(ClientCommand::Move(dir)));
            }
        }
        let rect = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(rect, Sense::click_and_drag());

        if response.dragged() {
            self.cam_offset += response.drag_delta();
        }

        if self.limit_view_range && self.zoom < 0.95 {
            self.zoom = 0.95;
        }
        let bg_painter = ui.painter_at(rect);
        let bg_fill = if self.limit_view_range {
            Color32::BLACK
        } else {
            Color32::from_rgb(18, 22, 28)
        };
        bg_painter.rect_filled(rect, 0.0, bg_fill);

        let tile_w = 64.0 * self.zoom;
        let tile_h = 32.0 * self.zoom;
        let scale = self.zoom;
        let raw_anim_ms = ctx.input(|i| (i.time * 1000.0) as u64);
        let anim_ms = match self.anim_mode.as_str() {
            "Off" => 0,
            "Half" => raw_anim_ms / 2,
            _ => raw_anim_ms,
        };
        // Generous overdraw margin so tall trees, walls, roofs, and dream effects
        // anchored outside the viewport still render when they poke into view.
        let cull_rect = egui::Rect::from_min_max(
            Pos2::new(rect.min.x - tile_w * 8.0, rect.min.y - tile_h * 8.0),
            Pos2::new(rect.max.x + tile_w * 8.0, rect.max.y + tile_h * 16.0),
        );

        let cur_region = if let (Some(map), Some((sx, sy))) = (&self.world.map, self.world.self_pos)
        {
            map.region_at(sx as usize, sy as usize)
        } else {
            0
        };
        let self_elev = if let Some((sx, sy)) = self.world.self_pos {
            self.resolve_map_tile_elev(&ctx, cur_region, sx as usize, sy as usize, anim_ms)
        } else {
            (0, 0)
        };

        let origin = if let Some((sx, sy)) = self.world.self_pos {
            let iso = tile_to_screen(sx as f32, sy as f32, tile_w, tile_h);
            rect.center()
                - iso
                - Vec2::new(self_elev.0 as f32, self_elev.1 as f32) * scale
                + self.cam_offset
        } else if let Some(map) = &self.world.map {
            let iso =
                tile_to_screen(map.width as f32 * 0.5, map.height as f32 * 0.5, tile_w, tile_h);
            rect.center() - iso + self.cam_offset
        } else {
            rect.center() + self.cam_offset
        };

        let chat_clip_rect = if self.limit_view_range {
            self.world.self_pos.map(|(sx, sy)| {
                let self_center = origin
                    + tile_to_screen(sx as f32, sy as f32, tile_w, tile_h)
                    + Vec2::new(self_elev.0 as f32, self_elev.1 as f32) * scale;
                egui::Rect::from_center_size(
                    self_center,
                    Vec2::new(624.0 * scale, 352.0 * scale),
                )
            })
        } else {
            None
        };
        let painter = if let Some(clip) = chat_clip_rect {
            bg_painter.rect_stroke(
                clip.intersect(rect),
                0.0,
                Stroke::new(1.0_f32, Color32::from_rgb(48, 42, 70)),
            );
            ui.painter_at(rect.intersect(clip))
        } else {
            bg_painter
        };

        struct TileDrawEntry {
            x: usize,
            y: usize,
            center: Pos2,
            tile_top_left: Pos2,
            floor_id: u16,
            item_id: u16,
            wall_nw: u16,
            wall_ne: u16,
            upper_nw: u16,
            upper_ne: u16,
            effect_id: u16,
            floor_elev: (i16, i16),
        }

        let mut tiles_to_draw: Vec<TileDrawEntry> = Vec::new();
        if let Some(map) = &self.world.map {
            let resolve_wall_byte = |mx: isize, my: isize, is_ne: bool| -> u16 {
                if mx < 0 || my < 0 {
                    return 0;
                }
                let (ux, uy) = (mx as usize, my as usize);
                if ux >= map.width || uy >= map.height {
                    return 0;
                }
                let raw = map.wall_at(ux, uy);
                let byte = if is_ne { (raw >> 8) & 0xff } else { raw & 0xff };
                if byte == 0 {
                    0
                } else {
                    self.world.resolve_tile_layer(
                        cur_region,
                        map.region_at(ux, uy),
                        RegionLayer::Wall,
                        byte,
                    )
                }
            };

            for y in 0..map.height {
                let odd = y & 1;
                for x in 0..map.width {
                    let center = origin + tile_to_screen(x as f32, y as f32, tile_w, tile_h);
                    if let Some(clip) = chat_clip_rect {
                        let chat_cull = egui::Rect::from_min_max(
                            Pos2::new(clip.min.x - tile_w * 8.0, clip.min.y - tile_h * 8.0),
                            Pos2::new(clip.max.x + tile_w * 8.0, clip.max.y + tile_h * 16.0),
                        );
                        if !chat_cull.contains(center) {
                            continue;
                        }
                    }
                    if !cull_rect.contains(center) {
                        continue;
                    }
                    let tile_region = map.region_at(x, y);
                    let raw_wall = map.wall_at(x, y);
                    let raw_nw = raw_wall & 0xff;
                    let raw_ne = (raw_wall >> 8) & 0xff;
                    let wall_nw = if raw_nw > 0 {
                        self.world.resolve_tile_layer(
                            cur_region,
                            tile_region,
                            RegionLayer::Wall,
                            raw_nw,
                        )
                    } else {
                        0
                    };
                    let wall_ne = if raw_ne > 0 {
                        self.world.resolve_tile_layer(
                            cur_region,
                            tile_region,
                            RegionLayer::Wall,
                            raw_ne,
                        )
                    } else {
                        0
                    };
                    let (ul_x, ul_y, ur_x, ur_y) = if odd != 0 {
                        (x as isize - 1, y as isize - 1, x as isize, y as isize - 1)
                    } else {
                        (x as isize, y as isize - 1, x as isize + 1, y as isize - 1)
                    };
                    let upper_nw = resolve_wall_byte(ul_x, ul_y, true);
                    let upper_ne = resolve_wall_byte(ur_x, ur_y, false);
                    tiles_to_draw.push(TileDrawEntry {
                        x,
                        y,
                        center,
                        tile_top_left: center - Vec2::new(33.0, 79.0) * scale,
                        floor_id: self.world.resolve_tile_layer(
                            cur_region,
                            tile_region,
                            RegionLayer::Floor,
                            map.floor_at(x, y),
                        ),
                        item_id: self.world.resolve_tile_layer(
                            cur_region,
                            tile_region,
                            RegionLayer::Item,
                            map.item_at(x, y),
                        ),
                        wall_nw,
                        wall_ne,
                        upper_nw,
                        upper_ne,
                        effect_id: self.world.resolve_tile_layer(
                            cur_region,
                            tile_region,
                            RegionLayer::Effect,
                            map.effect_at(x, y),
                        ),
                        floor_elev: (0, 0),
                    });
                }
            }
        } else {
            let (cx, cy) = self.world.self_pos.unwrap_or((20, 20));
            let min_x = cx.saturating_sub(18) as usize;
            let max_x = (cx as usize) + 18;
            let min_y = cy.saturating_sub(24) as usize;
            let max_y = (cy as usize) + 24;
            for y in min_y..=max_y {
                for x in min_x..=max_x {
                    let center = origin + tile_to_screen(x as f32, y as f32, tile_w, tile_h);
                    if !cull_rect.contains(center) {
                        continue;
                    }
                    tiles_to_draw.push(TileDrawEntry {
                        x,
                        y,
                        center,
                        tile_top_left: center - Vec2::new(33.0, 79.0) * scale,
                        floor_id: 0,
                        item_id: 0,
                        wall_nw: 0,
                        wall_ne: 0,
                        upper_nw: 0,
                        upper_ne: 0,
                        effect_id: 0,
                        floor_elev: (0, 0),
                    });
                }
            }
        }

        // Pass 1: row-major floor rendering with AnchorX=33, AnchorY=79 cell_origin.
        let mut floor_elevs: HashMap<(usize, usize), (i16, i16)> = HashMap::new();
        let mut tile_elevs: HashMap<(usize, usize), (i16, i16)> = HashMap::new();
        for tile in &mut tiles_to_draw {
            // Floor 161 is the DreamEd "no floor" placeholder (a pit sprite in the default
            // `floors.fox`); Furcadia renders it as empty black unless the dream defines its own.
            if tile.floor_id == 161
                && !self
                    .floors_cache
                    .dream_archive
                    .as_ref()
                    .is_some_and(|f| f.resolve_shape(161, 0).is_some())
            {
                continue;
            }
            if let Some(cs) = self.resolve_floor_sprite(&ctx, tile.floor_id, tile.x, tile.y) {
                tile.floor_elev = cs.elev;
                if cs.elev != (0, 0) {
                    floor_elevs.insert((tile.x, tile.y), cs.elev);
                    tile_elevs.insert((tile.x, tile.y), cs.elev);
                }
                if !cs.is_one_by_one {
                    draw_cached_sprite_at(&painter, tile.tile_top_left, (0, 0), scale, &cs);
                }
            } else {
                let diamond = [
                    Pos2::new(tile.center.x, tile.center.y - tile_h * 0.5),
                    Pos2::new(tile.center.x + tile_w * 0.5, tile.center.y),
                    Pos2::new(tile.center.x, tile.center.y + tile_h * 0.5),
                    Pos2::new(tile.center.x - tile_w * 0.5, tile.center.y),
                ];
                let floor_color = if tile.floor_id == 0 {
                    Color32::from_rgb(28, 34, 42)
                } else {
                    let [r, g, b] = self
                        .palette
                        .rgb(((tile.floor_id.wrapping_mul(17)) % 240 + 10) as u8);
                    Color32::from_rgb(r, g, b)
                };
                painter.add(egui::Shape::convex_polygon(
                    diamond.to_vec(),
                    floor_color,
                    Stroke::new(0.6_f32, Color32::from_rgba_unmultiplied(0, 0, 0, 70)),
                ));
            }
        }

        let portal_states: HashMap<(u16, u16), i32> = self
            .world
            .dream_labels
            .iter()
            .map(|(&pos, dl)| (pos, dl.state_flag))
            .collect();

        let mut sorted_avatars: Vec<_> = self.world.avatars.values().cloned().collect();
        sorted_avatars.sort_by_key(|a| (a.y, a.x, a.uid));

        let mut hitboxes: Vec<AvatarHitBox> = Vec::new();
        let mut name_badges: Vec<NameBadge> = Vec::new();
        let mut av_idx = 0usize;
        let mut any_animated = false;
        let mut shadow_ops: Vec<MapDrawOp> = Vec::new();
        let mut fg_ops: Vec<MapDrawOp> = Vec::new();
        let thresh = self.world.region_defaults.indoor_threshold;

        // Pass 2: depth-sorted junctions, walls, items (slots 0..2 & 2..4), effects, and avatars.
        for tile in &tiles_to_draw {
            while av_idx < sorted_avatars.len() {
                let av = &sorted_avatars[av_idx];
                let ay = av.y.max(0) as usize;
                let ax = av.x.max(0) as usize;
                if (ay, ax) >= (tile.y, tile.x) {
                    break;
                }
                let hidden = self.world.map.as_ref().is_some_and(|m| {
                    ax < m.width && ay < m.height && {
                        let tr = m.region_at(ax, ay);
                        tr >= thresh && cur_region >= thresh && tr != cur_region
                    }
                });
                if !hidden {
                    let av_elev = tile_elevs.get(&(ax, ay)).copied().unwrap_or((0, 0));
                    let pos = origin
                        + tile_to_screen(av.x as f32, av.y as f32, tile_w, tile_h)
                        + Vec2::new(av_elev.0 as f32, av_elev.1 as f32) * scale;
                    let in_chat_view = chat_clip_rect
                        .is_none_or(|clip| clip.expand2(Vec2::new(16.0 * scale, 16.0 * scale)).contains(pos));
                    if in_chat_view && cull_rect.contains(pos) {
                        self.collect_avatar_ops(
                            &ctx,
                            pos,
                            tile_h,
                            scale,
                            anim_ms,
                            av,
                            &mut hitboxes,
                            &mut name_badges,
                            &mut shadow_ops,
                            &mut fg_ops,
                            &mut any_animated,
                        );
                    }
                }
                av_idx += 1;
            }

            let (group_or, j_mask) = compute_junction_state(
                tile.upper_nw,
                tile.upper_ne,
                tile.wall_nw,
                tile.wall_ne,
            );
            if let Some(cs) = self.resolve_junction_sprite(&ctx, group_or, j_mask) {
                if !cs.is_one_by_one {
                    push_cached_sprite_op(&mut fg_ops, tile.tile_top_left, (0, -16), scale, &cs);
                }
            }

            if tile.wall_nw > 0 || tile.wall_ne > 0 {
                let diamond = [
                    Pos2::new(tile.center.x, tile.center.y - tile_h * 0.5),
                    Pos2::new(tile.center.x + tile_w * 0.5, tile.center.y),
                    Pos2::new(tile.center.x, tile.center.y + tile_h * 0.5),
                    Pos2::new(tile.center.x - tile_w * 0.5, tile.center.y),
                ];
                for (edge_byte, fox_dir, anchor_dx, _edge_line) in [
                    (tile.wall_nw, 9u8, -16i16, [diamond[3], diamond[0]]),
                    (tile.wall_ne, 7u8, 16i16, [diamond[0], diamond[1]]),
                ] {
                    let slot = edge_byte % 12;
                    let group = (edge_byte / 12) as usize;
                    if edge_byte == 0 || slot == 0 || group >= 18 {
                        continue;
                    }
                    if let Some(cs) = self.resolve_wall_edge_sprite(&ctx, edge_byte, fox_dir) {
                        if !cs.is_one_by_one {
                            push_cached_sprite_op(
                                &mut fg_ops,
                                tile.tile_top_left,
                                (anchor_dx, -8),
                                scale,
                                &cs,
                            );
                        }
                    } else if (slot >= 4 || slot == 1 || slot == 3)
                        && group > 1
                        && !self
                            .wall_caches
                            .get(&(group + 1))
                            .is_some_and(|c| c.is_loaded_for_mode(false))
                    {
                        // Missing wall_<n>.fox in dream/assets: skip silently instead of drawing wireframe lines.
                    }
                }
            }

            let portal_state = portal_states.get(&(tile.x as u16, tile.y as u16)).copied();
            let dream_overrides_item = tile.item_id == 128
                && self
                    .items_cache
                    .dream_archive
                    .as_ref()
                    .is_some_and(|f| f.resolve_shape(tile.item_id, 0).is_some());
            let eff_item = if tile.item_id > 0 {
                if dream_overrides_item {
                    tile.item_id
                } else {
                    resolve_item_variant(tile.item_id, tile.x, tile.y)
                }
            } else {
                0
            };
            let item_res = if eff_item > 0 || (self.world.is_modern && portal_state.is_some()) {
                self.resolve_item_layers(&ctx, eff_item, portal_state, anim_ms)
            } else {
                None
            };
            if let Some(ref res) = item_res {
                any_animated |= res.is_animated;
            } else if tile.item_id > 0 {
                let [r, g, b] = self
                    .palette
                    .rgb(((tile.item_id.wrapping_mul(29)) % 220 + 20) as u8);
                let pos = tile.center
                    + Vec2::new(tile.floor_elev.0 as f32, tile.floor_elev.1 as f32) * scale;
                fg_ops.push(MapDrawOp::CircleFilled(
                    pos,
                    (tile_h * 0.22).max(2.5),
                    Color32::from_rgb(r, g, b),
                ));
            }

            let eff_res = if tile.effect_id > 0 {
                self.resolve_effect_layers(&ctx, tile.effect_id, anim_ms)
            } else {
                None
            };
            if let Some(ref res) = eff_res {
                any_animated |= res.is_animated;
            }

            let item_elev = item_res.as_ref().map_or((0, 0), |r| r.elev);
            let fx_elev = eff_res.as_ref().map_or((0, 0), |r| r.elev);
            let fx_has_elev = eff_res.is_some() && fx_elev != (0, 0);
            let fx_shift = if fx_has_elev { tile.floor_elev } else { (0, 0) };
            let av_elev = if fx_has_elev {
                (
                    tile.floor_elev.0.saturating_add(fx_elev.0),
                    tile.floor_elev.1.saturating_add(fx_elev.1),
                )
            } else {
                (
                    tile.floor_elev.0.saturating_add(item_elev.0),
                    tile.floor_elev.1.saturating_add(item_elev.1),
                )
            };
            if av_elev != (0, 0) {
                tile_elevs.insert((tile.x, tile.y), av_elev);
            }

            let fx_pass = eff_res.as_ref().map_or(1, |r| r.effect_pass);
            // Pass 2a (slots 0 & 1 behind avatar): effect before item if fx_pass == 1.
            if fx_pass == 1 {
                if let Some(ref fx) = eff_res {
                    let fx_alpha = ((fx.alpha as u16 * 75) / 255) as u8;
                    collect_shape_slot_ops(
                        &fx.layers,
                        fx_alpha,
                        0..2,
                        tile.tile_top_left,
                        scale,
                        fx_shift,
                        &mut shadow_ops,
                        &mut fg_ops,
                    );
                }
                if let Some(ref it) = item_res {
                    collect_shape_slot_ops(
                        &it.layers,
                        it.alpha,
                        0..2,
                        tile.tile_top_left,
                        scale,
                        tile.floor_elev,
                        &mut shadow_ops,
                        &mut fg_ops,
                    );
                }
            } else {
                if let Some(ref it) = item_res {
                    collect_shape_slot_ops(
                        &it.layers,
                        it.alpha,
                        0..2,
                        tile.tile_top_left,
                        scale,
                        tile.floor_elev,
                        &mut shadow_ops,
                        &mut fg_ops,
                    );
                }
                if let Some(ref fx) = eff_res {
                    let fx_alpha = ((fx.alpha as u16 * 75) / 255) as u8;
                    collect_shape_slot_ops(
                        &fx.layers,
                        fx_alpha,
                        0..2,
                        tile.tile_top_left,
                        scale,
                        fx_shift,
                        &mut shadow_ops,
                        &mut fg_ops,
                    );
                }
            }

            let tile_hidden = self.world.map.as_ref().is_some_and(|m| {
                let tr = m.region_at(tile.x, tile.y);
                tr >= thresh && cur_region >= thresh && tr != cur_region
            });
            while av_idx < sorted_avatars.len() {
                let av = &sorted_avatars[av_idx];
                let ay = av.y.max(0) as usize;
                let ax = av.x.max(0) as usize;
                if (ay, ax) != (tile.y, tile.x) {
                    break;
                }
                if !tile_hidden {
                    let pos = tile.center + Vec2::new(av_elev.0 as f32, av_elev.1 as f32) * scale;
                    if cull_rect.contains(pos) {
                        self.collect_avatar_ops(
                            &ctx,
                            pos,
                            tile_h,
                            scale,
                            anim_ms,
                            av,
                            &mut hitboxes,
                            &mut name_badges,
                            &mut shadow_ops,
                            &mut fg_ops,
                            &mut any_animated,
                        );
                    }
                }
                av_idx += 1;
            }

            // Pass 2b (slots 2 & 3 in front of avatar): effect after item if fx_pass == 2.
            if fx_pass == 2 {
                if let Some(ref fx) = eff_res {
                    let fx_alpha = ((fx.alpha as u16 * 75) / 255) as u8;
                    collect_shape_slot_ops(
                        &fx.layers,
                        fx_alpha,
                        2..4,
                        tile.tile_top_left,
                        scale,
                        fx_shift,
                        &mut shadow_ops,
                        &mut fg_ops,
                    );
                }
                if let Some(ref it) = item_res {
                    collect_shape_slot_ops(
                        &it.layers,
                        it.alpha,
                        2..4,
                        tile.tile_top_left,
                        scale,
                        tile.floor_elev,
                        &mut shadow_ops,
                        &mut fg_ops,
                    );
                }
            } else {
                if let Some(ref it) = item_res {
                    collect_shape_slot_ops(
                        &it.layers,
                        it.alpha,
                        2..4,
                        tile.tile_top_left,
                        scale,
                        tile.floor_elev,
                        &mut shadow_ops,
                        &mut fg_ops,
                    );
                }
                if let Some(ref fx) = eff_res {
                    let fx_alpha = ((fx.alpha as u16 * 75) / 255) as u8;
                    collect_shape_slot_ops(
                        &fx.layers,
                        fx_alpha,
                        2..4,
                        tile.tile_top_left,
                        scale,
                        fx_shift,
                        &mut shadow_ops,
                        &mut fg_ops,
                    );
                }
            }
        }

        while av_idx < sorted_avatars.len() {
            let av = &sorted_avatars[av_idx];
            let ax = av.x.max(0) as usize;
            let ay = av.y.max(0) as usize;
            let hidden = self.world.map.as_ref().is_some_and(|m| {
                ax < m.width && ay < m.height && {
                    let tr = m.region_at(ax, ay);
                    tr >= thresh && cur_region >= thresh && tr != cur_region
                }
            });
            if !hidden {
                let av_elev = if let Some(&e) = tile_elevs.get(&(ax, ay)) {
                    e
                } else {
                    self.resolve_map_tile_elev(&ctx, cur_region, ax, ay, anim_ms)
                };
                let pos = origin
                    + tile_to_screen(av.x as f32, av.y as f32, tile_w, tile_h)
                    + Vec2::new(av_elev.0 as f32, av_elev.1 as f32) * scale;
                if cull_rect.contains(pos) {
                    self.collect_avatar_ops(
                        &ctx,
                        pos,
                        tile_h,
                        scale,
                        anim_ms,
                        av,
                        &mut hitboxes,
                        &mut name_badges,
                        &mut shadow_ops,
                        &mut fg_ops,
                        &mut any_animated,
                    );
                }
            }
            av_idx += 1;
        }

        emit_map_draw_ops(&painter, &shadow_ops);
        emit_map_draw_ops(&painter, &fg_ops);

        // Pass 3: floating dream portal titles & subtitles (`]s`), matching `tl` & `Br` (`L1951`, `L1456`).
        let dream_labels: Vec<furcadia_client::DreamLabel> =
            self.world.dream_labels.values().cloned().collect();
        let label_font = FontId::proportional((12.0 * self.zoom).clamp(10.0, 18.0));
        let label_max_w = 140.0 * self.zoom.max(0.85);
        for dl in dream_labels {
            let (lx, ly) = (dl.x as usize, dl.y as usize);
            let visible = if let Some(map) = &self.world.map {
                if lx >= map.width || ly >= map.height {
                    false
                } else {
                    let k = map.region_at(lx, ly);
                    k == cur_region || (cur_region < thresh && k < thresh)
                }
            } else {
                true
            };
            if !visible {
                continue;
            }
            let fe = if let Some(&e) = floor_elevs.get(&(lx, ly)) {
                e
            } else {
                self.resolve_map_floor_elev(&ctx, cur_region, lx, ly)
            };
            let anchor = origin
                + tile_to_screen(dl.x as f32, dl.y as f32, tile_w, tile_h)
                + Vec2::new(fe.0 as f32, -33.0 + fe.1 as f32) * scale;
            if !rect.expand(120.0).contains(anchor) {
                continue;
            }
            draw_dream_label_badge(
                &painter,
                anchor,
                &dl.title,
                &dl.subtitle,
                &label_font,
                label_max_w,
                &self.smiley_textures,
                scale,
            );
        }

        if any_animated && self.anim_mode != "Off" {
            let delay_ms = if self.anim_mode == "Half" { 160 } else { 80 };
            ctx.request_repaint_after(std::time::Duration::from_millis(delay_ms));
        }

        let badge_font = FontId::proportional((11.5 * self.zoom).clamp(9.0, 16.0));
        let prev_hover_uid = self.hovered_avatar_uid;
        let ctx_menu_uid = self.context_menu_avatar.as_ref().map(|c| c.uid);
        for badge in &name_badges {
            let is_highlighted =
                prev_hover_uid == Some(badge.uid) || ctx_menu_uid == Some(badge.uid);
            let show_this = self.show_avatar_names || is_highlighted;
            if !show_this {
                continue;
            }
            let label_pos = badge.pos - Vec2::new(0.0, tile_h * 1.25);
            if let Some(pill_rect) = draw_player_name_badge(
                &painter,
                rect,
                label_pos,
                &badge.name,
                badge.is_self,
                is_highlighted,
                &badge_font,
                &self.smiley_textures,
                scale,
            ) {
                if let Some(hb) = hitboxes.iter_mut().rev().find(|h| h.uid == badge.uid) {
                    hb.badge_rect = Some(pill_rect);
                }
            }
        }

        let hit_at = |p: Pos2| -> Option<&AvatarHitBox> {
            hitboxes
                .iter()
                .rev()
                .find(|h| h.contains_badge(p))
                .or_else(|| hitboxes.iter().rev().find(|h| h.contains_body(p)))
        };

        let new_hover_uid = response.hover_pos().and_then(|p| hit_at(p).map(|h| h.uid));
        if new_hover_uid != self.hovered_avatar_uid {
            self.hovered_avatar_uid = new_hover_uid;
            ctx.request_repaint();
        }
        if self.hovered_avatar_uid.is_some() {
            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
        }

        if response.secondary_clicked() {
            if let Some(cp) = response.interact_pointer_pos() {
                if let Some(hb) = hit_at(cp) {
                    self.context_menu_avatar = Some(ContextMenuTarget {
                        uid: hb.uid,
                        display_name: hb.display_name.clone(),
                        short_name: hb.short_name.clone(),
                        is_self: hb.is_self,
                        anchor_pos: cp,
                        opened_this_frame: true,
                    });
                } else {
                    self.context_menu_avatar = None;
                }
            } else {
                self.context_menu_avatar = None;
            }
        } else if response.clicked() {
            if self.context_menu_avatar.is_some() {
                self.context_menu_avatar = None;
            } else if let Some(cp) = response.interact_pointer_pos() {
                if let Some((uid, short_name, display_name, is_self)) = hit_at(cp).map(|hb| {
                    (
                        hb.uid,
                        hb.short_name.clone(),
                        hb.display_name.clone(),
                        hb.is_self,
                    )
                }) {
                    let ctrl_held = ui.input(|i| i.modifiers.ctrl || i.modifiers.mac_cmd);
                    if ctrl_held {
                        self.open_whisper_tab(&display_name, true);
                    } else {
                        if hitboxes.iter().rev().any(|h| h.contains_badge(cp)) {
                            self.context_menu_avatar = Some(ContextMenuTarget {
                                uid,
                                display_name,
                                short_name: short_name.clone(),
                                is_self,
                                anchor_pos: cp,
                                opened_this_frame: true,
                            });
                        }
                        if let Some(av) = self.world.avatars.get(&uid).cloned() {
                            if let Some((eff_species, _)) = self.world.resolved_avatar_species_gender(&av) {
                                let (file_id, _) = self.resolve_species_spec(eff_species as usize);
                                let loaded = self
                                    .species_caches
                                    .get(&file_id)
                                    .is_some_and(FoxSpriteCache::is_loaded);
                                if !loaded {
                                    self.pending_species_downloads.remove(&file_id);
                                    self.ensure_species_cache(file_id);
                                }
                            }
                            let base_file_id = self.resolve_species_spec(av.appearance.species_id as usize).0;
                            let base_loaded = self
                                .species_caches
                                .get(&base_file_id)
                                .is_some_and(FoxSpriteCache::is_loaded);
                            if !base_loaded {
                                self.pending_species_downloads.remove(&base_file_id);
                                self.ensure_species_cache(base_file_id);
                            }
                        }
                        if let Some(&pid) = self.player_portrait_ids.get(&short_name) {
                            if matches!(self.custom_portrait_rgba.get(&pid), Some(None)) {
                                self.custom_portrait_rgba.remove(&pid);
                                self.pending_portrait_downloads.remove(&pid);
                            }
                            if !self.custom_portrait_rgba.contains_key(&pid)
                                && self.pending_portrait_downloads.insert(pid)
                            {
                                let _ = self.to_net.send(UiToNet::DownloadPortrait { portrait_id: pid });
                            }
                        }
                        if self.is_connected {
                            let _ = self
                                .to_net
                                .send(UiToNet::Send(ClientCommand::Glook(short_name)));
                        }
                    }
                } else if self.is_connected {
                    let self_screen = self
                        .world
                        .self_pos
                        .map(|(sx, sy)| {
                            origin
                                + tile_to_screen(sx as f32, sy as f32, tile_w, tile_h)
                                + Vec2::new(self_elev.0 as f32, self_elev.1 as f32) * scale
                        })
                        .unwrap_or_else(|| rect.center());
                    let d = cp - self_screen;
                    let dir = match (d.x < 0.0, d.y < 0.0) {
                        (true, true) => 7,
                        (false, true) => 9,
                        (true, false) => 1,
                        (false, false) => 3,
                    };
                    let _ = self.to_net.send(UiToNet::Send(ClientCommand::Move(dir)));
                }
            }
        }

        if let Some((sx, sy)) = self.world.self_pos {
            let already_drawn = self
                .world
                .self_uid
                .is_some_and(|uid| self.world.avatars.contains_key(&uid));
            if !already_drawn {
                let pos = origin
                    + tile_to_screen(sx as f32, sy as f32, tile_w, tile_h)
                    + Vec2::new(self_elev.0 as f32, self_elev.1 as f32) * scale;
                painter.circle_filled(
                    pos - Vec2::new(0.0, tile_h * 0.3),
                    7.5 * self.zoom,
                    Color32::from_rgb(90, 220, 140),
                );
                if let Some(name) = &self.world.self_name {
                    painter.text(
                        pos - Vec2::new(0.0, tile_h * 0.75),
                        egui::Align2::CENTER_BOTTOM,
                        name,
                        FontId::proportional((12.0 * self.zoom).clamp(9.0, 18.0)),
                        Color32::WHITE,
                    );
                }
            }
        }

        self.last_visible_avatar_count = hitboxes.len();

        self.render_player_context_menu(&ctx, ctx.screen_rect());
    }

    fn toggle_pounce_watch(&mut self, display_name: &str, short_name: &str) {
        if self.pounce.contains(display_name) {
            self.pounce.remove_watch(display_name);
            self.save_config();
            if self.is_connected {
                self.send_raw_cmd(&format!("pounce remuser {short_name}"));
            }
        } else {
            self.pounce.add_watch(display_name.to_owned());
            self.save_config();
            if self.is_connected {
                self.send_raw_cmd(&format!("pounce adduser {short_name}"));
            }
        }
    }

    fn execute_player_action(
        &mut self,
        display_name: &str,
        short_name: &str,
        action: PlayerMenuAction,
    ) {
        match action {
            PlayerMenuAction::Look => {
                if self.is_connected {
                    let _ = self
                        .to_net
                        .send(UiToNet::Send(ClientCommand::Glook(short_name.to_owned())));
                }
            }
            PlayerMenuAction::Whisper => {
                self.open_whisper_tab(display_name, true);
            }
            PlayerMenuAction::Join => self.send_raw_cmd(&format!("join {short_name}")),
            PlayerMenuAction::Summon => self.send_raw_cmd(&format!("summon {short_name}")),
            PlayerMenuAction::Follow => self.send_raw_cmd(&format!("follow {short_name}")),
            PlayerMenuAction::Lead => self.send_raw_cmd(&format!("lead {short_name}")),
            PlayerMenuAction::Cuddle => self.send_raw_cmd(&format!("cuddle {short_name}")),
            PlayerMenuAction::TogglePounce => self.toggle_pounce_watch(display_name, short_name),
            PlayerMenuAction::ToggleIgnore => self.send_raw_cmd(&format!("ignore {short_name}")),
            PlayerMenuAction::ShareEdit => self.send_raw_cmd(&format!("share {short_name}")),
            PlayerMenuAction::UnshareEdit => self.send_raw_cmd(&format!("unshare {short_name}")),
            PlayerMenuAction::Raw(cmd) => self.send_raw_cmd(cmd),
        }
    }

    fn handle_chat_click_action(&mut self, action: ChatClickAction) {
        match action {
            ChatClickAction::OpenPlayerMenu {
                display_name,
                short_name,
                pos,
            } => {
                let short = if short_name.trim().is_empty() {
                    wire_player_name(&display_name)
                } else {
                    wire_player_name(&short_name)
                };
                if short.is_empty() {
                    return;
                }
                let disp = if display_name.trim().is_empty() {
                    short.clone()
                } else {
                    display_name.trim().to_owned()
                };
                let self_disp = self
                    .world
                    .self_name
                    .as_deref()
                    .or_else(|| self.selected_character().map(|c| c.name.as_str()))
                    .unwrap_or("");
                let is_self = !self_disp.is_empty() && wire_player_name(self_disp) == short;
                let uid = self
                    .world
                    .avatars
                    .values()
                    .find(|a| wire_player_name(&a.name) == short)
                    .map(|a| a.uid)
                    .unwrap_or(0);
                self.context_menu_avatar = Some(ContextMenuTarget {
                    uid,
                    display_name: disp,
                    short_name: short.clone(),
                    is_self,
                    anchor_pos: pos,
                    opened_this_frame: true,
                });
                if self.is_connected {
                    let _ = self
                        .to_net
                        .send(UiToNet::Send(ClientCommand::Glook(short)));
                }
            }
            ChatClickAction::EnterDreamUrl(url) => {
                self.send_raw_cmd(&format!("fdl {url}"));
            }
            ChatClickAction::SendCommand(cmd) => {
                let trimmed = cmd.trim();
                if !trimmed.is_empty() {
                    self.send_raw_cmd(trimmed);
                }
            }
        }
    }

    fn render_player_context_menu(&mut self, ctx: &egui::Context, viewport: Rect) {
        let Some(target) = self.context_menu_avatar.clone() else {
            return;
        };
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.context_menu_avatar = None;
            return;
        }
        let in_pounce = !target.is_self && self.pounce.contains(&target.display_name);
        let menu_w = 172.0_f32;
        let menu_h = if target.is_self { 245.0_f32 } else { 300.0_f32 };
        let menu_pos = Pos2::new(
            target
                .anchor_pos
                .x
                .min((viewport.max.x - menu_w - 6.0).max(viewport.min.x + 4.0)),
            target
                .anchor_pos
                .y
                .min((viewport.max.y - menu_h - 6.0).max(viewport.min.y + 4.0)),
        );
        let mut chosen_action: Option<PlayerMenuAction> = None;
        let area_resp = egui::Area::new(egui::Id::new("player_ctx_menu"))
            .fixed_pos(menu_pos)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(148.0);
                    ui.label(
                        RichText::new(&target.display_name)
                            .strong()
                            .color(self.theme.accent()),
                    );
                    ui.separator();
                    chosen_action =
                        render_player_menu_buttons(ui, target.is_self, in_pounce, false);
                });
            });
        if let Some(action) = chosen_action {
            self.execute_player_action(&target.display_name, &target.short_name, action);
            self.context_menu_avatar = None;
        } else if !target.opened_this_frame && area_resp.response.clicked_elsewhere() {
            self.context_menu_avatar = None;
        } else if let Some(cur) = self.context_menu_avatar.as_mut() {
            cur.opened_this_frame = false;
        }
    }

    fn render_looked_player_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(snapshot) = self.looked_player.clone() else {
            return;
        };
        let eff = {
            let sp = snapshot.info.appearance.species_id;
            if sp == 1000 {
                None
            } else if (1001..=1080).contains(&sp) {
                let (mapped_sp, mapped_gender) =
                    self.world.local_species_remap[(sp - 1001) as usize];
                if mapped_sp == 0 {
                    None
                } else {
                    Some((mapped_sp, mapped_gender))
                }
            } else {
                Some((
                    if sp == 0 { 1 } else { sp },
                    snapshot.info.appearance.gender,
                ))
            }
        };

        let (mut portrait_tex, mut badge_tex) = (snapshot.portrait_tex, snapshot.badge_tex);
        match eff {
            Some((eff_sp, eff_gender)) => {
                let cache_state = (eff_sp, eff_gender, snapshot.info.appearance.color_key);
                if snapshot.resolved_for != Some(cache_state) {
                    let (file_id, obj_id) = self.resolve_species_spec(eff_sp as usize);
                    let cache_key = self.ensure_species_cache(file_id);
                    if let Some(cache) = self.species_caches.get_mut(&cache_key) {
                        portrait_tex = cache.get_or_load_portrait_tex(
                            ctx,
                            &self.palette,
                            &snapshot.info.appearance,
                            obj_id,
                            eff_gender,
                            4,
                        );
                        badge_tex = cache.get_or_load_portrait_tex(
                            ctx,
                            &self.palette,
                            &snapshot.info.appearance,
                            obj_id,
                            eff_gender,
                            35,
                        );
                    } else {
                        portrait_tex = None;
                        badge_tex = None;
                    }
                    if let Some(card) = self.looked_player.as_mut() {
                        card.portrait_tex = portrait_tex.clone();
                        card.badge_tex = badge_tex.clone();
                        card.resolved_for = Some(cache_state);
                    }
                }
            }
            None => {
                portrait_tex = None;
                badge_tex = None;
                if snapshot.resolved_for.is_some() {
                    if let Some(card) = self.looked_player.as_mut() {
                        card.portrait_tex = None;
                        card.badge_tex = None;
                        card.resolved_for = None;
                    }
                }
            }
        }

        let accent = self.theme.accent();
        let display_name = snapshot.info.name.replace('|', " ");
        let in_pounce = self.pounce.contains(&display_name);
        let mut close_card = false;
        let mut action: Option<PlayerMenuAction> = None;

        egui::Frame::group(ui.style())
            .fill(Color32::from_rgb(28, 30, 48))
            .stroke(Stroke::new(1.0_f32, accent.linear_multiply(0.75)))
            .inner_margin(6.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(&display_name)
                            .strong()
                            .size(13.0)
                            .color(accent),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("×").on_hover_text("Dismiss").clicked() {
                            close_card = true;
                        }
                        if let Some(ref badge) = badge_tex {
                            let sz = badge.size_vec2();
                            let disp = if sz.x > 0.0 && sz.y > 0.0 {
                                let s = (20.0 / sz.y).min(1.0);
                                sz * s
                            } else {
                                Vec2::new(22.0, 19.0)
                            };
                            ui.image(egui::load::SizedTexture::new(badge.id(), disp));
                        }
                    });
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let port_size = Vec2::splat(self.portrait_size);
                    let custom_tex = snapshot
                        .portrait_id
                        .and_then(|pid| self.custom_portrait_texture(ctx, pid));
                    if let Some(ref tex) = custom_tex.or(portrait_tex) {
                        let resp = ui.image(egui::load::SizedTexture::new(tex.id(), port_size));
                        if let Some(pid) = snapshot.portrait_id {
                            resp.on_hover_text(format!("Portrait #{pid}"));
                        }
                    } else {
                        let (r, p) = ui.allocate_painter(port_size, Sense::hover());
                        p.rect_filled(r.rect, 4.0, Color32::from_rgb(20, 22, 34));
                        p.rect_stroke(
                            r.rect,
                            4.0,
                            Stroke::new(1.0_f32, Color32::from_rgb(55, 60, 82)),
                        );
                        let hint = snapshot
                            .portrait_id
                            .map(|id| format!("#{id}"))
                            .unwrap_or_else(|| "Portrait".to_owned());
                        p.text(
                            r.rect.center(),
                            egui::Align2::CENTER_CENTER,
                            hint,
                            FontId::proportional(10.5),
                            Color32::GRAY,
                        );
                    }
                    ui.vertical(|ui| {
                        ui.horizontal_wrapped(|ui| {
                            if ui.small_button("Whisper").clicked() {
                                action = Some(PlayerMenuAction::Whisper);
                            }
                            if ui.small_button("Join").clicked() {
                                action = Some(PlayerMenuAction::Join);
                            }
                            if ui.small_button("Summon").clicked() {
                                action = Some(PlayerMenuAction::Summon);
                            }
                            if ui.small_button("Follow").clicked() {
                                action = Some(PlayerMenuAction::Follow);
                            }
                            let pounce_lbl = if in_pounce { "-Pounce" } else { "+Pounce" };
                            if ui.small_button(pounce_lbl).clicked() {
                                action = Some(PlayerMenuAction::TogglePounce);
                            }
                            if self.is_connected
                                && !display_name.eq_ignore_ascii_case(&self.active_character_name())
                            {
                                if ui
                                    .small_button("🍪 Cookie")
                                    .on_hover_text(format!(
                                        "Give a cookie to {display_name} (uses one of your daily cookies; run `info` to check how many you have)"
                                    ))
                                    .clicked()
                                {
                                    self.send_raw_cmd(&format!("make-cookie {}", snapshot.short_name));
                                    self.push_system_line(
                                        "emit",
                                        &format!("[Cookie] Offering a cookie to {display_name}..."),
                                    );
                                }
                                ui.menu_button("🎁 Gift", |ui| {
                                    for (lbl, cmd) in [
                                        ("✋ Give Held Item", "give"),
                                        ("🌹 Rose", "give-rose"),
                                        ("🌹 Red Rose", "give-red-rose"),
                                        ("🥀 Black Rose", "give-black-rose"),
                                        ("💐 Bouquet", "give-bouquet"),
                                        ("📿 Bracelet", "give-bracelet"),
                                        ("🤗 Snug", "give-snug"),
                                        ("🍬 Candy Cane", "give-candy-cane"),
                                        ("💝 Valentine", "give-valentine"),
                                        ("🔥 Flaming Sword", "give-flaming-sword"),
                                    ] {
                                        if ui.button(lbl).clicked() {
                                            self.send_raw_cmd(&format!("{cmd} {}", snapshot.short_name));
                                            ui.close_menu();
                                        }
                                    }
                                });
                            }
                        });
                    });
                });
            });
        ui.add_space(6.0);

        if let Some(act) = action {
            self.execute_player_action(&display_name, &snapshot.short_name, act);
        }
        if close_card {
            self.looked_player = None;
        }
    }

    /// Computes active floor sprite elevation `(dx, dy)` for `(tx, ty)` under region rules.
    fn resolve_map_floor_elev(
        &mut self,
        ctx: &egui::Context,
        cur_region: u16,
        tx: usize,
        ty: usize,
    ) -> (i16, i16) {
        let Some((tile_region, raw_f)) = self.world.map.as_ref().and_then(|map| {
            if tx < map.width && ty < map.height {
                Some((map.region_at(tx, ty), map.floor_at(tx, ty)))
            } else {
                None
            }
        }) else {
            return (0, 0);
        };
        let eff_f =
            self.world
                .resolve_tile_layer(cur_region, tile_region, RegionLayer::Floor, raw_f);
        self.resolve_floor_sprite(ctx, eff_f, tx, ty)
            .map_or((0, 0), |cs| cs.elev)
    }

    /// Computes total `(floor_elev + item_or_effect_elev)` for `(tx, ty)` using active region rules.
    fn resolve_map_tile_elev(
        &mut self,
        ctx: &egui::Context,
        cur_region: u16,
        tx: usize,
        ty: usize,
        anim_ms: u64,
    ) -> (i16, i16) {
        let Some((tile_region, raw_f, raw_i, raw_e)) = self.world.map.as_ref().and_then(|map| {
            if tx < map.width && ty < map.height {
                Some((
                    map.region_at(tx, ty),
                    map.floor_at(tx, ty),
                    map.item_at(tx, ty),
                    map.effect_at(tx, ty),
                ))
            } else {
                None
            }
        }) else {
            return (0, 0);
        };
        let eff_f =
            self.world
                .resolve_tile_layer(cur_region, tile_region, RegionLayer::Floor, raw_f);
        let eff_i = resolve_item_variant(
            self.world
                .resolve_tile_layer(cur_region, tile_region, RegionLayer::Item, raw_i),
            tx,
            ty,
        );
        let eff_e =
            self.world
                .resolve_tile_layer(cur_region, tile_region, RegionLayer::Effect, raw_e);
        let portal_state = self
            .world
            .dream_labels
            .get(&(tx as u16, ty as u16))
            .map(|dl| dl.state_flag);
        let f_elev = self
            .resolve_floor_sprite(ctx, eff_f, tx, ty)
            .map_or((0, 0), |cs| cs.elev);
        let fx_elev = if eff_e > 0 {
            self.resolve_effect_layers(ctx, eff_e, anim_ms)
                .map_or((0, 0), |r| r.elev)
        } else {
            (0, 0)
        };
        let sub_elev = if fx_elev != (0, 0) {
            fx_elev
        } else if eff_i > 0 || (self.world.is_modern && portal_state.is_some()) {
            self.resolve_item_layers(ctx, eff_i, portal_state, anim_ms)
                .map_or((0, 0), |r| r.elev)
        } else {
            (0, 0)
        };
        (
            f_elev.0.saturating_add(sub_elev.0),
            f_elev.1.saturating_add(sub_elev.1),
        )
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    fn collect_avatar_ops(
        &mut self,
        ctx: &egui::Context,
        pos: Pos2,
        tile_h: f32,
        scale: f32,
        anim_ms: u64,
        av: &Avatar,
        hitboxes: &mut Vec<AvatarHitBox>,
        name_badges: &mut Vec<NameBadge>,
        shadow_ops: &mut Vec<MapDrawOp>,
        fg_ops: &mut Vec<MapDrawOp>,
        any_animated: &mut bool,
    ) {
        if !av.visible {
            return;
        }
        let Some((eff_species_u16, eff_gender)) = self.world.resolved_avatar_species_gender(av)
        else {
            return;
        };
        let is_self = self.world.self_uid == Some(av.uid);
        let (file_id, obj_id) = self.resolve_species_spec(eff_species_u16 as usize);
        let cache_key = self.ensure_species_cache(file_id);

        let mut drew_sprite = false;
        let mut sprite_bounds: Option<Rect> = None;
        let mut resolved_layers = self.species_caches.get_mut(&cache_key).and_then(|cache| {
            cache.get_or_load_avatar_layers(
                ctx,
                &self.palette,
                &av.appearance,
                obj_id,
                eff_gender,
                av.dir,
                av.pose,
                anim_ms,
            )
        });
        if resolved_layers.is_none() {
            let fallback_spec = av.appearance.species_id as usize;
            let (fb_file, fb_obj) = self.resolve_species_spec(fallback_spec);
            let fb_key = self.ensure_species_cache(fb_file);
            resolved_layers = self.species_caches.get_mut(&fb_key).and_then(|cache| {
                cache.get_or_load_avatar_layers(
                    ctx,
                    &self.palette,
                    &av.appearance,
                    fb_obj,
                    eff_gender,
                    av.dir,
                    av.pose,
                    anim_ms,
                )
            });
            if resolved_layers.is_none() {
                let s1_key = self.ensure_species_cache(1);
                resolved_layers = self.species_caches.get_mut(&s1_key).and_then(|cache| {
                    cache.get_or_load_avatar_layers(
                        ctx,
                        &self.palette,
                        &av.appearance,
                        0,
                        eff_gender,
                        av.dir,
                        av.pose,
                        anim_ms,
                    )
                });
            }
        }
        if let Some((layers, animated)) = resolved_layers {
            *any_animated |= animated;
            let av_s = if (av.scale - 1.0).abs() > f32::EPSILON {
                av.scale.max(0.01)
            } else {
                1.0
            };
            let pivot = if (av_s - 1.0).abs() > f32::EPSILON {
                let mut min_x = f32::INFINITY;
                let mut max_x = f32::NEG_INFINITY;
                let mut max_y = f32::NEG_INFINITY;
                for (tex, total_off, category) in &layers {
                    if (*category & 0x40) != 0 {
                        continue;
                    }
                    let sz = tex.size_vec2();
                    if sz.x <= 1.0 && sz.y <= 1.0 {
                        continue;
                    }
                    let lx0 = total_off.0 as f32 - 33.0;
                    let ly0 = total_off.1 as f32 - 79.0;
                    min_x = min_x.min(lx0);
                    max_x = max_x.max(lx0 + sz.x);
                    max_y = max_y.max(ly0 + sz.y);
                }
                if min_x.is_finite() {
                    Vec2::new((min_x + max_x) * 0.5, max_y)
                } else {
                    Vec2::ZERO
                }
            } else {
                Vec2::ZERO
            };
            let av_off = Vec2::new(av.offset.0 as f32 * scale, av.offset.1 as f32 * scale);
            for (tex, total_off, category) in layers {
                let is_shadow = (category & 0x40) != 0;
                let local_off = Vec2::new(total_off.0 as f32 - 33.0, total_off.1 as f32 - 79.0);
                let scaled_off = pivot + (local_off - pivot) * av_s;
                let top_left = pos + scaled_off * scale + av_off;
                let size = tex.size_vec2() * (scale * av_s);
                let sprite_rect = Rect::from_min_size(top_left, size);
                let tint = if is_shadow {
                    let shadow_a = if let Some((_, _, _, alpha)) = av.gloaming {
                        let out_a = 255_u8.saturating_sub(alpha);
                        ((115_u16 * out_a as u16) / 255) as u8
                    } else {
                        115
                    };
                    Color32::from_rgba_unmultiplied(255, 255, 255, shadow_a)
                } else if let Some((r, g, b, alpha)) = av.gloaming {
                    let out_a = 255_u8.saturating_sub(alpha);
                    if (r, g, b) == (0, 0, 0) {
                        Color32::from_rgba_unmultiplied(255, 255, 255, out_a)
                    } else {
                        Color32::from_rgba_unmultiplied(r, g, b, out_a)
                    }
                } else {
                    Color32::WHITE
                };
                let op = MapDrawOp::Sprite {
                    tex,
                    rect: sprite_rect,
                    tint,
                };
                if is_shadow {
                    shadow_ops.push(op);
                } else {
                    fg_ops.push(op);
                    sprite_bounds = Some(match sprite_bounds {
                        Some(b) => b.union(sprite_rect),
                        None => sprite_rect,
                    });
                    drew_sprite = true;
                }
            }
        }

        let mut fallback_center = None;
        if !drew_sprite {
            let body_color = if is_self {
                Color32::from_rgb(90, 220, 140)
            } else {
                Color32::from_rgb(100, 175, 245)
            };
            let body_center = pos - Vec2::new(0.0, tile_h * 0.35);
            let head_center = pos - Vec2::new(0.0, tile_h * 0.85);
            fg_ops.push(MapDrawOp::CircleFilled(
                body_center,
                8.0 * self.zoom,
                body_color,
            ));
            fg_ops.push(MapDrawOp::CircleStroke(
                body_center,
                8.0 * self.zoom,
                Stroke::new(1.2_f32, Color32::BLACK),
            ));
            fg_ops.push(MapDrawOp::CircleFilled(
                head_center,
                5.5 * self.zoom,
                body_color,
            ));
            fg_ops.push(MapDrawOp::CircleStroke(
                head_center,
                5.5 * self.zoom,
                Stroke::new(1.2_f32, Color32::BLACK),
            ));
            fallback_center = Some(body_center);
        }

        let display_name = av.name.replace('|', " ");
        let short_name = wire_player_name(&av.name);
        hitboxes.push(AvatarHitBox {
            uid: av.uid,
            display_name: display_name.clone(),
            short_name,
            is_self,
            sprite_rect: sprite_bounds,
            badge_rect: None,
            fallback_center,
            fallback_radius: 16.0 * self.zoom,
        });
        name_badges.push(NameBadge {
            uid: av.uid,
            pos,
            name: display_name,
            is_self,
        });
    }
}

/// Deterministic tile variant PRNG matching `Furcadia.exe` `0x4243c0(x, y)`.
fn furc_tile_prng(x: usize, y: usize) -> u16 {
    let xi = x as i32;
    let yi = y as i32;
    let seed = 18_i32
        .wrapping_mul(xi)
        .wrapping_add(yi)
        .wrapping_add(yi >> 2) as u32;
    (((seed.wrapping_mul(214_013).wrapping_add(2_531_011)) >> 16) & 0x7fff) as u16
}

/// Deterministic floor variant PRNG matching the Furcadia web/native floor renderer (`FloorVariant`).
fn resolve_floor_variant(floor_id: u16, x: usize, y: usize) -> u16 {
    match floor_id {
        0 | 32 | 40 => floor_id.wrapping_add(furc_tile_prng(x, y) & 7),
        8 | 116 | 120 | 124 => floor_id.wrapping_add(furc_tile_prng(x, y) & 3),
        _ => floor_id,
    }
}

/// Deterministic tile variant PRNG matching `Furcadia.exe` `0x47bf80` -> `0x4243c0(x, y, 16) + 128`.
fn resolve_item_variant(item_id: u16, x: usize, y: usize) -> u16 {
    if item_id == 128 {
        128 + (furc_tile_prng(x, y) & 0x0f)
    } else {
        item_id
    }
}

fn cache_has_portal_shapes(cache: &FoxSpriteCache, obj_id: u16) -> bool {
    [cache.dream_archive.as_ref(), cache.default_archive.as_ref()]
        .into_iter()
        .flatten()
        .any(|fox| fox.has_portal_shapes(obj_id))
}

/// Resolves portal layers from `cache`, preferring active `p=29` highest `s <= flag` when open (`flag >= 0`),
/// or closed `p=28 s=0` otherwise, matching `Furcadia.exe` `0x435950`.
fn resolve_portal_layers_from(
    cache: &mut FoxSpriteCache,
    ctx: &egui::Context,
    palette: &Palette,
    pid: u16,
    portal_state: Option<i32>,
    anim_ms: u64,
) -> Option<ResolvedShapeLayers> {
    let pick_best_state = |cache: &FoxSpriteCache, obj_id: u16, purpose: u8, cap: u8| -> Option<u8> {
        for maybe_fox in [cache.dream_archive.as_ref(), cache.default_archive.as_ref()] {
            let Some(fox) = maybe_fox else {
                continue;
            };
            let Some(shapes) = fox.object_avatar_shapes.get(&obj_id) else {
                continue;
            };
            if let Some(best) = shapes
                .iter()
                .filter(|s| {
                    s.purpose == purpose
                        && s.state <= cap
                        && (!s.frames.is_empty() || !s.layers.is_empty())
                })
                .map(|s| s.state)
                .max()
            {
                return Some(best);
            }
        }
        None
    };

    if let Some(flag) = portal_state.filter(|&f| f >= 0) {
        let slot = flag as usize;
        let prefix = cache.prefix.clone();
        for (is_dream, maybe_fox) in [
            (true, cache.dream_archive.as_mut()),
            (false, cache.default_archive.as_mut()),
        ] {
            let Some(fox) = maybe_fox else {
                continue;
            };
            let Some(shape) = fox.resolve_portal_swirl_shape(pid, slot).cloned() else {
                continue;
            };
            if let Some(res) = FoxSpriteCache::resolve_shape_slots(
                fox,
                if is_dream { ARCHIVE_DREAM } else { ARCHIVE_DEFAULT },
                &prefix,
                &mut cache.image_textures,
                ctx,
                palette,
                &shape,
                anim_ms,
            ) {
                return Some(res);
            }
        }
        let cap = flag.clamp(0, 255) as u8;
        let best_state = pick_best_state(cache, pid, 29, cap);
        if best_state.is_some() {
            if let res @ Some(_) =
                cache.get_or_load_ps_layers(ctx, palette, pid, 29, best_state, 0, anim_ms)
            {
                return res;
            }
        }
        if !cache_has_portal_shapes(cache, pid) {
            if let res @ Some(_) = cache.get_or_load_shape_layers(ctx, palette, pid, 0, anim_ms) {
                return res;
            }
        }
        if pid == 0 {
            cache
                .get_or_load_ps_layers(ctx, palette, pid, 29, None, 0, anim_ms)
                .or_else(|| cache.get_or_load_ps_layers(ctx, palette, pid, 28, Some(0), 0, anim_ms))
                .or_else(|| cache.get_or_load_shape_layers(ctx, palette, pid, 0, anim_ms))
        } else {
            None
        }
    } else {
        cache
            .get_or_load_ps_layers(ctx, palette, pid, 28, Some(0), 0, anim_ms)
            .or_else(|| cache.get_or_load_ps_layers(ctx, palette, pid, 28, None, 0, anim_ms))
            .or_else(|| cache.get_or_load_shape_layers(ctx, palette, pid, 0, anim_ms))
    }
}

/// Computes `junction.fox` group (`group_or`) and 4-neighbor connection bitmask at `(x, y)`.
fn compute_junction_state(upper_nw: u16, upper_ne: u16, cur_nw: u16, cur_ne: u16) -> (usize, u8) {
    let split = |b: u16| -> (usize, u16) {
        if b > 0 && b % 12 != 0 && (b / 12) < 18 {
            ((b / 12) as usize, b % 12)
        } else {
            (0, 0)
        }
    };
    let (g_unw, s_unw) = split(upper_nw);
    let (g_une, s_une) = split(upper_ne);
    let (g_cnw, s_cnw) = split(cur_nw);
    let (g_cne, s_cne) = split(cur_ne);
    let group_or = g_unw | g_une | g_cnw | g_cne;
    let mut mask = 0u8;
    if matches!(s_unw, 1 | 2) {
        mask |= 0x1;
    }
    if matches!(s_une, 1 | 2) {
        mask |= 0x2;
    }
    if matches!(s_cnw, 1 | 2) {
        mask |= 0x4;
    }
    if matches!(s_cne, 1 | 2) {
        mask |= 0x8;
    }
    (group_or, mask)
}

enum MapDrawOp {
    Sprite {
        tex: TextureHandle,
        rect: Rect,
        tint: Color32,
    },
    CircleFilled(Pos2, f32, Color32),
    CircleStroke(Pos2, f32, Stroke),
}

fn push_cached_sprite_op(
    ops: &mut Vec<MapDrawOp>,
    tile_top_left: Pos2,
    anchor_off: (i16, i16),
    scale: f32,
    cs: &CachedSprite,
) {
    let top_left = tile_top_left
        + Vec2::new(
            (cs.offset.0 + anchor_off.0) as f32,
            (cs.offset.1 + anchor_off.1) as f32,
        ) * scale;
    let size = cs.tex.size_vec2() * scale;
    ops.push(MapDrawOp::Sprite {
        tex: cs.tex.clone(),
        rect: Rect::from_min_size(top_left, size),
        tint: Color32::WHITE,
    });
}

#[allow(clippy::too_many_arguments)]
fn collect_shape_slot_ops(
    layers: &[ResolvedLayerTex],
    shape_alpha: u8,
    slots: std::ops::Range<u8>,
    tile_top_left: Pos2,
    scale: f32,
    elev_shift: (i16, i16),
    shadow_ops: &mut Vec<MapDrawOp>,
    fg_ops: &mut Vec<MapDrawOp>,
) {
    for l in layers {
        if l.is_one_by_one || (l.category & 0x80) != 0 || !slots.contains(&l.slot) {
            continue;
        }
        let is_shadow = (l.category & 0x40) != 0;
        let top_left = tile_top_left
            + Vec2::new(
                (l.offset.0 + elev_shift.0) as f32,
                (l.offset.1 + elev_shift.1) as f32,
            ) * scale;
        let size = l.tex.size_vec2() * scale;
        let tint = if is_shadow {
            let shadow_a = ((115_u16 * shape_alpha as u16) / 255) as u8;
            Color32::from_rgba_unmultiplied(255, 255, 255, shadow_a)
        } else {
            Color32::from_rgba_unmultiplied(255, 255, 255, shape_alpha)
        };
        let op = MapDrawOp::Sprite {
            tex: l.tex.clone(),
            rect: Rect::from_min_size(top_left, size),
            tint,
        };
        if is_shadow {
            shadow_ops.push(op);
        } else {
            fg_ops.push(op);
        }
    }
}

fn emit_map_draw_ops(painter: &egui::Painter, ops: &[MapDrawOp]) {
    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
    for op in ops {
        match op {
            MapDrawOp::Sprite { tex, rect, tint } => {
                painter.image(tex.id(), *rect, uv, *tint);
            }
            MapDrawOp::CircleFilled(center, radius, color) => {
                painter.circle_filled(*center, *radius, *color);
            }
            MapDrawOp::CircleStroke(center, radius, stroke) => {
                painter.circle_stroke(*center, *radius, *stroke);
            }
        }
    }
}

fn draw_sprite_at(
    painter: &egui::Painter,
    tex: &TextureHandle,
    top_left: Pos2,
    scale: f32,
    tint: Color32,
) {
    let size = tex.size_vec2() * scale;
    painter.image(
        tex.id(),
        Rect::from_min_size(top_left, size),
        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
        tint,
    );
}

fn draw_cached_sprite_at(
    painter: &egui::Painter,
    tile_top_left: Pos2,
    anchor_off: (i16, i16),
    scale: f32,
    cs: &CachedSprite,
) {
    let top_left = tile_top_left
        + Vec2::new(
            (cs.offset.0 + anchor_off.0) as f32,
            (cs.offset.1 + anchor_off.1) as f32,
        ) * scale;
    draw_sprite_at(painter, &cs.tex, top_left, scale, Color32::WHITE);
}

#[derive(Clone)]
enum BadgeSeg {
    Text(String),
    Smiley { idx: usize, code: char },
}

fn smiley_index_for_code(ch: char) -> Option<usize> {
    match ch {
        'A'..='P' => Some((ch as usize) - ('A' as usize)),
        'Q'..='Z' => Some(16 + (ch as usize) - ('Q' as usize)),
        'a'..='y' => Some(26 + (ch as usize) - ('a' as usize)),
        '1'..='3' => Some(51 + (ch as usize) - ('1' as usize)),
        _ => None,
    }
}

fn parse_furc_badge_segments(raw: &str) -> Vec<BadgeSeg> {
    let decoded = unescape_html(raw);
    let mut segs = Vec::new();
    let mut buf = String::new();
    let mut chars = decoded.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '#' && chars.peek() == Some(&'S') {
            let mut lookahead = chars.clone();
            lookahead.next();
            if let Some(code) = lookahead.next() {
                if let Some(idx) = smiley_index_for_code(code) {
                    if !buf.is_empty() {
                        segs.push(BadgeSeg::Text(std::mem::take(&mut buf)));
                    }
                    segs.push(BadgeSeg::Smiley { idx, code });
                    chars.next();
                    chars.next();
                    continue;
                }
            }
        }
        buf.push(c);
    }
    if !buf.is_empty() {
        segs.push(BadgeSeg::Text(buf));
    }
    segs
}

fn push_badge_seg_merged(out: &mut Vec<BadgeSeg>, seg: BadgeSeg) {
    if let BadgeSeg::Text(ref t) = seg {
        if let Some(BadgeSeg::Text(prev)) = out.last_mut() {
            prev.push_str(t);
            return;
        }
    }
    out.push(seg);
}

fn smiley_draw_size(tex: &TextureHandle, font: &FontId) -> Vec2 {
    let sz = tex.size_vec2();
    let h = (font.size * 1.18).max(10.0);
    let w = if sz.y > 0.0 {
        sz.x * (h / sz.y)
    } else {
        h
    };
    Vec2::new(w, h)
}

fn measure_badge_line(
    painter: &egui::Painter,
    segs: &[BadgeSeg],
    font: &FontId,
    smileys: &[Option<TextureHandle>],
) -> Vec2 {
    let mut w = 0.0_f32;
    let mut h = font.size;
    for seg in segs {
        match seg {
            BadgeSeg::Text(s) => {
                let g = painter.layout_no_wrap(s.clone(), font.clone(), Color32::BLACK);
                w += g.size().x;
                h = h.max(g.size().y);
            }
            BadgeSeg::Smiley { idx, code } => {
                if let Some(tex) = smileys.get(*idx).and_then(|o| o.as_ref()) {
                    let sz = smiley_draw_size(tex, font);
                    w += sz.x;
                    h = h.max(sz.y);
                } else {
                    let g = painter.layout_no_wrap(format!("#S{code}"), font.clone(), Color32::BLACK);
                    w += g.size().x;
                    h = h.max(g.size().y);
                }
            }
        }
    }
    Vec2::new(w, h)
}

fn wrap_badge_segments(
    painter: &egui::Painter,
    segs: &[BadgeSeg],
    font: &FontId,
    max_w: f32,
    smileys: &[Option<TextureHandle>],
) -> Vec<Vec<BadgeSeg>> {
    let mut logical_lines: Vec<Vec<BadgeSeg>> = vec![Vec::new()];
    for seg in segs {
        match seg {
            BadgeSeg::Text(s) => {
                let normalized = s.replace('\r', "");
                let mut first = true;
                for part in normalized.split('\n') {
                    if !first {
                        logical_lines.push(Vec::new());
                    }
                    first = false;
                    if !part.is_empty() {
                        if let Some(cur) = logical_lines.last_mut() {
                            cur.push(BadgeSeg::Text(part.to_string()));
                        }
                    }
                }
            }
            BadgeSeg::Smiley { .. } => {
                if let Some(cur) = logical_lines.last_mut() {
                    cur.push(seg.clone());
                }
            }
        }
    }

    let mut wrapped = Vec::new();
    for raw_line in logical_lines {
        let mut tokens: Vec<(BadgeSeg, bool)> = Vec::new();
        for seg in raw_line {
            match seg {
                BadgeSeg::Text(s) => {
                    let mut chunk = String::new();
                    let mut in_space = None;
                    for c in s.chars() {
                        let sp = c == ' ' || c == '\t';
                        if in_space.is_some_and(|prev| prev != sp) && !chunk.is_empty() {
                            tokens.push((
                                BadgeSeg::Text(std::mem::take(&mut chunk)),
                                in_space.unwrap_or(false),
                            ));
                        }
                        in_space = Some(sp);
                        chunk.push(c);
                    }
                    if !chunk.is_empty() {
                        tokens.push((BadgeSeg::Text(chunk), in_space.unwrap_or(false)));
                    }
                }
                BadgeSeg::Smiley { .. } => tokens.push((seg, false)),
            }
        }

        let mut cur: Vec<BadgeSeg> = Vec::new();
        let mut cur_w = 0.0_f32;
        for (tok, is_space) in tokens {
            let tok_w = measure_badge_line(painter, std::slice::from_ref(&tok), font, smileys).x;
            if !cur.is_empty() && cur_w + tok_w > max_w {
                while let Some(BadgeSeg::Text(last)) = cur.last() {
                    if last.trim().is_empty() {
                        cur.pop();
                    } else {
                        break;
                    }
                }
                if !cur.is_empty() {
                    wrapped.push(std::mem::take(&mut cur));
                }
                cur_w = 0.0;
                if is_space {
                    continue;
                }
            }
            if cur.is_empty() && is_space {
                continue;
            }
            cur_w += tok_w;
            push_badge_seg_merged(&mut cur, tok);
        }
        while let Some(BadgeSeg::Text(last)) = cur.last() {
            if last.trim().is_empty() {
                cur.pop();
            } else {
                break;
            }
        }
        if !cur.is_empty() {
            wrapped.push(cur);
        }
    }
    wrapped
}

fn draw_halo_text(
    painter: &egui::Painter,
    top_left: Pos2,
    text: &str,
    font: &FontId,
    fill: Color32,
    halo: Color32,
) {
    let fg = painter.layout_no_wrap(text.to_string(), font.clone(), fill);
    let bg = painter.layout_no_wrap(text.to_string(), font.clone(), halo);
    for (dx, dy) in [
        (-1.0_f32, -1.0),
        (0.0, -1.0),
        (1.0, -1.0),
        (-1.0, 0.0),
        (1.0, 0.0),
        (-1.0, 1.0),
        (0.0, 1.0),
        (1.0, 1.0),
    ] {
        painter.galley(top_left + Vec2::new(dx, dy), bg.clone(), halo);
    }
    painter.galley(top_left, fg, fill);
}

#[allow(clippy::too_many_arguments)]
fn draw_badge_line(
    painter: &egui::Painter,
    start: Pos2,
    line_h: f32,
    segs: &[BadgeSeg],
    font: &FontId,
    fill: Color32,
    halo: Color32,
    smileys: &[Option<TextureHandle>],
) {
    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
    let mut x = start.x;
    for seg in segs {
        match seg {
            BadgeSeg::Text(s) => {
                let g = painter.layout_no_wrap(s.clone(), font.clone(), fill);
                let sz = g.size();
                let y = start.y + (line_h - sz.y) * 0.5;
                draw_halo_text(painter, Pos2::new(x, y), s, font, fill, halo);
                x += sz.x;
            }
            BadgeSeg::Smiley { idx, code } => {
                if let Some(tex) = smileys.get(*idx).and_then(|o| o.as_ref()) {
                    let sz = smiley_draw_size(tex, font);
                    let y = start.y + (line_h - sz.y) * 0.5;
                    painter.image(tex.id(), Rect::from_min_size(Pos2::new(x, y), sz), uv, Color32::WHITE);
                    x += sz.x;
                } else {
                    let fallback = format!("#S{code}");
                    let g = painter.layout_no_wrap(fallback.clone(), font.clone(), fill);
                    let sz = g.size();
                    let y = start.y + (line_h - sz.y) * 0.5;
                    draw_halo_text(painter, Pos2::new(x, y), &fallback, font, fill, halo);
                    x += sz.x;
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_dream_label_badge(
    painter: &egui::Painter,
    anchor: Pos2,
    title: &str,
    subtitle: &str,
    font: &FontId,
    max_w: f32,
    smileys: &[Option<TextureHandle>],
    scale: f32,
) {
    let mut rows: Vec<(Vec<BadgeSeg>, Vec2, Color32)> = Vec::new();
    if !title.trim().is_empty() {
        let segs = parse_furc_badge_segments(title);
        for line in wrap_badge_segments(painter, &segs, font, max_w, smileys) {
            let sz = measure_badge_line(painter, &line, font, smileys);
            rows.push((line, sz, Color32::WHITE));
        }
    }
    if !subtitle.trim().is_empty() {
        let segs = parse_furc_badge_segments(subtitle);
        let sub_halo = Color32::from_rgb(0xc7, 0xcb, 0xe7);
        for line in wrap_badge_segments(painter, &segs, font, max_w, smileys) {
            let sz = measure_badge_line(painter, &line, font, smileys);
            rows.push((line, sz, sub_halo));
        }
    }
    if rows.is_empty() {
        return;
    }
    let line_gap = (1.0 * scale).clamp(0.5, 2.5);
    let total_h: f32 = rows.iter().map(|(_, sz, _)| sz.y).sum::<f32>()
        + line_gap * rows.len().saturating_sub(1) as f32;
    let mut y = anchor.y - total_h * 0.5;
    for (line, sz, halo) in rows {
        let x = anchor.x - sz.x * 0.5;
        draw_badge_line(
            painter,
            Pos2::new(x, y),
            sz.y,
            &line,
            font,
            Color32::BLACK,
            halo,
            smileys,
        );
        y += sz.y + line_gap;
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_player_name_badge(
    painter: &egui::Painter,
    viewport: Rect,
    label_pos: Pos2,
    raw_name: &str,
    is_self: bool,
    is_highlighted: bool,
    font: &FontId,
    smileys: &[Option<TextureHandle>],
    scale: f32,
) -> Option<Rect> {
    if !viewport.expand(24.0).contains(label_pos) {
        return None;
    }
    let segs = parse_furc_badge_segments(raw_name);
    if segs.is_empty() {
        return None;
    }
    let content_size = measure_badge_line(painter, &segs, font, smileys);
    if content_size.x <= 0.0 {
        return None;
    }
    let pad_x = (5.5 * scale).clamp(3.0, 9.0);
    let pad_y = (2.0 * scale).clamp(1.5, 4.0);
    let pill_size = content_size + Vec2::new(pad_x * 2.0, pad_y * 2.0);
    let min_x = viewport.min.x + 4.0;
    let max_x = (viewport.max.x - pill_size.x - 4.0).max(min_x);
    let min_y = viewport.min.y + 4.0;
    let max_y = (viewport.max.y - pill_size.y - 4.0).max(min_y);
    let pill_min = Pos2::new(
        (label_pos.x - pill_size.x * 0.5).clamp(min_x, max_x),
        (label_pos.y - pill_size.y * 0.5).clamp(min_y, max_y),
    );
    let pill_rect = Rect::from_min_size(pill_min, pill_size);
    let (fill_color, border_color) = if is_self {
        (
            Color32::from_rgb(0xc6, 0xc7, 0x7c),
            if is_highlighted {
                Color32::from_rgb(0xff, 0xf2, 0x99)
            } else {
                Color32::from_rgb(0x8d, 0x7c, 0x26)
            },
        )
    } else if is_highlighted {
        (
            Color32::from_rgba_unmultiplied(145, 198, 236, 215),
            Color32::from_rgb(0x66, 0xc4, 0xe8),
        )
    } else {
        (
            Color32::from_rgba_unmultiplied(119, 170, 209, 150),
            Color32::from_rgb(0x29, 0x70, 0x8a),
        )
    };
    painter.rect_filled(pill_rect, 3.0, fill_color);
    painter.rect_stroke(pill_rect, 3.0, Stroke::new(1.0_f32, border_color));
    draw_badge_line(
        painter,
        pill_rect.min + Vec2::new(pad_x, pad_y),
        content_size.y,
        &segs,
        font,
        Color32::BLACK,
        Color32::WHITE,
        smileys,
    );
    Some(pill_rect)
}

fn tile_to_screen(x: f32, y: f32, tile_w: f32, tile_h: f32) -> Vec2 {
    let sx = x * tile_w - if (y as i32) & 1 != 0 { tile_w * 0.5 } else { 0.0 };
    let sy = y * (tile_h * 0.5);
    Vec2::new(sx, sy)
}

/// Parses server cookie reports such as `Cookie bank has currently collected: 3` and
/// `All-time Cookie total: 12`, returning `(collected, all_time_total)`.
fn parse_cookie_counts(plain: &str) -> (Option<u32>, Option<u32>) {
    let lower = plain.to_ascii_lowercase();
    if !lower.contains("cookie") {
        return (None, None);
    }
    let after = |needle: &str| -> Option<u32> {
        let idx = lower.find(needle)? + needle.len();
        let digits: String = lower[idx..]
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        digits.parse().ok()
    };
    (
        after("currently collected:"),
        after("all-time cookie total:"),
    )
}

fn extract_whisper_peer(plain: &str) -> Option<String> {
    let s = plain.trim();
    if let Some(idx) = s.find(" whispers,") {
        let before = s[..idx].trim().trim_start_matches('[').trim().replace('|', " ");
        if !before.is_empty() {
            return Some(before);
        }
    }
    if s.starts_with("[ You whisper ") || s.starts_with("You whisper ") {
        if let Some(idx) = s.rfind(" to ") {
            let after = s[idx + 4..]
                .trim()
                .trim_end_matches(']')
                .trim()
                .trim_end_matches('.')
                .replace('|', " ");
            if !after.is_empty() {
                return Some(after);
            }
        }
    }
    None
}

/// Formats a whisper line for a dedicated peer whisper tab/window in regular chat style
/// (stripping the `[ You whisper "..." to Peer . ]` / `[ Peer whispers, "..." to you. ]` wrapper).
fn format_whisper_tab_line(
    self_name: &str,
    peer: &str,
    plain: &str,
    fallback: &ChatLine,
) -> ChatLine {
    let s = plain.trim().trim_start_matches('[').trim_end_matches(']').trim();
    let (speaker, body) = if let Some(rest) = s.strip_prefix("You whisper ") {
        if let Some(to_idx) = rest.rfind(" to ") {
            let quoted = rest[..to_idx].trim();
            let inner = quoted
                .strip_prefix('"')
                .and_then(|q| q.strip_suffix('"'))
                .unwrap_or(quoted);
            (self_name, inner)
        } else {
            return fallback.clone();
        }
    } else if let Some(w_idx) = s.find(" whispers,") {
        let after = s[w_idx + " whispers,".len()..].trim();
        if let Some(to_idx) = after.rfind(" to you") {
            let quoted = after[..to_idx].trim();
            let inner = quoted
                .strip_prefix('"')
                .and_then(|q| q.strip_suffix('"'))
                .unwrap_or(quoted);
            (peer, inner)
        } else {
            return fallback.clone();
        }
    } else {
        return fallback.clone();
    };

    let trimmed = body.trim();
    let speaker_disp = speaker.replace('|', " ");
    let speaker_space = speaker_disp.to_ascii_lowercase();
    let speaker_pipe = speaker.replace(' ', "|").to_ascii_lowercase();
    let lower = trimmed.to_ascii_lowercase();
    let is_name_prefixed_emote = lower.starts_with(&format!("{speaker_space} "))
        || lower.starts_with(&format!("{speaker_pipe} "))
        || lower.starts_with(&format!("{speaker_space}'"))
        || lower.starts_with(&format!("{speaker_pipe}'"));
    let escape_html = |text: &str| text.replace('&', "&amp;").replace('<', "&lt;");
    if let Some(emote) = trimmed.strip_prefix(':') {
        let emote_body = emote.trim_start();
        let line_html = format!(
            "<font color='emote'>{} {}</font>",
            escape_html(&speaker_disp),
            escape_html(emote_body)
        );
        parse_chat_line(&line_html)
    } else if is_name_prefixed_emote {
        let normalized = if lower.starts_with(&speaker_pipe) && speaker_pipe != speaker_space {
            let rest = &trimmed[speaker_pipe.len()..];
            format!("{speaker_disp}{rest}")
        } else {
            trimmed.to_owned()
        };
        parse_chat_line(&format!("<font color='emote'>{}</font>", escape_html(&normalized)))
    } else {
        parse_chat_line(&format!(
            "<name>{}</name>: {}",
            escape_html(&speaker_disp),
            escape_html(trimmed)
        ))
    }
}

fn render_chat_image_span(
    ui: &mut egui::Ui,
    src: &str,
    alt: Option<&str>,
    smileys: &[Option<TextureHandle>],
    desctags: &[Option<TextureHandle>],
    chat_imgs: &HashMap<String, Option<TextureHandle>>,
) {
    let trimmed_alt = alt.map(str::trim).unwrap_or("");
    let lower_alt = trimmed_alt.to_ascii_lowercase();
    let lower_src = src.trim().to_ascii_lowercase();
    let font_id = egui::TextStyle::Body.resolve(ui.style());

    if lower_src.starts_with("http://") || lower_src.starts_with("https://") {
        if let Some(Some(tex)) = chat_imgs.get(src.trim()) {
            let sz = tex.size_vec2();
            let target_h = (font_id.size * 1.45).clamp(14.0, 26.0);
            let draw_sz = if sz.y > 0.0 {
                Vec2::new(sz.x * (target_h / sz.y), target_h)
            } else {
                Vec2::new(target_h, target_h)
            };
            let resp = ui.image(egui::load::SizedTexture::new(tex.id(), draw_sz));
            if !trimmed_alt.is_empty() {
                resp.on_hover_text(trimmed_alt);
            }
            return;
        }
    }

    let parsed_desctag_idx = lower_src
        .strip_prefix("fsh://desctags.fox:")
        .or_else(|| lower_src.strip_prefix("fsh://desctags:"))
        .or_else(|| lower_src.strip_prefix("fsh://desctag:"))
        .and_then(|s| s.trim().parse::<usize>().ok());
    if let Some(idx) = parsed_desctag_idx {
        if let Some(tex) = desctags.get(idx).and_then(|o| o.as_ref()) {
            let sz = smiley_draw_size(tex, &font_id);
            let resp = ui.image(egui::load::SizedTexture::new(tex.id(), sz));
            if !trimmed_alt.is_empty() {
                resp.on_hover_text(trimmed_alt);
            }
            return;
        }
    }

    let parsed_smiley_idx = lower_src
        .strip_prefix("fsh://smileys.fsh:")
        .and_then(|s| s.trim().parse::<usize>().ok())
        .or_else(|| {
            lower_src
                .strip_prefix("fsh://smileys2.fsh:")
                .and_then(|s| s.trim().parse::<usize>().ok())
                .map(|i| i + 26)
        });
    if let Some(idx) = parsed_smiley_idx {
        if let Some(tex) = smileys.get(idx).and_then(|o| o.as_ref()) {
            let sz = smiley_draw_size(tex, &font_id);
            let resp = ui.image(egui::load::SizedTexture::new(tex.id(), sz));
            if !trimmed_alt.is_empty() {
                resp.on_hover_text(trimmed_alt);
            }
            return;
        }
    }

    let (glyph, color): (&str, Color32) = if lower_alt == "@emit" || lower_alt == "emit" {
        ("[◆]", Color32::from_rgb(245, 205, 110))
    } else if lower_alt == "@events" || lower_alt == "events" || lower_alt.contains("star") {
        ("★", GOLD)
    } else if lower_alt == "@paw" || lower_alt.contains("paw") {
        ("🐾", Color32::from_rgb(175, 195, 235))
    } else if !trimmed_alt.is_empty() {
        ui.label(
            RichText::new(format!("[{trimmed_alt}]")).color(Color32::from_rgb(170, 175, 195)),
        );
        return;
    } else if let Some(idx_str) = lower_src.strip_prefix("fsh://system.fsh:") {
        match idx_str.trim() {
            "86" => ("›", Color32::from_rgb(232, 160, 228)),
            "85" => ("🐾", Color32::from_rgb(175, 195, 235)),
            "84" => ("◆", Color32::from_rgb(245, 205, 110)),
            "81" | "82" | "83" => ("★", GOLD),
            _ => ("•", Color32::from_rgb(170, 175, 195)),
        }
    } else if lower_src.contains("smiley") || lower_src.contains("#s") {
        ("☺", Color32::from_rgb(245, 215, 120))
    } else {
        ("•", Color32::from_rgb(170, 175, 195))
    };

    ui.label(RichText::new(glyph).color(color));
}

#[derive(Debug, Clone)]
enum ChatClickAction {
    OpenPlayerMenu {
        display_name: String,
        short_name: String,
        pos: Pos2,
    },
    EnterDreamUrl(String),
    SendCommand(String),
}

fn desctag_index_for_code(ch: char) -> Option<usize> {
    let b = ch as u32;
    if (0x20..=0xff).contains(&b) {
        Some((b - 0x20) as usize)
    } else {
        None
    }
}

enum InlineChatPart<'a> {
    Text(&'a str),
    Smiley { idx: usize, code: char },
    Desctag { idx: usize, code: char },
}

fn split_inline_chat_codes(s: &str) -> Vec<InlineChatPart<'_>> {
    let mut parts = Vec::new();
    let bytes = s.as_bytes();
    let mut last = 0usize;
    let mut i = 0usize;
    while i + 2 < bytes.len() {
        if bytes[i] == b'#' && (bytes[i + 1] == b'S' || bytes[i + 1] == b'C') {
            if let Some(ch) = s[i + 2..].chars().next() {
                let kind = bytes[i + 1];
                let mapped = if kind == b'S' {
                    smiley_index_for_code(ch).map(|idx| InlineChatPart::Smiley { idx, code: ch })
                } else {
                    desctag_index_for_code(ch).map(|idx| InlineChatPart::Desctag { idx, code: ch })
                };
                if let Some(part) = mapped {
                    if i > last {
                        parts.push(InlineChatPart::Text(&s[last..i]));
                    }
                    parts.push(part);
                    i += 2 + ch.len_utf8();
                    last = i;
                    continue;
                }
            }
        }
        if let Some(ch) = s[i..].chars().next() {
            i += ch.len_utf8();
        } else {
            break;
        }
    }
    if last < s.len() {
        parts.push(InlineChatPart::Text(&s[last..]));
    }
    parts
}

fn accent_color_for_appearance(appearance: &furcadia_client::AvatarAppearance) -> Option<Color32> {
    let luts = RemapLuts::from_appearance(appearance);
    let lut = luts.channels[7].as_ref()?;
    let [mut r, mut g, mut b] = lut[160];
    // Keep very dark accent colors legible against the dark chat background.
    let luma = (r as u16 * 3 + g as u16 * 6 + b as u16) / 10;
    if luma < 110 {
        let lift = (110 - luma) as u8;
        r = r.saturating_add(lift);
        g = g.saturating_add(lift);
        b = b.saturating_add(lift);
    }
    Some(Color32::from_rgb(r, g, b))
}

fn render_chat_line(
    ui: &mut egui::Ui,
    line: &ChatLine,
    action_out: &mut Option<ChatClickAction>,
    smileys: &[Option<TextureHandle>],
    desctags: &[Option<TextureHandle>],
    chat_imgs: &HashMap<String, Option<TextureHandle>>,
    player_accent_colors: &HashMap<String, Color32>,
) {
    let has_name_link = line.spans.iter().any(|s| {
        matches!(
            s,
            ChatSpan::Text { style, .. }
                if style
                    .link
                    .as_deref()
                    .is_some_and(|u| u.starts_with("name://"))
        )
    });
    let fallback_peer = if has_name_link {
        None
    } else {
        let plain = line.to_plain_text();
        extract_whisper_peer(&plain).or_else(|| {
            plain
                .trim()
                .strip_prefix("(You see ")
                .and_then(|s| s.strip_suffix(".)"))
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
        })
    };

    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for span in &line.spans {
            match span {
                ChatSpan::Image { src, alt } => {
                    render_chat_image_span(ui, src, alt.as_deref(), smileys, desctags, chat_imgs);
                }
                ChatSpan::Text { text, style } => {
                    let is_emote = style
                        .color
                        .as_deref()
                        .is_some_and(|c| c.eq_ignore_ascii_case("emote"));
                    if is_emote && style.link.is_none() && text.contains('"') {
                        let segments = split_emote_quotes(text);
                        for (seg, is_quoted) in segments {
                            if seg.is_empty() {
                                continue;
                            }
                            let mut rt = RichText::new(seg);
                            if style.bold {
                                rt = rt.strong();
                            }
                            if style.underline {
                                rt = rt.underline();
                            }
                            if is_quoted {
                                rt = rt.color(Color32::from_rgb(242, 245, 250));
                            } else {
                                rt = rt.italics().color(map_chat_color("emote"));
                            }
                            ui.label(rt);
                        }
                        continue;
                    }
                    let mut rt = RichText::new(text);
                    if style.bold {
                        rt = rt.strong();
                    }
                    if style.italic || is_emote {
                        rt = rt.italics();
                    }
                    if style.underline || style.link.is_some() {
                        rt = rt.underline();
                    }
                    if let Some(color_name) = &style.color {
                        rt = rt.color(map_chat_color(color_name));
                    } else {
                        rt = rt.color(Color32::from_rgb(232, 236, 244));
                    }
                    if let Some(url) = &style.link {
                        let lower = url.to_ascii_lowercase();
                        if let Some(short) = url.strip_prefix("name://") {
                            let key = shortname_key(short);
                            let disp_key = shortname_key(text);
                            if let Some(&acc_col) = player_accent_colors
                                .get(&key)
                                .or_else(|| player_accent_colors.get(&disp_key))
                            {
                                rt = rt.color(acc_col);
                            }
                            let resp = ui
                                .add(egui::Label::new(rt).sense(egui::Sense::click()))
                                .on_hover_cursor(egui::CursorIcon::PointingHand);
                            if resp.clicked() || resp.secondary_clicked() {
                                let pos = resp
                                    .interact_pointer_pos()
                                    .unwrap_or_else(|| resp.rect.left_bottom());
                                *action_out = Some(ChatClickAction::OpenPlayerMenu {
                                    display_name: text.trim().to_owned(),
                                    short_name: short.to_owned(),
                                    pos,
                                });
                            }
                        } else if lower.starts_with("furc://") {
                            let resp = ui
                                .add(egui::Label::new(rt).sense(egui::Sense::click()))
                                .on_hover_cursor(egui::CursorIcon::PointingHand);
                            if resp.clicked() {
                                *action_out = Some(ChatClickAction::EnterDreamUrl(url.clone()));
                            }
                        } else if lower.starts_with("command://") {
                            let cmd = url[10..].trim_start_matches('/').to_owned();
                            let resp = ui
                                .add(egui::Label::new(rt).sense(egui::Sense::click()))
                                .on_hover_cursor(egui::CursorIcon::PointingHand);
                            if resp.clicked() {
                                *action_out = Some(ChatClickAction::SendCommand(cmd));
                            }
                        } else {
                            ui.hyperlink_to(rt, url);
                        }
                    } else if fallback_peer
                        .as_deref()
                        .is_some_and(|peer| !peer.is_empty() && text.contains(peer))
                    {
                        let peer = fallback_peer.as_deref().unwrap_or_default();
                        let key = shortname_key(peer);
                        if let Some(&acc_col) = player_accent_colors.get(&key) {
                            rt = rt.color(acc_col);
                        }
                        let resp = ui
                            .add(egui::Label::new(rt.underline()).sense(egui::Sense::click()))
                            .on_hover_cursor(egui::CursorIcon::PointingHand);
                        if resp.clicked() || resp.secondary_clicked() {
                            let pos = resp
                                .interact_pointer_pos()
                                .unwrap_or_else(|| resp.rect.left_bottom());
                            *action_out = Some(ChatClickAction::OpenPlayerMenu {
                                display_name: peer.to_owned(),
                                short_name: wire_player_name(peer),
                                pos,
                            });
                        }
                    } else if text.contains("#S") || text.contains("#C") {
                        let font_id = egui::TextStyle::Body.resolve(ui.style());
                        for part in split_inline_chat_codes(text) {
                            match part {
                                InlineChatPart::Text(seg) if !seg.is_empty() => {
                                    let mut seg_rt = RichText::new(seg);
                                    if style.bold {
                                        seg_rt = seg_rt.strong();
                                    }
                                    if style.italic || is_emote {
                                        seg_rt = seg_rt.italics();
                                    }
                                    if style.underline {
                                        seg_rt = seg_rt.underline();
                                    }
                                    if let Some(color_name) = &style.color {
                                        seg_rt = seg_rt.color(map_chat_color(color_name));
                                    } else {
                                        seg_rt = seg_rt.color(Color32::from_rgb(232, 236, 244));
                                    }
                                    ui.label(seg_rt);
                                }
                                InlineChatPart::Smiley { idx, code } => {
                                    if let Some(tex) = smileys.get(idx).and_then(|o| o.as_ref()) {
                                        let sz = smiley_draw_size(tex, &font_id);
                                        ui.image(egui::load::SizedTexture::new(tex.id(), sz));
                                    } else {
                                        ui.label(RichText::new(format!("#S{code}")));
                                    }
                                }
                                InlineChatPart::Desctag { idx, code } => {
                                    if let Some(tex) = desctags.get(idx).and_then(|o| o.as_ref()) {
                                        let sz = smiley_draw_size(tex, &font_id);
                                        ui.image(egui::load::SizedTexture::new(tex.id(), sz));
                                    } else {
                                        ui.label(RichText::new(format!("#C{code}")));
                                    }
                                }
                                _ => {}
                            }
                        }
                    } else {
                        ui.label(rt);
                    }
                }
            }
        }
    });
}

fn map_chat_color(name: &str) -> Color32 {
    if let Some(hex) = name.trim().strip_prefix('#') {
        if hex.len() == 6 {
            if let (Ok(r), Ok(g), Ok(b)) = (
                u8::from_str_radix(&hex[0..2], 16),
                u8::from_str_radix(&hex[2..4], 16),
                u8::from_str_radix(&hex[4..6], 16),
            ) {
                return Color32::from_rgb(r, g, b);
            }
        } else if hex.len() == 3 {
            if let (Ok(r), Ok(g), Ok(b)) = (
                u8::from_str_radix(&hex[0..1], 16),
                u8::from_str_radix(&hex[1..2], 16),
                u8::from_str_radix(&hex[2..3], 16),
            ) {
                return Color32::from_rgb(r * 17, g * 17, b * 17);
            }
        }
    }
    match name.trim().to_ascii_lowercase().as_str() {
        "success" | "green" => Color32::from_rgb(100, 220, 120),
        "warning" | "yellow" | "orange" => Color32::from_rgb(240, 195, 90),
        "error" | "danger" | "red" => Color32::from_rgb(240, 100, 100),
        "whisper" | "purple" | "magenta" | "trade" | "cmdlink" => {
            Color32::from_rgb(205, 130, 235)
        }
        "emote" => Color32::from_rgb(232, 160, 228),
        "emit" | "bcast" | "broadcast" | "cyan" | "link" | "furclink" | "dragonspeak" => {
            Color32::from_rgb(100, 210, 230)
        }
        "shout" | "blue" | "notify" | "highlight" | "query" => {
            Color32::from_rgb(125, 175, 250)
        }
        "channel" => Color32::from_rgb(175, 150, 240),
        "desc" | "description" | "history" => Color32::from_rgb(185, 198, 220),
        "roll" => Color32::from_rgb(195, 225, 145),
        "name" => Color32::from_rgb(145, 200, 245),
        "myspeech" => Color32::from_rgb(255, 248, 215),
        "speech" => Color32::from_rgb(242, 245, 250),
        _ => Color32::from_rgb(228, 232, 240),
    }
}

/// Splits an emote string into `(segment, is_quoted_speech)` pairs so `"..."` spoken text
/// renders as normal non-italicized speech while surrounding motion stays italicized.
fn split_emote_quotes(text: &str) -> Vec<(&str, bool)> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < text.len() {
        let Some(rel_open) = text[pos..].find('"') else {
            out.push((&text[pos..], false));
            break;
        };
        let open = pos + rel_open;
        let after_open = open + 1;
        let Some(rel_close) = text[after_open..].find('"') else {
            out.push((&text[pos..], false));
            break;
        };
        let close = after_open + rel_close + 1;
        if open > pos {
            out.push((&text[pos..open], false));
        }
        out.push((&text[open..close], true));
        pos = close;
    }
    if out.is_empty() && !text.is_empty() {
        out.push((text, false));
    }
    out
}

/// Rewrites Furcadia's `"Your friend request has been sent to <Name>."` Pounce response
/// to `"<Name> has been added to Pounce"`.
fn rewrite_pounce_add_message(mut line: ChatLine) -> ChatLine {
    let plain = line.to_plain_text();
    let trimmed = plain.trim();
    let prefix = "Your friend request has been sent to";
    if trimmed.len() >= prefix.len() && trimmed[..prefix.len()].eq_ignore_ascii_case(prefix) {
        let mut link_url: Option<String> = None;
        let mut link_name: Option<String> = None;
        for span in &line.spans {
            if let ChatSpan::Text { text, style } = span {
                if style
                    .link
                    .as_deref()
                    .is_some_and(|u| u.starts_with("name://"))
                    && !text.trim().is_empty()
                {
                    link_url = style.link.clone();
                    link_name = Some(text.trim().to_owned());
                    break;
                }
            }
        }
        let name = link_name.unwrap_or_else(|| {
            trimmed[prefix.len()..]
                .trim()
                .trim_end_matches('.')
                .trim()
                .to_owned()
        });
        if !name.is_empty() {
            let success_style = furcadia_client::chat::ChatStyle {
                color: Some("success".to_owned()),
                ..Default::default()
            };
            let name_style = furcadia_client::chat::ChatStyle {
                color: Some("success".to_owned()),
                link: link_url,
                ..Default::default()
            };
            line.spans = vec![
                ChatSpan::Text {
                    text: name,
                    style: name_style,
                },
                ChatSpan::Text {
                    text: " has been added to Pounce".to_owned(),
                    style: success_style,
                },
            ];
        }
    }
    line
}

fn normalize_main_map_id(lower: &str) -> Option<&'static str> {
    if lower == "furn" || lower == "pmfurn" {
        Some("pmfurn")
    } else if lower == "vinca" || lower == "pmvinca" {
        Some("pmvinca")
    } else if lower.starts_with("acrop") || lower == "pmacrop" || lower == "pmacropolis" {
        Some("pmacropolis")
    } else if lower.starts_with("alleg") || lower == "pmalleg" || lower == "pmallegria" {
        Some("pmallegria")
    } else if lower.starts_with("imagin") || lower == "pmimag" || lower == "pmimaginarium" {
        Some("pmimaginarium")
    } else if lower.starts_with("meov") || lower == "pmmeov" || lower == "pmmeovanni" {
        Some("pmmeovanni")
    } else if lower.starts_with("hawth") || lower == "pmhawth" || lower == "pmhawthorn" {
        Some("pmhawthorn")
    } else if lower.starts_with("oldetown") || lower == "pmoldetown" {
        Some("pmoldetown")
    } else if lower.starts_with("wylde") || lower == "pmwylde" {
        Some("pmwylde")
    } else {
        None
    }
}

fn custom_dictionary_path() -> Option<std::path::PathBuf> {
    default_logs_dir()
        .and_then(|d| d.parent().map(|p| p.join("custom_dictionary.txt")))
}

fn load_custom_dictionary() -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    let Some(path) = custom_dictionary_path() else {
        return set;
    };
    if let Ok(contents) = std::fs::read_to_string(path) {
        for line in contents.lines() {
            let w = line.trim().to_ascii_lowercase();
            if !w.is_empty() {
                set.insert(w);
            }
        }
    }
    set
}

fn find_hunspell_dict() -> Option<String> {
    let candidates = [
        "/usr/share/hunspell/en_US.dic",
        "/usr/share/hunspell/en_GB.dic",
        "/usr/share/myspell/en_US.dic",
        "/usr/share/myspell/dicts/en_US.dic",
    ];
    for p in candidates {
        if std::path::Path::new(p).is_file() {
            return Some(p.trim_end_matches(".dic").to_owned());
        }
    }
    if let Ok(rd) = std::fs::read_dir("/usr/share/wine/gecko") {
        for entry in rd.flatten() {
            let p = entry.path().join("dictionaries/en-US.dic");
            if p.is_file() {
                return Some(p.to_string_lossy().trim_end_matches(".dic").to_owned());
            }
        }
    }
    None
}

fn extract_spellcheck_words(text: &str) -> Vec<(String, usize, usize)> {
    let mut words = Vec::new();
    let trimmed_start = text.len() - text.trim_start().len();
    let mut scan_from = trimmed_start;
    if text[scan_from..].starts_with('`') {
        return words;
    }
    if text[scan_from..].starts_with('/') {
        // Skip slash command itself (e.g. /wh, /onln, /glook, /who).
        if let Some(sp) = text[scan_from..].find(char::is_whitespace) {
            scan_from += sp;
        } else {
            return words;
        }
    }
    let bytes = text.as_bytes();
    let mut i = scan_from;
    while i < bytes.len() {
        while i < bytes.len() && text[i..].chars().next().is_some_and(char::is_whitespace) {
            i += text[i..].chars().next().map_or(1, char::len_utf8);
        }
        let tok_start = i;
        while i < bytes.len() && !text[i..].chars().next().is_some_and(char::is_whitespace) {
            i += text[i..].chars().next().map_or(1, char::len_utf8);
        }
        let tok = &text[tok_start..i];
        if tok.is_empty()
            || tok.starts_with("http://")
            || tok.starts_with("https://")
            || tok.starts_with("furc://")
            || tok.starts_with('@')
            || tok.starts_with('#')
            || tok.starts_with('<')
        {
            continue;
        }
        let mut s = tok_start;
        let mut e = i;
        while s < e {
            let ch = text[s..e].chars().next().unwrap();
            if ch.is_ascii_alphabetic() {
                break;
            }
            s += ch.len_utf8();
        }
        while e > s {
            let ch = text[s..e].chars().next_back().unwrap();
            if ch.is_ascii_alphabetic() {
                break;
            }
            e -= ch.len_utf8();
        }
        let core = &text[s..e];
        if core.len() >= 2
            && !core.chars().any(|c| c.is_ascii_digit())
            && core.chars().all(|c| c.is_ascii_alphabetic() || c == '\'')
        {
            words.push((core.to_owned(), s, e));
        }
    }
    words
}

static BUNDLED_DICT_BZ2: &[u8] = include_bytes!("../../assets/dictionary.txt.bz2");
static BUNDLED_DICT: std::sync::OnceLock<std::collections::HashSet<String>> =
    std::sync::OnceLock::new();

fn bundled_dictionary() -> &'static std::collections::HashSet<String> {
    BUNDLED_DICT.get_or_init(|| {
        use std::io::Read;
        let mut set = std::collections::HashSet::with_capacity(130_000);
        let mut decoder = bzip2_rs::DecoderReader::new(BUNDLED_DICT_BZ2);
        let mut text = String::new();
        if decoder.read_to_string(&mut text).is_ok() {
            for line in text.lines() {
                let w = line.trim();
                if !w.is_empty() {
                    set.insert(w.to_owned());
                }
            }
        }
        set
    })
}

fn spellcheck_word_known(
    word: &str,
    custom_dict: &std::collections::HashSet<String>,
    dict: &std::collections::HashSet<String>,
) -> bool {
    let lower = word.to_ascii_lowercase();
    if custom_dict.contains(&lower) || dict.contains(&lower) {
        return true;
    }
    if let Some(base) = lower.strip_suffix("'s").or_else(|| lower.strip_suffix('\'')) {
        if custom_dict.contains(base) || dict.contains(base) {
            return true;
        }
    }
    false
}

fn edit1_suggestions(
    word: &str,
    dict: &std::collections::HashSet<String>,
    limit: usize,
) -> Vec<String> {
    let lower = word.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut found = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut push_if_valid = |cand: String| {
        if found.len() < limit && dict.contains(&cand) && seen.insert(cand.clone()) {
            found.push(cand);
        }
    };
    // Deletions
    for i in 0..bytes.len() {
        let mut s = String::with_capacity(bytes.len() - 1);
        s.push_str(&lower[..i]);
        s.push_str(&lower[i + 1..]);
        push_if_valid(s);
    }
    // Transpositions
    for i in 0..bytes.len().saturating_sub(1) {
        let mut b = bytes.to_vec();
        b.swap(i, i + 1);
        if let Ok(s) = String::from_utf8(b) {
            push_if_valid(s);
        }
    }
    // Replacements
    for i in 0..bytes.len() {
        for ch in b'a'..=b'z' {
            if bytes[i] == ch {
                continue;
            }
            let mut b = bytes.to_vec();
            b[i] = ch;
            if let Ok(s) = String::from_utf8(b) {
                push_if_valid(s);
            }
        }
    }
    // Insertions
    for i in 0..=bytes.len() {
        for ch in b'a'..=b'z' {
            let mut b = Vec::with_capacity(bytes.len() + 1);
            b.extend_from_slice(&bytes[..i]);
            b.push(ch);
            b.extend_from_slice(&bytes[i..]);
            if let Ok(s) = String::from_utf8(b) {
                push_if_valid(s);
            }
        }
    }
    found
}

fn run_bundled_spell_check(
    text: &str,
    custom_dict: &std::collections::HashSet<String>,
) -> Vec<MisspelledWord> {
    let dict = bundled_dictionary();
    extract_spellcheck_words(text)
        .into_iter()
        .filter(|(w, _, _)| !spellcheck_word_known(w, custom_dict, dict))
        .map(|(word, start, end)| {
            let suggestions = edit1_suggestions(&word, dict, 5);
            MisspelledWord {
                word,
                start,
                end,
                suggestions,
            }
        })
        .collect()
}

fn run_hunspell_check(
    text: &str,
    custom_dict: &std::collections::HashSet<String>,
) -> Vec<MisspelledWord> {
    let Some(dic) = find_hunspell_dict() else {
        return run_bundled_spell_check(text, custom_dict);
    };
    let dict = bundled_dictionary();
    let words = extract_spellcheck_words(text);
    if words.is_empty() {
        return Vec::new();
    }
    let mut stdin_buf = String::new();
    let mut active = Vec::new();
    for (w, s, e) in words {
        if spellcheck_word_known(&w, custom_dict, dict) {
            continue;
        }
        stdin_buf.push('^');
        stdin_buf.push_str(&w);
        stdin_buf.push('\n');
        active.push((w, s, e));
    }
    if active.is_empty() {
        return Vec::new();
    }
    let mut cmd = furcadia_client::sys::command("hunspell");
    cmd.args(["-d", &dic, "-a"]);
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let Ok(mut child) = cmd.spawn() else {
        return run_bundled_spell_check(text, custom_dict);
    };
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(stdin_buf.as_bytes());
    }
    let Ok(out) = child.wait_with_output() else {
        return run_bundled_spell_check(text, custom_dict);
    };
    if !out.status.success() {
        return run_bundled_spell_check(text, custom_dict);
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut results = Vec::new();
    let mut word_idx = 0usize;
    for line in stdout.lines().skip(1) {
        if line.is_empty() {
            word_idx += 1;
            continue;
        }
        let Some((word, start, end)) = active.get(word_idx) else {
            continue;
        };
        if let Some(rest) = line.strip_prefix("& ") {
            let suggestions = rest
                .split_once(": ")
                .map(|(_, s)| s.split(", ").take(5).map(|x| x.trim().to_owned()).collect())
                .unwrap_or_default();
            results.push(MisspelledWord {
                word: word.clone(),
                start: *start,
                end: *end,
                suggestions,
            });
        } else if line.starts_with("# ") {
            results.push(MisspelledWord {
                word: word.clone(),
                start: *start,
                end: *end,
                suggestions: Vec::new(),
            });
        }
    }
    results
}

fn build_spellcheck_layout_job(
    ui: &egui::Ui,
    text: &str,
    wrap_width: f32,
    misspelled: &[MisspelledWord],
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = wrap_width;
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    let text_color = ui.visuals().text_color();
    let base_fmt = egui::TextFormat {
        font_id: font_id.clone(),
        color: text_color,
        ..Default::default()
    };
    let err_fmt = egui::TextFormat {
        font_id,
        color: text_color,
        underline: egui::Stroke::new(1.5_f32, Color32::from_rgb(245, 90, 90)),
        ..Default::default()
    };
    let mut cursor = 0usize;
    for m in misspelled {
        if m.start < cursor || m.end > text.len() || !text.is_char_boundary(m.start) || !text.is_char_boundary(m.end) {
            continue;
        }
        if m.start > cursor {
            job.append(&text[cursor..m.start], 0.0, base_fmt.clone());
        }
        job.append(&text[m.start..m.end], 0.0, err_fmt.clone());
        cursor = m.end;
    }
    if cursor < text.len() {
        job.append(&text[cursor..], 0.0, base_fmt);
    }
    job
}

fn read_system_clipboard() -> Option<String> {
    for (bin, args) in [
        ("wl-paste", ["--no-newline"].as_slice()),
        ("xclip", ["-o", "-selection", "clipboard"].as_slice()),
        ("xsel", ["-ob"].as_slice()),
    ] {
        if let Ok(out) = std::process::Command::new(bin).args(args).output() {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout).to_string();
                if !s.is_empty() {
                    return Some(s);
                }
            }
        }
    }
    None
}

fn percent_encode_query(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn plain_server_text(raw_html: &str) -> String {
    let mut out = String::with_capacity(raw_html.len());
    let mut in_tag = false;
    for ch in raw_html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.trim().to_owned()
}

fn extract_dialog_notice(payload: &str) -> String {
    // Format: `<dialog> <code> <type> <message>` or `<dialog> <code> <message>`
    let mut parts = payload.splitn(4, ' ');
    let _dialog = parts.next();
    let _code = parts.next();
    let third = parts.next().unwrap_or("");
    let fourth = parts.next().unwrap_or("");
    let combined;
    let raw_notice = if fourth.is_empty() {
        if !third.is_empty() {
            third
        } else {
            payload
        }
    } else if third.chars().all(|c| c.is_ascii_digit() || c == '-')
        || matches!(third, "error" | "ok" | "info" | "warn")
    {
        fourth
    } else {
        combined = format!("{third} {fourth}");
        &combined
    };
    plain_server_text(raw_notice)
}

fn default_connection_config() -> ConnectionConfig {
    ConnectionConfig {
        host: DEFAULT_HOST.to_owned(),
        port: DEFAULT_PORT,
        connect_timeout: Duration::from_secs(8),
        read_timeout: Some(Duration::from_millis(50)),
    }
}

fn forward_pre_handshake_msgs(
    pre_msgs: Vec<ServerMessage>,
    tx: &Sender<NetToUi>,
    last_server_notice: &mut Option<String>,
) {
    for msg in pre_msgs {
        if let ServerMessage::Text(raw) = &msg {
            let txt = plain_server_text(raw);
            if !txt.is_empty() {
                *last_server_notice = Some(txt);
            }
        } else if let ServerMessage::Extended {
            code: Some('#'),
            payload,
        } = &msg
        {
            let txt = extract_dialog_notice(payload);
            if !txt.is_empty() {
                *last_server_notice = Some(txt);
            }
        }
        let _ = tx.send(NetToUi::Message(msg));
    }
}

fn connect_and_retry_pipe_account(
    email: &str,
    character: &str,
    password: &str,
    tx: &Sender<NetToUi>,
    seq: u64,
    last_server_notice: &mut Option<String>,
) -> Option<FurcadiaClient> {
    let pipe_char = character.trim().replace([' ', '_'], "|");
    let _ = tx.send(NetToUi::ConnectStage {
        seq,
        stage: format!("Retrying login as '{pipe_char}'..."),
    });
    let mut c = FurcadiaClient::connect(&default_connection_config()).ok()?;
    let pre_msgs = c.wait_for_handshake_timeout(Duration::from_secs(12)).ok()?;
    forward_pre_handshake_msgs(pre_msgs, tx, last_server_notice);
    c.login_account_raw_char(email, &pipe_char, password).ok()?;
    *last_server_notice = None;
    Some(c)
}

fn network_worker(rx: Receiver<UiToNet>, tx: Sender<NetToUi>, ctx: egui::Context) {
    let mut client: Option<FurcadiaClient> = None;
    let mut active_seq: u64 = 0;
    let mut awaiting_login_ack = false;
    let mut pending_desc: Option<String> = None;
    let mut pending_colors: Option<String> = None;
    let mut login_deadline: Option<Instant> = None;
    let mut last_server_notice: Option<String> = None;
    let mut account_retry: Option<(u64, String, String, String)> = None;

    loop {
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                UiToNet::AuthenticateAccount { email, password } => {
                    match fetch_account_characters(&email, &password) {
                        Ok(info) => {
                            let _ = tx.send(NetToUi::Authenticated(info));
                            ctx.request_repaint();
                            let news_tx = tx.clone();
                            let news_ctx = ctx.clone();
                            thread::spawn(move || {
                                if let Ok(entries) = fetch_news() {
                                    let _ = news_tx.send(NetToUi::News(entries));
                                    news_ctx.request_repaint();
                                }
                            });
                        }
                        Err(err) => {
                            let _ = tx.send(NetToUi::AuthError(err.to_string()));
                            ctx.request_repaint();
                        }
                    }
                }
                UiToNet::ConnectCharacter {
                    seq,
                    email,
                    character,
                    password,
                    updated_desc,
                    colors,
                } => {
                    if let Some(mut old) = client.take() {
                        let _ = old.disconnect();
                    }
                    active_seq = seq;
                    awaiting_login_ack = false;
                    pending_desc = updated_desc;
                    pending_colors = colors;
                    login_deadline = None;
                    last_server_notice = None;
                    account_retry = None;

                    let stage = format!("Connecting to {DEFAULT_HOST}:{DEFAULT_PORT}...");
                    let _ = tx.send(NetToUi::Status(stage.clone()));
                    let _ = tx.send(NetToUi::ConnectStage { seq, stage });
                    ctx.request_repaint();

                    let cfg = default_connection_config();

                    match FurcadiaClient::connect(&cfg) {
                        Ok(mut c) => {
                            let _ = tx.send(NetToUi::ConnectStage {
                                seq,
                                stage: "Waiting for server handshake (Dragonroar)...".into(),
                            });
                            ctx.request_repaint();

                            match c.wait_for_handshake_timeout(Duration::from_secs(12)) {
                                Ok(pre_msgs) => {
                                    forward_pre_handshake_msgs(pre_msgs, &tx, &mut last_server_notice);
                                }
                                Err(err) => {
                                    let msg = format!("Handshake failed: {err}");
                                    let _ = tx.send(NetToUi::Status(msg.clone()));
                                    let _ = tx.send(NetToUi::ConnectFailed { seq, error: msg });
                                    ctx.request_repaint();
                                    continue;
                                }
                            }

                            let _ = tx.send(NetToUi::ConnectStage {
                                seq,
                                stage: format!("Authenticating character '{character}'..."),
                            });
                            ctx.request_repaint();

                            let retry_candidate =
                                if !email.trim().is_empty() && character.contains([' ', '|']) {
                                    Some((seq, email.clone(), character.clone(), password.clone()))
                                } else {
                                    None
                                };
                            let login_res = if email.trim().is_empty() {
                                c.login_character(character, password)
                            } else {
                                c.login_account(email, character, password)
                            };
                            if let Err(err) = login_res {
                                let msg = format!("Login failed: {err}");
                                let _ = tx.send(NetToUi::Status(msg.clone()));
                                let _ = tx.send(NetToUi::ConnectFailed { seq, error: msg });
                                ctx.request_repaint();
                                continue;
                            }

                            account_retry = retry_candidate;
                            awaiting_login_ack = true;
                            login_deadline = Some(Instant::now() + Duration::from_secs(15));
                            client = Some(c);
                            ctx.request_repaint();
                        }
                        Err(err) => {
                            let msg = format!(
                                "Unable to connect to {DEFAULT_HOST}:{DEFAULT_PORT}: {err}"
                            );
                            let _ = tx.send(NetToUi::Status(msg.clone()));
                            let _ = tx.send(NetToUi::ConnectFailed { seq, error: msg });
                            ctx.request_repaint();
                        }
                    }
                }
                UiToNet::Send(c) => {
                    if let Some(cl) = client.as_mut() {
                        let _ = cl.send_command(&c);
                    }
                }
                UiToNet::DownloadDream {
                    url,
                    map_name,
                    checksum,
                } => {
                    let dl_tx = tx.clone();
                    let dl_ctx = ctx.clone();
                    thread::spawn(move || {
                        let Some(dreams_dir) = AssetLocator::dreams_cache_dir() else {
                            let _ = dl_tx.send(NetToUi::DreamError(
                                "Could not determine dream cache directory.".into(),
                            ));
                            dl_ctx.request_repaint();
                            return;
                        };
                        let _ = std::fs::create_dir_all(&dreams_dir);
                        let safe_name = FurcadiaApp::sanitize_dream_id(&map_name);
                        let suffix = format!("{:?}", std::thread::current().id())
                            .chars()
                            .filter(|c| c.is_ascii_alphanumeric())
                            .collect::<String>();
                        let dl_path = dreams_dir.join(format!("{safe_name}_{suffix}.tmp"));
                        let curl_out = furcadia_client::sys::command("curl")
                            .args(["-fsSL", "--max-time", "25", "-o"])
                            .arg(&dl_path)
                            .arg(&url)
                            .output();
                        match curl_out {
                            Ok(out) if out.status.success() => {}
                            Ok(out) => {
                                let err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
                                let _ = std::fs::remove_file(&dl_path);
                                let _ = dl_tx.send(NetToUi::DreamError(format!(
                                    "Dream download failed for '{map_name}' ({url}): {err}"
                                )));
                                dl_ctx.request_repaint();
                                return;
                            }
                            Err(e) => {
                                let _ = dl_tx.send(NetToUi::DreamError(format!(
                                    "Failed to launch curl for '{map_name}': {e}"
                                )));
                                dl_ctx.request_repaint();
                                return;
                            }
                        }

                        let bytes = match std::fs::read(&dl_path) {
                            Ok(b) => b,
                            Err(e) => {
                                let _ = dl_tx.send(NetToUi::DreamError(format!(
                                    "Failed to read downloaded dream '{map_name}': {e}"
                                )));
                                dl_ctx.request_repaint();
                                return;
                            }
                        };
                        let out_dir = dreams_dir.join(&safe_name);
                        let stage_dir = dreams_dir.join(format!("{safe_name}_{suffix}_extract"));
                        let _ = std::fs::remove_dir_all(&stage_dir);

                        let extracted = if is_fr01(&bytes) {
                            extract_fr01_to_dir(&bytes, &stage_dir)
                                .map(|_| true)
                                .map_err(|e| format!("Failed to extract FR01 dream '{map_name}': {e}"))
                        } else if bytes.starts_with(b"PK") {
                            let _ = std::fs::create_dir_all(&stage_dir);
                            extract_zip_archive(&dl_path, &stage_dir)
                                .map(|()| true)
                                .map_err(|e| format!("Failed to unzip dream '{map_name}': {e}"))
                        } else if bytes.starts_with(b"MAP ") {
                            let map_path = dreams_dir.join(format!("{safe_name}.map"));
                            std::fs::rename(&dl_path, &map_path)
                                .map(|_| false)
                                .map_err(|e| format!("Failed to store dream map '{map_name}': {e}"))
                        } else {
                            let preview = String::from_utf8_lossy(&bytes[..bytes.len().min(80)])
                                .trim()
                                .to_owned();
                            Err(format!(
                                "Unknown dream package format for '{map_name}' ({} bytes from {url}: {preview}).",
                                bytes.len()
                            ))
                        };

                        let _ = std::fs::remove_file(&dl_path);
                        match extracted {
                            Ok(true) => {
                                let _ = std::fs::remove_dir_all(&out_dir);
                                if let Err(e) = std::fs::rename(&stage_dir, &out_dir) {
                                    let _ = std::fs::remove_dir_all(&stage_dir);
                                    let _ = dl_tx.send(NetToUi::DreamError(format!(
                                        "Failed to finalize dream directory '{map_name}': {e}"
                                    )));
                                    dl_ctx.request_repaint();
                                    return;
                                }
                                if let Some(hash) = checksum.as_deref() {
                                    let _ = std::fs::write(out_dir.join(".hash"), hash);
                                }
                                if let Some(map_path) = FurcadiaApp::find_map_in_dir(&out_dir) {
                                    let _ = dl_tx.send(NetToUi::DreamDownloaded {
                                        map_name,
                                        path: map_path,
                                        dream_dir: Some(out_dir),
                                    });
                                } else {
                                    let _ = dl_tx.send(NetToUi::DreamError(format!(
                                        "Dream package '{map_name}' contained no .map file."
                                    )));
                                }
                            }
                            Ok(false) => {
                                let _ = std::fs::remove_dir_all(&stage_dir);
                                let map_path = dreams_dir.join(format!("{safe_name}.map"));
                                let _ = dl_tx.send(NetToUi::DreamDownloaded {
                                    map_name,
                                    path: map_path,
                                    dream_dir: None,
                                });
                            }
                            Err(msg) => {
                                let _ = std::fs::remove_dir_all(&stage_dir);
                                let _ = dl_tx.send(NetToUi::DreamError(msg));
                            }
                        }
                        dl_ctx.request_repaint();
                    });
                }
                UiToNet::DownloadSpecies {
                    file_id,
                    server_code,
                    if_newer_than,
                } => {
                    let dl_tx = tx.clone();
                    let dl_ctx = ctx.clone();
                    thread::spawn(move || {
                        let Some(species_dir) = AssetLocator::species_cache_dir() else {
                            let _ = dl_tx.send(NetToUi::SpeciesDownloaded {
                                file_id,
                                path: None,
                            });
                            return;
                        };
                        let _ = std::fs::create_dir_all(&species_dir);
                        let is_dynamic = file_id >= DYNAMIC_AVATAR_CACHE_BASE;
                        let avatar_id = if is_dynamic {
                            file_id - DYNAMIC_AVATAR_CACHE_BASE
                        } else {
                            file_id
                        };
                        let dest_name = if is_dynamic {
                            format!("dplayer{avatar_id}.fox")
                        } else {
                            format!("species{avatar_id}.fox")
                        };
                        let dest_path = species_dir.join(&dest_name);
                        let zip_path = species_dir.join(format!("Avatar{avatar_id}.zip.tmp"));
                        let stage_dir = species_dir.join(format!("stage_{file_id}"));
                        let _ = std::fs::remove_file(&zip_path);
                        let _ = std::fs::remove_dir_all(&stage_dir);
                        let srv = server_code
                            .filter(|c| *c != ' ' && c.is_ascii_alphanumeric())
                            .map(|c| c.to_string())
                            .unwrap_or_default();
                        let url = format!(
                            "https://apollo.furcadia.com/species{srv}/Avatar{avatar_id}.zip"
                        );
                        let mut cmd = furcadia_client::sys::command("curl");
                        cmd.args(["-fsSL", "--max-time", "25"]);
                        if let Some(ref existing) = if_newer_than {
                            if existing.is_file() {
                                cmd.arg("-z").arg(existing);
                            }
                        }
                        let curl_ok = cmd
                            .arg("-o")
                            .arg(&zip_path)
                            .arg(&url)
                            .output()
                            .is_ok_and(|o| o.status.success());
                        let mut final_path = None;
                        if curl_ok && zip_path.is_file() {
                            let _ = std::fs::create_dir_all(&stage_dir);
                            let unzip_ok = extract_zip_archive(&zip_path, &stage_dir).is_ok();
                            if unzip_ok {
                                let found = FurcadiaApp::find_file_case_insensitive(&stage_dir, &dest_name)
                                    .or_else(|| {
                                        std::fs::read_dir(&stage_dir).ok().and_then(|rd| {
                                            rd.flatten()
                                                .map(|e| e.path())
                                                .find(|p| {
                                                    p.extension()
                                                        .and_then(|s| s.to_str())
                                                        .is_some_and(|e| e.eq_ignore_ascii_case("fox"))
                                                })
                                        })
                                    });
                                if let Some(src) = found {
                                    if std::fs::rename(&src, &dest_path).is_ok()
                                        || std::fs::copy(&src, &dest_path).is_ok()
                                    {
                                        final_path = Some(dest_path);
                                    }
                                }
                            }
                        }
                        let _ = std::fs::remove_file(&zip_path);
                        let _ = std::fs::remove_dir_all(&stage_dir);
                        let _ = dl_tx.send(NetToUi::SpeciesDownloaded {
                            file_id,
                            path: final_path,
                        });
                        dl_ctx.request_repaint();
                    });
                }
                UiToNet::DownloadPortrait { portrait_id } => {
                    let dl_tx = tx.clone();
                    let dl_ctx = ctx.clone();
                    thread::spawn(move || {
                        let cache_path = AssetLocator::species_cache_dir()
                            .map(|d| d.join(format!("pp{portrait_id}.fprt")));
                        let cached_bytes = cache_path
                            .as_ref()
                            .and_then(|p| std::fs::read(p).ok())
                            .filter(|b| !b.is_empty());
                        let raw = cached_bytes.or_else(|| {
                            let fname = format!("pp{portrait_id}");
                            let bytes = download_furcadia_file_server(&fname)?;
                            if let Some(ref p) = cache_path {
                                if let Some(parent) = p.parent() {
                                    let _ = std::fs::create_dir_all(parent);
                                }
                                let _ = std::fs::write(p, &bytes);
                            }
                            Some(bytes)
                        });
                        let rgba = raw.and_then(|bytes| decode_furcadia_portrait(&bytes));
                        let _ = dl_tx.send(NetToUi::PortraitDownloaded { portrait_id, rgba });
                        dl_ctx.request_repaint();
                    });
                }
                UiToNet::DownloadChatImage { url } => {
                    let dl_tx = tx.clone();
                    let dl_ctx = ctx.clone();
                    thread::spawn(move || {
                        let mut h: u64 = 0xcbf29ce484222325;
                        for &b in url.as_bytes() {
                            h ^= b as u64;
                            h = h.wrapping_mul(0x100000001b3);
                        }
                        let cache_path = AssetLocator::species_cache_dir()
                            .map(|d| d.join(format!("chat_img_{h:016x}.bin")));
                        let raw = cache_path
                            .as_ref()
                            .and_then(|p| std::fs::read(p).ok())
                            .filter(|b| !b.is_empty())
                            .or_else(|| {
                                let out = furcadia_client::sys::command("curl")
                                    .args(["-fsSL", "--max-time", "10"])
                                    .arg(&url)
                                    .output()
                                    .ok()
                                    .filter(|o| o.status.success() && !o.stdout.is_empty())?;
                                if let Some(ref p) = cache_path {
                                    if let Some(parent) = p.parent() {
                                        let _ = std::fs::create_dir_all(parent);
                                    }
                                    let _ = std::fs::write(p, &out.stdout);
                                }
                                Some(out.stdout)
                            });
                        let rgba = raw
                            .and_then(|bytes| image::load_from_memory(&bytes).ok())
                            .map(|img| {
                                let rgba = img.to_rgba8();
                                let (w, h) = rgba.dimensions();
                                (w as usize, h as usize, rgba.into_raw())
                            });
                        let _ = dl_tx.send(NetToUi::ChatImageDownloaded { url, rgba });
                        dl_ctx.request_repaint();
                    });
                }
                UiToNet::SpellCheck {
                    seq,
                    text,
                    custom_dict,
                } => {
                    let sc_tx = tx.clone();
                    let sc_ctx = ctx.clone();
                    thread::spawn(move || {
                        let misspelled = run_hunspell_check(&text, &custom_dict);
                        let _ = sc_tx.send(NetToUi::SpellCheckResult { seq, misspelled });
                        sc_ctx.request_repaint();
                    });
                }
                UiToNet::Disconnect => {
                    if let Some(mut cl) = client.take() {
                        let _ = cl.disconnect();
                    }
                    awaiting_login_ack = false;
                    pending_desc = None;
                    pending_colors = None;
                    login_deadline = None;
                    last_server_notice = None;
                    let _ = tx.send(NetToUi::Connected {
                        seq: active_seq,
                        connected: false,
                    });
                    let _ = tx.send(NetToUi::Status("Disconnected".into()));
                    ctx.request_repaint();
                }
            }
        }

        if let Some(cl) = client.as_mut() {
            match cl.poll_message() {
                Ok(Some(msg)) => {
                    if awaiting_login_ack {
                        match &msg {
                            ServerMessage::LoginAck => {
                                awaiting_login_ack = false;
                                login_deadline = None;
                                account_retry = None;
                                if let Some(desc) = pending_desc.take() {
                                    let _ = cl.send_command(&ClientCommand::Desc(desc));
                                }
                                if let Some(colors) = pending_colors.take() {
                                    let _ = cl.send_command(&ClientCommand::Color(colors));
                                }
                                let _ = tx.send(NetToUi::Connected {
                                    seq: active_seq,
                                    connected: true,
                                });
                                let _ = tx.send(NetToUi::Status("Connected".into()));
                            }
                            ServerMessage::Text(raw) => {
                                let txt = plain_server_text(raw);
                                if !txt.is_empty() {
                                    last_server_notice = Some(txt);
                                }
                            }
                            ServerMessage::Extended {
                                code: Some('#'),
                                payload,
                            } => {
                                let txt = extract_dialog_notice(payload);
                                if !txt.is_empty() {
                                    last_server_notice = Some(txt);
                                }
                            }
                            _ => {}
                        }
                    } else if matches!(cl.state(), SessionState::Connected) {
                        let _ = tx.send(NetToUi::Status("Connected".into()));
                    }
                    let _ = tx.send(NetToUi::Message(msg));
                    ctx.request_repaint();
                }
                Ok(None) => {
                    if awaiting_login_ack && login_deadline.is_some_and(|d| Instant::now() >= d) {
                        client = None;
                        awaiting_login_ack = false;
                        pending_desc = None;
                        login_deadline = None;
                        let err_msg = last_server_notice.take().unwrap_or_else(|| {
                            "Timed out waiting for Furcadia login confirmation.".to_owned()
                        });
                        let _ = tx.send(NetToUi::Status(err_msg.clone()));
                        let _ = tx.send(NetToUi::ConnectFailed {
                            seq: active_seq,
                            error: err_msg,
                        });
                        ctx.request_repaint();
                    }
                }
                Err(err) => {
                    client = None;
                    if awaiting_login_ack {
                        if let Some((retry_seq, retry_email, retry_char, retry_pass)) =
                            account_retry.take()
                        {
                            if retry_seq == active_seq {
                                if let Some(retry_client) = connect_and_retry_pipe_account(
                                    &retry_email,
                                    &retry_char,
                                    &retry_pass,
                                    &tx,
                                    retry_seq,
                                    &mut last_server_notice,
                                ) {
                                    client = Some(retry_client);
                                    login_deadline = Some(Instant::now() + Duration::from_secs(15));
                                    ctx.request_repaint();
                                    continue;
                                }
                            }
                        }
                        awaiting_login_ack = false;
                        pending_desc = None;
                        login_deadline = None;
                        let err_msg = last_server_notice
                            .take()
                            .unwrap_or_else(|| format!("Login rejected or connection closed: {err}"));
                        let _ = tx.send(NetToUi::Status(err_msg.clone()));
                        let _ = tx.send(NetToUi::ConnectFailed {
                            seq: active_seq,
                            error: err_msg,
                        });
                    } else {
                        let _ = tx.send(NetToUi::Connected {
                            seq: active_seq,
                            connected: false,
                        });
                        let _ = tx.send(NetToUi::Status("Disconnected by server".into()));
                    }
                    ctx.request_repaint();
                }
            }
        } else {
            thread::sleep(Duration::from_millis(50));
        }
    }
}

fn read_line_cr_lf(stream: &mut std::net::TcpStream) -> Option<String> {
    use std::io::Read;
    let mut line = Vec::with_capacity(64);
    let mut byte = [0u8; 1];
    loop {
        if stream.read_exact(&mut byte).is_err() || line.len() > 4096 {
            return None;
        }
        if byte[0] == b'\n' {
            break;
        }
        if byte[0] != b'\r' {
            line.push(byte[0]);
        }
    }
    Some(String::from_utf8_lossy(&line).into_owned())
}

/// Downloads a file (such as a custom portrait `pp<id>`) from Furcadia's file server (`hades.furcadia.com:6500`).
fn download_furcadia_file_server(filename: &str) -> Option<Vec<u8>> {
    use std::io::{Read, Write};
    use std::net::{TcpStream, ToSocketAddrs};
    let mut stream = None;
    for port in [6500u16, 5000u16] {
        let Ok(addrs) = ("hades.furcadia.com", port).to_socket_addrs() else {
            continue;
        };
        for addr in addrs {
            if let Ok(s) = TcpStream::connect_timeout(&addr, Duration::from_secs(5)) {
                stream = Some(s);
                break;
            }
        }
        if stream.is_some() {
            break;
        }
    }
    let mut stream = stream?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

    let banner = read_line_cr_lf(&mut stream)?;
    if !banner.starts_with("10") {
        return None;
    }
    stream.write_all(b"USER furcdown\n").ok()?;
    let uresp = read_line_cr_lf(&mut stream)?;
    if !uresp.starts_with("20") {
        return None;
    }
    stream.write_all(b"PASS furcdown\n").ok()?;
    let presp = read_line_cr_lf(&mut stream)?;
    if !presp.starts_with("30") {
        return None;
    }

    let req = format!("RC furcdown furcdown {filename} 0 10 0\n");
    stream.write_all(req.as_bytes()).ok()?;
    let rc_resp = read_line_cr_lf(&mut stream)?;
    let mut parts = rc_resp.split_whitespace();
    if parts.next()? != "44" {
        return None;
    }
    let total_size: usize = parts.next()?.parse().ok()?;
    let _window: usize = parts.next()?.parse().ok()?;
    let pkt_size: usize = parts.next()?.parse().ok()?;
    if total_size == 0 || total_size > 8 * 1024 * 1024 || pkt_size <= 10 || pkt_size > 8192 {
        return None;
    }

    let payload_per_pkt = pkt_size - 10;
    let n_pkts = total_size.div_ceil(payload_per_pkt);
    let mut out = Vec::with_capacity(total_size);
    let mut pkt_buf = vec![0u8; pkt_size];
    stream.write_all(b"PKOK 0\n").ok()?;

    for pkt_idx in 0..n_pkts {
        if stream.read_exact(&mut pkt_buf).is_err() {
            let ack = format!("PKOK {pkt_idx}\n");
            let _ = stream.write_all(ack.as_bytes());
            stream.read_exact(&mut pkt_buf).ok()?;
        }
        if &pkt_buf[..2] != b"SC" {
            return None;
        }
        let take = payload_per_pkt.min(total_size - out.len());
        out.extend_from_slice(&pkt_buf[10..10 + take]);
    }
    let _ = stream.write_all(b"QUIT\n");
    Some(out)
}

/// Decompresses Furcadia's bitwise-inverted MSB-first LZW stream (`FPRT10`).
fn decompress_furcadia_lzw(src: &[u8]) -> Option<Vec<u8>> {
    const CLEAR: u16 = 0x100;
    const EOF: u16 = 0x101;
    const MAX_TABLE: usize = 4096;

    let mut pos = 0usize;
    let mut bitbuf: u32 = 0;
    let mut bitlen: u32 = 0;
    let mut nbits: u32 = 9;

    let mut next_code = |nbits: u32| -> u16 {
        while bitlen < nbits {
            if pos >= src.len() {
                return EOF;
            }
            let b = (src[pos] ^ 0xff) as u32;
            pos += 1;
            bitbuf = (bitbuf << 8) | b;
            bitlen += 8;
        }
        bitlen -= nbits;
        let code = (bitbuf >> bitlen) as u16;
        bitbuf &= (1u32 << bitlen).wrapping_sub(1);
        code
    };

    let mut prefix = [0u16; MAX_TABLE];
    let mut suffix = [0u8; MAX_TABLE];
    for i in 0..256usize {
        suffix[i] = i as u8;
    }

    let mut code = next_code(nbits);
    if code == CLEAR {
        code = next_code(nbits);
    }
    if code == EOF || code >= 256 {
        return None;
    }

    let mut out = Vec::with_capacity(95 * 95 * 3);
    out.push(code as u8);
    let mut old_code = code;
    let mut first_char = code as u8;
    let mut next_id: u16 = 0x102;
    let mut stack = Vec::with_capacity(64);

    loop {
        code = next_code(nbits);
        if code == EOF {
            break;
        }
        if code == CLEAR {
            next_id = 0x102;
            nbits = 9;
            code = next_code(nbits);
            if code == EOF || code >= 256 {
                break;
            }
            out.push(code as u8);
            old_code = code;
            first_char = code as u8;
            continue;
        }

        let in_code = code;
        stack.clear();
        if code == next_id {
            stack.push(first_char);
            code = old_code;
        } else if code > next_id {
            return None;
        }

        let mut cur = code as usize;
        while cur >= 256 {
            if stack.len() >= MAX_TABLE {
                return None;
            }
            stack.push(suffix[cur]);
            cur = prefix[cur] as usize;
        }
        first_char = suffix[cur];
        stack.push(first_char);
        for &b in stack.iter().rev() {
            out.push(b);
        }

        if (next_id as usize) < MAX_TABLE {
            prefix[next_id as usize] = old_code;
            suffix[next_id as usize] = first_char;
            next_id += 1;
            if (next_id as u32) + 1 == (1u32 << nbits) {
                nbits += 1;
            }
        }
        old_code = in_code;
    }

    Some(out)
}

fn extract_zip_archive(zip_path: &std::path::Path, dest_dir: &std::path::Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let out = furcadia_client::sys::command("tar")
        .arg("-xf")
        .arg(zip_path)
        .arg("-C")
        .arg(dest_dir)
        .output();
    #[cfg(not(target_os = "windows"))]
    let out = furcadia_client::sys::command("unzip")
        .args(["-oq"])
        .arg(zip_path)
        .arg("-d")
        .arg(dest_dir)
        .output();
    match out {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_owned()),
        Err(e) => Err(e.to_string()),
    }
}

fn decompress_bzip2(src: &[u8]) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut decoder = bzip2_rs::DecoderReader::new(src);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).ok()?;
    (!out.is_empty()).then_some(out)
}

/// Decodes a Furcadia custom portrait (`FPRT10`, `FPRT20`, or standard image bytes) into `(w, h, rgba)`.
fn decode_furcadia_portrait(data: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    if let Ok(img) = image::load_from_memory(data) {
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        return Some((w as usize, h as usize, rgba.into_raw()));
    }
    if data.len() < 9 {
        return None;
    }
    let header = &data[..6];
    let w = data[6] as usize;
    let h = data[7] as usize;
    let fmt = data[8];
    if w == 0 || h == 0 {
        return None;
    }
    let decoded_owned;
    let pixels: &[u8] = if header == b"FPRT10" {
        decoded_owned = decompress_furcadia_lzw(&data[9..])?;
        &decoded_owned
    } else if header == b"FPRT20" {
        &data[9..]
    } else {
        return None;
    };

    if fmt == 3 || fmt == 4 || pixels.starts_with(b"BZh") {
        let fox_owned;
        let fox_bytes = if pixels.starts_with(b"BZh") {
            fox_owned = decompress_bzip2(pixels)?;
            fox_owned
        } else {
            pixels.to_vec()
        };
        if let Ok(mut archive) = FoxArchive::from_bytes(fox_bytes) {
            let pal = Palette::default();
            if let Ok(sprite) = archive.decode_image(0, &pal) {
                return Some((sprite.width as usize, sprite.height as usize, sprite.rgba.clone()));
            }
        }
        return None;
    }

    if fmt == 2 || pixels.len() >= w * h * 3 {
        if pixels.len() < w * h * 3 {
            return None;
        }
        let mut rgba = vec![0u8; w * h * 4];
        for y in 0..h {
            let src_y = h - 1 - y;
            for x in 0..w {
                let s = (src_y * w + x) * 3;
                let d = (y * w + x) * 4;
                rgba[d] = pixels[s + 2];
                rgba[d + 1] = pixels[s + 1];
                rgba[d + 2] = pixels[s];
                rgba[d + 3] = 255;
            }
        }
        return Some((w, h, rgba));
    }

    None
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_title("Furcadia Native Client"),
        ..Default::default()
    };
    eframe::run_native(
        "Furcadia",
        options,
        Box::new(|cc| Ok(Box::new(FurcadiaApp::new(cc)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_emote_quotes_handles_multiple_spoken_segments() {
        let input = r#"moves to find a seat and smiles. "Hey what's up?" the beast says as he smiles to everyone then licks his lips. "Why hello, ladies." he winks to the females across the way"#;
        let parts = split_emote_quotes(input);
        assert_eq!(
            parts,
            vec![
                ("moves to find a seat and smiles. ", false),
                ("\"Hey what's up?\"", true),
                (" the beast says as he smiles to everyone then licks his lips. ", false),
                ("\"Why hello, ladies.\"", true),
                (" he winks to the females across the way", false),
            ]
        );
    }

    #[test]
    fn rewrite_pounce_add_message_reformats_friend_request() {
        let line = parse_chat_line(
            "<font color='success'>Your friend request has been sent to <a href='name://kiralyrica'>Kira Lyrica</a>.</font>",
        );
        let rewritten = rewrite_pounce_add_message(line);
        assert_eq!(rewritten.to_plain_text(), "Kira Lyrica has been added to Pounce");
    }

    #[test]
    fn format_whisper_tab_line_does_not_repeat_pipe_name_in_emote() {
        let plain = r#"[ Alty Bunnie whispers, "Alty|Bunnie shivers and waves." to you. ]"#;
        let fallback = parse_chat_line(plain);
        let formatted = format_whisper_tab_line("Amala|Devi", "Alty Bunnie", plain, &fallback);
        assert_eq!(formatted.to_plain_text(), "Alty Bunnie shivers and waves.");
    }

    #[test]
    fn cookie_counts_parse_server_reports() {
        assert_eq!(
            parse_cookie_counts("[@cookie] Cookie bank has currently collected: 7"),
            (Some(7), None)
        );
        assert_eq!(
            parse_cookie_counts("[@cookie] All-time Cookie total: 42"),
            (None, Some(42))
        );
        assert_eq!(parse_cookie_counts("hello world"), (None, None));
    }

    #[test]
    fn bundled_spellcheck_recognizes_furcadia_and_flags_typos() {
        let custom = std::collections::HashSet::new();
        let ok = run_bundled_spell_check("Furcadia handpaws nuzzled Alty's", &custom);
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].word, "Alty's");

        let typos = run_bundled_spell_check("teh furre", &custom);
        assert_eq!(typos.len(), 1);
        assert_eq!(typos[0].word, "teh");
        assert!(typos[0].suggestions.iter().any(|s| s == "the"));
    }

    #[test]
    fn main_map_ids_normalize_to_apollo_names() {
        assert_eq!(normalize_main_map_id("allegria023"), Some("pmallegria"));
        assert_eq!(normalize_main_map_id("pmimag"), Some("pmimaginarium"));
        assert_eq!(normalize_main_map_id("imagine"), Some("pmimaginarium"));
        assert_eq!(normalize_main_map_id("acropoli"), Some("pmacropolis"));
        assert_eq!(normalize_main_map_id("furn"), Some("pmfurn"));
    }

    #[test]
    fn update_version_comparison_and_json_parsing() {
        assert!(is_newer_version("1.0.2", "v1.0.3"));
        assert!(is_newer_version("1.0.2", "1.1.0"));
        assert!(!is_newer_version("1.0.2", "v1.0.2"));
        assert!(!is_newer_version("1.0.2", "1.0.1"));

        let sample = r#"{
            "tag_name": "v1.0.3",
            "html_url": "https://github.com/morganlowe/native-furcadia/releases/tag/v1.0.3",
            "assets": [
                {"browser_download_url": "https://github.com/morganlowe/native-furcadia/releases/download/v1.0.3/FurcadiaNative-Setup-1.0.3-x86_64.exe"},
                {"browser_download_url": "https://github.com/morganlowe/native-furcadia/releases/download/v1.0.3/furcadia-native_1.0.3_amd64.deb"}
            ]
        }"#;
        let info = parse_github_release_json(sample).expect("should parse release json");
        assert_eq!(info.tag, "v1.0.3");
        assert_eq!(info.version, "1.0.3");
        assert_eq!(
            info.html_url,
            "https://github.com/morganlowe/native-furcadia/releases/tag/v1.0.3"
        );
        assert_eq!(
            update_download_filename("https://example.com/releases/FurcadiaNative-Setup-1.0.3-x86_64.exe?dl=1"),
            "FurcadiaNative-Setup-1.0.3-x86_64.exe"
        );
        assert_eq!(
            update_download_filename("https://example.com/furcadia-native_1.0.3_amd64.deb"),
            "furcadia-native_1.0.3_amd64.deb"
        );
    }

    #[test]
    fn decode_furcadia_portrait_handles_fprt20_bottom_up_bgr() {
        // 2x1 bottom-up BGR: row 0 (bottom) = [B=1,G=2,R=3], [B=4,G=5,R=6]
        let mut data = b"FPRT20\x02\x02\x02".to_vec();
        // bottom row (y=1 in top-down)
        data.extend_from_slice(&[10, 20, 30, 40, 50, 60]);
        // top row (y=0 in top-down)
        data.extend_from_slice(&[100, 110, 120, 130, 140, 150]);
        let (w, h, rgba) = decode_furcadia_portrait(&data).expect("should decode FPRT20");
        assert_eq!((w, h), (2, 2));
        // Top-left pixel should be RGB(120, 110, 100, 255)
        assert_eq!(&rgba[0..4], &[120, 110, 100, 255]);
        // Bottom-right pixel should be RGB(60, 50, 40, 255)
        assert_eq!(&rgba[12..16], &[60, 50, 40, 255]);
    }

    #[test]
    fn decompress_furcadia_lzw_decodes_inverted_msb_stream() {
        // 9-bit MSB-first codes: CLEAR(0x100)=100000000, 'A'(0x41)=001000001, 'B'(0x42)=001000010, 0x102("AB")=100000010, EOF(0x101)=100000001
        // Bits: 10000000 00010000 01001000 01010000 00101000 00001_000 (padded)
        // Inverted (^ 0xff):
        let encoded = [0x80 ^ 0xff, 0x10 ^ 0xff, 0x48 ^ 0xff, 0x50 ^ 0xff, 0x28 ^ 0xff, 0x08 ^ 0xff];
        let dec = decompress_furcadia_lzw(&encoded).expect("should decompress LZW");
        assert_eq!(dec, b"ABAB");
    }
}
