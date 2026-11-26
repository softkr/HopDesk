//! macOS Dock tile of a session process: its own icon and the profile name's first character as a
//! badge, so sessions are told apart from the launcher and from each other.

use objc2::{AllocAnyThread as _, MainThreadMarker};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSImage};
use objc2_foundation::{NSData, NSString};

const SESSION_ICON: &[u8] = include_bytes!("../assets/icon/icon-session-256.png");

/// Sets the session icon and badge. Must run on the main thread after the app has launched.
pub fn mark_session(profile_name: &str) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let data = NSData::with_bytes(SESSION_ICON);
    if let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) {
        // SAFETY: called on the main thread with a valid image.
        unsafe { app.setApplicationIconImage(Some(&image)) };
    }
    app.dockTile().setBadgeLabel(Some(&NSString::from_str(&badge(profile_name))));
}

/// How long a session lives on after leaving the Dock. The Dock drops the tile only if it handles
/// the change before the process dies; a session that exits a few milliseconds later stays behind
/// as a "recent app" tile.
const DOCK_LEAVE_GRACE: std::time::Duration = std::time::Duration::from_millis(500);

/// Removes the session's Dock tile as the session ends, so the Dock does not keep the session as
/// a second "recent app" next to the launcher. Blocks for `DOCK_LEAVE_GRACE`, so call it last.
pub fn leave_dock() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    NSApplication::sharedApplication(mtm).setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
    std::thread::sleep(DOCK_LEAVE_GRACE);
}

/// Quits the session when the launcher stops it (SIGTERM), leaving the Dock first like a session
/// whose window was closed.
pub fn quit_on_terminate() -> anyhow::Result<()> {
    use anyhow::Context as _;
    use tokio::signal::unix::{SignalKind, signal};

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .context("signal runtime")?;
    // Registered before returning, so a stop right after launch is not missed.
    let mut terminate = runtime.block_on(async { signal(SignalKind::terminate()) }).context("SIGTERM handler")?;
    std::thread::spawn(move || {
        runtime.block_on(terminate.recv());
        // AppKit only on the main thread, which runs the session window's event loop.
        dispatch2::DispatchQueue::main().exec_async(|| {
            leave_dock();
            std::process::exit(0);
        });
    });
    Ok(())
}

/// The first character of the profile name; a longer badge covers the icon.
fn badge(name: &str) -> String {
    name.trim().chars().take(1).collect()
}

#[cfg(test)]
mod tests {
    use super::badge;

    #[test]
    fn badge_shows_the_first_character() {
        assert_eq!(badge("금융존 개발"), "금");
        assert_eq!(badge("  production-db-01"), "p");
        assert_eq!(badge(""), "");
    }
}
