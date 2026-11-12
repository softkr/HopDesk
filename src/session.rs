//! Builds the IronRDP configuration for a profile and runs the viewer window.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::Context as _;
use ironrdp_client::config::{ConfigBuilder, CustomTransport, Destination, TransportKind};
use ironrdp_client::output_channel::output_channel;
use ironrdp_client::rdp::{RdpClient, RdpOutputEvent};
use ironrdp_pdu::rdp::capability_sets::MajorPlatformType;
use ironrdp_tls::{CertificateValidation, CertificateValidationCallback};
use ironrdp_viewer::app::App;
use tracing::{error, info, warn};
use winit::dpi::PhysicalSize;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::WindowId;

use crate::profile::{DisplayMode, Profile};
use crate::status::{self, Phase};
use crate::trust::{Kind, TrustStore, Verdict, sha256_hex};
use crate::tunnel::Tunnel;

/// `screen` is the client screen size in logical points: fullscreen and maximized sessions start
/// at that size, so the server does not have to resize right after logon.
/// Without `tunnel` the session connects straight to the RDP target.
pub fn run(
    profile: &Profile,
    screen: Option<(u16, u16)>,
    rdp_password: String,
    tunnel: Option<Arc<Tunnel>>,
    trust: Arc<TrustStore>,
) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("create tokio runtime")?;

    // Fail fast on SSH problems before any window opens.
    if let Some(tunnel) = &tunnel {
        status::emit(Phase::Ssh, "");
        rt.block_on(tunnel.ensure_connected())?;
    }
    status::emit(Phase::Rdp, "");

    let desktop = match (profile.display, screen) {
        (DisplayMode::Fixed, _) | (_, None) => (profile.width, profile.height),
        (_, Some(screen)) => screen,
    };
    info!(width = desktop.0, height = desktop.1, mode = ?profile.display, "Requested session size");
    let config = build_config(profile, desktop, rdp_password, tunnel, trust)?;

    let event_loop = EventLoop::<RdpOutputEvent>::with_user_event().build()?;
    let event_loop_proxy = event_loop.create_proxy();
    let (output_event_sender, mut output_event_receiver) = output_channel(64);
    let initial_window_size = PhysicalSize::new(u32::from(desktop.0), u32::from(desktop.1));

    // A resize the server cannot apply in place (e.g. from the logon screen) keeps the session
    // instead of disconnecting and logging on again; the viewer retries after logon.
    let client = RdpClient::new(config, output_event_sender).without_resize_reconnect();
    // The engine bridges the clipboard itself on Windows and Linux only.
    #[cfg(target_os = "macos")]
    let client = {
        let proxy = ironrdp_viewer::clipboard::ClientClipboardMessageProxy::new(client.input_sender());
        client.with_cliprdr_backend_factory(Box::new(crate::clipboard::MacClipboardFactory::new(proxy)))
    };
    let input_event_sender = client.input_sender();
    // Maximized and fullscreen windows connect at the profile resolution; the viewer then asks
    // the server to match the window size in points. A fixed session keeps the profile
    // resolution and is scaled to the window.
    let app = App::new(&event_loop, &input_event_sender, initial_window_size, false)
        .context("create viewer")?
        .with_maximized(profile.display == DisplayMode::Maximized)
        .with_fullscreen(profile.display == DisplayMode::Fullscreen)
        .with_follow_window_size(profile.display != DisplayMode::Fixed)
        // One session pixel per point: normal text size on Retina, and at 200% a quarter of the
        // pixels to transfer through the tunnel.
        .with_logical_pixels(true)
        .with_title(format!("HopDesk — {}", profile.name));

    let output_forwarder = rt.spawn(async move {
        while let Some(event) = output_event_receiver.recv().await {
            report_progress(&event);
            if event_loop_proxy.send_event(event).is_err() {
                break;
            }
        }
    });

    std::thread::spawn(move || {
        rt.block_on(client.run());
        if let Err(e) = rt.block_on(output_forwarder) {
            warn!(error = %e, "RDP output forwarder failed");
        }
    });

    event_loop.run_app(&mut SessionWindow::new(app, &profile.name))?;
    Ok(())
}

/// The viewer, plus a Dock tile that tells the session apart from the launcher (macOS).
struct SessionWindow {
    app: App,
    #[cfg_attr(not(target_os = "macos"), expect(dead_code, reason = "used for the macOS Dock tile"))]
    profile_name: String,
    dock_marked: bool,
}

impl SessionWindow {
    fn new(app: App, profile_name: &str) -> Self {
        Self {
            app,
            profile_name: profile_name.to_owned(),
            dock_marked: false,
        }
    }
}

impl ApplicationHandler<RdpOutputEvent> for SessionWindow {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // The app has finished launching here, so the Dock tile is no longer reset to the bundle icon.
        if !self.dock_marked {
            self.dock_marked = true;
            #[cfg(target_os = "macos")]
            crate::dock::mark_session(&self.profile_name);
        }
        self.app.resumed(event_loop);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, window_id: WindowId, event: WindowEvent) {
        self.app.window_event(event_loop, window_id, event);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: RdpOutputEvent) {
        self.app.user_event(event_loop, event);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.app.about_to_wait(event_loop);
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        #[cfg(target_os = "macos")]
        crate::dock::leave_dock();
    }
}

/// Last session size reported by the server (`width << 16 | height`), to log changes only.
static LAST_SESSION_SIZE: AtomicU32 = AtomicU32::new(0);

/// Mirrors session milestones to the launcher (the viewer itself exits 0 even on failure).
fn report_progress(event: &RdpOutputEvent) {
    match event {
        RdpOutputEvent::Connected | RdpOutputEvent::AutoReconnected => status::emit(Phase::Connected, ""),
        RdpOutputEvent::Image { width, height, .. } => {
            let size = (u32::from(width.get()) << 16) | u32::from(height.get());
            if LAST_SESSION_SIZE.swap(size, Ordering::Relaxed) != size {
                info!(width = width.get(), height = height.get(), "Server session size");
            }
        }
        RdpOutputEvent::LoginComplete => status::emit(Phase::LoggedIn, ""),
        RdpOutputEvent::AutoReconnecting { .. } => status::emit(Phase::Reconnecting, ""),
        RdpOutputEvent::ConnectionFailure(error) => status::emit(Phase::Failed, &error.to_string()),
        RdpOutputEvent::Terminated(Ok(reason)) => status::emit(Phase::Ended, &reason.to_string()),
        RdpOutputEvent::Terminated(Err(error)) => status::emit(Phase::Ended, &error.to_string()),
        _ => {}
    }
}

fn build_config(
    profile: &Profile,
    (desktop_width, desktop_height): (u16, u16),
    rdp_password: String,
    tunnel: Option<Arc<Tunnel>>,
    trust: Arc<TrustStore>,
) -> anyhow::Result<ironrdp_client::config::Config> {
    let transport = match tunnel {
        Some(tunnel) => {
            let target_host = profile.target.host.clone();
            let target_port = profile.target.port;
            TransportKind::Custom(CustomTransport::new(move || {
                let tunnel = Arc::clone(&tunnel);
                let host = target_host.clone();
                Box::pin(async move { tunnel.open(&host, target_port).await })
            }))
        }
        None => TransportKind::Direct,
    };

    // RDP servers usually present self-signed certificates: strict validation fails and this
    // callback pins the certificate on first use, then requires the same one afterwards.
    let cert_callback: CertificateValidationCallback = Arc::new(move |der: &[u8], endpoint: &str, reason: &str| {
        let fingerprint = sha256_hex(der);
        match trust.check(Kind::Rdp, endpoint, &fingerprint) {
            Ok(Verdict::Known) => true,
            Ok(Verdict::NewlyPinned) => {
                warn!(%endpoint, %fingerprint, %reason, "Pinned new RDP server certificate (first connection)");
                true
            }
            Ok(Verdict::Mismatch { pinned }) => {
                error!(
                    %endpoint, %fingerprint, %pinned,
                    "RDP CERTIFICATE CHANGED - possible man-in-the-middle; refusing to send credentials \
                     (run `hopdesk forget <host>` if the change is legitimate)"
                );
                false
            }
            Err(e) => {
                error!(error = %e, "Trust store failure; rejecting certificate");
                false
            }
        }
    });

    let mut builder = ConfigBuilder::new()
        .with_destination(Destination::from_parts(&profile.target.host, profile.target.port))
        .with_username(&profile.target.user)
        .with_password(rdp_password)
        // Send the stored credentials in Client Info so the server skips its logon screen.
        .with_autologon(true)
        .with_transport(transport)
        .with_certificate_validation(CertificateValidation::Strict)
        .with_certificate_validation_callback(cert_callback)
        .with_client_build(client_build())
        .with_client_dir("C:\\Windows\\System32\\mstscax.dll")
        .with_client_name(whoami::hostname().unwrap_or_else(|_| "HopDesk".to_owned()))
        .with_platform(current_platform())
        .with_desktop_width(desktop_width)
        .with_desktop_height(desktop_height)
        // Graphics pipeline (MS-RDPEGFX): progressive/planar codecs instead of the legacy bitmap
        // path, which updates the screen with far less bandwidth and server work.
        .with_graphics_pipeline(true);
    if let Some(domain) = &profile.target.domain {
        builder = builder.with_domain(domain);
    }

    builder.build().context("build RDP configuration")
}

fn client_build() -> u32 {
    let mut parts = env!("CARGO_PKG_VERSION").split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let (major, minor, patch) = (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    );
    major * 100 + minor * 10 + patch
}

fn current_platform() -> MajorPlatformType {
    if cfg!(target_os = "windows") {
        MajorPlatformType::WINDOWS
    } else if cfg!(target_os = "macos") {
        MajorPlatformType::MACINTOSH
    } else if cfg!(target_os = "linux") {
        MajorPlatformType::UNIX
    } else {
        MajorPlatformType::UNSPECIFIED
    }
}
