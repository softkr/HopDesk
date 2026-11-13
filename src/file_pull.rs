//! Files pulled from the remote clipboard to this computer: the paste side of MS-RDPECLIP
//! file copy.
//!
//! A copy on the remote PC announces a `FileGroupDescriptorW` file list. The clipboard
//! backend asks for the list ([`plan`] turns it into local destinations) and then walks it
//! one entry at a time with [`Pull`], which streams every file to disk in
//! [`MAX_CHUNK`]-byte ranges instead of holding it in memory.

use std::collections::HashMap;
use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use ironrdp_cliprdr::pdu::{
    ClipboardFileAttributes, FileContentsFlags, FileContentsRequest, FileContentsResponse,
    FileDescriptor,
};
use tracing::{info, warn};

/// Largest byte range asked of the remote at once. Big enough that a multi-gigabyte file
/// needs few round trips, small enough that one response is cheap to buffer.
pub const MAX_CHUNK: u32 = 1024 * 1024;
/// Total bytes one remote copy may pull. File sizes come from the remote and must not be
/// trusted, so they are enforced here: a hostile peer that names a huge file cannot fill
/// the local disk through the clipboard.
pub const MAX_TOTAL_BYTES: u64 = 100 * 1024 * 1024 * 1024;

/// What the clipboard backend should do next.
#[derive(Debug)]
pub enum PullEvent {
    /// Send this request to the remote.
    Request(FileContentsRequest),
    /// The pull ended; `roots` are the local paths of the received top-level items and
    /// `failures` describes every entry that could not be received.
    Finished {
        roots: Vec<PathBuf>,
        failures: Vec<String>,
    },
}

/// One entry of a remote copy with its local destination.
#[derive(Debug)]
struct Entry {
    /// `lindex` of the descriptor in the remote file list.
    index: i32,
    /// Where the entry lands locally.
    path: PathBuf,
    kind: EntryKind,
}

#[derive(Debug)]
enum EntryKind {
    Directory,
    File {
        /// Bytes, when the descriptor stated them; asked of the remote otherwise.
        size: Option<u64>,
    },
}

/// Local destinations for the descriptors of one remote copy, in list order.
#[derive(Debug)]
pub struct PullPlan {
    entries: Vec<Entry>,
    /// Destinations of the top-level items, for the pasteboard.
    pub roots: Vec<PathBuf>,
}

/// Maps the descriptors of one remote copy onto `folder`.
///
/// Top-level items take names not yet used there (`name (1).ext`, …), and everything below
/// them keeps the structure the descriptors describe. Names are single path components
/// already (the engine sanitizes the file list before handing it over), which is asserted
/// rather than assumed: a name that still carries separators or traversal components ends
/// the whole plan rather than writing outside `folder`.
pub fn plan(files: &[FileDescriptor], folder: &Path) -> anyhow::Result<PullPlan> {
    let mut entries = Vec::with_capacity(files.len());
    let mut roots = Vec::new();
    // Top-level item name -> destination already assigned for it.
    let mut top: HashMap<&str, PathBuf> = HashMap::new();
    let mut total: u64 = 0;

    for (index, file) in files.iter().enumerate() {
        let index = i32::try_from(index).context("too many files in the remote copy")?;
        check_component(&file.name)?;
        let path = match file
            .relative_path
            .as_deref()
            .filter(|relative| !relative.is_empty())
        {
            None => {
                let destination =
                    crate::shared_folder::unique_destination(folder, Path::new(&file.name));
                top.insert(&file.name, destination.clone());
                roots.push(destination.clone());
                destination
            }
            Some(relative) => {
                // Entries below the top level land inside the destination assigned to their
                // root item, mirroring the structure the descriptors describe.
                let mut components = relative.split('\\');
                let root = components
                    .next()
                    .context("empty relative path in the remote copy")?;
                let mut path = top
                    .get(root)
                    .with_context(|| {
                        format!("remote copy lists `{}` before its root item", file.name)
                    })?
                    .clone();
                for component in components {
                    check_component(component)?;
                    path.push(component);
                }
                path.push(&file.name);
                path
            }
        };
        let kind = if file
            .attributes
            .is_some_and(|attributes| attributes.contains(ClipboardFileAttributes::DIRECTORY))
        {
            EntryKind::Directory
        } else {
            let size = file.file_size;
            if let Some(size) = size {
                total = total.saturating_add(size);
            }
            EntryKind::File { size }
        };
        entries.push(Entry { index, path, kind });
    }

    if MAX_TOTAL_BYTES < total {
        bail!(
            "the remote copy states {total} bytes in total, over the {MAX_TOTAL_BYTES} received at most"
        );
    }
    Ok(PullPlan { entries, roots })
}

/// A name the remote file list may use as one path component: the engine sanitizes the file
/// list before handing it over, so this asserts that invariant instead of trusting it.
fn check_component(component: &str) -> anyhow::Result<()> {
    if component.is_empty()
        || component == "."
        || component == ".."
        || component.contains(['/', '\\'])
        || component.contains('\0')
    {
        bail!("`{component}` is not a single safe path component");
    }
    Ok(())
}

/// Walks a [`PullPlan`], streaming every file to disk and telling the backend which request
/// to send next.
///
/// One file is fetched at a time: [`PullEvent::Request`] names the outstanding
/// `FileContentsRequest`, and [`Pull::on_response`] for its stream id produces the next
/// event. A response for any other stream id is ignored, so a late answer to an abandoned
/// file cannot corrupt the one in progress.
pub struct Pull {
    entries: Vec<Entry>,
    roots: Vec<PathBuf>,
    /// Next entry of `entries` to start.
    next: usize,
    /// The file being received.
    current: Option<Current>,
    /// Stream id of the next file; each file keeps one id for its size and range requests.
    next_stream_id: u32,
    /// `clipDataId` of the lock the file list arrived under, sent with every request so a
    /// transfer stays bound to the clipboard snapshot it was listed against.
    clip_data_id: Option<u32>,
    /// Whether huge-file positions may be requested; the engine rejects requests beyond
    /// 2 GiB otherwise, which would end the session.
    huge_files: bool,
    written_total: u64,
    failures: Vec<String>,
}

impl fmt::Debug for Pull {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `Current` holds a file handle, which says nothing useful here.
        f.debug_struct("Pull")
            .field("entries", &self.entries.len())
            .field("roots", &self.roots)
            .field("next", &self.next)
            .field(
                "current",
                &self
                    .current
                    .as_ref()
                    .map(|current| current.path.as_os_str()),
            )
            .field("clip_data_id", &self.clip_data_id)
            .field("huge_files", &self.huge_files)
            .field("written_total", &self.written_total)
            .field("failures", &self.failures)
            .finish()
    }
}

/// The file being received.
struct Current {
    index: i32,
    path: PathBuf,
    /// The write target; dropped (which closes it) when the file completes or fails.
    file: std::fs::File,
    /// Total bytes, from the descriptor or the remote's answer to the size request.
    size: Option<u64>,
    position: u64,
    stream_id: u32,
    awaiting: Awaiting,
}

/// What the outstanding request asked for, to validate the response against.
#[derive(Debug)]
enum Awaiting {
    Size,
    Range { requested: u32 },
}

/// What a response means for the file in progress, decided while it is only read.
enum Outcome {
    /// The file cannot be received; the reason is reported in the failures.
    Failed(String),
    /// The answer to a size request.
    Size(u64),
    /// Data of a range; the path is taken for the failure messages.
    Range { path: PathBuf, requested: u32 },
}

impl Pull {
    pub fn new(plan: PullPlan, clip_data_id: Option<u32>, huge_files: bool) -> Self {
        Self {
            entries: plan.entries,
            roots: plan.roots,
            next: 0,
            current: None,
            next_stream_id: 1,
            clip_data_id,
            huge_files,
            written_total: 0,
            failures: Vec::new(),
        }
    }

    /// The `clipDataId` the file list arrived under, which every request of this pull
    /// carries so it stays bound to that clipboard snapshot.
    pub fn clip_data_id(&self) -> Option<u32> {
        self.clip_data_id
    }

    /// Begins receiving: creates directories, opens the first file and returns its first
    /// request, or reports an empty copy as finished.
    pub fn start(&mut self) -> PullEvent {
        self.step()
    }

    /// Feeds one file-contents response to the pull. Returns `None` when the response does
    /// not belong to the file in progress (or none is), and the next event otherwise.
    pub fn on_response(&mut self, response: &FileContentsResponse<'_>) -> Option<PullEvent> {
        let outcome = {
            let current = self.current.as_ref()?;
            if current.stream_id != response.stream_id() {
                return None;
            }
            if response.is_error() {
                Outcome::Failed(format!(
                    "the remote refused to send `{}`",
                    current.path.display()
                ))
            } else {
                match &current.awaiting {
                    Awaiting::Size => match response.data_as_size() {
                        Ok(size) if MAX_TOTAL_BYTES < self.written_total.saturating_add(size) => {
                            Outcome::Failed(format!(
                                "`{}` states {size} bytes, over the {MAX_TOTAL_BYTES} received at most",
                                current.path.display()
                            ))
                        }
                        Ok(size) => Outcome::Size(size),
                        Err(_) => Outcome::Failed(format!(
                            "the remote sent a malformed size for `{}`",
                            current.path.display()
                        )),
                    },
                    Awaiting::Range { requested } => Outcome::Range {
                        path: current.path.clone(),
                        requested: *requested,
                    },
                }
            }
        };

        match outcome {
            Outcome::Failed(reason) => {
                self.fail_current(reason);
                return Some(self.step());
            }
            Outcome::Size(size) => {
                let current = self
                    .current
                    .as_mut()
                    .expect("a size response is only classified while its file is in progress");
                current.size = Some(size);
                if size == 0 {
                    self.complete_current();
                    return Some(self.step());
                }
            }
            Outcome::Range { path, requested } => {
                let data = response.data();
                // A compliant peer answers a range with data up to `requested` bytes and
                // never with an empty range before the file is complete
                // (MS-RDPECLIP defines no "not ready yet" signal), so both shapes mean
                // the transfer cannot continue.
                if data.is_empty() || usize::try_from(requested).unwrap_or(0) < data.len() {
                    self.fail_current(format!(
                        "the remote sent a malformed range for `{}`",
                        path.display()
                    ));
                    return Some(self.step());
                }
                let take = {
                    let current = self.current.as_mut().expect(
                        "a range response is only classified while its file is in progress",
                    );
                    // Clamp against the size in case the peer overruns the range it was
                    // asked for; the clamped bytes are the only ones known to belong to
                    // the file.
                    let remaining = current
                        .size
                        .unwrap_or(u64::MAX)
                        .saturating_sub(current.position);
                    data.len().min(usize::try_from(remaining).unwrap_or(0))
                };
                let written = self
                    .current
                    .as_mut()
                    .expect("the file is still in progress")
                    .file
                    .write_all(&data[..take]);
                if let Err(error) = written {
                    self.fail_current(format!("writing `{}` failed: {error}", path.display()));
                    return Some(self.step());
                }
                let current = self
                    .current
                    .as_mut()
                    .expect("the file is still in progress");
                current.position += u64::try_from(take).unwrap_or(0);
                self.written_total += u64::try_from(take).unwrap_or(0);
                if current.size.is_some_and(|size| size <= current.position) {
                    self.complete_current();
                    return Some(self.step());
                }
            }
        }

        Some(self.step())
    }

    /// Moves on from the current entry (finished or failed) to the next one, creating
    /// directories and opening files until a request is due or the plan is exhausted.
    fn step(&mut self) -> PullEvent {
        while self.current.is_none() {
            let Some(entry) = self.entries.get(self.next) else {
                let roots = std::mem::take(&mut self.roots);
                let failures = std::mem::take(&mut self.failures);
                return PullEvent::Finished { roots, failures };
            };
            self.next += 1;
            match entry.kind {
                EntryKind::Directory => {
                    if let Err(error) = std::fs::create_dir_all(&entry.path) {
                        self.failures.push(format!(
                            "creating `{}` failed: {error}",
                            entry.path.display()
                        ));
                    }
                }
                EntryKind::File { size } => {
                    let opened = std::fs::File::options()
                        .write(true)
                        .create(true)
                        .truncate(true)
                        .open(&entry.path)
                        .with_context(|| format!("create `{}`", entry.path.display()));
                    match opened {
                        Ok(file) => {
                            let stream_id = self.next_stream_id;
                            self.next_stream_id += 1;
                            self.current = Some(Current {
                                index: entry.index,
                                path: entry.path.clone(),
                                file,
                                size,
                                position: 0,
                                stream_id,
                                awaiting: Awaiting::Size,
                            });
                        }
                        Err(error) => {
                            self.failures.push(format!("{error:#}"));
                        }
                    }
                }
            }
        }

        match self.next_request() {
            Some(request) => PullEvent::Request(request),
            // `next_request` only answers `None` once it has finished or failed the
            // current file, so the pull simply moves to the next entry.
            None => self.step(),
        }
    }

    /// The next request for the file in progress, if it needs one. Returning `None` always
    /// ends the current file first (completion at size, or failure past the position
    /// limit), so a caller may simply move on to the next entry.
    fn next_request(&mut self) -> Option<FileContentsRequest> {
        let (stream_id, index, path, size, position) = {
            let current = self.current.as_ref()?;
            (
                current.stream_id,
                current.index,
                current.path.clone(),
                current.size,
                current.position,
            )
        };
        let Some(size) = size else {
            self.current.as_mut()?.awaiting = Awaiting::Size;
            return Some(FileContentsRequest {
                stream_id,
                index,
                flags: FileContentsFlags::SIZE,
                position: 0,
                requested_size: 8,
                data_id: self.clip_data_id,
            });
        };
        let remaining = size.saturating_sub(position);
        if remaining == 0 {
            self.complete_current();
            return None;
        }
        if !self.huge_files && 0x8000_0000 <= position {
            self.fail_current(format!(
                "`{}` is beyond the 2 GiB the server negotiated for single files",
                path.display()
            ));
            return None;
        }
        let requested = u32::try_from(remaining.min(u64::from(MAX_CHUNK))).unwrap_or(MAX_CHUNK);
        self.current.as_mut()?.awaiting = Awaiting::Range { requested };
        Some(FileContentsRequest {
            stream_id,
            index,
            flags: FileContentsFlags::RANGE,
            position,
            requested_size: requested,
            data_id: self.clip_data_id,
        })
    }

    /// Closes the current file, completed.
    fn complete_current(&mut self) {
        if let Some(current) = self.current.take() {
            info!(path = %current.path.display(), bytes = current.position, "Received remote file");
        }
    }

    /// Abandons the current file and removes the partial copy from disk.
    fn fail_current(&mut self, reason: String) {
        if let Some(current) = self.current.take() {
            // The file handle closes first (it is dropped with `current`) so the partial
            // copy can be removed.
            let _ = std::fs::remove_file(&current.path);
            warn!(path = %current.path.display(), %reason, "Remote file not received");
            self.failures.push(reason);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(
        name: &str,
        relative: Option<&str>,
        directory: bool,
        size: Option<u64>,
    ) -> FileDescriptor {
        let mut descriptor = FileDescriptor::new(name).with_attributes(if directory {
            ClipboardFileAttributes::DIRECTORY
        } else {
            ClipboardFileAttributes::ARCHIVE
        });
        if let Some(relative) = relative {
            descriptor = descriptor.with_relative_path(relative);
        }
        match size {
            Some(size) => descriptor.with_file_size(size),
            None => descriptor,
        }
    }

    fn request_of(event: &PullEvent) -> &FileContentsRequest {
        match event {
            PullEvent::Request(request) => request,
            PullEvent::Finished { .. } => panic!("expected a request, got {event:?}"),
        }
    }

    fn finished_of(event: PullEvent) -> (Vec<PathBuf>, Vec<String>) {
        match event {
            PullEvent::Finished { roots, failures } => (roots, failures),
            PullEvent::Request(_) => panic!("expected the pull to finish"),
        }
    }

    fn size_answer(request: &FileContentsRequest, size: u64) -> FileContentsResponse<'static> {
        assert!(request.flags.contains(FileContentsFlags::SIZE));
        assert_eq!(request.requested_size, 8);
        assert_eq!(request.position, 0);
        FileContentsResponse::new_data_response(request.stream_id, size.to_le_bytes().to_vec())
    }

    fn range_answer(request: &FileContentsRequest, data: &[u8]) -> FileContentsResponse<'static> {
        assert!(request.flags.contains(FileContentsFlags::RANGE));
        assert_eq!(request.requested_size as usize, data.len());
        FileContentsResponse::new_data_response(request.stream_id, data.to_vec())
    }

    #[test]
    fn plans_preserve_structure_and_take_unused_names() {
        let root = std::env::temp_dir().join(format!("hopdesk-pull-plan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("docs")).expect("create existing item");

        let files = vec![
            descriptor("docs", None, true, None),
            descriptor("sub", Some("docs"), true, None),
            descriptor("a.txt", Some("docs\\sub"), false, Some(3)),
            descriptor("note.txt", None, false, Some(4)),
        ];
        let plan = plan(&files, &root).expect("plan");
        assert_eq!(
            plan.roots,
            vec![root.join("docs (1)"), root.join("note.txt")]
        );
        assert_eq!(plan.entries.len(), 4);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn plans_reject_names_that_are_not_components() {
        let root =
            std::env::temp_dir().join(format!("hopdesk-pull-plan-bad-{}", std::process::id()));
        for name in ["../escape", "a/b", "", "..", "a\\b"] {
            assert!(
                plan(&[descriptor(name, None, false, Some(1))], &root).is_err(),
                "`{name}` passed"
            );
        }
        assert!(
            plan(
                &[descriptor("inner", Some("..\\root"), false, Some(1))],
                &root
            )
            .is_err(),
            "traversal component passed"
        );
    }

    #[test]
    fn plans_reject_copies_larger_than_the_cap() {
        let root =
            std::env::temp_dir().join(format!("hopdesk-pull-plan-cap-{}", std::process::id()));
        assert!(
            plan(
                &[descriptor(
                    "big.bin",
                    None,
                    false,
                    Some(MAX_TOTAL_BYTES + 1)
                )],
                &root
            )
            .is_err()
        );
    }

    #[test]
    fn pulls_stream_files_and_finish_with_roots() {
        let root = std::env::temp_dir().join(format!("hopdesk-pull-run-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create folder");

        // One file with a known size that needs two ranges, one whose size must be asked,
        // and one directory.
        let files = vec![
            descriptor("docs", None, true, None),
            descriptor("a.txt", Some("docs"), false, Some(u64::from(MAX_CHUNK) + 3)),
            descriptor("b.txt", None, false, None),
        ];
        let plan = plan(&files, &root).expect("plan");
        let mut pull = Pull::new(plan, Some(7), true);

        // Directory entries need no requests; the first request asks for the first file.
        let request = request_of(&pull.start()).clone();
        assert_eq!(request.index, 1);
        assert!(request.flags.contains(FileContentsFlags::RANGE));
        assert_eq!(request.requested_size, MAX_CHUNK);
        assert_eq!(request.data_id, Some(7));

        let first = vec![0xA5; MAX_CHUNK as usize];
        let request =
            request_of(&pull.on_response(&range_answer(&request, &first)).unwrap()).clone();
        assert_eq!(request.position, u64::from(MAX_CHUNK));
        assert_eq!(request.requested_size, 3);
        let request =
            request_of(&pull.on_response(&range_answer(&request, b"xyz")).unwrap()).clone();
        // Next file: unknown size, asked first.
        assert_eq!(request.index, 2);
        assert!(request.flags.contains(FileContentsFlags::SIZE));
        let request = request_of(&pull.on_response(&size_answer(&request, 5)).unwrap()).clone();
        assert_eq!(request.requested_size, 5);
        let event = pull.on_response(&range_answer(&request, b"hello")).unwrap();

        let (roots, failures) = finished_of(event);
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(roots, vec![root.join("docs"), root.join("b.txt")]);
        assert_eq!(
            std::fs::read(root.join("docs/a.txt")).expect("read a"),
            first.iter().chain(b"xyz").copied().collect::<Vec<_>>()
        );
        assert_eq!(std::fs::read(root.join("b.txt")).expect("read b"), b"hello");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_failed_file_leaves_no_partial_copy_and_the_rest_continues() {
        let root = std::env::temp_dir().join(format!("hopdesk-pull-fail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create folder");

        let files = vec![
            descriptor("a.txt", None, false, Some(2)),
            descriptor("b.txt", None, false, Some(2)),
        ];
        let plan = plan(&files, &root).expect("plan");
        let mut pull = Pull::new(plan, None, true);

        let request = request_of(&pull.start()).clone();
        let event = pull
            .on_response(&FileContentsResponse::new_error(request.stream_id))
            .unwrap();
        let request = request_of(&event).clone();
        assert_eq!(request.index, 1, "the pull moves to the next file");
        let event = pull.on_response(&range_answer(&request, b"ok")).unwrap();
        let (roots, failures) = finished_of(event);

        assert_eq!(failures.len(), 1);
        assert_eq!(roots, vec![root.join("a.txt"), root.join("b.txt")]);
        assert!(!root.join("a.txt").exists(), "the partial copy is removed");
        assert_eq!(std::fs::read(root.join("b.txt")).expect("read b"), b"ok");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn responses_for_other_streams_are_ignored() {
        let root = std::env::temp_dir().join(format!("hopdesk-pull-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create folder");

        let plan = plan(&[descriptor("a.txt", None, false, Some(2))], &root).expect("plan");
        let mut pull = Pull::new(plan, None, true);
        let request = request_of(&pull.start()).clone();

        assert!(
            pull.on_response(&FileContentsResponse::new_data_response(999, vec![0; 8]))
                .is_none()
        );

        let event = pull.on_response(&range_answer(&request, b"hi")).unwrap();
        assert!(finished_of(event).1.is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn oversized_or_malformed_ranges_fail_the_file() {
        let root = std::env::temp_dir().join(format!("hopdesk-pull-badr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create folder");

        // A file whose size must be asked: a size answer over the cap fails it.
        let plan = super::plan(&[descriptor("a.txt", None, false, None)], &root).expect("plan");
        let mut pull = Pull::new(plan, None, true);
        let request = request_of(&pull.start()).clone();
        let event = pull
            .on_response(&size_answer(&request, MAX_TOTAL_BYTES + 1))
            .unwrap();
        assert_eq!(finished_of(event).1.len(), 1);
        assert!(!root.join("a.txt").exists());

        // A range longer than requested fails the file.
        let plan = super::plan(&[descriptor("b.txt", None, false, Some(4))], &root).expect("plan");
        let mut pull = Pull::new(plan, None, true);
        let request = request_of(&pull.start()).clone();
        let long = FileContentsResponse::new_data_response(request.stream_id, vec![0; 5]);
        let event = pull.on_response(&long).unwrap();
        assert_eq!(finished_of(event).1.len(), 1);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_empty_copy_finishes_without_requests() {
        let root = std::env::temp_dir().join(format!("hopdesk-pull-empty-{}", std::process::id()));
        let plan = plan(&[], &root).expect("plan");
        let mut pull = Pull::new(plan, None, true);
        let (roots, failures) = finished_of(pull.start());
        assert!(roots.is_empty());
        assert!(failures.is_empty());
    }

    #[test]
    fn requests_stay_within_the_negotiated_position_limit() {
        let root = std::env::temp_dir().join(format!("hopdesk-pull-huge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create folder");

        // A file reaching past 2 GiB, against a server that did not agree to huge files.
        let size = 0x8000_0000u64 + 10;
        let plan =
            super::plan(&[descriptor("a.txt", None, false, Some(size))], &root).expect("plan");
        let mut pull = Pull::new(plan, None, false);
        let request = request_of(&pull.start()).clone();

        // Jump to just below the limit, then receive a range that crosses it: the pull
        // fails the file instead of issuing a request beyond 2 GiB, which the engine
        // would reject (and end the session for).
        pull.current.as_mut().expect("file in progress").position = 0x8000_0000 - 100;
        let event = pull
            .on_response(&FileContentsResponse::new_data_response(
                request.stream_id,
                vec![0; 100],
            ))
            .expect("the pull ends");
        let (roots, failures) = finished_of(event);
        assert_eq!(roots.len(), 1);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(!root.join("a.txt").exists(), "the partial copy is removed");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn zero_size_files_complete_without_any_request() {
        let root = std::env::temp_dir().join(format!("hopdesk-pull-zero-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create folder");

        let files: Vec<FileDescriptor> = (0..3)
            .map(|i| descriptor(&format!("{i}.txt"), None, false, Some(0)))
            .collect();
        let plan = plan(&files, &root).expect("plan");
        let mut pull = Pull::new(plan, None, true);
        let (roots, failures) = finished_of(pull.start());
        assert_eq!(roots.len(), 3);
        assert!(failures.is_empty(), "{failures:?}");
        assert!(root.join("0.txt").exists());

        let _ = std::fs::remove_dir_all(&root);
    }
}
