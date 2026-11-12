//! macOS clipboard bridge for CLIPRDR: plain text both ways.
//!
//! The RDP engine ships clipboard backends for Windows and Linux only. This one polls the general
//! pasteboard for local copies and fetches remote copies as soon as they are announced.

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ironrdp_cliprdr::backend::{ClipboardMessage, ClipboardMessageProxy, CliprdrBackend, CliprdrBackendFactory};
use ironrdp_cliprdr::pdu::{
    ClipboardFormat, ClipboardFormatId, ClipboardGeneralCapabilityFlags, FileContentsRequest, FileContentsResponse,
    FormatDataRequest, FormatDataResponse, LockDataId,
};
use ironrdp_core::impl_as_any;
use ironrdp_viewer::clipboard::ClientClipboardMessageProxy;
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::NSString;
use tracing::{debug, warn};

/// How often the pasteboard is checked for local copies.
const POLL_INTERVAL: Duration = Duration::from_millis(300);

/// Builds a backend for every (re)connection of the session.
pub struct MacClipboardFactory {
    proxy: ClientClipboardMessageProxy,
}

impl MacClipboardFactory {
    pub fn new(proxy: ClientClipboardMessageProxy) -> Self {
        Self { proxy }
    }
}

impl CliprdrBackendFactory for MacClipboardFactory {
    fn build_cliprdr_backend(&self) -> Box<dyn CliprdrBackend> {
        Box::new(MacClipboard::new(self.proxy.clone()))
    }
}

#[derive(Debug)]
struct MacClipboard {
    proxy: ClientClipboardMessageProxy,
    /// Pasteboard change count of our own last write, so the poller does not echo it back.
    own_change: Arc<AtomicIsize>,
    stop: Arc<AtomicBool>,
    polling: bool,
}

impl_as_any!(MacClipboard);

impl MacClipboard {
    fn new(proxy: ClientClipboardMessageProxy) -> Self {
        Self {
            proxy,
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
        ClipboardGeneralCapabilityFlags::USE_LONG_FORMAT_NAMES
    }

    fn on_ready(&mut self) {
        self.start_polling();
    }

    fn on_request_format_list(&mut self) {
        self.advertise_local_formats();
        // A format list may be sent before the channel is ready, so local copies are announced
        // from now on rather than only after the remote's answer.
        self.start_polling();
    }

    fn on_process_negotiated_capabilities(&mut self, capabilities: ClipboardGeneralCapabilityFlags) {
        debug!(?capabilities, "Clipboard capabilities negotiated");
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

    fn on_file_contents_request(&mut self, _request: FileContentsRequest) {}

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
