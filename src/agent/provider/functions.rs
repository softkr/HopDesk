//! Computer actions as plain function tools, for models without a computer use tool of their own
//! (open models behind OpenAI- or Claude-compatible servers). The names and inputs follow
//! Claude's computer use toolset, so one parser reads both.

use anyhow::{Context as _, bail};
use serde_json::{Value, json};

use crate::agent::action::{self, Action, Button, Coordinates, Direction, Scale};

/// A function tool: name, description and JSON schema of its input.
pub(super) struct Function {
    pub name: &'static str,
    pub description: String,
    pub parameters: Value,
}

/// The computer actions as function tools; positions are given in `coordinates`.
pub(super) fn functions(coordinates: Coordinates) -> Vec<Function> {
    let unit = match coordinates {
        Coordinates::Pixels => "pixels of the latest screenshot, [0, 0] at the top left",
        Coordinates::Normalized => "0 to 1000 across the latest screenshot, [0, 0] top left and [1000, 1000] bottom right",
    };
    let point = |what: &str| json!({
        "type": "array",
        "items": { "type": "number" },
        "minItems": 2,
        "maxItems": 2,
        "description": format!("{what} as [x, y] in {unit}"),
    });
    let modifiers = json!({
        "type": "string",
        "description": "Modifier keys held during the action, joined by '+', e.g. \"shift\" or \"ctrl+alt\"",
    });
    let object = |properties: Value, required: &[&str]| json!({
        "type": "object",
        "properties": properties,
        "required": required,
    });
    let click = |name: &'static str, what: &str| Function {
        name,
        description: format!("{what} at a position, or where the pointer is when no coordinate is given."),
        parameters: object(json!({ "coordinate": point("Where to click"), "text": modifiers.clone() }), &[]),
    };
    vec![
        Function {
            name: "screenshot",
            description: "Takes a screenshot of the whole screen.".into(),
            parameters: object(json!({}), &[]),
        },
        click("left_click", "Clicks the left mouse button"),
        click("right_click", "Clicks the right mouse button"),
        click("middle_click", "Clicks the middle mouse button"),
        click("double_click", "Double-clicks the left mouse button"),
        click("triple_click", "Triple-clicks the left mouse button"),
        Function {
            name: "left_click_drag",
            description: "Presses the left button at one position, moves to another and releases it.".into(),
            parameters: object(
                json!({ "start_coordinate": point("Where to press"), "coordinate": point("Where to release"), "text": modifiers.clone() }),
                &["start_coordinate", "coordinate"],
            ),
        },
        Function {
            name: "mouse_move",
            description: "Moves the pointer without clicking.".into(),
            parameters: object(json!({ "coordinate": point("Where to move") }), &["coordinate"]),
        },
        Function {
            name: "scroll",
            description: "Turns the mouse wheel, at a position or where the pointer is.".into(),
            parameters: object(
                json!({
                    "coordinate": point("Where to scroll"),
                    "scroll_direction": { "type": "string", "enum": ["up", "down", "left", "right"] },
                    "scroll_amount": { "type": "integer", "minimum": 1, "maximum": action::MAX_REPEAT, "description": "Wheel notches" },
                    "text": modifiers.clone(),
                }),
                &["scroll_direction", "scroll_amount"],
            ),
        },
        Function {
            name: "type",
            description: "Types text at the keyboard focus.".into(),
            parameters: object(json!({ "text": { "type": "string" } }), &["text"]),
        },
        Function {
            name: "key",
            description: "Presses a key or key combination, e.g. \"Return\", \"ctrl+s\" or \"alt+F4\".".into(),
            parameters: object(
                json!({
                    "text": { "type": "string" },
                    "repeat": { "type": "integer", "minimum": 1, "maximum": action::MAX_REPEAT },
                }),
                &["text"],
            ),
        },
        Function {
            name: "wait",
            description: "Waits, e.g. for a window to open.".into(),
            parameters: object(json!({ "duration": { "type": "number", "description": "Seconds, at most 300" } }), &["duration"]),
        },
    ]
}

/// A call by member name (Claude's toolset, or the same functions declared by
/// [`functions`]) as an [`Action`] on the remote screen.
pub(super) fn parse(name: &str, input: &Value, scale: Scale) -> anyhow::Result<Action> {
    let point = |key: &str| -> anyhow::Result<action::Point> {
        let pair = input[key].as_array().filter(|pair| pair.len() == 2);
        let pair = pair.with_context(|| format!("{key} must be [x, y]"))?;
        let x = pair[0].as_f64().with_context(|| format!("{key} must be numbers"))?;
        let y = pair[1].as_f64().with_context(|| format!("{key} must be numbers"))?;
        scale.point(x, y)
    };
    let optional_point = |key: &str| -> anyhow::Result<Option<action::Point>> {
        if input[key].is_null() { Ok(None) } else { point(key).map(Some) }
    };
    let modifiers = || action::parse_modifiers(input["text"].as_str().unwrap_or(""));
    let click = |button: Button, count: u8| -> anyhow::Result<Action> {
        Ok(Action::Click {
            at: optional_point("coordinate")?,
            button,
            count,
            modifiers: modifiers()?,
        })
    };
    Ok(match name {
        "screenshot" => Action::Screenshot,
        "cursor_position" => Action::CursorPosition,
        "zoom" => {
            let region = input["region"].as_array().filter(|region| region.len() == 4);
            let region = region.context("region must be [x0, y0, x1, y1]")?;
            let mut corners = [0.0; 4];
            for (corner, value) in corners.iter_mut().zip(region) {
                *corner = value.as_f64().context("region must be numbers")?;
            }
            Action::Zoom {
                region: scale.region(corners)?,
            }
        }
        "left_click" => click(Button::Left, 1)?,
        "right_click" => click(Button::Right, 1)?,
        "middle_click" => click(Button::Middle, 1)?,
        "double_click" => click(Button::Left, 2)?,
        "triple_click" => click(Button::Left, 3)?,
        "left_click_drag" => Action::Drag {
            from: point("start_coordinate")?,
            to: point("coordinate")?,
            modifiers: modifiers()?,
        },
        "mouse_move" => Action::MouseMove {
            to: point("coordinate")?,
        },
        "left_mouse_down" => Action::MouseDown,
        "left_mouse_up" => Action::MouseUp,
        "scroll" => Action::Scroll {
            at: optional_point("coordinate")?,
            direction: match input["scroll_direction"].as_str() {
                Some("up") => Direction::Up,
                Some("down") => Direction::Down,
                Some("left") => Direction::Left,
                Some("right") => Direction::Right,
                other => bail!("scroll_direction {other:?} is not up, down, left or right"),
            },
            amount: input["scroll_amount"]
                .as_u64()
                .and_then(|amount| u16::try_from(amount).ok())
                .filter(|amount| (1..=action::MAX_REPEAT).contains(amount))
                .with_context(|| format!("scroll_amount must be 1 to {}", action::MAX_REPEAT))?,
            modifiers: modifiers()?,
        },
        "type" => Action::Type {
            text: input["text"].as_str().context("text is missing")?.to_owned(),
        },
        "key" => Action::Key {
            combo: action::parse_combo(input["text"].as_str().context("text is missing")?)?,
            repeat: match input.get("repeat") {
                None | Some(Value::Null) => 1,
                Some(repeat) => repeat
                    .as_u64()
                    .and_then(|repeat| u16::try_from(repeat).ok())
                    .filter(|repeat| (1..=action::MAX_REPEAT).contains(repeat))
                    .with_context(|| format!("repeat must be 1 to {}", action::MAX_REPEAT))?,
            },
        },
        "wait" => Action::Wait {
            duration: action::duration(input["duration"].as_f64().context("duration is missing")?)?,
        },
        other => bail!("{other} is not supported in this session"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::action::Point;

    #[test]
    fn every_declared_function_parses() {
        let scale = Scale::new((200, 100), (200, 100), Coordinates::Pixels);
        let inputs = [
            ("screenshot", json!({})),
            ("left_click", json!({ "coordinate": [10, 10] })),
            ("right_click", json!({})),
            ("middle_click", json!({})),
            ("double_click", json!({ "coordinate": [1, 1], "text": "ctrl" })),
            ("triple_click", json!({})),
            ("left_click_drag", json!({ "start_coordinate": [1, 1], "coordinate": [5, 5] })),
            ("mouse_move", json!({ "coordinate": [3, 3] })),
            ("scroll", json!({ "scroll_direction": "up", "scroll_amount": 2 })),
            ("type", json!({ "text": "hi" })),
            ("key", json!({ "text": "ctrl+s" })),
            ("wait", json!({ "duration": 1 })),
        ];
        let declared: Vec<&str> = functions(Coordinates::Pixels).iter().map(|f| f.name).collect();
        assert_eq!(declared.len(), inputs.len());
        for (name, input) in inputs {
            assert!(declared.contains(&name), "{name}");
            parse(name, &input, scale).unwrap_or_else(|error| panic!("{name}: {error:#}"));
        }
    }

    #[test]
    fn normalized_positions_are_read_across_the_screen() {
        let scale = Scale::new((1920, 1080), (1280, 720), Coordinates::Normalized);
        let action = parse("left_click", &json!({ "coordinate": [500, 500] }), scale).unwrap();
        assert!(matches!(action, Action::Click { at: Some(Point { x: 960, y: 540 }), .. }));
    }
}
