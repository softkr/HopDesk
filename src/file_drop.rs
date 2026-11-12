//! Files dropped on a session window.
//!
//! On macOS they go to the remote clipboard and the viewer pastes them where they were dropped.
//! Elsewhere, or when the server does not allow file copy through the clipboard, they are copied
//! into the shared folder (`\\tsclient\HopDesk`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use ironrdp_viewer::app::DropHandler;
#[cfg(target_os = "macos")]
use ironrdp_viewer::app::DropPaste;
use tracing::{info, warn};
use winit::window::Window;

/// How long the window title shows the outcome of a drop.
const STATUS_DURATION: Duration = Duration::from_secs(6);

/// Builds the viewer's drop handler. `remote_folder` is the shared folder as the session sees it.
pub fn handler(
    shared_folder: Option<PathBuf>,
    remote_folder: String,
    #[cfg(target_os = "macos")] clipboard: Option<crate::clipboard::ClipboardFiles>,
) -> DropHandler {
    let status = Arc::new(TitleStatus::default());
    Box::new(move |window, paths| {
        let what = describe(&paths);

        #[cfg(target_os = "macos")]
        if let Some(clipboard) = &clipboard {
            match clipboard.offer(&paths) {
                Ok(ready) => {
                    let Some(position) = cursor_in_window(window) else {
                        status.show(window, format!("원격에서 Ctrl+V로 붙여넣으세요: {what}"));
                        return None;
                    };
                    status.show(window, format!("원격으로 보내는 중: {what}"));
                    return Some(DropPaste { position, ready });
                }
                Err(error) => info!(reason = %format!("{error:#}"), "Copying dropped files into the shared folder instead"),
            }
        }

        let Some(folder) = shared_folder.clone() else {
            status.show(window, format!("파일을 보낼 수 없습니다: {what}"));
            return None;
        };
        status.show(window, format!("공유 폴더로 복사 중: {what}"));
        let (window, status, remote_folder) = (Arc::clone(window), Arc::clone(&status), remote_folder.clone());
        std::thread::spawn(move || {
            let failed: Vec<String> = paths
                .iter()
                .filter_map(|path| match crate::shared_folder::copy_into(path, &folder) {
                    Ok(destination) => {
                        info!(source = %path.display(), destination = %destination.display(), "Copied dropped file");
                        None
                    }
                    Err(error) => {
                        warn!(%error, source = %path.display(), "Failed to copy dropped file");
                        Some(error.to_string())
                    }
                })
                .collect();
            let text = match failed.first() {
                None => format!("✓ {what} → {remote_folder}"),
                Some(error) => format!("복사 실패: {error}"),
            };
            status.show(&window, text);
        });
        None
    })
}

/// "a.txt" or "a.txt 외 2개".
fn describe(paths: &[PathBuf]) -> String {
    let first = paths
        .first()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match paths.len() {
        0 | 1 => first,
        n => format!("{first} 외 {}개", n - 1),
    }
}

/// Shows a message after the window title for a while, then restores the title.
#[derive(Default)]
struct TitleStatus {
    /// Title before the first message still on screen.
    original: Mutex<Option<String>>,
    generation: AtomicU64,
}

impl TitleStatus {
    fn show(self: &Arc<Self>, window: &Arc<Window>, text: String) {
        let original = self
            .original
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert_with(|| window.title())
            .clone();
        window.set_title(&format!("{original} — {text}"));
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let (this, window) = (Arc::clone(self), Arc::clone(window));
        std::thread::spawn(move || {
            std::thread::sleep(STATUS_DURATION);
            if this.generation.load(Ordering::Acquire) == generation
                && let Some(original) = this.original.lock().unwrap_or_else(PoisonError::into_inner).take()
            {
                window.set_title(&original);
            }
        });
    }
}

/// Mouse position inside the window's client area (physical pixels): winit reports drops without
/// one. Runs on the main thread, where the viewer calls the drop handler.
#[cfg(target_os = "macos")]
fn cursor_in_window(window: &Window) -> Option<winit::dpi::PhysicalPosition<f64>> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSEvent, NSScreen};

    let mtm = MainThreadMarker::new()?;
    // Global points with the origin at the bottom left of the primary screen; winit's window
    // positions use the top left of that screen.
    let mouse = NSEvent::mouseLocation();
    let primary_height = NSScreen::screens(mtm).firstObject()?.frame().size.height;
    let scale = window.scale_factor();
    let inner = window.inner_position().ok()?;
    let x = mouse.x * scale - f64::from(inner.x);
    let y = (primary_height - mouse.y) * scale - f64::from(inner.y);
    let size = window.inner_size();
    (x >= 0.0 && y >= 0.0 && x < f64::from(size.width) && y < f64::from(size.height))
        .then(|| winit::dpi::PhysicalPosition::new(x, y))
}

#[cfg(test)]
mod tests {
    use super::describe;
    use std::path::PathBuf;

    #[test]
    fn drops_are_described_by_first_name_and_count() {
        assert_eq!(describe(&[PathBuf::from("/a/report.xlsx")]), "report.xlsx");
        assert_eq!(
            describe(&[PathBuf::from("/a/report.xlsx"), PathBuf::from("/a/b"), PathBuf::from("/c")]),
            "report.xlsx 외 2개"
        );
    }
}
