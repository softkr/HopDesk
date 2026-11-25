//! HTTPS requests to the AI providers: JSON in and out, retried after rate limits, overloads and
//! dropped connections.

use core::time::Duration;

use anyhow::{Context as _, bail};
use base64::Engine as _;
use serde_json::Value;
use tracing::warn;

use crate::agent::capture::Shot;

/// Model turns can take minutes at higher effort.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
/// Attempts after a rate limit, overload or server error.
const MAX_RETRIES: u32 = 3;
const RETRY_DELAY: Duration = Duration::from_secs(2);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(60);

pub(super) struct Api {
    http: reqwest::Client,
    /// The provider's name in errors, e.g. "OpenAI API".
    name: &'static str,
}

impl Api {
    pub fn new(name: &'static str) -> anyhow::Result<Self> {
        // reqwest uses the process default provider and panics without one; an earlier install
        // (the same provider) is fine.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("create HTTP client")?;
        Ok(Self { http, name })
    }

    /// GETs a JSON document, such as a model list.
    pub async fn get(&self, url: &str, headers: &[(&str, &str)]) -> anyhow::Result<Value> {
        let name = self.name;
        let mut request = self.http.get(url);
        for (header, value) in headers {
            request = request.header(*header, *value);
        }
        let response = request.send().await.map_err(|error| anyhow::Error::new(error).context(format!("reach the {name}")))?;
        let status = response.status();
        let text = response.text().await.with_context(|| format!("read the {name} response"))?;
        if !status.is_success() {
            bail!("{name} error {status}: {}", error_message(&text));
        }
        serde_json::from_str(&text).with_context(|| format!("parse the {name} response"))
    }

    /// POSTs `body` and returns the JSON reply; an error reply becomes an error with the
    /// provider's message.
    pub async fn post(&self, url: &str, headers: &[(&str, &str)], body: &Value) -> anyhow::Result<Value> {
        let name = self.name;
        let mut attempt = 0;
        loop {
            let mut request = self.http.post(url).json(body);
            for (header, value) in headers {
                request = request.header(*header, *value);
            }
            let response = match request.send().await {
                Ok(response) => response,
                Err(error) if attempt < MAX_RETRIES && (error.is_connect() || error.is_timeout()) => {
                    attempt += 1;
                    warn!(%error, attempt, "{name} request failed; retrying");
                    tokio::time::sleep(RETRY_DELAY * attempt).await;
                    continue;
                }
                Err(error) => return Err(anyhow::Error::new(error).context(format!("reach the {name}"))),
            };
            let status = response.status();
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok()?.parse::<u64>().ok())
                .map(Duration::from_secs);
            let text = response.text().await.with_context(|| format!("read the {name} response"))?;
            if status.is_success() {
                return serde_json::from_str(&text).with_context(|| format!("parse the {name} response"));
            }
            let retryable = matches!(status.as_u16(), 429 | 500 | 502 | 503 | 504 | 529);
            if retryable && attempt < MAX_RETRIES {
                attempt += 1;
                let delay = retry_after.unwrap_or(RETRY_DELAY * attempt).min(MAX_RETRY_DELAY);
                warn!(%status, attempt, ?delay, "{name} request refused; retrying");
                tokio::time::sleep(delay).await;
                continue;
            }
            bail!("{name} error {status}: {}", error_message(&text));
        }
    }
}

/// The message of an error reply (`{"error": {"message": …}}` at every provider), else the text.
fn error_message(text: &str) -> String {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|error| {
            let error = &error["error"];
            error["message"].as_str().or_else(|| error.as_str()).map(str::to_owned)
        })
        .unwrap_or_else(|| text.chars().take(500).collect())
}

pub(super) fn base64(shot: &Shot) -> String {
    base64::engine::general_purpose::STANDARD.encode(&shot.png)
}

pub(super) fn data_url(shot: &Shot) -> String {
    format!("data:image/png;base64,{}", base64(shot))
}

/// The URL of `path` (e.g. "chat/completions") on a server given by its base URL, with or
/// without the `/v1` part or the path itself.
pub(super) fn endpoint(base: &str, path: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    if base.ends_with(&format!("/{path}")) {
        base.to_owned()
    } else if base.ends_with("/v1") {
        format!("{base}/{path}")
    } else {
        format!("{base}/v1/{path}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_accept_the_usual_base_urls() {
        for base in ["http://localhost:11434", "http://localhost:11434/", "http://localhost:11434/v1"] {
            assert_eq!(endpoint(base, "chat/completions"), "http://localhost:11434/v1/chat/completions");
        }
        let full = "https://llm.corp.example/v1/chat/completions";
        assert_eq!(endpoint(full, "chat/completions"), full);
    }

    #[test]
    fn error_replies_show_the_provider_message() {
        assert_eq!(error_message(r#"{"error":{"type":"x","message":"bad key"}}"#), "bad key");
        assert_eq!(error_message(r#"{"error":"model not found"}"#), "model not found");
        assert_eq!(error_message("Bad Gateway"), "Bad Gateway");
    }
}
