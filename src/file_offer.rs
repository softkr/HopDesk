//! Local files offered to the remote clipboard (MS-RDPECLIP file copy), and the answers to the
//! remote's `FileContentsRequest`s for them.

use std::fs::{self, File};
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail, ensure};
use ironrdp_cliprdr::pdu::{
    ClipboardFileAttributes, FileContentsFlags, FileContentsRequest, FileContentsResponse, FileDescriptor,
};

/// Most entries (files and folders) one drop may offer, so a huge tree cannot exhaust memory.
const MAX_ENTRIES: usize = 10_000;
/// Largest chunk sent for one request, whatever the remote asks for.
const MAX_CHUNK: u32 = 4 * 1024 * 1024;

/// Files and folders offered to the remote, in the order of their descriptors (`lindex`).
#[derive(Debug)]
pub struct FileOffer {
    entries: Vec<Entry>,
}

#[derive(Debug)]
struct Entry {
    path: PathBuf,
    descriptor: FileDescriptor,
}

impl FileOffer {
    /// Lists `paths` and, for folders, everything inside them. Names are relative to the dropped
    /// items (no local paths leave the computer); symbolic links are skipped.
    pub fn collect(paths: &[PathBuf]) -> anyhow::Result<Self> {
        let mut entries = Vec::new();
        for path in paths {
            add(&mut entries, path, None)?;
        }
        ensure!(!entries.is_empty(), "nothing to copy");
        Ok(Self { entries })
    }

    pub fn descriptors(&self) -> Vec<FileDescriptor> {
        self.entries.iter().map(|e| e.descriptor.clone()).collect()
    }

    /// Answers a size or range request for the file at `request.index`.
    pub fn respond(&self, request: &FileContentsRequest) -> FileContentsResponse<'static> {
        self.read(request)
            .map(|data| FileContentsResponse::new_data_response(request.stream_id, data))
            .unwrap_or_else(|_| FileContentsResponse::new_error(request.stream_id))
    }

    fn read(&self, request: &FileContentsRequest) -> anyhow::Result<Vec<u8>> {
        let entry = usize::try_from(request.index)
            .ok()
            .and_then(|i| self.entries.get(i))
            .context("file index out of range")?;
        ensure!(entry.descriptor.attributes != Some(ClipboardFileAttributes::DIRECTORY), "a folder has no contents");
        let mut file = File::open(&entry.path).with_context(|| format!("open {}", entry.path.display()))?;
        if request.flags.contains(FileContentsFlags::SIZE) {
            return Ok(file.metadata()?.len().to_le_bytes().to_vec());
        }
        file.seek(SeekFrom::Start(request.position))?;
        let mut data = Vec::new();
        file.take(u64::from(request.requested_size.min(MAX_CHUNK))).read_to_end(&mut data)?;
        Ok(data)
    }
}

fn add(entries: &mut Vec<Entry>, path: &Path, folder: Option<&str>) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path).with_context(|| format!("read {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if entries.len() >= MAX_ENTRIES {
        bail!("more than {MAX_ENTRIES} files and folders");
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .with_context(|| format!("unsupported file name: {}", path.display()))?
        .to_owned();
    let descriptor = FileDescriptor::new(&name);
    let descriptor = match folder {
        Some(folder) => descriptor.with_relative_path(folder),
        None => descriptor,
    };

    if metadata.is_dir() {
        entries.push(Entry {
            path: path.to_owned(),
            descriptor: descriptor.with_attributes(ClipboardFileAttributes::DIRECTORY),
        });
        // Windows paths inside the copied collection use `\`.
        let inner = match folder {
            Some(folder) => format!("{folder}\\{name}"),
            None => name,
        };
        let mut children: Vec<PathBuf> = fs::read_dir(path)?.map(|e| e.map(|e| e.path())).collect::<Result<_, _>>()?;
        children.sort();
        for child in children {
            add(entries, &child, Some(&inner))?;
        }
    } else {
        entries.push(Entry {
            path: path.to_owned(),
            descriptor: descriptor
                .with_attributes(ClipboardFileAttributes::ARCHIVE)
                .with_file_size(metadata.len()),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(index: i32, flags: FileContentsFlags, position: u64, requested_size: u32) -> FileContentsRequest {
        FileContentsRequest {
            stream_id: 7,
            index,
            flags,
            position,
            requested_size,
            data_id: None,
        }
    }

    fn data(response: FileContentsResponse<'_>) -> Option<Vec<u8>> {
        (!response.is_error()).then(|| response.data().to_vec())
    }

    #[test]
    fn folders_are_listed_with_relative_names_and_files_served_in_ranges() {
        let root = std::env::temp_dir().join(format!("hopdesk-offer-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("docs/sub")).expect("create folders");
        fs::write(root.join("docs/sub/a.txt"), b"hello world").expect("write file");
        fs::write(root.join("note.txt"), b"x").expect("write file");
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc/hosts", root.join("docs/link")).expect("create symlink");

        let offer = FileOffer::collect(&[root.join("docs"), root.join("note.txt")]).expect("collect");
        let listed: Vec<(Option<String>, String)> =
            offer.descriptors().into_iter().map(|d| (d.relative_path, d.name)).collect();
        assert_eq!(
            listed,
            vec![
                (None, "docs".to_owned()),
                (Some("docs".to_owned()), "sub".to_owned()),
                (Some("docs\\sub".to_owned()), "a.txt".to_owned()),
                (None, "note.txt".to_owned()),
            ]
        );

        assert_eq!(data(offer.respond(&request(2, FileContentsFlags::SIZE, 0, 8))), Some(11u64.to_le_bytes().to_vec()));
        assert_eq!(data(offer.respond(&request(2, FileContentsFlags::RANGE, 6, 100))), Some(b"world".to_vec()));
        assert_eq!(data(offer.respond(&request(0, FileContentsFlags::RANGE, 0, 10))), None, "folder");
        assert_eq!(data(offer.respond(&request(9, FileContentsFlags::RANGE, 0, 10))), None, "out of range");
        assert_eq!(data(offer.respond(&request(-1, FileContentsFlags::RANGE, 0, 10))), None, "negative");

        let _ = fs::remove_dir_all(&root);
    }
}
