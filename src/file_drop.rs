//! Files dropped on a session window.
//!
//! On macOS they go to the remote clipboard and the viewer pastes them where they were dropped.
//! Elsewhere, or when the server does not allow file copy through the clipboard, they are copied
//! into the shared folder (`\\tsclient\HopDesk`) as [`StagedCopies`]: the remote can read them
//! only while this session lives, so they are removed from the folder when it ends. The folder
//! then holds just what was placed there directly.

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
    // Copies this session makes into the shared folder; dropped with the session window.
    let staged = shared_folder
        .clone()
        .map(|folder| Arc::new(StagedCopies::new(folder)));
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
        status.show(window, format!("공유 폴더로 복사 중: {what} (세션 종료 시 제거)"));
        let (window, status, remote_folder, staged) = (
            Arc::clone(window),
            Arc::clone(&status),
            remote_folder.clone(),
            staged.clone(),
        );
        std::thread::spawn(move || {
            let failed: Vec<String> = paths
                .iter()
                .filter_map(|path| match crate::shared_folder::copy_into(path, &folder) {
                    Ok(destination) => {
                        info!(source = %path.display(), destination = %destination.display(), "Copied dropped file");
                        if let Some(staged) = &staged {
                            staged.remember(destination);
                        }
                        None
                    }
                    Err(error) => {
                        warn!(%error, source = %path.display(), "Failed to copy dropped file");
                        Some(error.to_string())
                    }
                })
                .collect();
            let text = match failed.first() {
                None => format!("✓ {what} → {remote_folder} (세션 종료 시 공유 폴더에서 제거)"),
                Some(error) => format!("복사 실패: {error}"),
            };
            status.show(&window, text);
        });
        None
    })
}

/// Copies a session made into the shared folder, remembered so [`Drop`] can remove them when the
/// session ends: the remote reaches `\\tsclient\HopDesk` only through a live session, and the
/// folder is then left to what was placed there directly. Destinations are unique names, so a
/// removal never takes a file that was already in the folder.
struct StagedCopies {
    folder: PathBuf,
    copied: Mutex<Vec<PathBuf>>,
}

impl StagedCopies {
    fn new(folder: PathBuf) -> Self {
        Self {
            folder,
            copied: Mutex::new(Vec::new()),
        }
    }

    /// Records a copy to remove at session end.
    fn remember(&self, path: PathBuf) {
        self.copied.lock().unwrap_or_else(PoisonError::into_inner).push(path);
    }
}

impl Drop for StagedCopies {
    fn drop(&mut self) {
        let mut copied = self.copied.lock().unwrap_or_else(PoisonError::into_inner);
        for path in copied.drain(..) {
            let removed = match std::fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(&path).is_ok(),
                Ok(_) => std::fs::remove_file(&path).is_ok(),
                Err(_) => false,
            };
            if removed {
                info!(path = %path.display(), folder = %self.folder.display(), "Removed the staged copy; the session ended");
            }
        }
    }
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
    use super::*;

    #[test]
    fn drops_are_described_by_first_name_and_count() {
        use std::path::PathBuf;
        assert_eq!(describe(&[PathBuf::from("/a/report.xlsx")]), "report.xlsx");
        assert_eq!(
            describe(&[PathBuf::from("/a/report.xlsx"), PathBuf::from("/a/b"), PathBuf::from("/c")]),
            "report.xlsx 외 2개"
        );
    }

    #[test]
    fn staged_copies_leave_directly_placed_files_alone() {
        let root = std::env::temp_dir().join(format!("hopdesk-staged-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sent-dir/inner")).expect("create folders");
        std::fs::write(root.join("sent.txt"), b"a").expect("write file");
        std::fs::write(root.join("sent-dir/inner/file.txt"), b"b").expect("write nested file");
        std::fs::write(root.join("manual.txt"), b"keep").expect("write manual file");

        {
            // What `copy_into` hands back for a dropped file and a dropped folder.
            let staged = StagedCopies::new(root.clone());
            staged.remember(root.join("sent.txt"));
            staged.remember(root.join("sent-dir"));
        } // the session ends here

        assert!(!root.join("sent.txt").exists(), "the staged file is removed");
        assert!(!root.join("sent-dir").exists(), "the staged folder is removed");
        assert_eq!(std::fs::read(root.join("manual.txt")).expect("read manual"), b"keep");

        let _ = std::fs::remove_dir_all(&root);
    }
}
