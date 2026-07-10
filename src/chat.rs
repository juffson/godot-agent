//! Chat backend: drives a headless Claude Code CLI process.
//!
//! The dock panel (lib.rs) sends user messages here; we pipe them to a
//! long-running `claude --print --input-format stream-json` process whose MCP
//! config points back at this editor's own MCP endpoint — so the assistant
//! the user chats with can directly inspect and modify the live editor.
//!
//! Threading: a reader thread parses the CLI's stream-json stdout into
//! ChatEvents; the plugin drains them on the main thread each frame.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};

use serde_json::{Value, json};

/// Events surfaced to the UI.
pub enum ChatEvent {
    AssistantText(String),
    ToolUse(String),
    TurnDone { error: Option<String> },
    ProcessExit(String),
}

pub struct ChatSession {
    child: Child,
    stdin: ChildStdin,
    pub events: Receiver<ChatEvent>,
}

impl ChatSession {
    /// Spawn a persistent Claude Code process rooted at the project directory,
    /// with MCP access to this editor instance only.
    pub fn spawn(project_root: &str, mcp_port: u16) -> Result<Self, String> {
        let claude = find_claude()?;
        // Include the game-side server too; if the game isn't running Claude
        // simply reports that server as unavailable.
        let game_port = std::env::var("GODOT_AGENT_GAME_PORT")
            .ok()
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(crate::server::DEFAULT_GAME_PORT);
        let mcp_config = format!(
            r#"{{"mcpServers":{{"godot-editor":{{"type":"http","url":"http://127.0.0.1:{mcp_port}/mcp"}},"godot-game":{{"type":"http","url":"http://127.0.0.1:{game_port}/mcp"}}}}}}"#
        );

        let mut child = Command::new(&claude)
            .args([
                "--print",
                "--input-format", "stream-json",
                "--output-format", "stream-json",
                "--verbose",
                "--permission-mode", "acceptEdits",
                "--allowedTools", "mcp__godot-editor,mcp__godot-game",
                "--mcp-config", &mcp_config,
                "--strict-mcp-config",
            ])
            .current_dir(project_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to start `{claude}`: {e}"))?;

        let stdout = child.stdout.take().ok_or("No stdout pipe")?;
        let stdin = child.stdin.take().ok_or("No stdin pipe")?;

        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("godot-agent-chat".into())
            .spawn(move || reader_loop(stdout, tx))
            .map_err(|e| format!("Failed to spawn reader thread: {e}"))?;

        Ok(Self { child, stdin, events: rx })
    }

    pub fn send(&mut self, text: &str) -> Result<(), String> {
        let msg = json!({
            "type": "user",
            "message": { "role": "user", "content": [ { "type": "text", "text": text } ] }
        });
        writeln!(self.stdin, "{msg}").map_err(|e| format!("Failed to write to Claude: {e}"))
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for ChatSession {
    fn drop(&mut self) {
        self.kill();
    }
}

fn reader_loop(stdout: std::process::ChildStdout, tx: Sender<ChatEvent>) {
    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(&line) else { continue };
        match event.get("type").and_then(Value::as_str) {
            Some("assistant") => {
                let blocks = event
                    .pointer("/message/content")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(text) = block.get("text").and_then(Value::as_str) {
                                if !text.trim().is_empty() {
                                    let _ = tx.send(ChatEvent::AssistantText(text.to_string()));
                                }
                            }
                        }
                        Some("tool_use") => {
                            let name = block
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("tool")
                                .trim_start_matches("mcp__godot-editor__")
                                .to_string();
                            let _ = tx.send(ChatEvent::ToolUse(name));
                        }
                        _ => {}
                    }
                }
            }
            Some("result") => {
                let error = match event.get("subtype").and_then(Value::as_str) {
                    Some("success") | None => None,
                    Some(other) => Some(
                        event
                            .get("result")
                            .and_then(Value::as_str)
                            .unwrap_or(other)
                            .to_string(),
                    ),
                };
                let _ = tx.send(ChatEvent::TurnDone { error });
            }
            _ => {}
        }
    }
    let _ = tx.send(ChatEvent::ProcessExit(
        "Claude process exited. Press New to start a fresh session (check `claude` login if this repeats).".to_string(),
    ));
}

/// Locate the claude CLI. Uses a login shell so Homebrew paths work even when
/// the editor was launched from Finder.
fn find_claude() -> Result<String, String> {
    if let Ok(path) = std::env::var("GODOT_AGENT_CLAUDE_BIN") {
        return Ok(path);
    }
    if let Ok(out) = Command::new("/bin/sh").args(["-lc", "command -v claude"]).output() {
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !path.is_empty() {
            return Ok(path);
        }
    }
    for candidate in [
        "/opt/homebrew/bin/claude",
        "/usr/local/bin/claude",
        &format!("{}/.local/bin/claude", std::env::var("HOME").unwrap_or_default()),
    ] {
        if std::path::Path::new(candidate).exists() {
            return Ok(candidate.to_string());
        }
    }
    Err("Could not find the `claude` CLI. Install Claude Code or set GODOT_AGENT_CLAUDE_BIN.".to_string())
}
