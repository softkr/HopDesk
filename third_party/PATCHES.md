# Third-party sources

## IronRDP

- Upstream: https://github.com/Devolutions/IronRDP
- Revision: `d5e70f02b670dbe79ad7ef47203bfa82d32c16f4` (2026-11-09)
- License: MIT OR Apache-2.0

Used from source (not crates.io) because the crates.io release of `ironrdp-client` (0.1.0)
pins pre-release crypto crates that conflict with `russh`, and lacks certificate-validation callbacks.

### Local patch: `ironrdp-custom-transport.patch`

Adds `Transport::Custom` / `TransportKind::Custom` to `ironrdp-client`:
an embedder-supplied byte stream (here: an SSH `direct-tcpip` channel through the jump host).
TLS, CredSSP and the connection sequence run on top of it exactly as for a direct connection,
with `Config::destination` as the server name, so certificate validation and Kerberos SPNs
use the real target host instead of a local port-forward address.

The factory is called on every connection attempt, so auto-reconnect reopens the channel.

### Local patch: `ironrdp-client-resize-no-reconnect.patch`

Adds `RdpClient::without_resize_reconnect`: a resize that Display Control cannot apply (channel
not open yet on the logon screen, or not supported) is dropped instead of disconnecting and
logging on again with the new size. The viewer sends the window size again after logon.

### Local patch: `ironrdp-viewer-window-mode.patch`

Adds `App::with_maximized` / `App::with_fullscreen` to `ironrdp-viewer` so the desktop window can
open maximized or in borderless fullscreen, a local Ctrl+Alt+Enter shortcut that toggles
fullscreen instead of reaching the session, and `App::with_follow_window_size(false)` to keep the
session at its connection size and scale (the frame is scaled to the window), and
`App::with_title` with the session desktop size appended to the window title.
It also stretches each frame over the client area (macOS otherwise presents it 1:1 in physical
pixels, at half size on Retina), and `App::with_logical_pixels` maps one session pixel to one
logical point for the initial window size and for resize requests (at 100% scale).

Search for `[PATCH HopDesk]` to find the changed code.

### Updating IronRDP

```sh
rm -rf third_party/IronRDP
git clone --depth 1 https://github.com/Devolutions/IronRDP third_party/IronRDP
rm -rf third_party/IronRDP/.git
(cd third_party/IronRDP && patch -p1 < ../ironrdp-custom-transport.patch)
(cd third_party/IronRDP && patch -p1 < ../ironrdp-client-resize-no-reconnect.patch)
(cd third_party/IronRDP && patch -p1 < ../ironrdp-viewer-window-mode.patch)
```

Consider proposing the custom transport upstream so the patch can be dropped.
