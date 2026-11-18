//! Built-in WireGuard VPN: a profile reaches its target through a WireGuard peer without a TUN
//! device, administrator rights or any change to the system's routing.
//!
//! The WireGuard protocol (boringtun) and a small TCP/IP stack (smoltcp) run inside the session
//! process. [`Vpn::open`] hands out in-memory streams, like the SSH jump host tunnel does, so the
//! RDP, SSH and VNC sessions need no VPN-specific code. Only connections opened through it use
//! the VPN; nothing else on the machine does.
//!
//! [`Engine`] is the synchronous core (WireGuard + TCP/IP, no sockets of its own); the `Link`
//! driver feeds it UDP datagrams and bridges its TCP connections to tokio streams.

use core::time::Duration;
use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use anyhow::{Context as _, bail, ensure};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use boringtun::noise::{Tunn, TunnResult};
use boringtun::x25519::{PublicKey, StaticSecret};
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::tcp;
use smoltcp::time::Instant as SmolInstant;
use smoltcp::wire::{HardwareAddress, IpAddress, IpCidr, IpEndpoint};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _, DuplexStream};
use tokio::net::UdpSocket;
use tokio::sync::{Notify, mpsc, oneshot};
use tracing::{debug, info, warn};

/// Largest IP packet the virtual interface sends. Leaves room for the WireGuard and UDP/IP
/// headers on a 1500 byte path, and for PPPoE and similar links.
const TUNNEL_MTU: usize = 1380;
/// Receive and transmit buffer of each TCP connection (window scaling is used above 64 KiB).
const TCP_BUFFER: usize = 256 * 1024;
/// A TCP connection with no answer from the remote end for this long is dropped.
const TCP_TIMEOUT: Duration = Duration::from_secs(60);
const TCP_KEEPALIVE: Duration = Duration::from_secs(30);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// WireGuard timers (retransmitted handshakes, keepalives) are serviced at least this often.
const TIMER_TICK: Duration = Duration::from_millis(250);
/// Largest WireGuard datagram (a UDP payload) and scratch buffer size.
const MAX_DATAGRAM: usize = 65_536;
/// Datagrams taken from the socket before the stack runs again.
const DATAGRAM_BATCH: usize = 64;
/// IP packets waiting to be encrypted; the stack pauses sending above this.
const MAX_QUEUED_PACKETS: usize = 256;
/// Bytes moved between a TCP connection and its stream in one piece.
const CHUNK: usize = 16 * 1024;
/// Chunks buffered between a TCP connection and its stream, per direction.
const CHANNEL_DEPTH: usize = 8;
const STREAM_BUFFER: usize = 256 * 1024;
const WG_KEY_LEN: usize = 32;
/// Ports used for outgoing connections, taken in turn.
const FIRST_EPHEMERAL_PORT: u16 = 49_152;
const LAST_EPHEMERAL_PORT: u16 = 65_534;

/// Settings of a WireGuard peer, as stored in a profile, plus the private key from the credential
/// store.
#[derive(Clone)]
pub struct VpnParams {
    /// `host:port` of the peer; a host name is resolved each time the VPN connects.
    pub endpoint: String,
    /// This client's addresses inside the VPN.
    pub addresses: Vec<IpAddr>,
    /// The peer's public key, base64.
    pub peer_public_key: String,
    /// This client's private key, base64.
    pub private_key: String,
    /// Seconds between keepalive packets that hold a NAT mapping open.
    pub keepalive: Option<u16>,
}

/// Decodes a base64 WireGuard key.
pub fn decode_key(text: &str) -> anyhow::Result<[u8; WG_KEY_LEN]> {
    let bytes = BASE64.decode(text.trim()).context("not valid base64")?;
    bytes.try_into().map_err(|_| anyhow::anyhow!("a WireGuard key is {WG_KEY_LEN} bytes"))
}

/// Whether `text` is `host:port` (an IPv6 address in brackets) with a non-zero port.
pub fn is_valid_endpoint(text: &str) -> bool {
    match text.rsplit_once(':') {
        Some((host, port)) => !host.is_empty() && port.parse::<u16>().is_ok_and(|p| p != 0),
        None => false,
    }
}

/// The settings of a WireGuard configuration file (`wg0.conf`) that HopDesk uses.
#[derive(Debug, PartialEq, Eq)]
pub struct ImportedConfig {
    pub private_key: String,
    pub addresses: Vec<IpAddr>,
    pub peer_public_key: String,
    pub endpoint: String,
    pub keepalive: Option<u16>,
}

/// Reads a `wg-quick` style configuration file.
///
/// Only what a userspace client needs is used. `DNS`, `AllowedIPs`, `MTU`, `ListenPort` and the
/// `PostUp`-style hooks are ignored; nothing in the file is ever executed.
pub fn parse_config(text: &str) -> anyhow::Result<ImportedConfig> {
    #[derive(PartialEq, Clone, Copy)]
    enum Section {
        None,
        Interface,
        Peer,
    }

    let mut section = Section::None;
    let mut peers = 0;
    let (mut private_key, mut peer_public_key, mut endpoint) = (None, None, None);
    let mut addresses = Vec::new();
    let mut keepalive = None;

    for line in text.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            section = match line.to_ascii_lowercase().as_str() {
                "[interface]" => Section::Interface,
                "[peer]" => {
                    peers += 1;
                    Section::Peer
                }
                other => bail!("unknown section {other}"),
            };
            continue;
        }
        let (key, value) = line.split_once('=').with_context(|| format!("expected `key = value`, got `{line}`"))?;
        let (key, value) = (key.trim().to_ascii_lowercase(), value.trim());
        match (section, key.as_str()) {
            (Section::Interface, "privatekey") => private_key = Some(value.to_owned()),
            (Section::Interface, "address") => {
                for item in value.split(',') {
                    let ip = item.trim().split('/').next().unwrap_or_default();
                    addresses.push(ip.parse::<IpAddr>().with_context(|| format!("invalid address `{item}`"))?);
                }
            }
            (Section::Peer, "publickey") => peer_public_key = Some(value.to_owned()),
            (Section::Peer, "endpoint") => endpoint = Some(value.to_owned()),
            (Section::Peer, "persistentkeepalive") => {
                keepalive = Some(value.parse::<u16>().context("invalid PersistentKeepalive")?).filter(|&s| s != 0);
            }
            (Section::Peer, "presharedkey") => bail!("PresharedKey is not supported"),
            _ => {}
        }
    }

    ensure!(peers == 1, "the file must contain exactly one [Peer], found {peers}");
    let private_key = private_key.context("the file has no [Interface] PrivateKey")?;
    let peer_public_key = peer_public_key.context("the [Peer] has no PublicKey")?;
    let endpoint = endpoint.context("the [Peer] has no Endpoint")?;
    ensure!(!addresses.is_empty(), "the file has no [Interface] Address");
    decode_key(&private_key).context("invalid PrivateKey")?;
    decode_key(&peer_public_key).context("invalid PublicKey")?;
    ensure!(is_valid_endpoint(&endpoint), "invalid Endpoint `{endpoint}`");
    Ok(ImportedConfig { private_key, addresses, peer_public_key, endpoint, keepalive })
}

/// What [`Engine`] needs, with the keys decoded.
struct EngineConfig {
    private_key: [u8; WG_KEY_LEN],
    peer_public_key: [u8; WG_KEY_LEN],
    keepalive: Option<u16>,
    addresses: Vec<IpAddr>,
}

/// Virtual network interface: IP packets in and out, no link layer.
struct PacketQueues {
    /// Decrypted packets from the peer, for the stack.
    rx: VecDeque<Vec<u8>>,
    /// Packets from the stack, to be encrypted.
    tx: VecDeque<Vec<u8>>,
}

struct RxPacket(Vec<u8>);

impl RxToken for RxPacket {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

struct TxSlot<'a>(&'a mut VecDeque<Vec<u8>>);

impl TxToken for TxSlot<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut packet = vec![0; len];
        let result = f(&mut packet);
        self.0.push_back(packet);
        result
    }
}

impl Device for PacketQueues {
    type RxToken<'a> = RxPacket;
    type TxToken<'a> = TxSlot<'a>;

    fn receive(&mut self, _: SmolInstant) -> Option<(RxPacket, TxSlot<'_>)> {
        let packet = self.rx.pop_front()?;
        Some((RxPacket(packet), TxSlot(&mut self.tx)))
    }

    fn transmit(&mut self, _: SmolInstant) -> Option<TxSlot<'_>> {
        (self.tx.len() < MAX_QUEUED_PACKETS).then_some(TxSlot(&mut self.tx))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ip;
        caps.max_transmission_unit = TUNNEL_MTU;
        caps
    }
}

/// WireGuard and a TCP/IP stack, driven by the caller: datagrams in with [`Engine::handle_datagram`],
/// datagrams out of `wire_out`, time moved on by [`Engine::poll`].
struct Engine {
    tunn: Tunn,
    device: PacketQueues,
    iface: Interface,
    sockets: SocketSet<'static>,
    started: std::time::Instant,
    /// Encrypted datagrams for the peer.
    wire_out: VecDeque<Vec<u8>>,
    scratch: Vec<u8>,
    next_port: u16,
    addresses: Vec<IpAddr>,
}

impl Engine {
    fn new(config: &EngineConfig) -> Self {
        let tunn = Tunn::new(
            StaticSecret::from(config.private_key),
            PublicKey::from(config.peer_public_key),
            None,
            config.keepalive,
            // Session index: random so that restarts do not reuse one.
            uuid::Uuid::new_v4().as_u128() as u32,
            None,
        );

        let mut device = PacketQueues { rx: VecDeque::new(), tx: VecDeque::new() };
        let started = std::time::Instant::now();
        let mut iface_config = Config::new(HardwareAddress::Ip);
        iface_config.random_seed = uuid::Uuid::new_v4().as_u128() as u64;
        let mut iface = Interface::new(iface_config, &mut device, smol_now(started));
        iface.update_ip_addrs(|list| {
            for ip in &config.addresses {
                let cidr = match ip {
                    IpAddr::V4(v4) => IpCidr::new(IpAddress::Ipv4(*v4), 32),
                    IpAddr::V6(v6) => IpCidr::new(IpAddress::Ipv6(*v6), 128),
                };
                // Bounded by the interface's address limit; the profile holds a few at most.
                let _ = list.push(cidr);
            }
        });
        // Everything else is "on the VPN": there is only one way out. A link without a link
        // layer never resolves the gateway, so any address will do as one.
        for ip in &config.addresses {
            match ip {
                IpAddr::V4(v4) => {
                    let _ = iface.routes_mut().add_default_ipv4_route(*v4);
                }
                IpAddr::V6(v6) => {
                    let _ = iface.routes_mut().add_default_ipv6_route(*v6);
                }
            }
        }

        let port_count = LAST_EPHEMERAL_PORT - FIRST_EPHEMERAL_PORT + 1;
        let first_port = FIRST_EPHEMERAL_PORT + (uuid::Uuid::new_v4().as_u128() as u16) % port_count;
        Self {
            tunn,
            device,
            iface,
            sockets: SocketSet::new(Vec::new()),
            started,
            wire_out: VecDeque::new(),
            scratch: vec![0; MAX_DATAGRAM],
            next_port: first_port,
            addresses: config.addresses.clone(),
        }
    }

    fn now(&self) -> SmolInstant {
        smol_now(self.started)
    }

    /// Starts the WireGuard handshake; the peer answers with the first datagram.
    fn start_handshake(&mut self) {
        match self.tunn.format_handshake_initiation(&mut self.scratch, false) {
            TunnResult::WriteToNetwork(datagram) => self.wire_out.push_back(datagram.to_vec()),
            TunnResult::Err(e) => warn!(error = ?e, "WireGuard handshake initiation failed"),
            _ => {}
        }
    }

    /// The round trip of the last handshake.
    fn latency(&self) -> Option<Duration> {
        self.tunn.stats().4.map(|ms| Duration::from_millis(u64::from(ms)))
    }

    /// Whether a handshake completed, so traffic can flow.
    fn is_established(&self) -> bool {
        self.tunn.time_since_last_handshake().is_some()
    }

    /// Takes an encrypted datagram received from the peer at `src`.
    fn handle_datagram(&mut self, datagram: &[u8], src: IpAddr) {
        let mut result = self.tunn.decapsulate(Some(src), datagram, &mut self.scratch);
        loop {
            match result {
                // Answers (handshake response, keepalive) and queued packets come one at a time.
                TunnResult::WriteToNetwork(reply) => {
                    self.wire_out.push_back(reply.to_vec());
                    result = self.tunn.decapsulate(None, &[], &mut self.scratch);
                }
                TunnResult::WriteToTunnelV4(packet, _) | TunnResult::WriteToTunnelV6(packet, _) => {
                    self.device.rx.push_back(packet.to_vec());
                    return;
                }
                TunnResult::Done => return,
                TunnResult::Err(e) => {
                    debug!(error = ?e, "WireGuard datagram dropped");
                    return;
                }
            }
        }
    }

    /// Runs WireGuard timers and the TCP/IP stack, and encrypts what the stack sent.
    fn poll(&mut self) {
        match self.tunn.update_timers(&mut self.scratch) {
            TunnResult::WriteToNetwork(datagram) => self.wire_out.push_back(datagram.to_vec()),
            TunnResult::Err(e) => debug!(error = ?e, "WireGuard timer"),
            _ => {}
        }
        loop {
            self.iface.poll(self.now(), &mut self.device, &mut self.sockets);
            if self.device.tx.is_empty() {
                return;
            }
            while let Some(packet) = self.device.tx.pop_front() {
                match self.tunn.encapsulate(&packet, &mut self.scratch) {
                    TunnResult::WriteToNetwork(datagram) => self.wire_out.push_back(datagram.to_vec()),
                    TunnResult::Err(e) => debug!(error = ?e, "WireGuard packet dropped"),
                    _ => {}
                }
            }
        }
    }

    /// How long until the stack has something to do.
    fn poll_delay(&mut self) -> Duration {
        let now = self.now();
        let delay = self.iface.poll_delay(now, &self.sockets).map(|d| Duration::from_micros(d.total_micros()));
        delay.map_or(TIMER_TICK, |d| d.min(TIMER_TICK))
    }

    fn open_tcp(&mut self, remote: SocketAddr) -> anyhow::Result<SocketHandle> {
        ensure!(
            self.addresses.iter().any(|a| a.is_ipv4() == remote.is_ipv4()),
            "the VPN address list has no {} address for {remote}",
            if remote.is_ipv4() { "IPv4" } else { "IPv6" },
        );
        let mut socket = tcp::Socket::new(tcp::SocketBuffer::new(vec![0; TCP_BUFFER]), tcp::SocketBuffer::new(vec![0; TCP_BUFFER]));
        // Remote desktop input and acknowledgements are small packets; Nagle would delay them.
        socket.set_nagle_enabled(false);
        socket.set_timeout(Some(TCP_TIMEOUT.into()));
        socket.set_keep_alive(Some(TCP_KEEPALIVE.into()));

        let port = self.next_port;
        self.next_port = if port == LAST_EPHEMERAL_PORT { FIRST_EPHEMERAL_PORT } else { port + 1 };
        socket
            .connect(self.iface.context(), IpEndpoint::from(remote), port)
            .map_err(|e| anyhow::anyhow!("TCP connect to {remote}: {e}"))?;
        Ok(self.sockets.add(socket))
    }

    fn tcp(&mut self, handle: SocketHandle) -> &mut tcp::Socket<'static> {
        self.sockets.get_mut::<tcp::Socket>(handle)
    }
}

fn smol_now(started: std::time::Instant) -> SmolInstant {
    SmolInstant::from_micros(i64::try_from(started.elapsed().as_micros()).unwrap_or(i64::MAX))
}

// ---------------------------------------------------------------------------------------------
// Link: the engine on a UDP socket, serving streams.

/// A request to the driver task.
enum Command {
    Open { remote: SocketAddr, reply: oneshot::Sender<io::Result<DuplexStream>> },
    /// The round trip to the peer, as measured by the last WireGuard handshake.
    Latency { reply: oneshot::Sender<Option<Duration>> },
}

/// A running VPN connection.
struct Link {
    commands: mpsc::Sender<Command>,
    driver: tokio::task::JoinHandle<()>,
}

impl Link {
    fn is_alive(&self) -> bool {
        !self.driver.is_finished()
    }

    /// Resolves the endpoint, starts the driver and waits for the WireGuard handshake.
    async fn start(params: &VpnParams) -> anyhow::Result<Self> {
        let config = EngineConfig {
            private_key: decode_key(&params.private_key).context("VPN private key")?,
            peer_public_key: decode_key(&params.peer_public_key).context("VPN peer public key")?,
            keepalive: params.keepalive,
            addresses: params.addresses.clone(),
        };
        ensure!(!config.addresses.is_empty(), "VPN has no address");

        let endpoint = tokio::net::lookup_host(params.endpoint.as_str())
            .await
            .with_context(|| format!("VPN endpoint lookup failed for {}", params.endpoint))?
            .next()
            .with_context(|| format!("VPN endpoint lookup failed for {}", params.endpoint))?;
        let bind = if endpoint.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
        let udp = UdpSocket::bind(bind).await.context("VPN bind UDP socket")?;
        udp.connect(endpoint).await.context("VPN connect UDP socket")?;

        info!(%endpoint, "Starting WireGuard VPN");
        let (commands, command_rx) = mpsc::channel(8);
        let (ready_tx, ready_rx) = oneshot::channel();
        let driver = tokio::spawn(drive(Engine::new(&config), udp, endpoint, command_rx, ready_tx));
        let link = Self { commands, driver };
        // A dropped `ready_tx` means the driver stopped before the handshake.
        ready_rx.await.context("VPN stopped before the handshake")??;
        info!("WireGuard handshake complete");
        Ok(link)
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.driver.abort();
    }
}

/// The WireGuard VPN of a profile; connects on first use and again when the link is gone.
pub struct Vpn {
    params: VpnParams,
    link: tokio::sync::Mutex<Option<Link>>,
}

impl Vpn {
    pub fn new(params: VpnParams) -> Self {
        Self { params, link: tokio::sync::Mutex::new(None) }
    }

    /// Establishes the VPN now so configuration errors surface before the window opens.
    pub async fn ensure_connected(&self) -> anyhow::Result<()> {
        let mut guard = self.link.lock().await;
        self.live_link(&mut guard).await.map(drop)
    }

    async fn live_link<'a>(&self, guard: &'a mut Option<Link>) -> anyhow::Result<&'a Link> {
        if !guard.as_ref().is_some_and(Link::is_alive) {
            *guard = Some(Link::start(&self.params).await?);
        }
        Ok(guard.as_ref().expect("link set above"))
    }

    /// Opens a TCP connection through the VPN to `host:port`. `host` must be an IP address:
    /// names are not resolved on the VPN's side.
    pub async fn open(&self, host: &str, port: u16) -> io::Result<DuplexStream> {
        let ip: IpAddr = host.trim_matches(['[', ']']).parse().map_err(|_| {
            io::Error::other(format!("VPN target must be an IP address, got `{host}`"))
        })?;
        let commands = {
            let mut guard = self.link.lock().await;
            self.live_link(&mut guard).await.map_err(|e| io::Error::other(format!("{e:#}")))?.commands.clone()
        };

        info!(%host, port, "Opening TCP connection through the VPN");
        let (reply, answer) = oneshot::channel();
        commands
            .send(Command::Open { remote: SocketAddr::new(ip, port), reply })
            .await
            .map_err(|_| io::Error::other("VPN stopped"))?;
        answer.await.map_err(|_| io::Error::other("VPN stopped"))?
    }
}

impl Vpn {
    /// The round trip to the VPN peer; `None` while the VPN is down or before it is known.
    pub async fn latency(&self) -> Option<Duration> {
        let commands = self.link.lock().await.as_ref().filter(|link| link.is_alive())?.commands.clone();
        let (reply, answer) = oneshot::channel();
        commands.send(Command::Latency { reply }).await.ok()?;
        answer.await.ok()?
    }
}

/// One TCP connection of the engine, with the channels to its stream.
enum Conn {
    /// Waiting for the TCP handshake; the caller gets its stream once it completes.
    Connecting { reply: Option<oneshot::Sender<io::Result<DuplexStream>>>, deadline: tokio::time::Instant },
    Open(Pipe),
}

struct Pipe {
    /// Bytes from the stream, to the remote end.
    to_remote: mpsc::Receiver<Vec<u8>>,
    /// Bytes from the remote end, to the stream; `None` once the remote end has closed.
    from_remote: Option<mpsc::Sender<Vec<u8>>>,
    /// The chunk being written to the socket, and how much of it is written.
    pending: Vec<u8>,
    written: usize,
    /// The stream closed its writing side; close the socket when `pending` is out.
    stream_done: bool,
    closed_by_us: bool,
}

/// The driver task: moves datagrams between the UDP socket and the engine, and bytes between the
/// engine's TCP connections and the streams handed out by [`Vpn::open`].
async fn drive(
    mut engine: Engine,
    udp: UdpSocket,
    endpoint: SocketAddr,
    mut commands: mpsc::Receiver<Command>,
    ready: oneshot::Sender<anyhow::Result<()>>,
) {
    let wake = Arc::new(Notify::new());
    let mut ready = Some(ready);
    let handshake_deadline = tokio::time::Instant::now() + HANDSHAKE_TIMEOUT;
    let mut conns: HashMap<SocketHandle, Conn> = HashMap::new();
    let mut buf = vec![0; MAX_DATAGRAM];
    engine.start_handshake();

    loop {
        engine.poll();
        serve(&mut engine, &mut conns, &wake);
        // Serving moves data into the sockets; the stack has to send it.
        engine.poll();

        while let Some(datagram) = engine.wire_out.pop_front() {
            if let Err(e) = udp.send(&datagram).await {
                // An ICMP error from an unreachable peer surfaces here; WireGuard just retries.
                debug!(error = %e, "VPN UDP send failed");
            }
        }

        if ready.is_some() {
            if engine.is_established() {
                if let Some(ready) = ready.take() {
                    let _ = ready.send(Ok(()));
                }
            } else if tokio::time::Instant::now() >= handshake_deadline {
                if let Some(ready) = ready.take() {
                    let _ = ready.send(Err(anyhow::anyhow!(
                        "VPN handshake timed out (check the endpoint {endpoint}, the keys and that the peer is reachable)"
                    )));
                }
                return;
            }
        }

        let delay = engine.poll_delay();
        tokio::select! {
            received = udp.recv(&mut buf) => {
                match received {
                    Ok(len) => engine.handle_datagram(&buf[..len], endpoint.ip()),
                    Err(e) => debug!(error = %e, "VPN UDP receive failed"),
                }
                // Take what else is waiting before the stack runs.
                for _ in 0..DATAGRAM_BATCH {
                    match udp.try_recv(&mut buf) {
                        Ok(len) => engine.handle_datagram(&buf[..len], endpoint.ip()),
                        Err(_) => break,
                    }
                }
            }
            command = commands.recv() => match command {
                Some(Command::Open { remote, reply }) => match engine.open_tcp(remote) {
                    Ok(handle) => {
                        let deadline = tokio::time::Instant::now() + CONNECT_TIMEOUT;
                        conns.insert(handle, Conn::Connecting { reply: Some(reply), deadline });
                    }
                    Err(e) => {
                        let _ = reply.send(Err(io::Error::other(format!("{e:#}"))));
                    }
                },
                Some(Command::Latency { reply }) => {
                    let _ = reply.send(engine.latency());
                }
                // The `Link` was dropped.
                None => return,
            },
            () = wake.notified() => {}
            () = tokio::time::sleep(delay) => {}
        }
    }
}

/// Moves data between the TCP connections and their streams, and finishes the ones that ended.
fn serve(engine: &mut Engine, conns: &mut HashMap<SocketHandle, Conn>, wake: &Arc<Notify>) {
    let mut finished = Vec::new();
    for (&handle, conn) in conns.iter_mut() {
        let socket = engine.tcp(handle);
        match conn {
            Conn::Connecting { reply, deadline } => match socket.state() {
                tcp::State::Established => {
                    let (near, far) = tokio::io::duplex(STREAM_BUFFER);
                    let pipe = bridge(far, Arc::clone(wake));
                    if let Some(reply) = reply.take() {
                        let _ = reply.send(Ok(near));
                    }
                    *conn = Conn::Open(pipe);
                }
                tcp::State::Closed | tcp::State::TimeWait => {
                    if let Some(reply) = reply.take() {
                        let _ = reply.send(Err(io::Error::new(io::ErrorKind::ConnectionRefused, "VPN TCP connection refused")));
                    }
                    finished.push(handle);
                }
                _ if tokio::time::Instant::now() >= *deadline => {
                    socket.abort();
                    if let Some(reply) = reply.take() {
                        let _ = reply.send(Err(io::Error::new(io::ErrorKind::TimedOut, "VPN TCP connection timed out")));
                    }
                    finished.push(handle);
                }
                _ => {}
            },
            Conn::Open(pipe) => {
                serve_open(socket, pipe);
                if matches!(socket.state(), tcp::State::Closed | tcp::State::TimeWait) {
                    finished.push(handle);
                }
            }
        }
    }
    for handle in finished {
        conns.remove(&handle);
        engine.sockets.remove(handle);
    }
}

fn serve_open(socket: &mut tcp::Socket<'static>, pipe: &mut Pipe) {
    // Remote end -> stream.
    if let Some(sender) = &pipe.from_remote {
        while socket.can_recv() {
            match sender.try_reserve() {
                Ok(permit) => {
                    let chunk = socket.recv(|data| {
                        let len = data.len().min(CHUNK);
                        (len, data[..len].to_vec())
                    });
                    match chunk {
                        Ok(chunk) => permit.send(chunk),
                        Err(_) => break,
                    }
                }
                // The stream is full: leave the data in the socket, which closes the TCP window.
                Err(mpsc::error::TrySendError::Full(())) => break,
                // The stream was dropped.
                Err(mpsc::error::TrySendError::Closed(())) => {
                    if !pipe.closed_by_us {
                        socket.close();
                        pipe.closed_by_us = true;
                    }
                    break;
                }
            }
        }
        if !socket.may_recv() && !socket.can_recv() {
            // The remote end is done: the stream reads end-of-file.
            pipe.from_remote = None;
        }
    }

    // Stream -> remote end.
    while !pipe.closed_by_us {
        if pipe.written == pipe.pending.len() {
            match pipe.to_remote.try_recv() {
                Ok(chunk) => {
                    pipe.pending = chunk;
                    pipe.written = 0;
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    pipe.stream_done = true;
                    break;
                }
            }
        }
        if !socket.may_send() {
            break;
        }
        match socket.send_slice(&pipe.pending[pipe.written..]) {
            Ok(0) | Err(_) => break,
            Ok(len) => pipe.written += len,
        }
    }
    if pipe.stream_done && pipe.written == pipe.pending.len() && !pipe.closed_by_us {
        socket.close();
        pipe.closed_by_us = true;
    }
}

/// Connects `far`, the driver's end of a stream, to a [`Pipe`]. Wakes the driver whenever the
/// stream has data or has room again.
fn bridge(far: DuplexStream, wake: Arc<Notify>) -> Pipe {
    let (mut reader, mut writer) = tokio::io::split(far);
    let (to_remote, to_remote_rx) = mpsc::channel::<Vec<u8>>(CHANNEL_DEPTH);
    let (from_remote_tx, mut from_remote) = mpsc::channel::<Vec<u8>>(CHANNEL_DEPTH);

    let reader_wake = Arc::clone(&wake);
    tokio::spawn(async move {
        let mut buf = vec![0; CHUNK];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(len) => {
                    if to_remote.send(buf[..len].to_vec()).await.is_err() {
                        break;
                    }
                    reader_wake.notify_one();
                }
            }
        }
        // Dropping the sender tells the driver the stream is done.
        drop(to_remote);
        reader_wake.notify_one();
    });
    tokio::spawn(async move {
        while let Some(chunk) = from_remote.recv().await {
            if writer.write_all(&chunk).await.is_err() {
                break;
            }
            wake.notify_one();
        }
        let _ = writer.shutdown().await;
        // Closing the channel tells the driver nobody reads any more.
        drop(from_remote);
        wake.notify_one();
    });

    Pipe {
        to_remote: to_remote_rx,
        from_remote: Some(from_remote_tx),
        pending: Vec::new(),
        written: 0,
        stream_done: false,
        closed_by_us: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLIENT_KEY: [u8; WG_KEY_LEN] = [1; WG_KEY_LEN];
    const SERVER_KEY: [u8; WG_KEY_LEN] = [2; WG_KEY_LEN];
    const CLIENT_IP: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 2));
    const SERVER_IP: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 1));
    const PEER_UDP_ADDR: IpAddr = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);

    fn public(private: [u8; WG_KEY_LEN]) -> [u8; WG_KEY_LEN] {
        PublicKey::from(&StaticSecret::from(private)).to_bytes()
    }

    /// A client and a server engine that exchange datagrams in memory.
    fn pair() -> (Engine, Engine) {
        let client = Engine::new(&EngineConfig {
            private_key: CLIENT_KEY,
            peer_public_key: public(SERVER_KEY),
            keepalive: None,
            addresses: vec![CLIENT_IP],
        });
        let server = Engine::new(&EngineConfig {
            private_key: SERVER_KEY,
            peer_public_key: public(CLIENT_KEY),
            keepalive: None,
            addresses: vec![SERVER_IP],
        });
        (client, server)
    }

    /// Runs both engines and carries their datagrams across until nothing moves.
    fn pump(a: &mut Engine, b: &mut Engine) {
        for _ in 0..200 {
            a.poll();
            b.poll();
            if a.wire_out.is_empty() && b.wire_out.is_empty() {
                return;
            }
            while let Some(datagram) = a.wire_out.pop_front() {
                b.handle_datagram(&datagram, PEER_UDP_ADDR);
            }
            while let Some(datagram) = b.wire_out.pop_front() {
                a.handle_datagram(&datagram, PEER_UDP_ADDR);
            }
        }
        panic!("the engines never went quiet");
    }

    fn listen(server: &mut Engine, port: u16) -> SocketHandle {
        let mut socket = tcp::Socket::new(tcp::SocketBuffer::new(vec![0; 4096]), tcp::SocketBuffer::new(vec![0; 4096]));
        socket.listen(port).expect("listen");
        server.sockets.add(socket)
    }

    #[test]
    fn two_engines_complete_a_handshake_and_carry_a_tcp_connection() {
        let (mut client, mut server) = pair();
        let listener = listen(&mut server, 8080);
        client.start_handshake();
        let connection = client.open_tcp(SocketAddr::new(SERVER_IP, 8080)).expect("open");
        pump(&mut client, &mut server);

        assert!(client.is_established() && server.is_established());
        assert_eq!(client.tcp(connection).state(), tcp::State::Established);
        assert_eq!(server.tcp(listener).state(), tcp::State::Established);

        client.tcp(connection).send_slice(b"hello").expect("send");
        pump(&mut client, &mut server);
        let mut got = [0; 16];
        let len = server.tcp(listener).recv_slice(&mut got).expect("recv");
        assert_eq!(&got[..len], b"hello");

        server.tcp(listener).send_slice(b"world").expect("send");
        pump(&mut client, &mut server);
        let len = client.tcp(connection).recv_slice(&mut got).expect("recv");
        assert_eq!(&got[..len], b"world");
    }

    #[test]
    fn a_peer_with_another_key_never_completes_the_handshake() {
        let (mut client, _) = pair();
        let mut stranger = Engine::new(&EngineConfig {
            private_key: [3; WG_KEY_LEN],
            peer_public_key: public(CLIENT_KEY),
            keepalive: None,
            addresses: vec![SERVER_IP],
        });
        client.start_handshake();
        pump(&mut client, &mut stranger);
        assert!(!client.is_established());
    }

    #[test]
    fn a_closed_port_refuses_the_connection() {
        let (mut client, mut server) = pair();
        client.start_handshake();
        let connection = client.open_tcp(SocketAddr::new(SERVER_IP, 9)).expect("open");
        pump(&mut client, &mut server);
        assert_eq!(client.tcp(connection).state(), tcp::State::Closed);
    }

    #[test]
    fn connecting_needs_an_address_of_the_same_family() {
        let (mut client, _) = pair();
        let v6 = SocketAddr::new(IpAddr::V6(std::net::Ipv6Addr::LOCALHOST), 80);
        assert!(client.open_tcp(v6).is_err());
    }

    /// A WireGuard peer on a loopback UDP socket that echoes every TCP connection to port 7.
    async fn echo_peer(udp: UdpSocket) {
        let mut engine = pair().1;
        let mut buf = vec![0; MAX_DATAGRAM];
        let mut echo = vec![0; CHUNK];
        let mut client_addr = None;
        let mut waiting = listen(&mut engine, 7);
        let mut active: Vec<SocketHandle> = Vec::new();
        loop {
            engine.poll();
            if engine.tcp(waiting).state() == tcp::State::Established {
                active.push(waiting);
                waiting = listen(&mut engine, 7);
            }
            for &handle in &active {
                let socket = engine.tcp(handle);
                let room = (socket.send_capacity() - socket.send_queue()).min(echo.len());
                if socket.can_recv() && socket.can_send() && room > 0 {
                    if let Ok(len) = socket.recv_slice(&mut echo[..room]) {
                        let _ = socket.send_slice(&echo[..len]);
                    }
                } else if !socket.may_recv() && !socket.can_recv() && socket.may_send() {
                    socket.close();
                }
            }
            if let Some(addr) = client_addr {
                while let Some(datagram) = engine.wire_out.pop_front() {
                    let _ = udp.send_to(&datagram, addr).await;
                }
            }
            tokio::select! {
                Ok((len, from)) = udp.recv_from(&mut buf) => {
                    client_addr = Some(from);
                    engine.handle_datagram(&buf[..len], from.ip());
                }
                () = tokio::time::sleep(Duration::from_millis(5)) => {}
            }
        }
    }

    fn vpn_to(endpoint: SocketAddr) -> Vpn {
        Vpn::new(VpnParams {
            endpoint: endpoint.to_string(),
            addresses: vec![CLIENT_IP],
            peer_public_key: BASE64.encode(public(SERVER_KEY)),
            private_key: BASE64.encode(CLIENT_KEY),
            keepalive: None,
        })
    }

    #[tokio::test]
    async fn a_stream_through_the_vpn_echoes_a_megabyte_and_ends_cleanly() {
        let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let endpoint = udp.local_addr().unwrap();
        let peer = tokio::spawn(echo_peer(udp));
        let vpn = vpn_to(endpoint);

        let stream = tokio::time::timeout(Duration::from_secs(10), vpn.open("10.0.0.1", 7))
            .await
            .expect("no timeout")
            .expect("open");
        let (mut reader, mut writer) = tokio::io::split(stream);
        let sent: Vec<u8> = (0..1_000_000u32).map(|i| (i % 251) as u8).collect();
        let to_send = sent.clone();
        let writing = tokio::spawn(async move {
            writer.write_all(&to_send).await.expect("write");
            writer.shutdown().await.expect("shutdown");
        });
        let mut echoed = Vec::new();
        tokio::time::timeout(Duration::from_secs(20), reader.read_to_end(&mut echoed))
            .await
            .expect("no timeout")
            .expect("read");
        writing.await.unwrap();
        assert_eq!(echoed.len(), sent.len());
        assert!(echoed == sent, "echoed bytes differ");
        peer.abort();
    }

    #[tokio::test]
    async fn a_closed_port_fails_the_open_with_connection_refused() {
        let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let endpoint = udp.local_addr().unwrap();
        let peer = tokio::spawn(echo_peer(udp));
        let vpn = vpn_to(endpoint);

        let error = vpn.open("10.0.0.1", 9).await.expect_err("refused");
        assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused, "{error}");
        // The VPN stays up for the next connection.
        assert!(vpn.open("10.0.0.1", 7).await.is_ok());
        peer.abort();
    }

    #[tokio::test]
    async fn a_host_name_is_not_a_vpn_target() {
        let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let vpn = vpn_to(udp.local_addr().unwrap());
        let error = vpn.open("server.internal", 22).await.expect_err("name");
        assert!(error.to_string().contains("must be an IP address"), "{error}");
    }

    #[test]
    fn keys_are_base64_of_32_bytes() {
        assert_eq!(decode_key(&BASE64.encode([7; 32])).expect("key"), [7; 32]);
        assert!(decode_key(&BASE64.encode([7; 31])).is_err());
        assert!(decode_key("not base64!").is_err());
    }

    #[test]
    fn endpoints_need_a_host_and_a_port() {
        assert!(is_valid_endpoint("vpn.example.com:51820"));
        assert!(is_valid_endpoint("[2001:db8::1]:51820"));
        assert!(!is_valid_endpoint("vpn.example.com"));
        assert!(!is_valid_endpoint("vpn.example.com:0"));
        assert!(!is_valid_endpoint(":51820"));
    }

    fn sample_config(extra_peer: &str) -> String {
        format!(
            "[Interface]\nPrivateKey = {}\nAddress = 10.8.0.2/32, fd00::2/128\nDNS = 10.8.0.1\n\n\
             [Peer]\nPublicKey = {}\nEndpoint = vpn.example.com:51820\nAllowedIPs = 10.8.0.0/24 # office\n{extra_peer}",
            BASE64.encode([1; 32]),
            BASE64.encode([2; 32]),
        )
    }

    #[test]
    fn a_wg_quick_file_is_read() {
        let config = parse_config(&sample_config("PersistentKeepalive = 25\n")).expect("parse");
        assert_eq!(config.endpoint, "vpn.example.com:51820");
        assert_eq!(config.keepalive, Some(25));
        assert_eq!(config.addresses, ["10.8.0.2".parse::<IpAddr>().unwrap(), "fd00::2".parse().unwrap()]);
        assert_eq!(decode_key(&config.private_key).unwrap(), [1; 32]);
        assert_eq!(decode_key(&config.peer_public_key).unwrap(), [2; 32]);
    }

    #[test]
    fn a_preshared_key_is_refused_instead_of_ignored() {
        let error = parse_config(&sample_config(&format!("PresharedKey = {}\n", BASE64.encode([9; 32])))).unwrap_err();
        assert!(error.to_string().contains("PresharedKey"));
    }

    #[test]
    fn an_incomplete_file_names_what_is_missing() {
        let error = parse_config("[Interface]\nAddress = 10.8.0.2/32\n").unwrap_err();
        assert!(error.to_string().contains("[Peer]"), "{error}");
    }
}
