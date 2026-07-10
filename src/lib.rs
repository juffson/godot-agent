//! Godot MCP Editor — an MCP server embedded in the Godot editor.
//!
//! A GDExtension EditorPlugin that serves the Model Context Protocol over
//! HTTP (streamable HTTP, stateless JSON mode) directly from the editor
//! process. AI assistants connect straight to the live editor — no external
//! bridge process needed.
//!
//! Threading model: the HTTP thread (server.rs) never touches Godot APIs.
//! It queues jobs; `process()` drains them each frame on the main thread.

mod chat;
mod ops;
mod server;

use std::sync::mpsc::{Receiver, channel};

use godot::classes::control::SizeFlags;
use godot::classes::editor_plugin::DockSlot;
use godot::classes::{
    Button, EditorInterface, EditorPlugin, Engine, GDScript, HBoxContainer, IEditorPlugin,
    LineEdit, Node, ProjectSettings, RichTextLabel, Script, VBoxContainer,
};
use godot::global::Error as GdError;
use godot::prelude::*;
use serde_json::{Value, json};

use chat::{ChatEvent, ChatSession};
use ops::{EditorOp, Job};
use server::McpHttpServer;

struct GodotMcpEditorExtension;

#[gdextension]
unsafe impl ExtensionLibrary for GodotMcpEditorExtension {}

#[derive(GodotClass)]
#[class(tool, init, base=EditorPlugin)]
pub struct GodotMcpEditor {
    base: Base<EditorPlugin>,
    jobs: Option<Receiver<Job>>,
    http: Option<McpHttpServer>,
    mcp_port: u16,
    // Chat dock
    dock: Option<Gd<VBoxContainer>>,
    transcript: Option<Gd<RichTextLabel>>,
    input: Option<Gd<LineEdit>>,
    chat: Option<ChatSession>,
}

#[godot_api]
impl IEditorPlugin for GodotMcpEditor {
    fn enter_tree(&mut self) {
        let port = std::env::var("GODOT_MCP_HTTP_PORT")
            .ok()
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(server::DEFAULT_PORT);

        let (tx, rx) = channel();
        match server::start(port, tx) {
            Ok(http) => {
                self.jobs = Some(rx);
                self.http = Some(http);
                self.mcp_port = port;
                godot_print!("[MCP] Editor MCP server listening on http://127.0.0.1:{port}/mcp");
            }
            Err(e) => godot_error!("[MCP] Failed to start MCP server: {e}"),
        }

        self.build_chat_dock();
    }

    fn exit_tree(&mut self) {
        if let Some(mut chat) = self.chat.take() {
            chat.kill();
        }
        if let Some(mut dock) = self.dock.take() {
            self.base_mut().remove_control_from_docks(&dock);
            dock.queue_free();
        }
        self.transcript = None;
        self.input = None;
        if let Some(http) = self.http.take() {
            http.shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
            http.server.unblock();
        }
        self.jobs = None;
    }

    fn process(&mut self, _delta: f64) {
        // Drain pending MCP jobs on the main thread; editor APIs are not thread-safe.
        if let Some(rx) = &self.jobs {
            while let Ok(job) = rx.try_recv() {
                let result = execute_op(&job.op);
                let _ = job.reply.send(result);
            }
        }

        // Drain chat events from the Claude CLI reader thread.
        let mut lines: Vec<String> = Vec::new();
        let mut process_died = false;
        if let Some(chat) = &self.chat {
            while let Ok(event) = chat.events.try_recv() {
                match event {
                    ChatEvent::AssistantText(text) => {
                        lines.push(format!("{}\n", esc_bbcode(&text)));
                    }
                    ChatEvent::ToolUse(name) => {
                        lines.push(format!("[color=#7f8c8d]⚙ {}[/color]\n", esc_bbcode(&name)));
                    }
                    ChatEvent::TurnDone { error } => {
                        if let Some(err) = error {
                            lines.push(format!("[color=#e06c75]✗ {}[/color]\n", esc_bbcode(&err)));
                        }
                    }
                    ChatEvent::ProcessExit(msg) => {
                        lines.push(format!("[color=#e06c75]{}[/color]\n", esc_bbcode(&msg)));
                        process_died = true;
                    }
                }
            }
        }
        if process_died {
            self.chat = None;
        }
        for line in lines {
            self.append_transcript(&line);
        }
    }
}

#[godot_api]
impl GodotMcpEditor {
    /// Build the "AI Chat" dock panel and attach it to the editor.
    fn build_chat_dock(&mut self) {
        let mut dock = VBoxContainer::new_alloc();
        dock.set_name("AI Chat");

        let mut transcript = RichTextLabel::new_alloc();
        transcript.set_use_bbcode(true);
        transcript.set_scroll_follow(true);
        transcript.set_selection_enabled(true);
        transcript.set_v_size_flags(SizeFlags::EXPAND_FILL);
        transcript.append_text(
            "[color=#7f8c8d]Chat with Claude about this project. It can see and edit the live editor (scene tree, nodes, scripts).[/color]\n",
        );
        dock.add_child(&transcript);

        let mut row = HBoxContainer::new_alloc();

        let mut input = LineEdit::new_alloc();
        input.set_h_size_flags(SizeFlags::EXPAND_FILL);
        input.set_placeholder("Ask or instruct… (Enter to send)");
        input.connect("text_submitted", &self.to_gd().callable("on_input_submitted"));
        row.add_child(&input);

        let mut send = Button::new_alloc();
        send.set_text("Send");
        send.connect("pressed", &self.to_gd().callable("on_send_pressed"));
        row.add_child(&send);

        let mut fresh = Button::new_alloc();
        fresh.set_text("New");
        fresh.set_tooltip_text("End the current conversation and start a new one");
        fresh.connect("pressed", &self.to_gd().callable("on_new_pressed"));
        row.add_child(&fresh);

        dock.add_child(&row);

        self.base_mut().add_control_to_dock(DockSlot::RIGHT_UL, &dock);
        self.dock = Some(dock);
        self.transcript = Some(transcript);
        self.input = Some(input);
    }

    #[func]
    fn on_input_submitted(&mut self, _text: GString) {
        self.submit_message();
    }

    #[func]
    fn on_send_pressed(&mut self) {
        self.submit_message();
    }

    #[func]
    fn on_new_pressed(&mut self) {
        if let Some(mut chat) = self.chat.take() {
            chat.kill();
        }
        if let Some(transcript) = &mut self.transcript {
            transcript.clear();
        }
        self.append_transcript("[color=#7f8c8d]New conversation started.[/color]\n");
    }

    fn submit_message(&mut self) {
        let Some(input) = &mut self.input else { return };
        let text = input.get_text().to_string();
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        input.clear();

        // Lazily start the Claude process on first message.
        if self.chat.is_none() {
            let project_root = ProjectSettings::singleton()
                .globalize_path("res://")
                .to_string();
            match ChatSession::spawn(&project_root, self.mcp_port) {
                Ok(session) => self.chat = Some(session),
                Err(e) => {
                    self.append_transcript(&format!(
                        "[color=#e06c75]{}[/color]\n",
                        esc_bbcode(&e)
                    ));
                    return;
                }
            }
        }

        self.append_transcript(&format!(
            "\n[color=#6da9ff][b]You[/b][/color]  {}\n",
            esc_bbcode(&text)
        ));

        if let Some(chat) = &mut self.chat {
            if let Err(e) = chat.send(&text) {
                self.append_transcript(&format!("[color=#e06c75]{}[/color]\n", esc_bbcode(&e)));
                self.chat = None;
            }
        }
    }

    fn append_transcript(&mut self, bbcode: &str) {
        if let Some(transcript) = &mut self.transcript {
            transcript.append_text(bbcode);
        }
    }
}

/// Escape user/model text so it renders literally inside the bbcode transcript.
fn esc_bbcode(text: &str) -> String {
    text.replace('[', "[lb]")
}

fn execute_op(op: &EditorOp) -> Result<Value, String> {
    match op {
        EditorOp::GetEditorInfo => get_editor_info(),
        EditorOp::GetSceneTree { max_depth } => get_scene_tree(*max_depth),
        EditorOp::OpenScene { path } => open_scene(path),
        EditorOp::SaveAllScenes => {
            EditorInterface::singleton().save_all_scenes();
            Ok(json!("All scenes saved"))
        }
        EditorOp::PlayScene { scene_path } => play_scene(scene_path.as_deref()),
        EditorOp::StopPlaying => {
            EditorInterface::singleton().stop_playing_scene();
            Ok(json!("Stopped playing"))
        }
        EditorOp::ExecuteScript { code } => execute_script(code),
    }
}

fn get_editor_info() -> Result<Value, String> {
    let editor = EditorInterface::singleton();
    let version = Engine::singleton()
        .get_version_info()
        .get("string")
        .map(|v| v.to_string())
        .unwrap_or_default();
    let project_name = ProjectSettings::singleton()
        .get_setting("application/config/name")
        .to_string();
    let edited_scene = editor
        .get_edited_scene_root()
        .map(|root| root.get_scene_file_path().to_string())
        .unwrap_or_default();
    let open_scenes: Vec<String> = editor
        .get_open_scenes()
        .to_vec()
        .iter()
        .map(|s| s.to_string())
        .collect();

    Ok(json!({
        "godot_version": version,
        "project_name": project_name,
        "edited_scene": edited_scene,
        "open_scenes": open_scenes,
        "is_playing": editor.is_playing_scene(),
    }))
}

fn get_scene_tree(max_depth: i64) -> Result<Value, String> {
    let root = EditorInterface::singleton()
        .get_edited_scene_root()
        .ok_or("No scene is currently being edited in the editor")?;
    Ok(serialize_node(&root, max_depth))
}

fn serialize_node(node: &Gd<Node>, depth: i64) -> Value {
    let mut info = json!({
        "name": node.get_name().to_string(),
        "type": node.get_class().to_string(),
    });

    if let Some(script) = node.get_script() {
        let path = script.get_path().to_string();
        if !path.is_empty() {
            info["script"] = json!(path);
        }
    }

    if depth > 0 && node.get_child_count() > 0 {
        let children: Vec<Value> = node
            .get_children()
            .iter_shared()
            .map(|child| serialize_node(&child, depth - 1))
            .collect();
        info["children"] = json!(children);
    }

    info
}

fn open_scene(path: &str) -> Result<Value, String> {
    if !path.starts_with("res://") || !path.ends_with(".tscn") {
        return Err(format!("Expected a res:// path to a .tscn file, got: {path}"));
    }
    EditorInterface::singleton().open_scene_from_path(path);
    Ok(json!(format!("Opened {path}")))
}

fn play_scene(scene_path: Option<&str>) -> Result<Value, String> {
    let mut editor = EditorInterface::singleton();
    match scene_path {
        Some(path) => {
            editor.play_custom_scene(path);
            Ok(json!(format!("Playing {path}")))
        }
        None => {
            editor.play_main_scene();
            Ok(json!("Playing main scene"))
        }
    }
}

/// Run arbitrary GDScript in the editor: the code becomes the body of
/// `func run():` in a @tool RefCounted script, so `EditorInterface` and the
/// full editor API are available. The return value is serialized to JSON.
fn execute_script(code: &str) -> Result<Value, String> {
    let mut body = String::new();
    for line in code.lines() {
        body.push('\t');
        body.push_str(line);
        body.push('\n');
    }
    let source = format!("@tool\nextends RefCounted\nfunc run():\n{body}");

    let mut script = GDScript::new_gd();
    script.set_source_code(&source);
    let err = script.reload();
    if err != GdError::OK {
        return Err(format!(
            "GDScript parse error ({err:?}). The code runs inside `func run():` — check indentation and syntax."
        ));
    }

    let instance = script.call("new", &[]);
    let mut instance = instance
        .try_to::<Gd<RefCounted>>()
        .map_err(|e| format!("Failed to instantiate script: {e}"))?;
    let result = instance.call("run", &[]);
    Ok(variant_to_json(&result, 0))
}

fn variant_to_json(value: &Variant, depth: u32) -> Value {
    if depth > 8 {
        return json!(value.to_string());
    }
    match value.get_type() {
        VariantType::NIL => Value::Null,
        VariantType::BOOL => json!(value.to::<bool>()),
        VariantType::INT => json!(value.to::<i64>()),
        VariantType::FLOAT => json!(value.to::<f64>()),
        VariantType::STRING | VariantType::STRING_NAME | VariantType::NODE_PATH => {
            json!(value.to_string())
        }
        VariantType::ARRAY => {
            let arr = value.to::<VarArray>();
            Value::Array(
                arr.iter_shared()
                    .map(|v| variant_to_json(&v, depth + 1))
                    .collect(),
            )
        }
        VariantType::DICTIONARY => {
            let dict = value.to::<Dictionary<Variant, Variant>>();
            let mut map = serde_json::Map::new();
            for (k, v) in dict.iter_shared() {
                map.insert(k.to_string(), variant_to_json(&v, depth + 1));
            }
            Value::Object(map)
        }
        VariantType::PACKED_STRING_ARRAY => {
            let arr = value.to::<PackedStringArray>();
            Value::Array(arr.to_vec().iter().map(|s| json!(s.to_string())).collect())
        }
        _ => json!(value.to_string()),
    }
}
