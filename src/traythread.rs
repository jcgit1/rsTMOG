//! System tray on its own thread.
//!
//! Linux requires `gtk::init` + a GTK main loop on the owning thread
//! (eframe/winit never init GTK), so the GTK path below runs there and
//! translates muda's `Rc`-based `MenuEvent`s into app-level `TrayEvent`s
//! over a channel the eframe loop polls; a second (upward) channel keeps
//! the active refresh-rate tick in sync. The thread reports success over a
//! oneshot so `spawn` can honestly return Option.
//!
//! On Windows the same tray works without GTK (tray-icon uses Win32
//! Shell_NotifyIcon there); the only difference is the event pump — no
//! GLib main loop, so this thread polls and sleeps until the app drops the
//! handle (a dedicated `_alive` channel's disconnect is the exit signal).

use crate::icons::TrayEvent;
use tray_icon::TrayIconBuilder;
use tray_icon::menu::{CheckMenuItem, Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu};

/// Handle for talking to the tray thread from the UI thread. Dropping it
/// ends the tray thread (via `_alive`).
pub struct TrayHandle {
    rx_events: crossbeam_channel::Receiver<TrayEvent>,
    tx_rate: crossbeam_channel::Sender<u32>,
    _alive: crossbeam_channel::Sender<()>,
}

impl TrayHandle {
    /// Poll a pending menu event (non-blocking).
    pub fn try_event(&self) -> Option<TrayEvent> {
        self.rx_events.try_recv().ok()
    }

    /// Update the tray's active refresh-rate tick mark.
    pub fn set_refresh(&self, ms: u32) {
        let _ = self.tx_rate.send(ms);
    }
}

/// Spawn the tray thread; returns None if the session can't host a tray
/// (GTK init failure on Linux, Shell_NotifyIcon refusal, headless CI, …).
pub fn spawn(theme: &str, app_name: &str, initial_refresh_ms: u32) -> Option<TrayHandle> {
    let theme = theme.to_string();
    let app_name = app_name.to_string();
    let (tx_ev, rx_ev) = crossbeam_channel::unbounded();
    let (tx_rate, rx_rate) = crossbeam_channel::unbounded();
    let (tx_alive, rx_alive) = crossbeam_channel::bounded::<()>(0);
    let (tx_ok, rx_ok) = crossbeam_channel::bounded::<()>(1);

    std::thread::Builder::new()
        .name("rstmog-tray".into())
        .spawn(move || {
            // Used by the non-GTK pump loop below; Linux exits via gtk::main.
            let _ = &rx_alive;
            #[cfg(target_os = "linux")]
            if gtk::init().is_err() {
                return; // tx_ok dropped -> spawn() returns None
            }
            let open_id = MenuId::new("rstmog-open");
            let quit_id = MenuId::new("rstmog-quit");
            let menu = Menu::new();
            let _ = menu.append(&MenuItem::with_id(
                open_id.clone(),
                format!("Open {app_name}"),
                true,
                None,
            ));
            let _ = menu.append(&PredefinedMenuItem::separator());
            let sub = Submenu::new("Refresh rate", true);
            let mk = |ms: u32| {
                CheckMenuItem::with_id(
                    format!("rstmog-refresh-{ms}"),
                    format!("{ms} ms"),
                    true,
                    ms == initial_refresh_ms,
                    None,
                )
            };
            let rates: Vec<(u32, CheckMenuItem)> =
                [(500u32, mk(500)), (1000, mk(1000)), (2000, mk(2000))].to_vec();
            for (_, item) in rates.iter() {
                let _ = sub.append(item);
            }
            let _ = menu.append(&sub);
            let _ = menu.append(&PredefinedMenuItem::separator());
            let _ = menu.append(&MenuItem::with_id(quit_id.clone(), "Quit", true, None));

            let _tray = match TrayIconBuilder::new()
                .with_icon(crate::icons::tray_icon(&theme))
                .with_tooltip(format!("{app_name} — Task Manager OG"))
                .with_menu(Box::new(menu))
                .build()
            {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("tray: build failed: {e}");
                    return;
                }
            };
            let _ = tx_ok.send(());

            // Drain muda's global menu-event receiver and the rate-sync
            // channel: inside the GTK main loop on Linux; in a poll loop
            // elsewhere (Windows) that ends when the app drops the handle.
            #[cfg(target_os = "linux")]
            {
                glib::timeout_add_local(std::time::Duration::from_millis(120), move || {
                    pump(&tx_ev, &rx_rate, &open_id, &quit_id, &rates);
                    glib::ControlFlow::Continue
                });
                gtk::main();
            }
            #[cfg(not(target_os = "linux"))]
            while rx_alive
                .recv_timeout(std::time::Duration::from_millis(120))
                .is_err()
            {
                pump(&tx_ev, &rx_rate, &open_id, &quit_id, &rates);
            }
        })
        .ok()?;

    // Did the thread get far enough to build the indicator?
    rx_ok.recv_timeout(std::time::Duration::from_secs(2)).ok()?;
    Some(TrayHandle {
        rx_events: rx_ev,
        tx_rate,
        _alive: tx_alive,
    })
}

/// One event-pump pass.
fn pump(
    tx_ev: &crossbeam_channel::Sender<TrayEvent>,
    rx_rate: &crossbeam_channel::Receiver<u32>,
    open_id: &MenuId,
    quit_id: &MenuId,
    rates: &[(u32, CheckMenuItem)],
) {
    while let Ok(ev) = tray_icon::menu::MenuEvent::receiver().try_recv() {
        let id = ev.id.as_ref();
        if id == open_id.as_ref() {
            let _ = tx_ev.send(TrayEvent::OpenOrShow);
        } else if id == quit_id.as_ref() {
            let _ = tx_ev.send(TrayEvent::Quit);
        } else if let Some((ms, _)) = rates.iter().find(|(_, it)| it.id().as_ref() == id) {
            let _ = tx_ev.send(TrayEvent::SetRefresh(*ms));
        }
    }
    while let Ok(ms) = rx_rate.try_recv() {
        for (rate, item) in rates.iter() {
            item.set_checked(*rate == ms);
        }
    }
}
