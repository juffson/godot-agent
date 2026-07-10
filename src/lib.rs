//! Godot MCP Editor — an MCP server embedded in the Godot editor.
//!
//! A GDExtension EditorPlugin that serves the Model Context Protocol over
//! HTTP (streamable HTTP, stateless JSON mode) directly from the editor
//! process. AI assistants connect straight to the live editor — no external
//! bridge process needed.
//!
//! Threading model: the HTTP thread (server.rs) never touches Godot APIs.
//! It queues jobs; `process()` drains them each frame on the main thread.

mod ops;
mod server;

use std::sync::mpsc::{Receiver, channel};

use godot::classes::{
    EditorInterface, EditorPlugin, Engine, GDScript, IEditorPlugin, Node, ProjectSettings, Script,
};
use godot::global::Error as GdError;
use godot::prelude::*;
use serde_json::{Value, json};

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
                godot_print!("[MCP] Editor MCP server listening on http://127.0.0.1:{port}/mcp");
            }
            Err(e) => godot_error!("[MCP] Failed to start MCP server: {e}"),
        }
    }

    fn exit_tree(&mut self) {
        if let Some(http) = self.http.take() {
            http.shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
            http.server.unblock();
        }
        self.jobs = None;
    }

    fn process(&mut self, _delta: f64) {
        let Some(rx) = &self.jobs else { return };
        // Drain pending jobs on the main thread; editor APIs are not thread-safe.
        while let Ok(job) = rx.try_recv() {
            let result = execute_op(&job.op);
            let _ = job.reply.send(result);
        }
    }
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
