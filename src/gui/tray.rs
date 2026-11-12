//! Launcher icon in the macOS menu bar / Windows notification area.
//!
//! Closing the launcher window only hides it, so HopDesk keeps running after an accidental close.
//! The icon's menu opens the window again or quits.

use std::sync::Arc;
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
    _icon: TrayIcon,
    /// Set by the "종료" menu item, so the following close request is let through.
    quitting: Arc<AtomicBool>,
}

impl Tray {
    /// Must run on the main thread once the event loop is running (eframe's app creator).
    pub fn new(ctx: &egui::Context) -> anyhow::Result<Self> {
        let open = MenuItem::new("HopDesk 열기", true, None);
        let quit = MenuItem::new("종료", true, None);
        let menu = Menu::with_items(&[&open, &PredefinedMenuItem::separator(), &quit]).context("tray menu")?;
        let builder = TrayIconBuilder::new().with_tooltip("HopDesk").with_menu(Box::new(menu));
        #[cfg(target_os = "macos")]
        let builder = builder.with_icon_templated(icon()?);
        #[cfg(windows)]
        let builder = builder.with_icon(icon()?);
        let icon = builder.build().context("tray icon")?;

        let quitting = Arc::new(AtomicBool::new(false));
        let (open_id, quit_id) = (open.id().clone(), quit.id().clone());
        let ctx = ctx.clone();
        let quit_flag = Arc::clone(&quitting);
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if event.id == open_id {
                show(&ctx);
            } else if event.id == quit_id {
                quit_flag.store(true, Ordering::Relaxed);
                ctx.send_viewport_cmd(ViewportCommand::Close);
                ctx.request_repaint();
            }
        }));
        Ok(Self { _icon: icon, quitting })
    }

    /// Call every frame: turns closing the window into hiding it, unless quitting from the menu.
    pub fn hide_on_close(&self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting.load(Ordering::Relaxed) {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            set_in_dock(false);
        }
    }
}

fn show(ctx: &egui::Context) {
    set_in_dock(true);
    ctx.send_viewport_cmd(ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(ViewportCommand::Focus);
    ctx.request_repaint();
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
