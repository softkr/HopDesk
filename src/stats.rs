//! Traffic counters of a session process, reported to the launcher once a second.
//!
//! Streams to the target are wrapped in [`Counted`]; the totals go out as status lines (see
//! [`crate::status::emit_stats`]) from the first counted stream on.

use std::io;
use std::pin::Pin;
use std::sync::Once;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::status;

/// How often the totals are reported.
pub const REPORT_INTERVAL: Duration = Duration::from_secs(1);

static RECEIVED: AtomicU64 = AtomicU64::new(0);
static SENT: AtomicU64 = AtomicU64::new(0);
static REPORTER: Once = Once::new();

/// A stream whose bytes add to the session's totals.
pub struct Counted<S>(S);

impl<S> Counted<S> {
    pub fn new(stream: S) -> Self {
        REPORTER.call_once(|| {
            if std::env::var_os(status::ENV_VAR).is_some() {
                std::thread::spawn(report);
            }
        });
        Self(stream)
    }
}

/// Reports the totals for as long as the session process lives.
fn report() {
    loop {
        status::emit_stats(RECEIVED.load(Ordering::Relaxed), SENT.load(Ordering::Relaxed));
        std::thread::sleep(REPORT_INTERVAL);
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Counted<S> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let poll = Pin::new(&mut self.0).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = poll {
            RECEIVED.fetch_add((buf.filled().len() - before) as u64, Ordering::Relaxed);
        }
        poll
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Counted<S> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        let poll = Pin::new(&mut self.0).poll_write(cx, buf);
        if let Poll::Ready(Ok(written)) = poll {
            SENT.fetch_add(written as u64, Ordering::Relaxed);
        }
        poll
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use super::*;

    #[tokio::test]
    async fn bytes_through_a_counted_stream_add_to_the_totals() {
        let (near, mut far) = tokio::io::duplex(64);
        let mut near = Counted::new(near);
        // Other tests share the process-wide totals, so compare against what this one adds.
        let (received, sent) = (RECEIVED.load(Ordering::Relaxed), SENT.load(Ordering::Relaxed));

        near.write_all(b"hello").await.expect("write");
        far.write_all(b"hi!").await.expect("write");
        let mut buf = [0; 3];
        near.read_exact(&mut buf).await.expect("read");

        assert!(SENT.load(Ordering::Relaxed) - sent >= 5);
        assert!(RECEIVED.load(Ordering::Relaxed) - received >= 3);
    }
}
