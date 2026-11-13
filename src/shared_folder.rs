//! Local folder shared with every session as a redirected drive (`\\tsclient\HopDesk`).
//!
//! Files dropped on a session window are copied here, so they show up on the remote PC.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use ironrdp_client::rdp::RdpdrBackendFactory;

/// Drive name the remote PC shows (`\\tsclient\HopDesk`).
pub const DRIVE_NAME: &str = "HopDesk";
/// Folder name under the home directory. Not under Documents/Desktop/Downloads, which macOS
/// guards with a privacy prompt.
const FOLDER_NAME: &str = "HopDesk 공유";
/// RDPDR device ID of the shared drive; 0 is reserved.
const DEVICE_ID: u32 = 1;

/// The shared folder, created when missing. The location set in the profile store wins over
/// the default under the home directory.
pub fn path() -> anyhow::Result<PathBuf> {
    let configured = crate::profile::ProfileStore::load()
        .ok()
        .and_then(|store| store.shared_folder().map(Path::to_path_buf));
    match configured {
        Some(dir) => {
            std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
            Ok(dir)
        }
        None => default_path(),
    }
}

/// The default location: directly under the home directory, not under Documents/Desktop/
/// Downloads, which macOS guards with a privacy prompt.
pub fn default_path() -> anyhow::Result<PathBuf> {
    let home = directories::UserDirs::new().context("no home directory")?;
    let dir = home.home_dir().join(FOLDER_NAME);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    Ok(dir)
}

/// Where files received from the remote clipboard wait to be pasted. Deliberately outside the
/// shared folder, so that folder holds only what was put there directly: the latest received
/// copy stays pasteable here until the next one replaces it.
pub fn received_dir() -> anyhow::Result<PathBuf> {
    let dir = crate::config_dir()?.join("received");
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    Ok(dir)
}

/// Copies a file or folder into `folder` under a name not yet taken (`name (1).ext`, …) and
/// returns the destination. Symbolic links inside folders are skipped.
pub fn copy_into(source: &Path, folder: &Path) -> std::io::Result<PathBuf> {
    let name = source
        .file_name()
        .ok_or_else(|| std::io::Error::other("the dropped item has no name"))?;
    let destination = unique_destination(folder, Path::new(name));
    copy_recursive(source, &destination)?;
    Ok(destination)
}

pub(crate) fn unique_destination(folder: &Path, name: &Path) -> PathBuf {
    let candidate = folder.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let stem = name.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let extension = name.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (1..)
        .map(|n| folder.join(format!("{stem} ({n}){extension}")))
        .find(|path| !path.exists())
        .expect("an unused name exists")
}

fn copy_recursive(source: &Path, destination: &Path) -> std::io::Result<()> {
    let metadata = std::fs::symlink_metadata(source)?;
    if metadata.is_dir() {
        std::fs::create_dir(destination)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            if entry.file_type()?.is_symlink() {
                continue;
            }
            copy_recursive(&entry.path(), &destination.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(source, destination).map(|_| ())
    }
}

/// Drive redirection backend serving only the shared folder.
pub fn backend_factory(dir: &Path) -> anyhow::Result<Box<dyn RdpdrBackendFactory + Send>> {
    #[cfg(windows)]
    {
        use ironrdp_rdpdr_native::{RedirectedDrive, WindowsRdpdrBackendFactory};
        let drive = RedirectedDrive::new(DEVICE_ID, DRIVE_NAME, dir, false)
            .map_err(|e| anyhow::anyhow!("share {}: {e:?}", dir.display()))?;
        Ok(Box::new(WindowsRdpdrBackendFactory::new(drive)))
    }
    #[cfg(unix)]
    {
        use ironrdp_rdpdr_native::backend::NixRdpdrBackendFactory;
        let base = dir.to_str().context("shared folder path is not UTF-8")?.to_owned();
        Ok(Box::new(NixRdpdrBackendFactory::new(base).with_drive(DEVICE_ID, DRIVE_NAME.to_owned())))
    }
}

#[cfg(test)]
mod tests {
    use super::copy_into;

    #[test]
    fn copies_get_unused_names_and_folders_copy_without_symlinks() {
        let root = std::env::temp_dir().join(format!("hopdesk-drop-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (source, shared) = (root.join("source"), root.join("shared"));
        std::fs::create_dir_all(source.join("dir/sub")).expect("create source");
        std::fs::create_dir_all(&shared).expect("create shared");
        std::fs::write(source.join("report.txt"), b"a").expect("write file");
        std::fs::write(source.join("dir/sub/inner.txt"), b"b").expect("write nested file");
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc", source.join("dir/link")).expect("create symlink");

        assert_eq!(copy_into(&source.join("report.txt"), &shared).expect("copy"), shared.join("report.txt"));
        assert_eq!(copy_into(&source.join("report.txt"), &shared).expect("copy"), shared.join("report (1).txt"));
        assert_eq!(copy_into(&source.join("dir"), &shared).expect("copy"), shared.join("dir"));
        assert_eq!(std::fs::read(shared.join("dir/sub/inner.txt")).expect("read"), b"b");
        assert!(!shared.join("dir/link").exists());

        let _ = std::fs::remove_dir_all(&root);
    }
}
