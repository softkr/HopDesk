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

Search for `[PATCH HopDesk]` to find the changed code.

### Updating IronRDP

```sh
rm -rf third_party/IronRDP
git clone --depth 1 https://github.com/Devolutions/IronRDP third_party/IronRDP
rm -rf third_party/IronRDP/.git
(cd third_party/IronRDP && patch -p1 < ../ironrdp-custom-transport.patch)
```

Consider proposing the custom transport upstream so the patch can be dropped.
