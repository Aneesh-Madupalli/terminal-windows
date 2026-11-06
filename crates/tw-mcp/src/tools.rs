use serde_json::{Value, json};

pub const INSTRUCTIONS: &str = "Controls a separate virtual Windows PC, not the user's own desktop. Call `screenshot` first; every x/y is a pixel in that screenshot (top-left origin), which equals guest screen pixels. Take another screenshot after actions to check the result. If the guest is at its sign-in or lock screen, or a UAC prompt or password appears, ask the user to handle it. Do not lock the PC, sign out or shut down: that ends your session.";

fn schema(properties: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": properties, "required": required, "additionalProperties": false })
}

fn point() -> Value {
    json!({ "type": "integer", "minimum": 0 })
}

fn after() -> Value {
    json!({ "type": "boolean", "description": "Also return a screenshot taken shortly after the action." })
}

/// The eight tools, with the same names and argument shapes as terminal-mac.
pub fn list() -> Value {
    json!([
        {
            "name": "screenshot",
            "description": "Capture the virtual PC's screen. Coordinates for other tools are pixels in this image.",
            "inputSchema": schema(json!({}), &[]),
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "click",
            "description": "Click at a point. clicks: 2 double-clicks, 3 triple-clicks.",
            "inputSchema": schema(json!({
                "x": point(), "y": point(),
                "button": { "type": "string", "enum": ["left", "right"] },
                "clicks": { "type": "integer", "minimum": 1, "maximum": 3 },
                "screenshot": after()
            }), &["x", "y"])
        },
        {
            "name": "move",
            "description": "Move the pointer, for example to reveal a hover menu or tooltip.",
            "inputSchema": schema(json!({ "x": point(), "y": point(), "screenshot": after() }), &["x", "y"])
        },
        {
            "name": "drag",
            "description": "Press the left button at (x, y), drag to (toX, toY) and release. Selects text, moves windows and items.",
            "inputSchema": schema(json!({
                "x": point(), "y": point(), "toX": point(), "toY": point(), "screenshot": after()
            }), &["x", "y", "toX", "toY"])
        },
        {
            "name": "scroll",
            "description": "Scroll at a point. Negative dy scrolls the view down, positive dy scrolls it up. Pixels, -1000 to 1000.",
            "inputSchema": schema(json!({
                "x": point(), "y": point(),
                "dy": { "type": "integer" }, "dx": { "type": "integer" },
                "screenshot": after()
            }), &["x", "y", "dy"])
        },
        {
            "name": "type",
            "description": "Type literal text into the focused field. Use `key` for return, tab and shortcuts.",
            "inputSchema": schema(json!({ "text": { "type": "string" }, "screenshot": after() }), &["text"])
        },
        {
            "name": "key",
            "description": "Press a key or chord: return, tab, escape, backspace, up, win+r, alt+tab, ctrl+c, ctrl+shift+esc, ctrl+alt+del. Modifiers: ctrl, alt, shift, win.",
            "inputSchema": schema(json!({ "key": { "type": "string" }, "screenshot": after() }), &["key"])
        },
        {
            "name": "status",
            "description": "Report whether the VM is running and the guest agent is ready.",
            "inputSchema": schema(json!({}), &[]),
            "annotations": { "readOnlyHint": true }
        }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_are_eight_tools_with_closed_schemas() {
        let tools = list();
        let names: Vec<&str> = tools.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["screenshot", "click", "move", "drag", "scroll", "type", "key", "status"]);
        for tool in tools.as_array().unwrap() {
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
            assert_eq!(tool["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn required_arguments_match_terminal_mac() {
        let tools = list();
        let required = |name: &str| -> Vec<String> {
            let tool = tools.as_array().unwrap().iter().find(|t| t["name"] == name).unwrap();
            tool["inputSchema"]["required"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(required("click"), ["x", "y"]);
        assert_eq!(required("drag"), ["x", "y", "toX", "toY"]);
        assert_eq!(required("scroll"), ["x", "y", "dy"]);
        assert_eq!(required("type"), ["text"]);
        assert_eq!(required("key"), ["key"]);
    }
}
