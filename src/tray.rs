//! Cross-platform system tray integration.
//!
//! On Linux (KDE Plasma, GNOME AppIndicator, Waybar, XFCE, etc.), registers a native
//! Freedesktop/KDE `org.kde.StatusNotifierItem` with a `com.canonical.dbusmenu` context menu
//! over the session D-Bus via `zbus` (no GTK or `libappindicator` system dependency).

use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};

/// User action triggered from the system tray icon or its context menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    Toggle,
    Show,
    Hide,
    Quit,
}

/// Handle to the background system tray icon.
pub struct TrayHandle {
    rx: mpsc::Receiver<TrayAction>,
    active: Arc<AtomicBool>,
}

impl TrayHandle {
    /// Returns `true` if a system tray icon was successfully registered with the desktop session.
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    /// Non-blockingly polls the next user action from the tray icon.
    pub fn try_recv(&self) -> Option<TrayAction> {
        self.rx.try_recv().ok()
    }
}

/// Converts raw PNG bytes into a 32x32 ARGB32 (network byte order) pixmap for StatusNotifierItem.
pub fn png_to_sni_pixmap(png_bytes: &[u8]) -> Option<(i32, i32, Vec<u8>)> {
    let img = image::load_from_memory(png_bytes).ok()?;
    let resized = img
        .resize_exact(32, 32, image::imageops::FilterType::Lanczos3)
        .to_rgba8();
    let (w, h) = resized.dimensions();
    let mut argb = Vec::with_capacity((w * h * 4) as usize);
    for px in resized.pixels() {
        argb.push(px[3]); // A
        argb.push(px[0]); // R
        argb.push(px[1]); // G
        argb.push(px[2]); // B
    }
    Some((w as i32, h as i32, argb))
}

/// Spawns the system tray icon on a background thread.
///
/// `on_event` is invoked immediately after each `TrayAction` is queued (e.g. `ctx.request_repaint()`)
/// so hidden windows wake up right away on Wayland/X11.
pub fn spawn_tray<F>(icon_png_bytes: &'static [u8], on_event: F) -> TrayHandle
where
    F: Fn() + Send + Sync + 'static,
{
    let (tx, rx) = mpsc::channel::<TrayAction>();
    let active = Arc::new(AtomicBool::new(false));

    #[cfg(target_os = "linux")]
    {
        let active_bg = Arc::clone(&active);
        let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(on_event);
        std::thread::Builder::new()
            .name("furc-tray".to_owned())
            .spawn(move || {
                linux::run_sni_tray(icon_png_bytes, tx, notify, active_bg);
            })
            .ok();
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = (icon_png_bytes, on_event, tx);
    }

    TrayHandle { rx, active }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{png_to_sni_pixmap, TrayAction};
    use std::collections::HashMap;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    };
    use std::time::Duration;
    use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

    const MENU_SHOW_HIDE_ID: i32 = 1;
    const MENU_SEPARATOR_ID: i32 = 2;
    const MENU_QUIT_ID: i32 = 3;

    struct StatusNotifierItem {
        tx: mpsc::Sender<TrayAction>,
        notify: Arc<dyn Fn() + Send + Sync>,
        pixmap: Vec<(i32, i32, Vec<u8>)>,
    }

    impl StatusNotifierItem {
        fn send(&self, action: TrayAction) {
            let _ = self.tx.send(action);
            (self.notify)();
        }
    }

    #[zbus::interface(name = "org.kde.StatusNotifierItem")]
    impl StatusNotifierItem {
        #[zbus(property)]
        fn category(&self) -> &str {
            "Communications"
        }

        #[zbus(property)]
        fn id(&self) -> &str {
            "furcadia-native"
        }

        #[zbus(property)]
        fn title(&self) -> &str {
            "Furcadia Native Client"
        }

        #[zbus(property)]
        fn status(&self) -> &str {
            "Active"
        }

        #[zbus(property)]
        fn window_id(&self) -> i32 {
            0
        }

        #[zbus(property)]
        fn icon_name(&self) -> &str {
            if self.pixmap.is_empty() {
                "furcadia-native"
            } else {
                ""
            }
        }

        #[zbus(property)]
        fn icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
            self.pixmap.clone()
        }

        #[zbus(property)]
        fn item_is_menu(&self) -> bool {
            false
        }

        #[zbus(property)]
        fn menu(&self) -> OwnedObjectPath {
            ObjectPath::from_static_str_unchecked("/MenuBar").into()
        }

        #[zbus(property)]
        fn tool_tip(&self) -> (String, Vec<(i32, i32, Vec<u8>)>, String, String) {
            (
                String::new(),
                Vec::new(),
                "Furcadia Native Client".to_owned(),
                "Click to show or hide window".to_owned(),
            )
        }

        fn activate(&self, _x: i32, _y: i32) {
            self.send(TrayAction::Toggle);
        }

        fn secondary_activate(&self, _x: i32, _y: i32) {
            self.send(TrayAction::Toggle);
        }

        fn context_menu(&self, _x: i32, _y: i32) {}

        fn scroll(&self, _delta: i32, _orientation: &str) {}
    }

    struct TrayDbusMenu {
        tx: mpsc::Sender<TrayAction>,
        notify: Arc<dyn Fn() + Send + Sync>,
    }

    type MenuLayoutItem = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

    fn menu_props(id: i32) -> HashMap<String, OwnedValue> {
        let mut m = HashMap::new();
        match id {
            0 => {
                m.insert(
                    "children-display".to_owned(),
                    Value::from("submenu").try_into().unwrap(),
                );
            }
            MENU_SHOW_HIDE_ID => {
                m.insert(
                    "label".to_owned(),
                    Value::from("Show / Hide Furcadia").try_into().unwrap(),
                );
                m.insert("enabled".to_owned(), Value::from(true).try_into().unwrap());
                m.insert("visible".to_owned(), Value::from(true).try_into().unwrap());
            }
            MENU_SEPARATOR_ID => {
                m.insert(
                    "type".to_owned(),
                    Value::from("separator").try_into().unwrap(),
                );
                m.insert("visible".to_owned(), Value::from(true).try_into().unwrap());
            }
            MENU_QUIT_ID => {
                m.insert(
                    "label".to_owned(),
                    Value::from("Quit Furcadia").try_into().unwrap(),
                );
                m.insert("enabled".to_owned(), Value::from(true).try_into().unwrap());
                m.insert("visible".to_owned(), Value::from(true).try_into().unwrap());
            }
            _ => {}
        }
        m
    }

    impl TrayDbusMenu {
        fn handle_event(&self, id: i32, event_id: &str) {
            if event_id != "clicked" {
                return;
            }
            match id {
                MENU_SHOW_HIDE_ID => {
                    let _ = self.tx.send(TrayAction::Toggle);
                    (self.notify)();
                }
                MENU_QUIT_ID => {
                    let _ = self.tx.send(TrayAction::Quit);
                    (self.notify)();
                }
                _ => {}
            }
        }
    }

    #[zbus::interface(name = "com.canonical.dbusmenu")]
    impl TrayDbusMenu {
        #[zbus(property)]
        fn version(&self) -> u32 {
            3
        }

        #[zbus(property)]
        fn text_direction(&self) -> &str {
            "ltr"
        }

        #[zbus(property)]
        fn status(&self) -> &str {
            "normal"
        }

        #[zbus(property)]
        fn icon_theme_path(&self) -> Vec<String> {
            Vec::new()
        }

        fn get_layout(
            &self,
            parent_id: i32,
            _recursion_depth: i32,
            _property_names: Vec<String>,
        ) -> (u32, MenuLayoutItem) {
            if parent_id == 0 {
                let children: Vec<OwnedValue> = [
                    MENU_SHOW_HIDE_ID,
                    MENU_SEPARATOR_ID,
                    MENU_QUIT_ID,
                ]
                .into_iter()
                .filter_map(|id| {
                    let item: MenuLayoutItem = (id, menu_props(id), Vec::new());
                    Value::from(item).try_into().ok()
                })
                .collect();
                (1, (0, menu_props(0), children))
            } else {
                (1, (parent_id, menu_props(parent_id), Vec::new()))
            }
        }

        fn get_group_properties(
            &self,
            ids: Vec<i32>,
            _property_names: Vec<String>,
        ) -> Vec<(i32, HashMap<String, OwnedValue>)> {
            ids.into_iter().map(|id| (id, menu_props(id))).collect()
        }

        fn get_property(&self, id: i32, name: &str) -> OwnedValue {
            menu_props(id)
                .remove(name)
                .unwrap_or_else(|| Value::from("").try_into().unwrap())
        }

        fn event(&self, id: i32, event_id: &str, _data: OwnedValue, _timestamp: u32) {
            self.handle_event(id, event_id);
        }

        fn event_group(&self, events: Vec<(i32, String, OwnedValue, u32)>) -> Vec<i32> {
            for (id, event_id, _, _) in events {
                self.handle_event(id, &event_id);
            }
            Vec::new()
        }

        fn about_to_show(&self, _id: i32) -> bool {
            false
        }

        fn about_to_show_group(&self, _ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
            (Vec::new(), Vec::new())
        }
    }

    pub fn run_sni_tray(
        icon_png_bytes: &[u8],
        tx: mpsc::Sender<TrayAction>,
        notify: Arc<dyn Fn() + Send + Sync>,
        active: Arc<AtomicBool>,
    ) {
        let pixmap = png_to_sni_pixmap(icon_png_bytes).into_iter().collect();
        let bus_name = format!("org.kde.StatusNotifierItem-{}-1", std::process::id());
        let item = StatusNotifierItem {
            tx: tx.clone(),
            notify: Arc::clone(&notify),
            pixmap,
        };
        let menu = TrayDbusMenu { tx, notify };

        let Ok(builder) = zbus::blocking::connection::Builder::session() else {
            return;
        };
        let Ok(builder) = builder.name(bus_name.as_str()) else {
            return;
        };
        let Ok(builder) = builder.serve_at("/StatusNotifierItem", item) else {
            return;
        };
        let Ok(builder) = builder.serve_at("/MenuBar", menu) else {
            return;
        };
        let Ok(conn) = builder.build() else {
            return;
        };

        // Register with KDE / Freedesktop StatusNotifierWatcher (retry briefly in case
        // the desktop shell tray host is still starting up).
        let mut registered = false;
        for _ in 0..5 {
            if conn
                .call_method(
                    Some("org.kde.StatusNotifierWatcher"),
                    "/StatusNotifierWatcher",
                    Some("org.kde.StatusNotifierWatcher"),
                    "RegisterStatusNotifierItem",
                    &(bus_name.as_str(),),
                )
                .is_ok()
            {
                registered = true;
                break;
            }
            std::thread::sleep(Duration::from_secs(2));
        }

        if !registered {
            return;
        }
        active.store(true, Ordering::Relaxed);

        // Keep the D-Bus connection alive for the lifetime of the process.
        loop {
            std::thread::sleep(Duration::from_secs(60));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_to_sni_pixmap_converts_embedded_icon() {
        let png = include_bytes!("../packaging/linux/furcadia-native.png");
        let (w, h, argb) = png_to_sni_pixmap(png).expect("valid PNG icon");
        assert_eq!((w, h), (32, 32));
        assert_eq!(argb.len(), 32 * 32 * 4);
    }
}
