//! Launcher icon in the macOS menu bar / Windows notification area.
//!
//! Closing the launcher window only hides it, so HopDesk keeps running after an accidental close.
//! The icon's menu opens the window again, checks for an update, or quits.
//!
//! On macOS the icon only stands in for the hidden window: while the window is open, the app menu
//! (with its own "업데이트 확인") is there instead, so the icon is hidden.

use std::cell::Cell;
use std::sync::{Arc, mpsc};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Context as _;
use eframe::egui::{self, ViewportCommand};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

/// Template image: black on transparent, macOS tints it for the menu bar appearance.
#[cfg(target_os = "macos")]
const TRAY_ICON: &[u8] = include_bytes!("../../assets/icon/icon-tray-44.png");

pub struct Tray {
    /// Removes the icon when dropped.
    icon: TrayIcon,
    icon_visible: Cell<bool>,
    /// The launcher window is hidden: set when it is closed, cleared when the menu shows it.
    hidden: Arc<AtomicBool>,
    /// Set by the "종료" menu item, so the following close request is let through.
    quitting: Arc<AtomicBool>,
    /// Keeps the app menu alive.
    #[cfg(target_os = "macos")]
    _app_menu: Menu,
}

impl Tray {
    /// Must run on the main thread once the event loop is running (eframe's app creator).
    /// "업데이트 확인" shows the window and sends a request on `check_update`.
    pub fn new(ctx: &egui::Context, check_update: mpsc::Sender<()>) -> anyhow::Result<Self> {
        let open = MenuItem::new("HopDesk 열기", true, None);
        let check = MenuItem::new("업데이트 확인", true, None);
        let quit = MenuItem::new("종료", true, None);
        let menu = Menu::with_items(&[&open, &check, &PredefinedMenuItem::separator(), &quit]).context("tray menu")?;
        let builder = TrayIconBuilder::new().with_tooltip("HopDesk").with_menu(Box::new(menu));
        #[cfg(target_os = "macos")]
        let builder = builder.with_icon_templated(icon()?);
        #[cfg(windows)]
        let builder = builder.with_icon(icon()?);
        let icon = builder.build().context("tray icon")?;

        // The app menu's own item: one menu item belongs to one menu.
        let app_check = MenuItem::new("업데이트 확인", true, None);
        #[cfg(target_os = "macos")]
        let app_menu = app_menu(&app_check)?;

        let quitting = Arc::new(AtomicBool::new(false));
        let hidden = Arc::new(AtomicBool::new(false));
        let (open_id, quit_id) = (open.id().clone(), quit.id().clone());
        let check_ids = [check.id().clone(), app_check.id().clone()];
        let ctx = ctx.clone();
        let (quit_flag, hidden_flag) = (Arc::clone(&quitting), Arc::clone(&hidden));
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if event.id == open_id {
                show(&ctx, &hidden_flag);
            } else if check_ids.contains(&event.id) {
                // The result appears in the window, so bring it up.
                show(&ctx, &hidden_flag);
                let _ = check_update.send(());
            } else if event.id == quit_id {
                quit_flag.store(true, Ordering::Relaxed);
                ctx.send_viewport_cmd(ViewportCommand::Close);
                ctx.request_repaint();
            }
        }));
        let tray = Self {
            icon,
            icon_visible: Cell::new(true),
            hidden,
            quitting,
            #[cfg(target_os = "macos")]
            _app_menu: app_menu,
        };
        tray.sync_icon();
        Ok(tray)
    }

    /// Call every frame: turns closing the window into hiding it, unless quitting from the menu,
    /// and shows the icon only while it is needed.
    pub fn hide_on_close(&self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting.load(Ordering::Relaxed) {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            set_in_dock(false);
            self.hidden.store(true, Ordering::Relaxed);
        }
        self.sync_icon();
    }

    /// macOS shows the icon only while the window is hidden; Windows always shows it.
    fn sync_icon(&self) {
        let visible = !cfg!(target_os = "macos") || self.hidden.load(Ordering::Relaxed);
        if visible == self.icon_visible.get() {
            return;
        }
        match self.icon.set_visible(visible) {
            Ok(()) => self.icon_visible.set(visible),
            Err(error) => tracing::warn!(error = %error, "Tray icon visibility unchanged"),
        }
    }
}

fn show(ctx: &egui::Context, hidden: &AtomicBool) {
    hidden.store(false, Ordering::Relaxed);
    set_in_dock(true);
    ctx.send_viewport_cmd(ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(ViewportCommand::Focus);
    ctx.request_repaint();
}

/// The macOS app menu (replaces winit's default), with "업데이트 확인" under "HopDesk에 관하여".
#[cfg(target_os = "macos")]
fn app_menu(check_update: &MenuItem) -> anyhow::Result<Menu> {
    use tray_icon::menu::{AboutMetadata, Submenu};

    let about = AboutMetadata {
        name: Some("HopDesk".to_owned()),
        version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        ..AboutMetadata::default()
    };
    let app = Submenu::with_items(
        "HopDesk",
        true,
        &[
            &PredefinedMenuItem::about(Some("HopDesk에 관하여"), Some(about)),
            &PredefinedMenuItem::separator(),
            check_update,
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::services(Some("서비스")),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::hide(Some("HopDesk 가리기")),
            &PredefinedMenuItem::hide_others(Some("기타 가리기")),
            &PredefinedMenuItem::show_all(Some("모두 보기")),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::quit(Some("HopDesk 종료")),
        ],
    )
    .context("app menu")?;
    let menu = Menu::with_items(&[&app]).context("app menu")?;
    menu.init_for_nsapp();
    Ok(menu)
}

#[cfg(target_os = "macos")]
fn icon() -> anyhow::Result<Icon> {
    let data = eframe::icon_data::from_png_bytes(TRAY_ICON).context("decode tray icon")?;
    Icon::from_rgba(data.rgba, data.width, data.height).context("tray icon image")
}

/// The exe icon embedded by build.rs (resource 1), at the system's default icon size.
#[cfg(windows)]
fn icon() -> anyhow::Result<Icon> {
    Icon::from_resource(1, None).context("tray icon resource")
}

/// macOS: a hidden launcher leaves the Dock (only the menu bar icon remains), because the Dock
/// icon could not bring the window back.
#[cfg(target_os = "macos")]
fn set_in_dock(visible: bool) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let policy = if visible {
        NSApplicationActivationPolicy::Regular
    } else {
        NSApplicationActivationPolicy::Accessory
    };
    NSApplication::sharedApplication(mtm).setActivationPolicy(policy);
}

/// Windows: hiding the window already removes its taskbar button.
#[cfg(windows)]
fn set_in_dock(_visible: bool) {}
