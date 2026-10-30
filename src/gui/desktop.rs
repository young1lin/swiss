// Copyright 2026 young1lin. Licensed under the Apache License, Version 2.0.

//! Window-only preferences and the limited panel bridge (SPEC §host.gui).

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::io::Read;

#[link(name = "kernel32")]
unsafe extern "system" {
    #[link_name = "FreeConsole"]
    fn free_console() -> i32;
}

#[link(name = "user32")]
unsafe extern "system" {
    #[link_name = "MessageBoxW"]
    fn message_box(owner: *mut std::ffi::c_void, text: *const u16, caption: *const u16, flags: u32) -> i32;
}

pub fn detach_console() {
    // SAFETY: this syscall detaches only the calling process. It never hides or closes the
    // parent's terminal, and an already detached launch needs no special handling.
    unsafe { free_console(); }
}

pub fn startup_error(reason: &str) {
    eprintln!("swiss gui: {reason}");
    let text: Vec<u16> = reason.encode_utf16().chain(Some(0)).collect();
    let caption: Vec<u16> = "swiss".encode_utf16().chain(Some(0)).collect();
    // SAFETY: the null owner is allowed; both UTF-16 buffers are terminated and remain alive
    // for this synchronous dialog. MB_ICONERROR also supplies the default OK button.
    unsafe { message_box(std::ptr::null_mut(), text.as_ptr(), caption.as_ptr(), 0x10); }
}

pub const MIN_WIDTH: u32 = 960;
pub const MIN_HEIGHT: u32 = 640;

pub struct Screen {
    pub position: (i32, i32),
    pub size: (u32, u32),
    pub scale: f64,
    pub primary: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub width: u32,
    pub height: u32,
    pub position: Option<(i32, i32)>,
    pub maximized: bool,
    pub zoom: f64,
    pub dark: Option<bool>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self { width: 1280, height: 820, position: None, maximized: false, zoom: 1.0, dark: None }
    }
}

impl Preferences {
    pub fn load(path: &Path) -> Self {
        // A corrupt preference is disposable; it can never hold startup hostage or grow a
        // desktop read into an unbounded allocation. No gateway configuration lives here.
        let loaded = std::fs::File::open(path).ok().and_then(|file| {
            let mut bytes = Vec::new();
            file.take(4097).read_to_end(&mut bytes).ok()?;
            if bytes.len() > 4096 { return None; }
            serde_json::from_slice::<Self>(&bytes).ok()
        });
        loaded.unwrap_or_default().normalized()
    }

    pub fn save(&self, path: &Path) {
        if let Ok(bytes) = serde_json::to_vec(self) {
            if let Err(err) = std::fs::write(path, bytes) {
                eprintln!("cannot save window preferences: {err}");
            }
        }
    }

    pub fn normalized(mut self) -> Self {
        self.width = self.width.clamp(MIN_WIDTH, 10000);
        self.height = self.height.clamp(MIN_HEIGHT, 10000);
        self.zoom = if self.zoom.is_finite() { self.zoom.clamp(0.8, 1.5) } else { 1.0 };
        self
    }

    /// Called after the native event batch settles, never from the intermediate WM_MOVE.
    pub fn remember(&mut self, size: (u32, u32), position: Option<(i32, i32)>, maximized: bool, minimized: bool) {
        if minimized || size.0 == 0 || size.1 == 0 { return; }
        self.maximized = maximized;
        if !maximized {
            self.width = size.0;
            self.height = size.1;
            if position.is_some() { self.position = position; }
        }
    }

    pub fn fit(&mut self, screens: &[Screen]) {
        let saved = screens.iter().find(|screen| self.visible_on(&[(screen.position.0, screen.position.1, screen.size.0, screen.size.1)]));
        // A connected secondary display owns its saved size and DPI. Only a missing or
        // unreachable position falls back to the primary screen and OS placement.
        if saved.is_none() { self.position = None; }
        if let Some(screen) = saved.or_else(|| screens.iter().find(|screen| screen.primary)).or(screens.first()) {
            self.width = self.width.min((f64::from(screen.size.0) / screen.scale - 64.0).max(f64::from(MIN_WIDTH)) as u32);
            self.height = self.height.min((f64::from(screen.size.1) / screen.scale - 88.0).max(f64::from(MIN_HEIGHT)) as u32);
        }
    }

    pub fn visible_on(&self, monitors: &[(i32, i32, u32, u32)]) -> bool {
        let Some((x, y)) = self.position else { return false; };
        // Keep an actual draggable piece of caption on a surviving monitor, including
        // monitors left of the primary. Intersection of the body alone is not enough.
        let (x, y) = (i64::from(x), i64::from(y));
        monitors.iter().any(|&(mx, my, mw, mh)| {
            let (mx, my) = (i64::from(mx), i64::from(my));
            x + 120 <= mx + i64::from(mw) && x + 600 >= mx + 120
                && y >= my && y + 32 <= my + i64::from(mh)
        })
    }
}

#[derive(Debug, PartialEq)]
pub enum Message { Theme(bool), Title(String), Zoom(i8) }

pub fn message(origin: &str, port: u16, body: &str) -> Option<Message> {
    let origin = reqwest::Url::parse(origin).ok()?;
    if origin.scheme() != "http" || origin.host_str() != Some("127.0.0.1")
        || origin.port_or_known_default() != Some(port) || origin.path() != "/"
        || !origin.username().is_empty() || origin.password().is_some() || body.len() > 512
    { return None; }
    match body {
        "theme:dark" => Some(Message::Theme(true)),
        "theme:light" => Some(Message::Theme(false)),
        "zoom:in" => Some(Message::Zoom(1)),
        "zoom:out" => Some(Message::Zoom(-1)),
        "zoom:reset" => Some(Message::Zoom(0)),
        _ => body.strip_prefix("title:").filter(|title| {
            !title.is_empty() && title.chars().count() <= 128 && !title.chars().any(char::is_control)
        }).map(|title| Message::Title(title.into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_preferences_restore_size_position_theme_and_zoom() {
        let path = std::env::temp_dir().join(format!("swiss-gui-{}-prefs-test.json", std::process::id()));
        let saved = Preferences {
            width: 1120, height: 720, position: Some((-1500, 20)),
            maximized: true, zoom: 1.2, dark: Some(true),
        };
        std::fs::write(&path, serde_json::to_vec(&saved).expect("serialize preferences")).expect("write fixture");
        let loaded = Preferences::load(&path);
        let _ = std::fs::remove_file(&path);
        assert_eq!(loaded, saved);
    }

    #[test]
    fn damaged_or_unbounded_preferences_do_not_break_startup() {
        let prefs = Preferences { width: 0, height: u32::MAX, zoom: f64::NAN, ..Default::default() }.normalized();
        assert_eq!(prefs.width, MIN_WIDTH);
        assert_eq!(prefs.height, 10000);
        assert_eq!(prefs.zoom, 1.0);
        assert_eq!(Preferences::load(Path::new("missing-gui-preferences.json")), Preferences::default());
        let path = std::env::temp_dir().join(format!("swiss-gui-{}-damaged-test.json", std::process::id()));
        for bytes in [b"{broken".to_vec(), format!("{{}}{}", " ".repeat(5000)).into_bytes()] {
            std::fs::write(&path, bytes).expect("write malformed fixture");
            assert_eq!(Preferences::load(&path), Preferences::default());
        }
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_removed_monitor_never_restores_an_unreachable_title_bar() {
        let prefs = Preferences { position: Some((-1500, 20)), ..Default::default() };
        assert!(prefs.visible_on(&[(-1920, 0, 1920, 1080), (0, 0, 1920, 1080)]));
        assert!(!prefs.visible_on(&[(0, 0, 1920, 1080)]));
        assert!(!Preferences { position: Some((100, -300)), ..prefs.clone() }.visible_on(&[(0, 0, 1920, 1080)]));
    }

    #[test]
    fn a_larger_secondary_screen_keeps_its_saved_window_size() {
        let screens = [
            Screen { position: (0, 0), size: (1280, 720), scale: 1.0, primary: true },
            Screen { position: (1280, 0), size: (2560, 1440), scale: 1.0, primary: false },
        ];
        let mut prefs = Preferences { width: 1600, height: 1000, position: Some((1280, 20)), ..Default::default() };
        prefs.fit(&screens);
        assert_eq!((prefs.width, prefs.height), (1600, 1000));
        prefs.fit(&screens[..1]);
        assert_eq!((prefs.width, prefs.height), (1216, MIN_HEIGHT));
        assert_eq!(prefs.position, None, "a removed screen falls back to OS placement");
    }

    #[test]
    fn maximized_and_minimized_frames_never_replace_normal_bounds() {
        let mut prefs = Preferences { width: 1100, height: 700, position: Some((100, 80)), ..Default::default() };
        prefs.remember((1536, 800), Some((-9, -9)), true, false);
        assert_eq!((prefs.width, prefs.height, prefs.position), (1100, 700, Some((100, 80))));
        assert!(prefs.maximized);
        prefs.remember((0, 0), Some((-32000, -32000)), false, true);
        assert!(prefs.maximized);
        assert_eq!(prefs.position, Some((100, 80)));
        prefs.remember((1000, 680), Some((120, 90)), false, false);
        assert_eq!((prefs.width, prefs.height, prefs.position), (1000, 680, Some((120, 90))));
        assert!(!prefs.maximized);
    }

    #[test]
    fn bridge_accepts_only_its_own_panel_and_known_small_messages() {
        assert_eq!(message("http://127.0.0.1:19998/", 19998, "theme:dark"), Some(Message::Theme(true)));
        assert_eq!(message("http://127.0.0.1:19998/", 19998, "title:Data · swiss"), Some(Message::Title("Data · swiss".into())));
        assert_eq!(message("http://127.0.0.1:19998/", 19998, "zoom:in"), Some(Message::Zoom(1)));
        for origin in ["https://evil.test/", "http://127.0.0.1:19999/", "http://localhost:19998/", "http://127.0.0.1:19998/mcp/remote"] {
            assert_eq!(message(origin, 19998, "theme:dark"), None, "{origin}");
        }
        for body in ["theme:auto", "shell:whoami", "title:bad\ncaption"] {
            assert_eq!(message("http://127.0.0.1:19998/", 19998, body), None);
        }
    }
}
