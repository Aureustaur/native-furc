//! Core library for the native Linux Furcadia client.

pub mod assets;
pub mod auth;
pub mod chat;
pub mod client;
pub mod config;
pub mod dragonspeak;
pub mod encoding;
pub mod fox;
pub mod furc_colors;
pub mod logs;
pub mod map;
pub mod news;
pub mod pounce;
pub mod protocol;
pub mod rch;
pub mod sys;
pub mod tray;
pub mod win_import;
pub mod world;

pub use assets::{AssetLocator, Palette};
pub use auth::{fetch_account_characters, parse_clogin_xml, AccountInfo, AuthError, CharacterSummary};
pub use chat::{parse_chat_line, unescape_html, ChatLine, ChatSpan, ChatStyle};
pub use client::{ClientError, ConnectionConfig, FurcadiaClient, SessionState};
pub use config::ClientConfig;
pub use dragonspeak::{DsArg, DsCategory, DsError, DsLine, DsScript, DsTrigger};
pub use fox::{
    decode_fsh_sprites, FoxActiveSlot, FoxAnimStep, FoxArchive, FoxAvatarShape, FoxError,
    FoxFrame, FoxImageEntry, FoxLayer, FoxResolvedSlots, FoxShapeRef, FoxSprite,
};
pub use furc_colors::{parse_color_code, AvatarAppearance, RemapLuts};
pub use map::{FurcadiaMap, MapError};
pub use news::{fetch_news, parse_news_feed, NewsEntry};
pub use pounce::{OnlineStatus, PounceEntry, PounceEvent, PounceMonitor};
pub use protocol::{ClientCommand, ProtocolError, ServerMessage, DEFAULT_HOST, DEFAULT_PORT};
pub use rch::{extract_fr01_to_dir, is_fr01, parse_fr01, RchEntry, RchError};
pub use world::{Avatar, DreamLabel, LookedPlayerInfo, RegionDefaults, RegionLayer, WorldState};
