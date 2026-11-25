//! Gemini through the Gemini API (GenerateContent) and its computer use tool in the desktop
//! environment.
//!
//! The model gives positions from 0 to 1000 across the screenshot and its desktop functions take
//! an `intent`. Its turns go back unchanged (thought signatures included). The documentation
//! leaves the exact function response shape for the desktop open: results follow the general
//! multimodal function response (`functionResponse` with the screenshot in its `parts`).

use anyhow::bail;
use serde_json::{Value, json};

use super::http::{self, Api};
use super::{Call, CallResult, Output, Provider, Reply, SYSTEM};
use crate::agent::action::{self, Action, Button, Direction, Point, Scale};
use crate::agent::capture::Shot;

const API_BASE: &str = "https://generativelanguage.googleapis.com/v1/models";
/// The model of the computer use examples in Google's documentation.
pub const DEFAULT_MODEL: &str = "gemini-4-hydrogen";
/// Scroll distance per wheel notch.
const SCROLL_PER_NOTCH: i64 = 100;
const DEFAULT_SCROLL: i64 = 300;

pub struct Gemini {
    api: Api,
    key: String,
    model: String,
    contents: Vec<Value>,
    /// Name, id and whether a safety confirmation is due, for each call awaiting its result.
    pending: Vec<(String, Option<String>, bool, String)>,
}

impl Gemini {
    pub fn new(key: String, model: String) -> anyhow::Result<Self> {
        Ok(Self {
            api: Api::new("Gemini API")?,
            key,
            model,
            contents: Vec::new(),
            pending: Vec::new(),
        })
    }

    fn body(&self) -> Value {
        json!({
            "systemInstruction": { "parts": [{ "text": SYSTEM }] },
            "contents": self.contents,
            "tools": [{ "computerUse": { "environment": "ENVIRONMENT_DESKTOP" } }],
        })
    }

    /// Appends `parts` to the last turn when it is the person's, else starts one.
    fn push_user(&mut self, parts: Vec<Value>) {
        if let Some(last) = self.contents.last_mut()
            && last["role"] == "user"
            && let Some(existing) = last["parts"].as_array_mut()
        {
            existing.extend(parts);
            return;
        }
        self.contents.push(json!({ "role": "user", "parts": parts }));
    }

    fn receive(&mut self, response: &Value, scale: Scale) -> anyhow::Result<Reply> {
        let candidate = &response["candidates"][0];
        if candidate.is_null() {
            let reason = response["promptFeedback"]["blockReason"].as_str().unwrap_or("no answer");
            bail!("Gemini did not answer ({reason})");
        }
        let content = candidate["content"].clone();
        let mut reply = Reply::default();
        for (index, part) in content["parts"].as_array().into_iter().flatten().enumerate() {
            if part["thought"] == true {
                continue;
            }
            if let Some(text) = part["text"].as_str().map(str::trim).filter(|text| !text.is_empty()) {
                if !reply.text.is_empty() {
                    reply.text.push('\n');
                }
                reply.text.push_str(text);
            }
            let call = &part["functionCall"];
            if call.is_null() {
                continue;
            }
            let name = call["name"].as_str().unwrap_or("").to_owned();
            let id = call["id"].as_str().map(str::to_owned);
            let args = &call["args"];
            let safety = &args["safety_decision"];
            let confirm = (safety["decision"] == "require_confirmation")
                .then(|| safety["explanation"].as_str().unwrap_or("safety check").to_owned());
            let actions = parse(&name, args, scale).map_err(|error| format!("{error:#}"));
            let key = id.clone().unwrap_or_else(|| format!("{name}#{index}"));
            self.pending.push((name, id, confirm.is_some(), key.clone()));
            reply.calls.push(Call { id: key, actions, confirm });
        }
        if !content.is_null() {
            self.contents.push(content);
        }
        Ok(reply)
    }
}

fn image(shot: &Shot) -> Value {
    json!({ "inlineData": { "mimeType": "image/png", "data": http::base64(shot) } })
}

impl Provider for Gemini {
    fn user(&mut self, text: &str, screen: Option<&Shot>) {
        let mut parts = vec![json!({ "text": text })];
        parts.extend(screen.map(image));
        self.push_user(parts);
    }

    fn results(&mut self, results: Vec<CallResult>, screen: Option<&Shot>) {
        let count = results.len();
        let parts = results
            .into_iter()
            .enumerate()
            .map(|(index, result)| {
                let (name, id, confirm) = self
                    .pending
                    .iter()
                    .find(|(_, _, _, key)| *key == result.id)
                    .map(|(name, id, confirm, _)| (name.clone(), id.clone(), *confirm))
                    .unwrap_or_default();
                let mut images = Vec::new();
                let mut response = match result.outcome {
                    Ok(Output::Text(text)) => json!({ "status": text }),
                    Ok(Output::Image(shot)) => {
                        images.push(image(&shot));
                        json!({ "status": "OK" })
                    }
                    Err(error) => json!({ "error": error }),
                };
                // Carried out, so the person confirmed it.
                if confirm && response.get("error").is_none() {
                    response["safety_acknowledgement"] = json!(true);
                }
                if index + 1 == count {
                    images.extend(screen.map(image));
                }
                let mut function = json!({ "name": name, "response": response });
                if let Some(id) = id {
                    function["id"] = json!(id);
                }
                if !images.is_empty() {
                    function["parts"] = json!(images);
                }
                json!({ "functionResponse": function })
            })
            .collect();
        self.pending.clear();
        self.push_user(parts);
    }

    async fn next(&mut self, scale: Scale) -> anyhow::Result<Reply> {
        let url = format!("{API_BASE}/{}:generateContent", self.model);
        let response = self.api.post(&url, &[("x-goog-api-key", self.key.as_str())], &self.body()).await?;
        self.receive(&response, scale)
    }
}

/// A desktop function call as [`Action`]s on the remote screen (positions from 0 to 1000).
fn parse(name: &str, args: &Value, scale: Scale) -> anyhow::Result<Vec<Action>> {
    let point = |x: &str, y: &str| -> anyhow::Result<Point> {
        match (args[x].as_f64(), args[y].as_f64()) {
            (Some(x), Some(y)) => scale.point(x, y),
            _ => bail!("{x} and {y} must be numbers"),
        }
    };
    let at = || point("x", "y");
    let click = |button: Button, count: u8| -> anyhow::Result<Vec<Action>> {
        Ok(vec![Action::Click {
            at: Some(at()?),
            button,
            count,
            modifiers: Vec::new(),
        }])
    };
    let combo = |keys: &str| -> anyhow::Result<Vec<Action>> {
        Ok(vec![Action::Key {
            combo: action::parse_combo(keys)?,
            repeat: 1,
        }])
    };
    Ok(match name {
        "click" => click(Button::Left, 1)?,
        "double_click" => click(Button::Left, 2)?,
        "triple_click" => click(Button::Left, 3)?,
        "middle_click" => click(Button::Middle, 1)?,
        "right_click" => click(Button::Right, 1)?,
        "move" => vec![Action::MouseMove { to: at()? }],
        "mouse_down" => vec![Action::MouseMove { to: at()? }, Action::MouseDown],
        "mouse_up" => vec![Action::MouseMove { to: at()? }, Action::MouseUp],
        "drag_and_drop" => vec![Action::Drag {
            from: point("start_x", "start_y")?,
            to: point("end_x", "end_y")?,
            modifiers: Vec::new(),
        }],
        "type" => {
            let mut actions = vec![Action::Type {
                text: args["text"].as_str().unwrap_or("").to_owned(),
            }];
            if args["press_enter"] == true {
                actions.extend(combo("Return")?);
            }
            actions
        }
        "press_key" => combo(args["key"].as_str().unwrap_or(""))?,
        "hotkey" => {
            let keys: Vec<&str> = args["keys"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            combo(&keys.join("+"))?
        }
        "scroll" => {
            let magnitude = args["magnitude_in_pixels"].as_i64().unwrap_or(DEFAULT_SCROLL);
            let direction = match args["direction"].as_str() {
                Some("up") => Direction::Up,
                Some("down") => Direction::Down,
                Some("left") => Direction::Left,
                Some("right") => Direction::Right,
                other => bail!("direction {other:?} is not up, down, left or right"),
            };
            let at = if args["x"].is_null() { None } else { Some(at()?) };
            vec![Action::Scroll {
                at,
                direction,
                amount: u16::try_from((magnitude / SCROLL_PER_NOTCH).clamp(1, i64::from(action::MAX_REPEAT))).unwrap_or(1),
                modifiers: Vec::new(),
            }]
        }
        "wait" => vec![Action::Wait {
            duration: action::duration(args["seconds"].as_f64().unwrap_or(1.0))?,
        }],
        "take_screenshot" => vec![Action::Screenshot],
        other => bail!("{other} is not supported in this session"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::action::Coordinates;

    fn gemini() -> Gemini {
        Gemini::new("test-key".into(), DEFAULT_MODEL.into()).unwrap()
    }

    fn scale() -> Scale {
        Scale::new((2000, 1000), (1280, 640), Coordinates::Normalized)
    }

    fn shot() -> Shot {
        Shot {
            png: vec![1, 2, 3],
            width: 1280,
            height: 640,
        }
    }

    #[test]
    fn the_request_asks_for_the_desktop_environment() {
        let mut gemini = gemini();
        gemini.user("open notepad", Some(&shot()));
        let body = gemini.body();
        assert_eq!(body["tools"][0]["computerUse"]["environment"], "ENVIRONMENT_DESKTOP");
        assert_eq!(body["contents"][0]["parts"][1]["inlineData"]["data"], "AQID");
    }

    #[test]
    fn function_calls_read_normalized_positions_and_keep_the_turn() {
        let mut gemini = gemini();
        gemini.user("go", None);
        let response = json!({ "candidates": [{ "content": { "role": "model", "parts": [
            { "text": "Opening the menu.", "thoughtSignature": "sig" },
            { "functionCall": { "id": "f1", "name": "click", "args": { "x": 500, "y": 500, "intent": "open" } } },
            { "functionCall": { "name": "type", "args": { "text": "hi", "press_enter": true } } },
            { "functionCall": { "name": "mouse_down", "args": { "x": 0, "y": 0 } } },
            { "functionCall": { "name": "hotkey", "args": { "keys": ["Meta", "A"] } } },
            { "functionCall": { "name": "key_down", "args": { "key": "Shift" } } },
        ]}}]});
        let reply = gemini.receive(&response, scale()).unwrap();
        assert_eq!(reply.text, "Opening the menu.");
        assert_eq!(
            reply.calls[0].actions.as_ref().unwrap()[0],
            Action::Click { at: Some(Point { x: 1000, y: 500 }), button: Button::Left, count: 1, modifiers: vec![] }
        );
        assert_eq!(reply.calls[1].actions.as_ref().unwrap().len(), 2, "text, then Enter");
        assert_eq!(reply.calls[2].actions.as_ref().unwrap()[1], Action::MouseDown);
        assert!(reply.calls[3].actions.is_ok());
        assert!(reply.calls[4].actions.is_err(), "holding keys is not supported");
        assert_eq!(gemini.contents[1]["parts"][0]["thoughtSignature"], "sig", "the turn goes back unchanged");
    }

    #[test]
    fn results_answer_each_call_and_acknowledge_confirmed_ones() {
        let mut gemini = gemini();
        let response = json!({ "candidates": [{ "content": { "role": "model", "parts": [
            { "functionCall": { "id": "f1", "name": "click", "args": { "x": 1, "y": 1,
                "safety_decision": { "decision": "require_confirmation", "explanation": "Deletes files" } } } },
            { "functionCall": { "name": "take_screenshot", "args": {} } },
        ]}}]});
        let reply = gemini.receive(&response, scale()).unwrap();
        assert_eq!(reply.calls[0].confirm.as_deref(), Some("Deletes files"));
        let ids: Vec<String> = reply.calls.iter().map(|call| call.id.clone()).collect();
        gemini.results(
            vec![
                CallResult { id: ids[0].clone(), outcome: Ok(Output::Text("OK".into())) },
                CallResult { id: ids[1].clone(), outcome: Err("Not executed".into()) },
            ],
            Some(&shot()),
        );
        let parts = &gemini.contents[1]["parts"];
        assert_eq!(parts[0]["functionResponse"]["id"], "f1");
        assert_eq!(parts[0]["functionResponse"]["response"]["safety_acknowledgement"], true);
        assert_eq!(parts[1]["functionResponse"]["name"], "take_screenshot");
        assert_eq!(parts[1]["functionResponse"]["response"]["error"], "Not executed");
        assert_eq!(parts[1]["functionResponse"]["parts"][0]["inlineData"]["data"], "AQID");
    }
}
