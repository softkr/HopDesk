//! Progress reports from a session process (`hopdesk connect`) to the launcher window.
//!
//! The launcher sets [`ENV_VAR`] and reads stderr; lines starting with [`PREFIX`] are status
//! updates (`@hopdesk <phase>\t<detail>`), everything else is ordinary log output.

pub const ENV_VAR: &str = "HOPDESK_LAUNCHER";
pub const PREFIX: &str = "@hopdesk ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Connecting / authenticating to the SSH jump host.
    Ssh,
    /// SSH is up; connection to the target (RDP, SSH, VNC) in progress.
    Remote,
    /// Connected to the target (session window is showing).
    Connected,
    /// Logon on the target finished.
    LoggedIn,
    /// Connection dropped, auto-reconnect in progress.
    Reconnecting,
    /// Connection failed; detail carries the error.
    Failed,
    /// Session ended; detail carries the reason.
    Ended,
}

impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Phase::Ssh => "ssh",
            Phase::Remote => "remote",
            Phase::Connected => "connected",
            Phase::LoggedIn => "logged-in",
            Phase::Reconnecting => "reconnecting",
            Phase::Failed => "failed",
            Phase::Ended => "ended",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "ssh" => Phase::Ssh,
            // `rdp`: the name used before other protocols existed.
            "remote" | "rdp" => Phase::Remote,
            "connected" => Phase::Connected,
            "logged-in" => Phase::LoggedIn,
            "reconnecting" => Phase::Reconnecting,
            "failed" => Phase::Failed,
            "ended" => Phase::Ended,
            _ => return None,
        })
    }
}

/// Reports a phase change when running under the launcher; no-op otherwise.
pub fn emit(phase: Phase, detail: &str) {
    if std::env::var_os(ENV_VAR).is_some() {
        use std::io::Write as _;
        let detail = detail.replace(['\n', '\r'], " | ");
        // Ignore write errors: the launcher may have been closed while the session keeps running.
        let _ = writeln!(std::io::stderr(), "{PREFIX}{}\t{detail}", phase.as_str());
    }
}

/// What follows [`PREFIX`] on a traffic report: `stats\t<bytes received> <bytes sent>`.
const STATS: &str = "stats\t";

/// Reports the session's traffic totals when running under the launcher; no-op otherwise.
pub fn emit_stats(received: u64, sent: u64) {
    if std::env::var_os(ENV_VAR).is_some() {
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), "{PREFIX}{STATS}{received} {sent}");
    }
}

/// What follows [`PREFIX`] on a latency report: `latency\t<microseconds>`.
const LATENCY: &str = "latency\t";

/// Reports the round trip to the tunnel's far end when running under the launcher.
pub fn emit_latency(latency: std::time::Duration) {
    if std::env::var_os(ENV_VAR).is_some() {
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), "{PREFIX}{LATENCY}{}", latency.as_micros());
    }
}

pub fn parse_latency(line: &str) -> Option<std::time::Duration> {
    let micros = line.strip_prefix(PREFIX)?.strip_prefix(LATENCY)?;
    Some(std::time::Duration::from_micros(micros.parse().ok()?))
}

/// Parses a traffic report into `(received, sent)` byte totals.
pub fn parse_stats(line: &str) -> Option<(u64, u64)> {
    let (received, sent) = line.strip_prefix(PREFIX)?.strip_prefix(STATS)?.split_once(' ')?;
    Some((received.parse().ok()?, sent.parse().ok()?))
}

/// Parses a stderr line; `None` for ordinary log output.
pub fn parse(line: &str) -> Option<(Phase, String)> {
    let rest = line.strip_prefix(PREFIX)?;
    let (phase, detail) = rest.split_once('\t').unwrap_or((rest, ""));
    Some((Phase::parse(phase)?, detail.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_phase_round_trips_and_accepts_the_old_name() {
        assert_eq!(Phase::parse(Phase::Remote.as_str()), Some(Phase::Remote));
        assert_eq!(parse("@hopdesk rdp\t"), Some((Phase::Remote, String::new())));
    }

    #[test]
    fn a_traffic_report_is_not_a_phase() {
        assert_eq!(parse_stats("@hopdesk stats\t1024 16"), Some((1024, 16)));
        assert_eq!(parse("@hopdesk stats\t1024 16"), None);
        assert_eq!(parse_stats("@hopdesk connected\t"), None);
        assert_eq!(parse_stats("@hopdesk stats\tmany few"), None);
        assert_eq!(parse_latency("@hopdesk latency\t12500"), Some(std::time::Duration::from_micros(12_500)));
        assert_eq!(parse("@hopdesk latency\t12500"), None);
    }
}
