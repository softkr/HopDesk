//! macOS clipboard bridge for CLIPRDR: plain text both ways, and files from this Mac to the remote.
//!
//! The RDP engine ships clipboard backends for Windows and Linux only. This one polls the general
//! pasteboard for local copies and fetches remote copies as soon as they are announced. Files are
//! offered through [`ClipboardFiles`] (files dropped on the session window).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::ensure;

use ironrdp_cliprdr::backend::{ClipboardMessage, ClipboardMessageProxy, CliprdrBackend, CliprdrBackendFactory};
use ironrdp_cliprdr::pdu::{
    ClipboardFormat, ClipboardFormatId, ClipboardGeneralCapabilityFlags, FileContentsRequest, FileContentsResponse,
    FormatDataRequest, FormatDataResponse, LockDataId,
};
use ironrdp_core::impl_as_any;
use ironrdp_viewer::clipboard::ClientClipboardMessageProxy;
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::NSString;
use tracing::{debug, info, warn};

use crate::file_offer::FileOffer;

/// How often the pasteboard is checked for local copies.
const POLL_INTERVAL: Duration = Duration::from_millis(300);

/// State shared by the backend of the current connection and [`ClipboardFiles`].
#[derive(Debug, Default)]
struct Shared {
    /// The channel answered our first format list.
    ready: AtomicBool,
    /// The server negotiated stream-based file copy (`CB_STREAM_FILECLIP_ENABLED`).
    file_transfer: AtomicBool,
    /// Files on offer, and whether the remote accepted their format list.
    offer: Mutex<Option<(FileOffer, Arc<AtomicBool>)>>,
}

impl Shared {
    fn offer(&self) -> std::sync::MutexGuard<'_, Option<(FileOffer, Arc<AtomicBool>)>> {
        self.offer.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Builds a backend for every (re)connection of the session.
pub struct MacClipboardFactory {
    proxy: ClientClipboardMessageProxy,
    shared: Arc<Shared>,
}

impl MacClipboardFactory {
    pub fn new(proxy: ClientClipboardMessageProxy) -> Self {
        Self {
            proxy,
            shared: Arc::default(),
        }
    }

    /// Handle that puts local files on the remote clipboard.
    pub fn files(&self) -> ClipboardFiles {
        ClipboardFiles {
            proxy: self.proxy.clone(),
            shared: Arc::clone(&self.shared),
        }
    }
}

impl CliprdrBackendFactory for MacClipboardFactory {
    fn build_cliprdr_backend(&self) -> Box<dyn CliprdrBackend> {
        // A new channel starts unnegotiated and without files on offer.
        self.shared.ready.store(false, Ordering::Release);
        self.shared.file_transfer.store(false, Ordering::Release);
        *self.shared.offer() = None;
        Box::new(MacClipboard::new(self.proxy.clone(), Arc::clone(&self.shared)))
    }
}

/// Puts local files on the remote clipboard.
#[derive(Clone)]
pub struct ClipboardFiles {
    proxy: ClientClipboardMessageProxy,
    shared: Arc<Shared>,
}

impl ClipboardFiles {
    /// Offers `paths` (files and folders) to the remote clipboard. The returned flag is set once
    /// the remote accepted the list, so a paste there receives these files.
    ///
    /// Fails when the channel is not ready or the server does not allow file copy; sending the
    /// list then would end the session.
    pub fn offer(&self, paths: &[PathBuf]) -> anyhow::Result<Arc<AtomicBool>> {
        ensure!(self.shared.ready.load(Ordering::Acquire), "the clipboard channel is not ready");
        ensure!(
            self.shared.file_transfer.load(Ordering::Acquire),
            "the server does not allow copying files through the clipboard"
        );
        let offer = FileOffer::collect(paths)?;
        let descriptors = offer.descriptors();
        let accepted = Arc::new(AtomicBool::new(false));
        *self.shared.offer() = Some((offer, Arc::clone(&accepted)));
        info!(entries = descriptors.len(), "Offering dropped files to the remote clipboard");
        self.proxy
            .send_clipboard_message(ClipboardMessage::SendInitiateFileCopy(descriptors));
        Ok(accepted)
    }
}

#[derive(Debug)]
struct MacClipboard {
    proxy: ClientClipboardMessageProxy,
    shared: Arc<Shared>,
    /// Pasteboard change count of our own last write, so the poller does not echo it back.
    own_change: Arc<AtomicIsize>,
    stop: Arc<AtomicBool>,
    polling: bool,
}

impl_as_any!(MacClipboard);

impl MacClipboard {
    fn new(proxy: ClientClipboardMessageProxy, shared: Arc<Shared>) -> Self {
        Self {
            proxy,
            shared,
            own_change: Arc::new(AtomicIsize::new(-1)),
            stop: Arc::new(AtomicBool::new(false)),
            polling: false,
        }
    }

    /// Announces the local formats to the remote: text when there is some, otherwise an empty list.
    ///
    /// The list must be sent even when empty: the channel only becomes ready once the remote has
    /// answered the client's first format list (MS-RDPECLIP 1.3.2.1).
    fn advertise_local_formats(&self) {
        let formats = if read_text().is_some() {
            vec![ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)]
        } else {
            Vec::new()
        };
        self.proxy
            .send_clipboard_message(ClipboardMessage::SendInitiateCopy(formats));
    }

    /// Watches the pasteboard and announces local copies until the backend is dropped.
    fn start_polling(&mut self) {
        if std::mem::replace(&mut self.polling, true) {
            return;
        }
        let proxy = self.proxy.clone();
        let shared = Arc::clone(&self.shared);
        let own_change = Arc::clone(&self.own_change);
        let stop = Arc::clone(&self.stop);
        let spawned = std::thread::Builder::new().name("clipboard-poll".to_owned()).spawn(move || {
            let mut seen = change_count();
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(POLL_INTERVAL);
                let current = change_count();
                if current == seen {
                    continue;
                }
                seen = current;
                if current != own_change.load(Ordering::Relaxed) && read_text().is_some() {
                    debug!("Local clipboard changed; announcing text to the remote");
                    // The text list replaces any files on offer.
                    *shared.offer() = None;
                    proxy.send_clipboard_message(ClipboardMessage::SendInitiateCopy(vec![ClipboardFormat::new(
                        ClipboardFormatId::CF_UNICODETEXT,
                    )]));
                }
            }
        });
        if let Err(error) = spawned {
            warn!(%error, "Unable to watch the local clipboard");
        }
    }
}

impl Drop for MacClipboard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl CliprdrBackend for MacClipboard {
    fn temporary_directory(&self) -> &str {
        ".cliprdr"
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        // File lists carry names relative to the dropped items, never local paths.
        ClipboardGeneralCapabilityFlags::USE_LONG_FORMAT_NAMES
            | ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED
            | ClipboardGeneralCapabilityFlags::FILECLIP_NO_FILE_PATHS
            | ClipboardGeneralCapabilityFlags::HUGE_FILE_SUPPORT_ENABLED
    }

    fn on_ready(&mut self) {
        self.shared.ready.store(true, Ordering::Release);
        self.start_polling();
    }

    fn on_format_list_response(&mut self, ok: bool) {
        let mut offer = self.shared.offer();
        if let Some((_, accepted)) = offer.as_ref()
            && !accepted.load(Ordering::Acquire)
        {
            if ok {
                accepted.store(true, Ordering::Release);
            } else {
                warn!("The remote rejected the dropped files");
                *offer = None;
            }
        }
    }

    fn on_request_format_list(&mut self) {
        self.advertise_local_formats();
        // A format list may be sent before the channel is ready, so local copies are announced
        // from now on rather than only after the remote's answer.
        self.start_polling();
    }

    fn on_process_negotiated_capabilities(&mut self, capabilities: ClipboardGeneralCapabilityFlags) {
        debug!(?capabilities, "Clipboard capabilities negotiated");
        let files = capabilities.contains(ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED);
        self.shared.file_transfer.store(files, Ordering::Release);
    }

    fn on_remote_copy(&mut self, available_formats: &[ClipboardFormat]) {
        // Text is small: fetch it right away so a local paste needs no round trip.
        if available_formats.iter().any(|f| f.id == ClipboardFormatId::CF_UNICODETEXT) {
            self.proxy
                .send_clipboard_message(ClipboardMessage::SendInitiatePaste(ClipboardFormatId::CF_UNICODETEXT));
        }
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        let response = match read_text() {
            Some(text) if request.format == ClipboardFormatId::CF_UNICODETEXT => {
                FormatDataResponse::new_unicode_string(&text)
            }
            _ => FormatDataResponse::new_error(),
        };
        self.proxy
            .send_clipboard_message(ClipboardMessage::SendFormatData(response));
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        if response.is_error() {
            return;
        }
        match response.to_unicode_string() {
            Ok(text) => write_text(&text, &self.own_change),
            Err(error) => warn!(%error, "Remote clipboard text could not be decoded"),
        }
    }

    fn on_file_contents_request(&mut self, request: FileContentsRequest) {
        let response = match self.shared.offer().as_ref() {
            Some((offer, _)) => offer.respond(&request),
            None => FileContentsResponse::new_error(request.stream_id),
        };
        self.proxy
            .send_clipboard_message(ClipboardMessage::SendFileContentsResponse(response));
    }

    fn on_file_contents_response(&mut self, _response: FileContentsResponse<'_>) {}

    fn on_lock(&mut self, _data_id: LockDataId) {}

    fn on_unlock(&mut self, _data_id: LockDataId) {}
}

fn change_count() -> isize {
    NSPasteboard::generalPasteboard().changeCount()
}

fn read_text() -> Option<String> {
    // SAFETY: NSPasteboardTypeString is an immutable AppKit constant.
    let string_type = unsafe { NSPasteboardTypeString };
    NSPasteboard::generalPasteboard()
        .stringForType(string_type)
        .map(|s| s.to_string())
}

/// Replaces the pasteboard with `text`, recording the change count before the text lands so the
/// poller never announces our own write back to the remote.
fn write_text(text: &str, own_change: &AtomicIsize) {
    // SAFETY: NSPasteboardTypeString is an immutable AppKit constant.
    let string_type = unsafe { NSPasteboardTypeString };
    let pasteboard = NSPasteboard::generalPasteboard();
    own_change.store(pasteboard.clearContents(), Ordering::Relaxed);
    if !pasteboard.setString_forType(&NSString::from_str(text), string_type) {
        warn!("Unable to write the remote clipboard text locally");
    }
}
