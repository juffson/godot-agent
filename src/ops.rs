//! Editor operations: requested by the HTTP thread, executed on the Godot
//! main thread (editor APIs are not thread-safe).

use serde_json::{Value, json};

/// One operation the HTTP thread asks the main thread to perform.
#[derive(Debug, Clone)]
pub enum EditorOp {
    GetEditorInfo,
    GetSceneTree { max_depth: i64 },
    OpenScene { path: String },
    SaveAllScenes,
    PlayScene { scene_path: Option<String> },
    StopPlaying,
    ExecuteScript { code: String },
}

/// MCP tool definitions served by `tools/list`.
pub fn tool_definitions() -> Value {
    json!([
        {
            "name": "get_editor_info",
            "description": "Get live state of the running Godot editor: version, project name, currently edited scene, open scenes, and whether a scene is playing.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "get_scene_tree",
            "description": "Get the node tree (name, type, script, children) of the scene currently being edited in the Godot editor.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "max_depth": { "type": "number", "description": "Maximum tree depth to serialize (default 16)" }
                }
            }
        },
        {
            "name": "open_scene",
            "description": "Open a scene file in the live Godot editor.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "res:// path to a .tscn file" }
                },
                "required": ["path"]
            }
        },
        {
            "name": "save_all_scenes",
            "description": "Save all open scenes in the Godot editor.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "play_scene",
            "description": "Run the project from the editor (F5). Optionally play a specific scene instead of the main scene.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "scene_path": { "type": "string", "description": "Optional res:// path of the scene to play; defaults to the project main scene" }
                }
            }
        },
        {
            "name": "stop_playing",
            "description": "Stop the scene currently being played from the editor.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "execute_editor_script",
            "description": "Execute arbitrary GDScript inside the live Godot editor with full EditorInterface access. The code runs as the body of `func run():` in a @tool script; its return value is serialized back. Example: `return EditorInterface.get_edited_scene_root().name`",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "code": { "type": "string", "description": "GDScript statements to execute in the editor" }
                },
                "required": ["code"]
            }
        }
    ])
}

/// Parse a `tools/call` into an EditorOp.
pub fn parse_tool_call(name: &str, args: &Value) -> Result<EditorOp, String> {
    match name {
        "get_editor_info" => Ok(EditorOp::GetEditorInfo),
        "get_scene_tree" => Ok(EditorOp::GetSceneTree {
            max_depth: args.get("max_depth").and_then(Value::as_i64).unwrap_or(16),
        }),
        "open_scene" => {
            let path = args
                .get("path")
                .and_then(Value::as_str)
                .ok_or("open_scene requires a `path` argument")?;
            Ok(EditorOp::OpenScene { path: path.to_string() })
        }
        "save_all_scenes" => Ok(EditorOp::SaveAllScenes),
        "play_scene" => Ok(EditorOp::PlayScene {
            scene_path: args.get("scene_path").and_then(Value::as_str).map(String::from),
        }),
        "stop_playing" => Ok(EditorOp::StopPlaying),
        "execute_editor_script" => {
            let code = args
                .get("code")
                .and_then(Value::as_str)
                .ok_or("execute_editor_script requires a `code` argument")?;
            Ok(EditorOp::ExecuteScript { code: code.to_string() })
        }
        other => Err(format!("Unknown tool: {other}")),
    }
}
