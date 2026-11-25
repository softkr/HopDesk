//! Claude through the Anthropic Messages API and its computer use toolset
//! (`computer_toolset_20260801`); also open models behind Claude-compatible servers, which get the
//! same actions as plain function tools.
//!
//! The conversation only grows: replies go back unchanged (thinking blocks included), and old
//! screenshots are cleared by the API's context management instead of by editing the history.

use anyhow::bail;
use serde_json::{Value, json};

use super::functions::{self, parse};
use super::http::{self, Api};
use super::{Call, CallResult, Output, Provider, Reply, SYSTEM};
use crate::agent::action::{Coordinates, Scale};
use crate::agent::capture::Shot;

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2026-10-21";
/// Old tool results cleared on the server.
const CONTEXT_BETA: &str = "context-management-2025-06-27";
/// Refusals retried on a fallback model (not offered for Haiku).
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";
/// Per reply; non-streaming requests stay well within the HTTP timeout at this size.
const MAX_TOKENS: u32 = 16_000;
/// Screen work is many short steps: medium keeps each one quick.
const EFFORT: &str = "medium";

/// The members HopDesk does not carry out: holding a key needs timing the session cannot give.
fn toolset() -> Value {
    json!({
        "type": "computer_toolset_20260801",
        "configs": { "hold_key": { "enabled": false } },
    })
}

pub struct Claude {
    api: Api,
    key: String,
    model: String,
    url: String,
    /// Positions as plain function tools in these coordinates, for a compatible server; `None`
    /// is Anthropic's API with the computer use toolset.
    functions: Option<Coordinates>,
    messages: Vec<Value>,
    /// The toolset name of each call awaiting its result, echoed back in the result.
    pending: Vec<(String, Option<String>)>,
}

impl Claude {
    /// Anthropic's API.
    pub fn new(key: String, model: String) -> anyhow::Result<Self> {
        Ok(Self {
            api: Api::new("Anthropic API")?,
            key,
            model,
            url: API_URL.to_owned(),
            functions: None,
            messages: Vec::new(),
            pending: Vec::new(),
        })
    }

    /// A server with the Messages API of its own (an open model), given by its base URL.
    pub fn compatible(base_url: &str, key: String, model: String, coordinates: Coordinates) -> anyhow::Result<Self> {
        Ok(Self {
            api: Api::new("AI server")?,
            key,
            model,
            url: http::endpoint(base_url, "messages"),
            functions: Some(coordinates),
            messages: Vec::new(),
            pending: Vec::new(),
        })
    }

    /// Server-side refusal fallback exists on Anthropic's API for Opus and Sonnet, not Haiku.
    fn has_fallback(&self) -> bool {
        self.functions.is_none() && !self.model.contains("haiku")
    }

    fn betas(&self) -> String {
        if self.has_fallback() { format!("{CONTEXT_BETA},{FALLBACK_BETA}") } else { CONTEXT_BETA.to_owned() }
    }

    fn body(&self) -> Value {
        let Some(coordinates) = self.functions else {
            let mut body = json!({
                "model": self.model,
                "max_tokens": MAX_TOKENS,
                "system": SYSTEM,
                "tools": [toolset()],
                "messages": self.messages,
                "output_config": { "effort": EFFORT },
                "cache_control": { "type": "ephemeral" },
                "context_management": { "edits": [{ "type": "clear_tool_uses_20250919" }] },
            });
            if self.has_fallback() {
                body["fallbacks"] = json!("default");
            }
            return body;
        };
        // A compatible server gets the plain Messages API only.
        let tools: Vec<Value> = functions::functions(coordinates)
            .into_iter()
            .map(|f| json!({ "name": f.name, "description": f.description, "input_schema": f.parameters }))
            .collect();
        json!({
            "model": self.model,
            "max_tokens": MAX_TOKENS,
            "system": SYSTEM,
            "tools": tools,
            "messages": self.messages,
        })
    }

    /// Appends `blocks` to the last message when it is the person's, else starts one.
    fn push_user(&mut self, blocks: Vec<Value>) {
        if let Some(last) = self.messages.last_mut()
            && last["role"] == "user"
            && let Some(content) = last["content"].as_array_mut()
        {
            content.extend(blocks);
            return;
        }
        self.messages.push(json!({ "role": "user", "content": blocks }));
    }

    async fn send(&self) -> anyhow::Result<Value> {
        let betas = self.betas();
        let mut headers = vec![("anthropic-version", API_VERSION)];
        if self.functions.is_none() {
            headers.push(("anthropic-beta", betas.as_str()));
        }
        if !self.key.is_empty() {
            headers.push(("x-api-key", self.key.as_str()));
        }
        self.api.post(&self.url, &headers, &self.body()).await
    }

    /// Takes the model's reply into the conversation and reads its calls.
    fn receive(&mut self, response: &Value, scale: Scale) -> anyhow::Result<Reply> {
        let content = response["content"].as_array().cloned().unwrap_or_default();
        if response["stop_reason"] == "refusal" {
            let explanation = response["stop_details"]["explanation"].as_str().unwrap_or("");
            bail!("Claude declined this request. {explanation}");
        }
        let mut reply = Reply::default();
        for block in &content {
            match block["type"].as_str() {
                Some("text") => {
                    let text = block["text"].as_str().unwrap_or("").trim();
                    if !text.is_empty() {
                        if !reply.text.is_empty() {
                            reply.text.push('\n');
                        }
                        reply.text.push_str(text);
                    }
                }
                Some("tool_use") => {
                    let id = block["id"].as_str().unwrap_or("").to_owned();
                    let toolset = block["toolset_name"].as_str().map(str::to_owned);
                    let name = block["name"].as_str().unwrap_or("");
                    // Toolset members on Anthropic's API; the same names as functions elsewhere.
                    let known = toolset.as_deref() == Some("computer") || (self.functions.is_some() && toolset.is_none());
                    let action = if known {
                        parse(name, &block["input"], scale).map_err(|error| format!("{error:#}"))
                    } else {
                        Err(format!("unknown tool {name:?}"))
                    };
                    self.pending.push((id.clone(), toolset));
                    reply.calls.push(Call::one(id, action));
                }
                _ => {}
            }
        }
        self.messages.push(json!({ "role": "assistant", "content": content }));
        Ok(reply)
    }
}

impl Provider for Claude {
    fn user(&mut self, text: &str, screen: Option<&Shot>) {
        // The words before the screenshot: knowing what to look for, Claude clicks more
        // accurately.
        let mut blocks = vec![json!({ "type": "text", "text": text })];
        blocks.extend(screen.map(image));
        self.push_user(blocks);
    }

    fn results(&mut self, results: Vec<CallResult>, screen: Option<&Shot>) {
        let count = results.len();
        let blocks = results
            .into_iter()
            .enumerate()
            .map(|(index, result)| {
                let toolset = self
                    .pending
                    .iter()
                    .find(|(id, _)| *id == result.id)
                    .and_then(|(_, toolset)| toolset.clone());
                let (mut content, is_error) = match result.outcome {
                    Ok(Output::Text(text)) => (vec![json!({ "type": "text", "text": text })], false),
                    Ok(Output::Image(shot)) => (vec![image(&shot)], false),
                    Err(error) => (vec![json!({ "type": "text", "text": error })], true),
                };
                if index + 1 == count
                    && let Some(screen) = screen
                {
                    content.push(image(screen));
                }
                let mut block = json!({
                    "type": "tool_result",
                    "tool_use_id": result.id,
                    "content": content,
                });
                if is_error {
                    block["is_error"] = json!(true);
                }
                if let Some(toolset) = toolset {
                    block["toolset_name"] = json!(toolset);
                }
                block
            })
            .collect();
        self.pending.clear();
        self.push_user(blocks);
    }

    async fn next(&mut self, scale: Scale) -> anyhow::Result<Reply> {
        let response = self.send().await?;
        self.receive(&response, scale)
    }
}

fn image(shot: &Shot) -> Value {
    json!({
        "type": "image",
        "source": { "type": "base64", "media_type": "image/png", "data": http::base64(shot) },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::action::{self, Action, Button, Direction, Point, Region, keysym};

    fn claude() -> Claude {
        Claude::new("test-key".into(), DEFAULT_MODEL.into()).unwrap()
    }

    /// Screenshots at half the screen size.
    fn scale() -> Scale {
        Scale::new((2560, 1600), (1280, 800), Coordinates::Pixels)
    }

    fn shot() -> Shot {
        Shot {
            png: vec![1, 2, 3],
            width: 1280,
            height: 800,
        }
    }

    fn tool_use(id: &str, name: &str, input: Value) -> Value {
        json!({ "type": "tool_use", "id": id, "name": name, "input": input, "toolset_name": "computer" })
    }

    fn response(content: Vec<Value>) -> Value {
        json!({ "role": "assistant", "content": content, "stop_reason": "tool_use" })
    }

    /// The API client and IronRDP's TLS need a process crypto provider. Checked in a fresh test
    /// process, since any earlier TLS use in this one would install it.
    #[test]
    fn the_client_starts_in_a_fresh_process() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "agent::provider::claude::tests::fresh_process_client", "--ignored"])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success() && stdout.contains("1 passed"), "{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
    }

    #[test]
    #[ignore = "run by the_client_starts_in_a_fresh_process"]
    fn fresh_process_client() {
        claude();
        // RDP connections build their TLS config the same way afterwards.
        let _ = rustls::ClientConfig::builder();
    }

    #[test]
    fn the_request_uses_the_computer_toolset() {
        let mut claude = claude();
        claude.user("open notepad", Some(&shot()));
        let body = claude.body();
        assert_eq!(body["model"], DEFAULT_MODEL);
        assert_eq!(body["tools"][0]["type"], "computer_toolset_20260801");
        assert!(body["tools"][0].get("name").is_none());
        assert_eq!(body["fallbacks"], "default");
        // The task comes before the screen: Claude clicks more accurately knowing what it looks
        // for when it sees the screenshot.
        let content = &body["messages"][0]["content"];
        assert_eq!(content[0]["text"], "open notepad");
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"]["data"], "AQID");
    }

    #[test]
    fn a_compatible_server_gets_function_tools_and_the_plain_api() {
        let mut server = Claude::compatible("http://localhost:8000", String::new(), "qwen".into(), Coordinates::Normalized).unwrap();
        assert_eq!(server.url, "http://localhost:8000/v1/messages");
        let body = server.body();
        assert_eq!(body["tools"][1]["name"], "left_click");
        assert!(body["tools"][1]["input_schema"]["properties"]["coordinate"].is_object());
        assert!(body.get("fallbacks").is_none() && body.get("context_management").is_none());
        // Its function calls carry no toolset name.
        let reply = server
            .receive(&response(vec![json!({ "type": "tool_use", "id": "a", "name": "left_click", "input": { "coordinate": [500, 500] } })]), Scale::new((2000, 1000), (1000, 500), Coordinates::Normalized))
            .unwrap();
        assert_eq!(
            reply.calls[0].actions.as_ref().unwrap()[0],
            Action::Click { at: Some(Point { x: 1000, y: 500 }), button: Button::Left, count: 1, modifiers: vec![] }
        );
    }

    #[test]
    fn haiku_is_asked_without_a_refusal_fallback() {
        let haiku = Claude::new("test-key".into(), "claude-haiku-5-5".into()).unwrap();
        assert!(haiku.body().get("fallbacks").is_none());
        assert!(!haiku.betas().contains(FALLBACK_BETA));
        assert!(claude().betas().contains(FALLBACK_BETA));
    }

    #[test]
    fn calls_are_read_onto_the_remote_screen() {
        let mut claude = claude();
        claude.user("go", None);
        let reply = claude
            .receive(
                &response(vec![
                    json!({ "type": "thinking", "thinking": "", "signature": "sig" }),
                    json!({ "type": "text", "text": "Opening the menu." }),
                    tool_use("a", "left_click", json!({ "coordinate": [100, 50], "text": "shift" })),
                    tool_use("b", "key", json!({ "text": "ctrl+s", "repeat": 2 })),
                    tool_use("c", "scroll", json!({ "scroll_direction": "down", "scroll_amount": 3 })),
                    tool_use("d", "zoom", json!({ "region": [0, 0, 640, 400] })),
                ]),
                scale(),
            )
            .unwrap();
        assert_eq!(reply.text, "Opening the menu.");
        let actions: Vec<_> = reply.calls.into_iter().flat_map(|call| call.actions.unwrap()).collect();
        assert_eq!(
            actions,
            [
                Action::Click {
                    at: Some(Point { x: 201, y: 101 }),
                    button: Button::Left,
                    count: 1,
                    modifiers: vec![keysym::SHIFT],
                },
                Action::Key {
                    combo: action::parse_combo("ctrl+s").unwrap(),
                    repeat: 2,
                },
                Action::Scroll {
                    at: None,
                    direction: Direction::Down,
                    amount: 3,
                    modifiers: vec![],
                },
                Action::Zoom {
                    region: Region {
                        left: 0,
                        top: 0,
                        right: 1280,
                        bottom: 800
                    },
                },
            ]
        );
        // The reply goes back unchanged, thinking included.
        assert_eq!(claude.messages[1]["content"][0]["signature"], "sig");
    }

    #[test]
    fn calls_the_session_cannot_carry_out_become_errors() {
        let mut claude = claude();
        let reply = claude
            .receive(
                &response(vec![
                    tool_use("a", "left_click", json!({ "coordinate": [5000, 50] })),
                    tool_use("b", "hold_key", json!({ "text": "a", "duration": 1 })),
                    json!({ "type": "tool_use", "id": "c", "name": "bash", "input": {} }),
                ]),
                scale(),
            )
            .unwrap();
        assert!(reply.calls.iter().all(|call| call.actions.is_err()));
    }

    #[test]
    fn results_echo_the_toolset_and_end_with_the_screen() {
        let mut claude = claude();
        claude.user("go", None);
        claude
            .receive(
                &response(vec![
                    tool_use("a", "left_click", json!({ "coordinate": [1, 1] })),
                    tool_use("b", "type", json!({ "text": "hi" })),
                ]),
                scale(),
            )
            .unwrap();
        claude.results(
            vec![
                CallResult {
                    id: "a".into(),
                    outcome: Ok(Output::Text("OK".into())),
                },
                CallResult {
                    id: "b".into(),
                    outcome: Err("Not executed".into()),
                },
            ],
            Some(&shot()),
        );
        claude.user("now save it", None);
        let content = claude.messages[2]["content"].as_array().unwrap();
        assert_eq!(content.len(), 3, "results and the next message share one turn");
        assert_eq!(content[0]["toolset_name"], "computer");
        assert!(content[0].get("is_error").is_none());
        assert_eq!(content[1]["is_error"], true);
        assert_eq!(content[1]["content"][1]["type"], "image");
        assert_eq!(content[2]["text"], "now save it");
    }

    #[test]
    fn a_refusal_is_an_error_and_stays_out_of_the_conversation() {
        let mut claude = claude();
        claude.user("go", None);
        let refusal = json!({
            "content": [],
            "stop_reason": "refusal",
            "stop_details": { "type": "refusal", "category": "cyber", "explanation": "Not allowed." },
        });
        let error = claude.receive(&refusal, scale()).unwrap_err();
        assert!(error.to_string().contains("Not allowed."));
        assert_eq!(claude.messages.len(), 1);
    }
}
