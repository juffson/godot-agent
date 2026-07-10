//! HTTP thread: a minimal MCP "streamable HTTP" server (stateless JSON mode).
//!
//! Speaks JSON-RPC 2.0 over POST. Every `tools/call` is forwarded to the Godot
//! main thread through the job queue and the HTTP worker blocks until the
//! editor replies (or times out). Bound to 127.0.0.1 only.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::time::Duration;

use serde_json::{Value, json};
use tiny_http::{Header, Method, Response, Server};

use crate::ops::{Job, parse_tool_call, tool_definitions};

pub const DEFAULT_PORT: u16 = 6010;
const TOOL_TIMEOUT: Duration = Duration::from_secs(30);

pub struct McpHttpServer {
    pub server: Arc<Server>,
    pub shutdown: Arc<AtomicBool>,
}

/// Start the HTTP server thread. Returns handles for shutdown.
pub fn start(port: u16, jobs: Sender<Job>) -> Result<McpHttpServer, String> {
    let server = Server::http(("127.0.0.1", port))
        .map_err(|e| format!("Failed to bind 127.0.0.1:{port}: {e}"))?;
    let server = Arc::new(server);
    let shutdown = Arc::new(AtomicBool::new(false));

    let srv = Arc::clone(&server);
    let stop = Arc::clone(&shutdown);
    std::thread::Builder::new()
        .name("godot-mcp-http".into())
        .spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match srv.recv() {
                    Ok(request) => handle_request(request, &jobs),
                    Err(_) => break, // unblock() or fatal error
                }
            }
        })
        .map_err(|e| format!("Failed to spawn HTTP thread: {e}"))?;

    Ok(McpHttpServer { server, shutdown })
}

fn json_response(status: u16, body: &Value) -> Response<std::io::Cursor<Vec<u8>>> {
    let data = serde_json::to_vec(body).unwrap_or_default();
    Response::from_data(data)
        .with_status_code(status)
        .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap())
}

fn handle_request(mut request: tiny_http::Request, jobs: &Sender<Job>) {
    match *request.method() {
        Method::Post => {}
        Method::Delete => {
            let _ = request.respond(Response::empty(200));
            return;
        }
        _ => {
            let _ = request.respond(Response::empty(405));
            return;
        }
    }

    let mut body = String::new();
    if request.as_reader().read_to_string(&mut body).is_err() {
        let _ = request.respond(Response::empty(400));
        return;
    }

    let message: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            let resp = json!({
                "jsonrpc": "2.0", "id": null,
                "error": { "code": -32700, "message": format!("Parse error: {e}") }
            });
            let _ = request.respond(json_response(400, &resp));
            return;
        }
    };

    // Notifications (no id) just get 202 Accepted.
    let id = message.get("id").cloned();
    if id.is_none() || id == Some(Value::Null) {
        let _ = request.respond(Response::empty(202));
        return;
    }
    let id = id.unwrap();

    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let params = message.get("params").cloned().unwrap_or_else(|| json!({}));

    let response = match method {
        "initialize" => {
            let protocol = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("2024-11-05");
            json!({
                "jsonrpc": "2.0", "id": id,
                "result": {
                    "protocolVersion": protocol,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "godot-agent", "version": env!("CARGO_PKG_VERSION") }
                }
            })
        }
        "ping" => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
        "tools/list" => json!({
            "jsonrpc": "2.0", "id": id,
            "result": { "tools": tool_definitions() }
        }),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            match call_tool(name, &args, jobs) {
                Ok(result) => json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": {
                        "content": [ { "type": "text", "text": stringify_result(&result) } ]
                    }
                }),
                Err(message) => json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": {
                        "content": [ { "type": "text", "text": message } ],
                        "isError": true
                    }
                }),
            }
        }
        other => json!({
            "jsonrpc": "2.0", "id": id,
            "error": { "code": -32601, "message": format!("Method not found: {other}") }
        }),
    };

    let _ = request.respond(json_response(200, &response));
}

fn call_tool(name: &str, args: &Value, jobs: &Sender<Job>) -> Result<Value, String> {
    let op = parse_tool_call(name, args)?;
    let (reply_tx, reply_rx) = channel();
    jobs.send(Job { op, reply: reply_tx })
        .map_err(|_| "Editor plugin is shutting down".to_string())?;
    reply_rx
        .recv_timeout(TOOL_TIMEOUT)
        .map_err(|_| {
            "Timed out waiting for the editor main thread. The editor may be busy (e.g. a modal dialog is open) or the scene is playing with the editor paused.".to_string()
        })?
}

fn stringify_result(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    }
}
