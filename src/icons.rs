//! Application icons: window icon (theme-aware), taskbar/desktop entry,
//! and the system-tray icon with its menu.
//!
//! Icon artwork comes from assets/icons/{dark,light} (glossy "T" package:
//! ice-blue serif T on near-black / gray). Selection follows the user's
//! theme setting when set, otherwise the system dark/light preference.
//!
//! On Linux the tray is implemented with GTK/libappindicator, so it is
//! built on a dedicated thread that owns `gtk::init` + the GTK main loop
//! (eframe/winit never initialize GTK). Menu events flow through muda's
//! global `MenuEvent::receiver()`, which the eframe loop polls.

use eframe::egui;

/// Build `IconData` from an embedded PNG.
fn icon_data(bytes: &[u8]) -> egui::IconData {
    let img = image::load_from_memory(bytes)
        .expect("embedded icon png must decode")
        .to_rgba8();
    let (width, height) = img.dimensions();
    egui::IconData {
        rgba: img.into_raw(),
        width,
        height,
    }
}

/// True when the effective theme is a dark one: the explicit theme name
/// wins ("light" is the only light palette); otherwise the OS preference.
fn want_dark(theme: &str) -> bool {
    if !theme.is_empty() && theme != "dark" {
        theme != "light"
    } else {
        matches!(dark_light::detect(), Ok(dark_light::Mode::Dark))
    }
}

/// Window/taskbar icon (winit shows it in the titlebar and the DE taskbar).
/// 64 px is plenty for a titlebar and scales cleanly for taskbar previews.
pub fn window_icon(theme: &str) -> egui::IconData {
    if want_dark(theme) {
        icon_data(include_bytes!("../assets/icons/dark/icon-64.png"))
    } else {
        icon_data(include_bytes!("../assets/icons/light/icon-64.png"))
    }
}

/// Tray icons need a small square; use 32 px.
pub fn tray_icon(theme: &str) -> tray_icon::Icon {
    let bytes: &[u8] = if want_dark(theme) {
        include_bytes!("../assets/icons/dark/icon-32.png")
    } else {
        include_bytes!("../assets/icons/light/icon-32.png")
    };
    let img = image::load_from_memory(bytes)
        .expect("tray icon png")
        .to_rgba8();
    let (w, h) = img.dimensions();
    tray_icon::Icon::from_rgba(img.into_raw(), w, h).expect("valid tray icon")
}

/// Events the tray can raise for the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    OpenOrShow,
    Quit,
    SetRefresh(u32),
}
