//! Furcadia Pounce watchlist and online character monitoring.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::encoding::{decode_b220_raw, decode_win1252};

/// Minimum per-character `onln` poll interval (Furcadia guideline: 3 minutes).
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(180);
/// Minimum spacing between consecutive `onln` queries on the socket.
pub const DEFAULT_QUERY_SPACING: Duration = Duration::from_secs(1);

/// Presence state of a watched character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnlineStatus {
    Unknown,
    Online,
    Offline,
}

/// A single character entry in the Pounce watchlist or server Pounce list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PounceEntry {
    pub name: String,
    pub status: OnlineStatus,
    pub last_checked: Option<Instant>,
    pub last_seen_online: Option<Instant>,
    pub self_uid: Option<u32>,
    pub friend_uid: Option<u32>,
    pub mutual: bool,
    pub incoming_request: bool,
    pub server_synced: bool,
}

/// Transition or check event produced when an `onln` response arrives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PounceEvent {
    CameOnline(String),
    WentOffline(String),
    Checked { name: String, online: bool },
}

/// Manages watched characters, rate-limited `onln` scheduling, and presence transitions.
#[derive(Debug)]
pub struct PounceMonitor {
    entries: BTreeMap<String, PounceEntry>,
    poll_interval: Duration,
    query_spacing: Duration,
    last_query_sent: Option<Instant>,
}

impl Default for PounceMonitor {
    fn default() -> Self {
        Self::new(DEFAULT_POLL_INTERVAL, DEFAULT_QUERY_SPACING)
    }
}

impl PounceMonitor {
    pub fn new(poll_interval: Duration, query_spacing: Duration) -> Self {
        Self {
            entries: BTreeMap::new(),
            poll_interval,
            query_spacing,
            last_query_sent: None,
        }
    }

    fn normalize(name: &str) -> String {
        name.replace('|', " ").trim().to_ascii_lowercase()
    }

    /// Adds a character to the watchlist. Returns `true` if newly added.
    pub fn add_watch(&mut self, name: impl Into<String>) -> bool {
        let name = name.into().replace('|', " ").trim().to_owned();
        if name.is_empty() {
            return false;
        }
        let key = Self::normalize(&name);
        if self.entries.contains_key(&key) {
            return false;
        }
        self.entries.insert(
            key,
            PounceEntry {
                name,
                status: OnlineStatus::Unknown,
                last_checked: None,
                last_seen_online: None,
                self_uid: None,
                friend_uid: None,
                mutual: false,
                incoming_request: false,
                server_synced: false,
            },
        );
        true
    }

    /// Removes a character from the watchlist. Returns `true` if removed.
    pub fn remove_watch(&mut self, name: &str) -> bool {
        let key = Self::normalize(name);
        self.entries.remove(&key).is_some()
    }

    /// Returns true if `name` (case-insensitive) is present in the watchlist/Pounce list.
    pub fn contains(&self, name: &str) -> bool {
        self.entries.contains_key(&Self::normalize(name))
    }

    /// Returns all watched entries sorted alphabetically by normalized name.
    pub fn entries(&self) -> impl Iterator<Item = &PounceEntry> {
        self.entries.values()
    }

    /// Returns entries sorted with Online first, then incoming requests, Unknown, Offline,
    /// ordered case-insensitively by name within each group.
    pub fn sorted_entries(&self) -> Vec<&PounceEntry> {
        let mut out: Vec<&PounceEntry> = self.entries.values().collect();
        out.sort_by(|a, b| {
            let rank = |e: &PounceEntry| -> u8 {
                if e.status == OnlineStatus::Online {
                    0
                } else if e.incoming_request {
                    1
                } else if e.status == OnlineStatus::Unknown {
                    2
                } else {
                    3
                }
            };
            rank(a)
                .cmp(&rank(b))
                .then_with(|| Self::normalize(&a.name).cmp(&Self::normalize(&b.name)))
        });
        out
    }

    /// Returns the number of watched characters.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn has_unknown(&self) -> bool {
        self.entries
            .values()
            .any(|e| !(e.server_synced && e.mutual) && e.status == OnlineStatus::Unknown)
    }

    /// Returns the next character due for an `onln` check at `now`, marking it as
    /// queried so it won't be re-selected until `poll_interval` elapses.
    /// Mutual server-synced friends receive live push updates (`]?o`/`]?S`) and skip polling.
    pub fn next_due_check(&mut self, now: Instant) -> Option<String> {
        let due_key = self
            .entries
            .iter()
            .filter(|(_, entry)| !(entry.server_synced && entry.mutual))
            .find(|(_, entry)| match entry.last_checked {
                None => true,
                Some(checked_at) => now.saturating_duration_since(checked_at) >= self.poll_interval,
            })
            .map(|(k, _)| k.clone())?;

        if let Some(last) = self.last_query_sent {
            if now.saturating_duration_since(last) < self.query_spacing {
                return None;
            }
        }

        self.last_query_sent = Some(now);
        if let Some(entry) = self.entries.get_mut(&due_key) {
            entry.last_checked = Some(now);
            return Some(entry.name.clone());
        }
        None
    }

    /// Applies a raw server Pounce payload after `]?` (`I`, `A`, `S`, `o`) using Base220 encoding
    /// (matching Furcadia web client `class Ls`).
    pub fn apply_server_pounce(&mut self, data: &[u8], now: Instant) -> Vec<PounceEvent> {
        let Some((&sub, payload)) = data.split_first() else {
            return Vec::new();
        };
        let mut events = Vec::new();
        let decode_status = |b: u8| match b {
            b'+' => OnlineStatus::Online,
            b'-' => OnlineStatus::Offline,
            _ => OnlineStatus::Unknown,
        };

        match sub {
            b'I' | b'A' => {
                if sub == b'I' {
                    self.entries.retain(|_, e| !e.server_synced);
                }
                let mut pos = 0usize;
                while pos + 23 <= payload.len() {
                    let self_uid = decode_b220_raw(&payload[pos..pos + 4]) as u32;
                    let friend_uid = decode_b220_raw(&payload[pos + 4..pos + 8]) as u32;
                    let status = decode_status(payload[pos + 8]);
                    let flags = decode_b220_raw(&payload[pos + 21..pos + 22]) as u8;
                    let mutual = (flags & 2) != 0;
                    let incoming_request = (flags & 1) != 0;
                    let name_len = decode_b220_raw(&payload[pos + 22..pos + 23]) as usize;
                    pos += 23;
                    if pos + name_len > payload.len() {
                        break;
                    }
                    let name = decode_win1252(&payload[pos..pos + name_len])
                        .replace('|', " ")
                        .trim()
                        .to_owned();
                    pos += name_len;
                    if name.is_empty() {
                        continue;
                    }
                    let key = Self::normalize(&name);
                    if let Some(existing) = self.entries.get_mut(&key) {
                        existing.name = name;
                        existing.status = status;
                        // The initial Pounce list can contain '?' for an unchecked
                        // status. Do not treat that placeholder as a completed poll:
                        // queue it for the first onln pass immediately.
                        existing.last_checked = (status != OnlineStatus::Unknown).then_some(now);
                        if status == OnlineStatus::Online {
                            existing.last_seen_online = Some(now);
                        }
                        existing.self_uid = Some(self_uid);
                        existing.friend_uid = Some(friend_uid);
                        existing.mutual = mutual;
                        existing.incoming_request = incoming_request;
                        existing.server_synced = true;
                    } else {
                        self.entries.insert(
                            key,
                            PounceEntry {
                                name,
                                status,
                                last_checked: (status != OnlineStatus::Unknown).then_some(now),
                                last_seen_online: (status == OnlineStatus::Online).then_some(now),
                                self_uid: Some(self_uid),
                                friend_uid: Some(friend_uid),
                                mutual,
                                incoming_request,
                                server_synced: true,
                            },
                        );
                    }
                }
            }
            b'S' => {
                let mut pos = 0usize;
                while pos + 17 <= payload.len() {
                    let self_uid = decode_b220_raw(&payload[pos..pos + 4]) as u32;
                    let friend_uid = decode_b220_raw(&payload[pos + 4..pos + 8]) as u32;
                    let status = decode_status(payload[pos + 8]);
                    pos += 17;
                    for entry in self.entries.values_mut() {
                        if entry.friend_uid == Some(friend_uid)
                            && (entry.self_uid.is_none() || entry.self_uid == Some(self_uid))
                        {
                            entry.status = status;
                            entry.last_checked = Some(now);
                            if status == OnlineStatus::Online {
                                entry.last_seen_online = Some(now);
                            }
                        }
                    }
                }
            }
            b'o'
                if payload.len() >= 5 => {
                    let friend_uid = decode_b220_raw(&payload[0..4]) as u32;
                    let new_status = decode_status(payload[4]);
                    let mut notified = HashSet::new();
                    for entry in self.entries.values_mut() {
                        if entry.friend_uid == Some(friend_uid) {
                            let prev = entry.status;
                            entry.status = new_status;
                            entry.last_checked = Some(now);
                            if new_status == OnlineStatus::Online {
                                entry.last_seen_online = Some(now);
                            }
                            if prev != new_status
                                && (entry.mutual || entry.server_synced)
                                && notified.insert(Self::normalize(&entry.name))
                            {
                                match new_status {
                                    OnlineStatus::Online => {
                                        events.push(PounceEvent::CameOnline(entry.name.clone()));
                                    }
                                    OnlineStatus::Offline => {
                                        events.push(PounceEvent::WentOffline(entry.name.clone()));
                                    }
                                    OnlineStatus::Unknown => {}
                                }
                            }
                        }
                    }
                }
            _ => {}
        }

        events
    }

    /// Processes a `ServerMessage::OnlineStatus { online, name }` response.
    /// Returns `Some(PounceEvent)` if the character is in the watchlist.
    pub fn handle_online_status(
        &mut self,
        name: &str,
        online: bool,
        now: Instant,
    ) -> Option<PounceEvent> {
        let key = Self::normalize(name);
        let entry = self.entries.get_mut(&key)?;
        entry.last_checked = Some(now);
        if online {
            entry.name = name.replace('|', " ").trim().to_owned();
            entry.last_seen_online = Some(now);
        }

        let new_status = if online {
            OnlineStatus::Online
        } else {
            OnlineStatus::Offline
        };
        let prev_status = entry.status;
        entry.status = new_status;

        let event = match (prev_status, new_status) {
            (OnlineStatus::Offline | OnlineStatus::Unknown, OnlineStatus::Online) => {
                PounceEvent::CameOnline(entry.name.clone())
            }
            (OnlineStatus::Online, OnlineStatus::Offline) => {
                PounceEvent::WentOffline(entry.name.clone())
            }
            _ => PounceEvent::Checked {
                name: entry.name.clone(),
                online,
            },
        };
        Some(event)
    }

    /// Loads watched character names (one per line, ignoring `#` comments and blanks).
    pub fn load_watchlist(&mut self, path: &Path) -> io::Result<usize> {
        let content = fs::read_to_string(path)?;
        let mut added = 0;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('[') {
                continue;
            }
            let name = trimmed.split('=').next().unwrap_or(trimmed).trim();
            if self.add_watch(name) {
                added += 1;
            }
        }
        Ok(added)
    }

    /// Saves watched character names (one per line) to `path`.
    pub fn save_watchlist(&self, path: &Path) -> io::Result<()> {
        let mut out = String::from("# Furcadia Pounce watchlist\n");
        for entry in self.entries.values() {
            out.push_str(&entry.name);
            out.push('\n');
        }
        fs::write(path, out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pounce_transitions_and_rate_limiting() {
        let mut monitor = PounceMonitor::new(Duration::from_secs(180), Duration::from_secs(1));
        assert!(monitor.add_watch("tactful"));
        assert!(!monitor.add_watch("Tactful"));

        let t0 = Instant::now();
        assert_eq!(monitor.next_due_check(t0), Some("tactful".into()));
        // Immediately after, not due again
        assert_eq!(monitor.next_due_check(t0 + Duration::from_secs(10)), None);

        // Canonicalizes name and emits CameOnline
        let ev1 = monitor.handle_online_status("Tactful", true, t0 + Duration::from_secs(1));
        assert_eq!(ev1, Some(PounceEvent::CameOnline("Tactful".into())));

        // Still online -> Checked
        let ev2 = monitor.handle_online_status("Tactful", true, t0 + Duration::from_secs(182));
        assert_eq!(
            ev2,
            Some(PounceEvent::Checked {
                name: "Tactful".into(),
                online: true
            })
        );

        // Goes offline -> WentOffline
        let ev3 = monitor.handle_online_status("Tactful", false, t0 + Duration::from_secs(365));
        assert_eq!(ev3, Some(PounceEvent::WentOffline("Tactful".into())));

        // Due again after 180s
        assert_eq!(
            monitor.next_due_check(t0 + Duration::from_secs(546)),
            Some("Tactful".into())
        );

        // Pipe-separated server name matches space-separated watchlist entry
        monitor.add_watch("Sunni McCalister");
        let ev4 =
            monitor.handle_online_status("Sunni|McCalister", true, t0 + Duration::from_secs(550));
        assert_eq!(
            ev4,
            Some(PounceEvent::CameOnline("Sunni McCalister".into()))
        );
    }

    #[test]
    fn server_pounce_base220_init_add_status_online_and_sorting() {
        use crate::encoding::encode_base220;

        let mut monitor = PounceMonitor::new(Duration::from_secs(180), Duration::from_secs(1));
        monitor.add_watch("LocalWatch");
        let t0 = Instant::now();

        let make_rec = |self_uid: u64, friend_uid: u64, status: u8, flags: u64, name: &str| {
            let mut buf = Vec::new();
            buf.extend(encode_base220(self_uid, 4).unwrap());
            buf.extend(encode_base220(friend_uid, 4).unwrap());
            buf.push(status);
            for _ in 0..3 {
                buf.extend(encode_base220(1000, 4).unwrap());
            }
            buf.extend(encode_base220(flags, 1).unwrap());
            buf.extend(encode_base220(name.len() as u64, 1).unwrap());
            buf.extend_from_slice(name.as_bytes());
            buf
        };

        // ]?I with an offline mutual friend ("Zed|Wolf") and an incoming request ("Ada").
        let mut init_pkt = vec![b'I'];
        init_pkt.extend(make_rec(10, 200, b'-', 2, "Zed|Wolf"));
        init_pkt.extend(make_rec(10, 300, b'-', 1, "Ada"));
        assert!(monitor.apply_server_pounce(&init_pkt, t0).is_empty());

        // ]?A adds an online mutual friend ("Bram").
        let mut add_pkt = vec![b'A'];
        add_pkt.extend(make_rec(10, 400, b'+', 2, "Bram"));
        assert!(monitor.apply_server_pounce(&add_pkt, t0).is_empty());

        // Sorting: Online ("Bram") -> Incoming request ("Ada") -> Unknown ("LocalWatch") -> Offline ("Zed Wolf")
        let order: Vec<&str> = monitor
            .sorted_entries()
            .into_iter()
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(order, vec!["Bram", "Ada", "LocalWatch", "Zed Wolf"]);

        // Mutual server-synced entries skip `onln` polling; local watchlist still polls.
        assert_eq!(
            monitor.next_due_check(t0 + Duration::from_secs(1)),
            Some("LocalWatch".into())
        );
        assert_eq!(monitor.next_due_check(t0 + Duration::from_secs(5)), None);

        // ]?S updates Bram to offline silently.
        let mut s_pkt = vec![b'S'];
        s_pkt.extend(encode_base220(10, 4).unwrap());
        s_pkt.extend(encode_base220(400, 4).unwrap());
        s_pkt.push(b'-');
        s_pkt.extend(encode_base220(100, 4).unwrap());
        s_pkt.extend(encode_base220(200, 4).unwrap());
        assert!(monitor
            .apply_server_pounce(&s_pkt, t0 + Duration::from_secs(10))
            .is_empty());

        // ]?o flips Zed Wolf online and emits CameOnline event.
        let mut o_pkt = vec![b'o'];
        o_pkt.extend(encode_base220(200, 4).unwrap());
        o_pkt.push(b'+');
        let evs = monitor.apply_server_pounce(&o_pkt, t0 + Duration::from_secs(20));
        assert_eq!(evs, vec![PounceEvent::CameOnline("Zed Wolf".into())]);
    }
}
