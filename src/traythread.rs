//! GTK-backed system tray on its own thread (Linux requires `gtk::init` +
//! a GTK main loop on the owning thread; eframe/winit never init GTK).
//!
//! muda's menu items are `Rc`-based (not `Send`), so the tray handle can
//! never cross to the UI thread. Instead this thread translates
//! `MenuEvent`s into app-level `TrayEvent`s over a channel the eframe
//! loop polls, and keeps the active refresh-rate tick mark in sync via a
//! second (upward) channel. The thread reports success/failure over a
//! oneshot so `spawn` can honestly return Option.

use crate::icons::TrayEvent;
use tray_icon::TrayIconBuilder;
use tray_icon::menu::{CheckMenuItem, Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu};

/// Handle for talking to the tray thread from the UI thread.
pub struct TrayHandle {
    rx_events: crossbeam_channel::Receiver<TrayEvent>,
    tx_rate: crossbeam_channel::Sender<u32>,
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
/// (GTK init failure, StatusNotifier absent, headless CI, …).
pub fn spawn(theme: &str, app_name: &str, initial_refresh_ms: u32) -> Option<TrayHandle> {
    let theme = theme.to_string();
    let app_name = app_name.to_string();
    let (tx_ev, rx_ev) = crossbeam_channel::unbounded();
    let (tx_rate, rx_rate) = crossbeam_channel::unbounded();
    let (tx_ok, rx_ok) = crossbeam_channel::bounded::<()>(1);

    std::thread::Builder::new()
        .name("rstmog-tray".into())
        .spawn(move || {
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
            // channel from inside the GTK main loop.
            glib::timeout_add_local(std::time::Duration::from_millis(120), move || {
                while let Ok(ev) = tray_icon::menu::MenuEvent::receiver().try_recv() {
                    let id = ev.id.as_ref();
                    if id == open_id.as_ref() {
                        let _ = tx_ev.send(TrayEvent::OpenOrShow);
                    } else if id == quit_id.as_ref() {
                        let _ = tx_ev.send(TrayEvent::Quit);
                    } else if let Some((ms, _)) =
                        rates.iter().find(|(_, it)| it.id().as_ref() == id)
                    {
                        let _ = tx_ev.send(TrayEvent::SetRefresh(*ms));
                    }
                }
                while let Ok(ms) = rx_rate.try_recv() {
                    for (rate, item) in rates.iter() {
                        item.set_checked(*rate == ms);
                    }
                }
                glib::ControlFlow::Continue
            });

            gtk::main();
        })
        .ok()?;

    // Did the thread get far enough to build the indicator?
    rx_ok.recv_timeout(std::time::Duration::from_secs(2)).ok()?;
    Some(TrayHandle {
        rx_events: rx_ev,
        tx_rate,
    })
}
