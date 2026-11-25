//! Open models behind an OpenAI-compatible server (Ollama, vLLM, LM Studio, …) through the Chat
//! Completions API, with the computer actions as function tools.
//!
//! Tool results are text; screenshots follow them in a user message, since such servers take
//! images in user messages only.

use serde_json::{Value, json};

use super::functions::{self, parse};
use super::http::{self, Api};
use super::{Call, CallResult, Output, Provider, Reply, SYSTEM};
use crate::agent::action::{Coordinates, Scale};
use crate::agent::capture::Shot;

pub struct Chat {
    api: Api,
    url: String,
    /// Empty for servers without authentication.
    key: String,
    model: String,
    coordinates: Coordinates,
    messages: Vec<Value>,
}

impl Chat {
    pub fn new(base_url: &str, key: String, model: String, coordinates: Coordinates) -> anyhow::Result<Self> {
        Ok(Self {
            api: Api::new("AI server")?,
            url: http::endpoint(base_url, "chat/completions"),
            key,
            model,
            coordinates,
            messages: Vec::new(),
        })
    }

    fn body(&self) -> Value {
        let tools: Vec<Value> = functions::functions(self.coordinates)
            .into_iter()
            .map(|f| json!({ "type": "function", "function": { "name": f.name, "description": f.description, "parameters": f.parameters } }))
            .collect();
        let mut messages = vec![json!({ "role": "system", "content": SYSTEM })];
        messages.extend(self.messages.iter().cloned());
        json!({ "model": self.model, "messages": messages, "tools": tools, "tool_choice": "auto" })
    }

    fn receive(&mut self, response: &Value, scale: Scale) -> Reply {
        let message = &response["choices"][0]["message"];
        let mut reply = Reply {
            text: message["content"].as_str().unwrap_or("").trim().to_owned(),
            calls: Vec::new(),
        };
        for call in message["tool_calls"].as_array().into_iter().flatten() {
            let id = call["id"].as_str().unwrap_or("").to_owned();
            let name = call["function"]["name"].as_str().unwrap_or("");
            // Arguments come as a JSON string; some servers send the object itself.
            let arguments = match &call["function"]["arguments"] {
                Value::String(text) => serde_json::from_str(text).map_err(|error| format!("arguments are not JSON: {error}")),
                Value::Null => Ok(json!({})),
                object => Ok(object.clone()),
            };
            let action = arguments.and_then(|input| parse(name, &input, scale).map_err(|error| format!("{error:#}")));
            reply.calls.push(Call::one(id, action));
        }
        // The reply goes back as the server sent it, tool calls included.
        let mut assistant = json!({ "role": "assistant", "content": message["content"].clone() });
        if let Some(calls) = message.get("tool_calls").filter(|calls| calls.is_array()) {
            assistant["tool_calls"] = calls.clone();
        }
        self.messages.push(assistant);
        reply
    }
}

fn image(shot: &Shot) -> Value {
    json!({ "type": "image_url", "image_url": { "url": http::data_url(shot) } })
}

impl Provider for Chat {
    fn user(&mut self, text: &str, screen: Option<&Shot>) {
        let mut content = vec![json!({ "type": "text", "text": text })];
        content.extend(screen.map(image));
        self.messages.push(json!({ "role": "user", "content": content }));
    }

    fn results(&mut self, results: Vec<CallResult>, screen: Option<&Shot>) {
        let mut images = Vec::new();
        for result in results {
            let content = match result.outcome {
                Ok(Output::Text(text)) => text,
                Ok(Output::Image(shot)) => {
                    images.push(image(&shot));
                    "The screenshot follows.".to_owned()
                }
                Err(error) => format!("Error: {error}"),
            };
            self.messages.push(json!({ "role": "tool", "tool_call_id": result.id, "content": content }));
        }
        images.extend(screen.map(image));
        if !images.is_empty() {
            let mut content = vec![json!({ "type": "text", "text": "The screen after these actions:" })];
            content.extend(images);
            self.messages.push(json!({ "role": "user", "content": content }));
        }
    }

    async fn next(&mut self, scale: Scale) -> anyhow::Result<Reply> {
        let authorization = format!("Bearer {}", self.key);
        let headers: Vec<(&str, &str)> = if self.key.is_empty() { Vec::new() } else { vec![("authorization", authorization.as_str())] };
        let response = self.api.post(&self.url, &headers, &self.body()).await?;
        Ok(self.receive(&response, scale))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::action::{Action, Button, Point};

    fn shot() -> Shot {
        Shot {
            png: vec![1, 2, 3],
            width: 1280,
            height: 800,
        }
    }

    #[test]
    fn a_conversation_uses_function_tools_and_sends_screens_as_user_images() {
        let mut chat = Chat::new("http://localhost:11434", String::new(), "qwen3-vl".into(), Coordinates::Pixels).unwrap();
        assert_eq!(chat.url, "http://localhost:11434/v1/chat/completions");
        chat.user("open notepad", Some(&shot()));
        let body = chat.body();
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["content"][1]["image_url"]["url"], "data:image/png;base64,AQID");
        assert_eq!(body["tools"][1]["function"]["name"], "left_click");

        let response = json!({ "choices": [{ "message": {
            "role": "assistant",
            "content": "Clicking Start.",
            "tool_calls": [
                { "id": "c1", "type": "function", "function": { "name": "left_click", "arguments": "{\"coordinate\": [10, 20]}" } },
                { "id": "c2", "type": "function", "function": { "name": "type", "arguments": { "text": "hi" } } },
                { "id": "c3", "type": "function", "function": { "name": "left_click", "arguments": "not json" } },
            ],
        }}]});
        let reply = chat.receive(&response, Scale::new((2560, 1600), (1280, 800), Coordinates::Pixels));
        assert_eq!(reply.text, "Clicking Start.");
        assert_eq!(
            reply.calls[0].actions.as_ref().unwrap()[0],
            Action::Click { at: Some(Point { x: 20, y: 40 }), button: Button::Left, count: 1, modifiers: vec![] }
        );
        assert_eq!(reply.calls[1].actions.as_ref().unwrap()[0], Action::Type { text: "hi".into() });
        assert!(reply.calls[2].actions.is_err());

        chat.results(
            vec![
                CallResult { id: "c1".into(), outcome: Ok(Output::Text("OK".into())) },
                CallResult { id: "c2".into(), outcome: Err("Not executed".into()) },
            ],
            Some(&shot()),
        );
        let messages = &chat.messages;
        assert_eq!(messages[1]["tool_calls"].as_array().unwrap().len(), 3);
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[3]["content"], "Error: Not executed");
        assert_eq!(messages[4]["role"], "user");
        assert_eq!(messages[4]["content"][1]["type"], "image_url");
    }
}
