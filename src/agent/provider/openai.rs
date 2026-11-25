//! GPT through the OpenAI Responses API and its computer use tool (`computer`).
//!
//! The conversation lives on OpenAI's side: each request continues the previous response
//! (`previous_response_id`) and sends only what is new. A `computer_call` carries several actions
//! in order, and its result is always a screenshot.

use anyhow::bail;
use serde_json::{Value, json};

use super::http::{self, Api};
use super::{Call, CallResult, Output, Provider, Reply, SYSTEM};
use crate::agent::action::{self, Action, Button, Direction, Point, Scale};
use crate::agent::capture::Shot;

const API_URL: &str = "https://api.openai.com/v1/responses";
/// A model of the computer use examples in OpenAI's documentation; any model with the tool works.
pub const DEFAULT_MODEL: &str = "gpt-6.2-sol";
/// Scroll distance per wheel notch, as OpenAI's own sample runtime counts it.
const SCROLL_PER_NOTCH: i64 = 100;

pub struct OpenAi {
    api: Api,
    key: String,
    model: String,
    /// The response this conversation continues.
    previous: Option<String>,
    /// What the next request adds.
    input: Vec<Value>,
    /// Safety checks of each call awaiting its result, acknowledged once the person approved.
    pending: Vec<(String, Vec<Value>)>,
    /// The latest screen sent, for results that come without a new one.
    last_screen: Option<String>,
}

impl OpenAi {
    pub fn new(key: String, model: String) -> anyhow::Result<Self> {
        Ok(Self {
            api: Api::new("OpenAI API")?,
            key,
            model,
            previous: None,
            input: Vec::new(),
            pending: Vec::new(),
            last_screen: None,
        })
    }

    fn body(&mut self) -> Value {
        let mut body = json!({
            "model": self.model,
            "instructions": SYSTEM,
            "tools": [{ "type": "computer" }],
            "input": core::mem::take(&mut self.input),
        });
        if let Some(previous) = &self.previous {
            body["previous_response_id"] = json!(previous);
        }
        body
    }

    fn receive(&mut self, response: &Value, scale: Scale) -> anyhow::Result<Reply> {
        if let Some(id) = response["id"].as_str() {
            self.previous = Some(id.to_owned());
        }
        if response["status"] == "failed" {
            bail!("OpenAI API error: {}", response["error"]["message"].as_str().unwrap_or("the response failed"));
        }
        let mut reply = Reply::default();
        for item in response["output"].as_array().into_iter().flatten() {
            match item["type"].as_str() {
                Some("message") => {
                    for part in item["content"].as_array().into_iter().flatten() {
                        let text = part["text"].as_str().or_else(|| part["refusal"].as_str()).unwrap_or("").trim();
                        if !text.is_empty() {
                            if !reply.text.is_empty() {
                                reply.text.push('\n');
                            }
                            reply.text.push_str(text);
                        }
                    }
                }
                Some("computer_call") => {
                    let id = item["call_id"].as_str().unwrap_or("").to_owned();
                    let actions: Vec<&Value> = match item["actions"].as_array() {
                        Some(actions) => actions.iter().collect(),
                        None => vec![&item["action"]],
                    };
                    let actions = actions
                        .into_iter()
                        .map(|action| parse(action, scale))
                        .collect::<anyhow::Result<Vec<Vec<Action>>>>()
                        .map(|batch| batch.into_iter().flatten().collect())
                        .map_err(|error| format!("{error:#}"));
                    let checks: Vec<Value> = item["pending_safety_checks"].as_array().cloned().unwrap_or_default();
                    let confirm = (!checks.is_empty()).then(|| {
                        checks
                            .iter()
                            .map(|check| check["message"].as_str().or_else(|| check["code"].as_str()).unwrap_or("safety check"))
                            .collect::<Vec<_>>()
                            .join("; ")
                    });
                    self.pending.push((id.clone(), checks));
                    reply.calls.push(Call { id, actions, confirm });
                }
                _ => {}
            }
        }
        Ok(reply)
    }
}

impl Provider for OpenAi {
    fn user(&mut self, text: &str, screen: Option<&Shot>) {
        let mut content = vec![json!({ "type": "input_text", "text": text })];
        if let Some(screen) = screen {
            let url = http::data_url(screen);
            content.push(json!({ "type": "input_image", "image_url": url, "detail": "original" }));
            self.last_screen = Some(url);
        }
        self.input.push(json!({ "role": "user", "content": content }));
    }

    fn results(&mut self, results: Vec<CallResult>, screen: Option<&Shot>) {
        if let Some(screen) = screen {
            self.last_screen = Some(http::data_url(screen));
        }
        let mut errors = Vec::new();
        for result in results {
            let checks = self
                .pending
                .iter()
                .find(|(id, _)| *id == result.id)
                .map(|(_, checks)| checks.clone())
                .unwrap_or_default();
            // A call's output is always the screen; what went wrong is told in words after it.
            let screenshot = match &result.outcome {
                Ok(Output::Image(shot)) if screen.is_none() => Some(http::data_url(shot)),
                _ => self.last_screen.clone(),
            };
            let mut output = json!({
                "type": "computer_call_output",
                "call_id": result.id,
                "output": { "type": "computer_screenshot", "image_url": screenshot.unwrap_or_default(), "detail": "original" },
            });
            match result.outcome {
                // Carried out, so the person approved it: its safety checks are acknowledged.
                Ok(_) if !checks.is_empty() => output["acknowledged_safety_checks"] = json!(checks),
                Ok(_) => {}
                Err(error) => errors.push(error),
            }
            self.input.push(output);
        }
        self.pending.clear();
        if !errors.is_empty() {
            let text = format!("Some actions were not carried out: {}", errors.join(" / "));
            self.input.push(json!({ "role": "user", "content": [{ "type": "input_text", "text": text }] }));
        }
    }

    async fn next(&mut self, scale: Scale) -> anyhow::Result<Reply> {
        let body = self.body();
        let authorization = format!("Bearer {}", self.key);
        let response = self.api.post(API_URL, &[("authorization", authorization.as_str())], &body).await?;
        self.receive(&response, scale)
    }
}

/// One action of a `computer_call` as [`Action`]s on the remote screen.
fn parse(action: &Value, scale: Scale) -> anyhow::Result<Vec<Action>> {
    let point = |x: &Value, y: &Value| -> anyhow::Result<Point> {
        match (x.as_f64(), y.as_f64()) {
            (Some(x), Some(y)) => scale.point(x, y),
            _ => bail!("x and y must be numbers"),
        }
    };
    let at = || point(&action["x"], &action["y"]);
    let modifiers = || -> anyhow::Result<Vec<u32>> {
        let keys: Vec<&str> = action["keys"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        action::parse_modifiers(&keys.join("+"))
    };
    Ok(match action["type"].as_str().unwrap_or("") {
        "screenshot" => vec![Action::Screenshot],
        "click" | "double_click" => {
            let button = match action["button"].as_str().unwrap_or("left") {
                "left" => Button::Left,
                "right" => Button::Right,
                "wheel" | "middle" => Button::Middle,
                other => bail!("the {other} mouse button is not supported"),
            };
            let count = if action["type"] == "double_click" { 2 } else { 1 };
            vec![Action::Click {
                at: Some(at()?),
                button,
                count,
                modifiers: modifiers()?,
            }]
        }
        "move" => vec![Action::MouseMove { to: at()? }],
        "drag" => {
            let path: Vec<Point> = action["path"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|step| match step.as_array() {
                    Some(pair) if pair.len() == 2 => point(&pair[0], &pair[1]),
                    _ => point(&step["x"], &step["y"]),
                })
                .collect::<anyhow::Result<_>>()?;
            let (Some(from), Some(to)) = (path.first(), path.last()) else { bail!("drag needs a path") };
            vec![Action::Drag {
                from: *from,
                to: *to,
                modifiers: modifiers()?,
            }]
        }
        "scroll" => {
            let at = at()?;
            let modifiers = modifiers()?;
            let mut scrolls = Vec::new();
            for (delta, negative, positive) in [
                (action["scroll_y"].as_i64().unwrap_or(0), Direction::Up, Direction::Down),
                (action["scroll_x"].as_i64().unwrap_or(0), Direction::Left, Direction::Right),
            ] {
                if delta == 0 {
                    continue;
                }
                let notches = (delta.abs() / SCROLL_PER_NOTCH).clamp(1, i64::from(action::MAX_REPEAT));
                scrolls.push(Action::Scroll {
                    at: Some(at),
                    direction: if delta < 0 { negative } else { positive },
                    amount: u16::try_from(notches).unwrap_or(1),
                    modifiers: modifiers.clone(),
                });
            }
            scrolls
        }
        "type" => vec![Action::Type {
            text: action["text"].as_str().unwrap_or("").to_owned(),
        }],
        "keypress" => {
            let keys: Vec<&str> = action["keys"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            vec![Action::Key {
                combo: action::parse_combo(&keys.join("+"))?,
                repeat: 1,
            }]
        }
        "wait" => vec![Action::Wait {
            duration: core::time::Duration::from_secs(1),
        }],
        other => bail!("action {other:?} is not supported"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::action::{Coordinates, keysym};

    fn openai() -> OpenAi {
        OpenAi::new("test-key".into(), DEFAULT_MODEL.into()).unwrap()
    }

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

    #[test]
    fn the_first_request_sends_the_task_and_the_screen() {
        let mut openai = openai();
        openai.user("open notepad", Some(&shot()));
        let body = openai.body();
        assert_eq!(body["tools"][0]["type"], "computer");
        assert!(body.get("previous_response_id").is_none());
        assert_eq!(body["input"][0]["content"][1]["image_url"], "data:image/png;base64,AQID");
        assert_eq!(openai.input.len(), 0, "sent input is not sent again");
    }

    #[test]
    fn a_computer_call_batches_actions_and_continues_the_response() {
        let mut openai = openai();
        let response = json!({
            "id": "resp_1",
            "output": [
                { "type": "reasoning", "summary": [] },
                { "type": "message", "content": [{ "type": "output_text", "text": "Typing it." }] },
                { "type": "computer_call", "call_id": "call_1", "status": "completed", "actions": [
                    { "type": "click", "button": "left", "x": 100, "y": 50, "keys": ["SHIFT"] },
                    { "type": "type", "text": "penguin" },
                    { "type": "keypress", "keys": ["CTRL", "S"] },
                    { "type": "scroll", "x": 10, "y": 10, "scroll_x": 0, "scroll_y": -300 },
                    { "type": "drag", "path": [{ "x": 0, "y": 0 }, [50, 50]] },
                ]},
            ],
        });
        let reply = openai.receive(&response, scale()).unwrap();
        assert_eq!(reply.text, "Typing it.");
        let actions = reply.calls[0].actions.as_ref().unwrap();
        assert_eq!(
            actions[0],
            Action::Click { at: Some(Point { x: 201, y: 101 }), button: Button::Left, count: 1, modifiers: vec![keysym::SHIFT] }
        );
        assert_eq!(actions[2], Action::Key { combo: action::parse_combo("ctrl+s").unwrap(), repeat: 1 });
        assert!(matches!(actions[3], Action::Scroll { direction: Direction::Up, amount: 3, .. }));
        assert!(matches!(actions[4], Action::Drag { to: Point { x: 101, y: 101 }, .. }));
        openai.results(
            vec![CallResult { id: "call_1".into(), outcome: Ok(Output::Text("OK".into())) }],
            Some(&shot()),
        );
        let body = openai.body();
        assert_eq!(body["previous_response_id"], "resp_1");
        assert_eq!(body["input"][0]["type"], "computer_call_output");
        assert_eq!(body["input"][0]["output"]["image_url"], "data:image/png;base64,AQID");
    }

    #[test]
    fn safety_checks_ask_the_person_and_are_acknowledged_when_carried_out() {
        let mut openai = openai();
        let check = json!({ "id": "sc_1", "code": "malicious_instructions", "message": "Suspicious text on screen" });
        let response = json!({ "id": "resp_1", "output": [
            { "type": "computer_call", "call_id": "call_1", "actions": [{ "type": "screenshot" }], "pending_safety_checks": [check] },
        ]});
        let reply = openai.receive(&response, scale()).unwrap();
        assert_eq!(reply.calls[0].confirm.as_deref(), Some("Suspicious text on screen"));
        openai.results(vec![CallResult { id: "call_1".into(), outcome: Ok(Output::Text("OK".into())) }], Some(&shot()));
        assert_eq!(openai.input[0]["acknowledged_safety_checks"][0]["id"], "sc_1");
    }

    #[test]
    fn declined_calls_still_answer_with_the_screen_and_say_why() {
        let mut openai = openai();
        openai.user("go", Some(&shot()));
        let _ = openai.body();
        let response = json!({ "id": "resp_1", "output": [
            { "type": "computer_call", "call_id": "call_1", "actions": [{ "type": "click", "button": "back", "x": 1, "y": 1 }] },
        ]});
        let reply = openai.receive(&response, scale()).unwrap();
        assert!(reply.calls[0].actions.is_err(), "the back button is not supported");
        openai.results(vec![CallResult { id: "call_1".into(), outcome: Err("declined".into()) }], None);
        assert_eq!(openai.input[0]["output"]["image_url"], "data:image/png;base64,AQID");
        assert!(openai.input[0].get("acknowledged_safety_checks").is_none());
        assert!(openai.input[1]["content"][0]["text"].as_str().unwrap().contains("declined"));
    }
}
