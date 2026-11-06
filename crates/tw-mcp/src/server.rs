use std::collections::BTreeMap;
use std::time::Duration;

use base64::Engine;
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tw_client::{Guest, GuestError};
use tw_core::keys::{Modifier, parse_chord};

use crate::image::png_to_jpeg;
use crate::{tools, tools::INSTRUCTIONS};

/// One MCP session over line-delimited JSON-RPC.
pub struct McpServer<G: Guest> {
    guest: G,
    name: String,
    version: String,
}

fn text(message: impl Into<String>) -> Value {
    json!({ "type": "text", "text": message.into() })
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// A required integer argument, as the decimal string the agent protocol uses.
fn integer(args: &Value, key: &str) -> Result<String, String> {
    let value = &args[key];
    value
        .as_i64()
        .or_else(|| value.as_f64().map(|f| f as i64))
        .map(|n| n.to_string())
        .ok_or_else(|| format!("Missing integer '{key}'"))
}

fn string(args: &Value, key: &str) -> Result<String, String> {
    match args[key].as_str() {
        Some(value) if !value.is_empty() => Ok(value.to_string()),
        _ => Err(format!("Missing '{key}'")),
    }
}

fn map(pairs: &[(&str, String)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect()
}

/// Why a tool call failed, in words for the agent.
enum Failure {
    Guest(GuestError),
    Message(String),
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Failure::Message(message)
    }
}

impl From<GuestError> for Failure {
    fn from(error: GuestError) -> Self {
        Failure::Guest(error)
    }
}

impl<G: Guest> McpServer<G> {
    pub fn new(guest: G, name: impl Into<String>, version: impl Into<String>) -> Self {
        Self { guest, name: name.into(), version: version.into() }
    }

    pub async fn serve<R, W>(&self, reader: R, mut writer: W) -> std::io::Result<()>
    where
        R: AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let mut lines = reader.lines();
        while let Some(line) = lines.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            if let Some(response) = self.handle(&line).await {
                let mut bytes = response.to_string().into_bytes();
                bytes.push(b'\n');
                writer.write_all(&bytes).await?;
                writer.flush().await?;
            }
        }
        Ok(())
    }

    /// Handles one message. Notifications (no `id`) get no response.
    pub async fn handle(&self, line: &str) -> Option<Value> {
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            return Some(rpc_error(Value::Null, -32700, "Parse error"));
        };
        if !message.is_object() {
            return Some(rpc_error(Value::Null, -32700, "Parse error"));
        }
        let id = message.get("id")?.clone();
        let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
        let result = match message["method"].as_str() {
            Some("initialize") => json!({
                "protocolVersion": params["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "terminal-windows", "version": self.version },
                "instructions": INSTRUCTIONS,
            }),
            Some("ping") => json!({}),
            Some("tools/list") => json!({ "tools": tools::list() }),
            Some("tools/call") => {
                let tool = params["name"].as_str().unwrap_or_default();
                let arguments = if params["arguments"].is_object() { params["arguments"].clone() } else { json!({}) };
                match self.call_tool(tool, &arguments).await {
                    Ok(content) => json!({ "content": content }),
                    Err(failure) => {
                        let message = match failure {
                            Failure::Guest(error) => error.explain(&self.name),
                            Failure::Message(message) => message,
                        };
                        json!({ "content": [text(message)], "isError": true })
                    }
                }
            }
            _ => return Some(rpc_error(id, -32601, "Method not found")),
        };
        Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
    }

    async fn call_tool(&self, tool: &str, args: &Value) -> Result<Vec<Value>, Failure> {
        let second = Duration::from_secs;
        match tool {
            "screenshot" => return self.screenshot().await,
            "status" => {
                let reply = self.guest.call("status", BTreeMap::new(), second(5)).await?;
                let lines: Vec<String> = reply.values.iter().map(|(k, v)| format!("{k}: {v}")).collect();
                return Ok(vec![text(format!("VM '{}' is running.\n{}", self.name, lines.join("\n")))]);
            }
            "click" => {
                let mut request = map(&[("x", integer(args, "x")?), ("y", integer(args, "y")?)]);
                if args["button"].as_str() == Some("right") {
                    request.insert("button".into(), "right".into());
                }
                let clicks = args["clicks"].as_i64().unwrap_or(1).clamp(1, 3);
                if clicks > 1 {
                    request.insert("count".into(), clicks.to_string());
                }
                self.guest.call("click", request, second(10)).await?;
            }
            "move" => {
                let request = map(&[("x", integer(args, "x")?), ("y", integer(args, "y")?)]);
                self.guest.call("move", request, second(10)).await?;
            }
            "drag" => {
                let request = map(&[
                    ("x", integer(args, "x")?),
                    ("y", integer(args, "y")?),
                    ("x2", integer(args, "toX")?),
                    ("y2", integer(args, "toY")?),
                ]);
                self.guest.call("drag", request, second(15)).await?;
            }
            "scroll" => {
                let mut request =
                    map(&[("x", integer(args, "x")?), ("y", integer(args, "y")?), ("dy", integer(args, "dy")?)]);
                if !args["dx"].is_null() {
                    request.insert("dx".into(), integer(args, "dx")?);
                }
                self.guest.call("scroll", request, second(10)).await?;
            }
            "type" => {
                self.guest.call("type", map(&[("text", string(args, "text")?)]), second(30)).await?;
            }
            "key" => {
                let key = string(args, "key")?;
                if is_ctrl_alt_del(&key) {
                    self.guest.ctrl_alt_del().await?;
                } else {
                    self.guest.call("key", map(&[("key", key)]), second(10)).await?;
                }
            }
            other => return Err(Failure::Message(format!("Unknown tool: {other}"))),
        }
        let mut content = vec![text(format!("Done: {tool}."))];
        if args["screenshot"].as_bool() == Some(true) {
            // Give animations and redraws a moment before looking.
            tokio::time::sleep(Duration::from_millis(400)).await;
            content.extend(self.screenshot().await?);
        }
        Ok(content)
    }

    async fn screenshot(&self) -> Result<Vec<Value>, Failure> {
        let reply = self.guest.call("screenshot", BTreeMap::new(), Duration::from_secs(15)).await?;
        let (jpeg, width, height) = png_to_jpeg(&reply.payload, 85)
            .map_err(|e| Failure::Message(format!("The guest agent returned an invalid screenshot: {e}")))?;
        Ok(vec![
            json!({ "type": "image", "data": base64::engine::general_purpose::STANDARD.encode(jpeg), "mimeType": "image/jpeg" }),
            text(format!("Screen: {width}x{height}. Use these pixel coordinates.")),
        ])
    }
}

/// `ctrl+alt+del` cannot be injected from inside the guest, so it goes through the hypervisor keyboard.
fn is_ctrl_alt_del(key: &str) -> bool {
    parse_chord(key).is_ok_and(|chord| {
        chord.key.vk == 0x2E
            && chord.modifiers.len() == 2
            && chord.modifiers.contains(&Modifier::Ctrl)
            && chord.modifiers.contains(&Modifier::Alt)
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use tw_client::Reply;

    use super::*;

    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<(String, BTreeMap<String, String>)>>,
        ctrl_alt_del: Mutex<u32>,
        fail: bool,
    }

    fn png() -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, 4, 3);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header().unwrap().write_image_data(&[128u8; 4 * 3 * 4]).unwrap();
        out
    }

    #[async_trait::async_trait]
    impl Guest for &Fake {
        async fn call(&self, action: &str, args: BTreeMap<String, String>, _: Duration) -> Result<Reply, GuestError> {
            if self.fail {
                return Err(GuestError::NotCreated("default".into()));
            }
            self.calls.lock().unwrap().push((action.to_string(), args));
            let payload = if action == "screenshot" { png() } else { Vec::new() };
            let values = BTreeMap::from([
                ("session".to_string(), "ready".to_string()),
                ("width".to_string(), "1280".to_string()),
            ]);
            Ok(Reply { values, payload })
        }

        async fn ctrl_alt_del(&self) -> Result<(), GuestError> {
            *self.ctrl_alt_del.lock().unwrap() += 1;
            Ok(())
        }
    }

    fn server(fake: &Fake) -> McpServer<&Fake> {
        McpServer::new(fake, "default", "0.1.0")
    }

    async fn request(server: &McpServer<&Fake>, method: &str, params: Value) -> Value {
        let line = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }).to_string();
        server.handle(&line).await.unwrap()
    }

    async fn call(server: &McpServer<&Fake>, tool: &str, arguments: Value) -> Value {
        request(server, "tools/call", json!({ "name": tool, "arguments": arguments })).await["result"].clone()
    }

    fn last(fake: &Fake) -> (String, BTreeMap<String, String>) {
        fake.calls.lock().unwrap().last().unwrap().clone()
    }

    #[tokio::test]
    async fn the_handshake_methods() {
        let fake = Fake::default();
        let s = server(&fake);
        let init = request(&s, "initialize", json!({ "protocolVersion": "2024-11-05" })).await;
        assert_eq!(init["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(init["result"]["serverInfo"]["name"], "terminal-windows");
        assert!(init["result"]["instructions"].as_str().unwrap().contains("virtual Windows PC"));
        assert_eq!(request(&s, "ping", json!({})).await["result"], json!({}));
        let list = request(&s, "tools/list", json!({})).await;
        assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 8);
        let default = request(&s, "initialize", json!({})).await;
        assert_eq!(default["result"]["protocolVersion"], "2025-06-18");
    }

    #[tokio::test]
    async fn notifications_get_no_reply_and_junk_gets_errors() {
        let fake = Fake::default();
        let s = server(&fake);
        assert!(s.handle(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).await.is_none());
        assert_eq!(s.handle("not json").await.unwrap()["error"]["code"], -32700);
        assert_eq!(s.handle("[1]").await.unwrap()["error"]["code"], -32700);
        let unknown = request(&s, "resources/list", json!({})).await;
        assert_eq!(unknown["error"]["code"], -32601);
        assert_eq!(unknown["id"], 1);
    }

    #[tokio::test]
    async fn click_variants_map_to_agent_requests() {
        let fake = Fake::default();
        let s = server(&fake);
        call(&s, "click", json!({ "x": 10, "y": 20 })).await;
        assert_eq!(last(&fake), ("click".into(), map(&[("x", "10".into()), ("y", "20".into())])));
        call(&s, "click", json!({ "x": 1, "y": 2, "button": "right", "clicks": 3 })).await;
        let (_, args) = last(&fake);
        assert_eq!((args["button"].as_str(), args["count"].as_str()), ("right", "3"));
        assert_eq!(fake.calls.lock().unwrap().len(), 2, "a triple click is one request");
        call(&s, "click", json!({ "x": 1.0, "y": 2.0, "clicks": 9 })).await;
        assert_eq!(last(&fake).1["count"], "3", "clicks are clamped to 3");
    }

    #[tokio::test]
    async fn other_tools_map_to_agent_requests() {
        let fake = Fake::default();
        let s = server(&fake);
        call(&s, "move", json!({ "x": 5, "y": 6 })).await;
        assert_eq!(last(&fake).0, "move");
        call(&s, "drag", json!({ "x": 1, "y": 2, "toX": 3, "toY": 4 })).await;
        assert_eq!(last(&fake).1, map(&[("x", "1".into()), ("y", "2".into()), ("x2", "3".into()), ("y2", "4".into())]));
        call(&s, "scroll", json!({ "x": 1, "y": 2, "dy": -300 })).await;
        assert!(!last(&fake).1.contains_key("dx"));
        call(&s, "scroll", json!({ "x": 1, "y": 2, "dy": 0, "dx": 40 })).await;
        assert_eq!(last(&fake).1["dx"], "40");
        call(&s, "type", json!({ "text": "hello" })).await;
        assert_eq!(last(&fake).1["text"], "hello");
        call(&s, "key", json!({ "key": "win+r" })).await;
        assert_eq!(last(&fake), ("key".into(), map(&[("key", "win+r".into())])));
    }

    #[tokio::test]
    async fn ctrl_alt_del_bypasses_the_agent() {
        let fake = Fake::default();
        let s = server(&fake);
        for key in ["ctrl+alt+del", "Ctrl+Alt+Delete", "alt+ctrl+delete"] {
            let result = call(&s, "key", json!({ "key": key })).await;
            assert!(result.get("isError").is_none(), "{key}");
        }
        assert_eq!(*fake.ctrl_alt_del.lock().unwrap(), 3);
        assert!(fake.calls.lock().unwrap().is_empty());
        call(&s, "key", json!({ "key": "ctrl+shift+esc" })).await;
        assert_eq!(fake.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn screenshot_returns_a_jpeg_and_the_size() {
        let fake = Fake::default();
        let result = call(&server(&fake), "screenshot", json!({})).await;
        let content = result["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "image");
        assert_eq!(content[0]["mimeType"], "image/jpeg");
        assert!(content[0]["data"].as_str().unwrap().starts_with("/9j/"), "base64 of a JPEG");
        assert_eq!(content[1]["text"], "Screen: 4x3. Use these pixel coordinates.");
    }

    #[tokio::test]
    async fn actions_can_append_a_screenshot() {
        let fake = Fake::default();
        let result = call(&server(&fake), "move", json!({ "x": 1, "y": 2, "screenshot": true })).await;
        let content = result["content"].as_array().unwrap();
        assert_eq!(content[0]["text"], "Done: move.");
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content.len(), 3);
    }

    #[tokio::test]
    async fn status_lists_the_guest_values() {
        let fake = Fake::default();
        let result = call(&server(&fake), "status", json!({})).await;
        let message = result["content"][0]["text"].as_str().unwrap();
        assert!(message.starts_with("VM 'default' is running."), "{message}");
        assert!(message.contains("session: ready"));
    }

    #[tokio::test]
    async fn failures_are_tool_errors_with_guidance() {
        let failing = Fake { fail: true, ..Fake::default() };
        let result = call(&server(&failing), "click", json!({ "x": 1, "y": 2 })).await;
        assert_eq!(result["isError"], true);
        assert!(result["content"][0]["text"].as_str().unwrap().contains("terminal-windows up"));

        let fake = Fake::default();
        let s = server(&fake);
        for (tool, arguments) in
            [("click", json!({ "x": 1 })), ("type", json!({})), ("key", json!({ "key": "" })), ("nope", json!({}))]
        {
            let result = call(&s, tool, arguments).await;
            assert_eq!(result["isError"], true, "{tool}");
        }
        assert!(fake.calls.lock().unwrap().is_empty(), "invalid calls never reach the guest");
    }

    #[tokio::test]
    async fn serves_a_whole_session_over_a_stream() {
        let fake = Fake::default();
        let s = server(&fake);
        let input = [
            json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }).to_string(),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string(),
            String::new(),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }).to_string(),
        ]
        .join("\n");
        let mut output = Vec::new();
        s.serve(input.as_bytes(), &mut output).await.unwrap();
        let replies: Vec<Value> =
            String::from_utf8(output).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(replies.len(), 2, "the notification and the blank line get no reply");
        assert_eq!(replies[0]["id"], 1);
        assert_eq!(replies[1]["id"], 2);
    }
}
