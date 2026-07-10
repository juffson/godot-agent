//! In-game runtime: an MCP server inside the *running game* process.
//!
//! `GodotAgentRuntime` is registered as an autoload (the editor plugin adds it
//! to project settings automatically). When the game starts it serves MCP on
//! 127.0.0.1:6011, exposing what the editor process cannot reach: input
//! injection, the live runtime scene tree, screenshots of the rendered frame,
//! and script execution in the game context.
//!
//! Input simulation is scheduled on a timeline (press/release pairs, waits)
//! and dispatched from `process()` so events land on separate frames like
//! real user input would.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, Sender, channel};

use godot::classes::node::ProcessMode;
use godot::classes::{
    Engine, INode, Input, InputEvent, InputEventAction, InputEventKey, InputEventMouseButton,
    InputEventMouseMotion, Marshalls, Node, Os, SceneTree,
};
use godot::global::MouseButton;
use godot::prelude::*;
use serde_json::{Value, json};

use crate::gd_util::{run_gdscript, serialize_node};
use crate::server::{self, Job, McpHttpServer};

/// Gap between the press and release of a synthesized tap/click, and the
/// default spacing between consecutive events. Keeps events on separate
/// frames so UI code sees them like real input.
const TAP_GAP: f64 = 0.06;
const EVENT_GAP: f64 = 0.05;

struct ActiveSequence {
    timeline: VecDeque<(f64, Gd<InputEvent>)>, // (delay before dispatch, event)
    countdown: f64,
    dispatched: usize,
    reply: Sender<Result<Value, String>>,
}

#[derive(GodotClass)]
#[class(init, base=Node)]
pub struct GodotAgentRuntime {
    base: Base<Node>,
    jobs: Option<Receiver<Job>>,
    http: Option<McpHttpServer>,
    active: Option<ActiveSequence>,
}

#[godot_api]
impl INode for GodotAgentRuntime {
    fn ready(&mut self) {
        if Engine::singleton().is_editor_hint() {
            return;
        }
        let port = std::env::var("GODOT_AGENT_GAME_PORT")
            .ok()
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(server::DEFAULT_GAME_PORT);

        let (tx, rx) = channel();
        match server::start(port, "godot-agent-game", game_tool_definitions(), tx) {
            Ok(http) => {
                self.jobs = Some(rx);
                self.http = Some(http);
                godot_print!("[MCP] Game MCP server listening on http://127.0.0.1:{port}/mcp");
            }
            Err(e) => godot_error!("[MCP] Failed to start game MCP server: {e}"),
        }

        // Keep serving even when the scene tree is paused.
        self.base_mut().set_process_mode(ProcessMode::ALWAYS);
    }

    fn exit_tree(&mut self) {
        if let Some(http) = self.http.take() {
            http.shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
            http.server.unblock();
        }
        self.jobs = None;
    }

    fn process(&mut self, delta: f64) {
        self.advance_input_sequence(delta);

        // Drain queued MCP jobs (collect first to avoid holding the borrow).
        let mut pending = Vec::new();
        if let Some(rx) = &self.jobs {
            while let Ok(job) = rx.try_recv() {
                pending.push(job);
            }
        }
        for job in pending {
            match job.name.as_str() {
                "simulate_input" => {
                    if self.active.is_some() {
                        let _ = job
                            .reply
                            .send(Err("An input sequence is already in progress".into()));
                        continue;
                    }
                    match build_timeline(&job.args) {
                        Ok(timeline) if timeline.is_empty() => {
                            let _ = job.reply.send(Err("events array is empty".into()));
                        }
                        Ok(timeline) => {
                            let countdown = timeline.front().map(|(d, _)| *d).unwrap_or(0.0);
                            self.active = Some(ActiveSequence {
                                timeline,
                                countdown,
                                dispatched: 0,
                                reply: job.reply,
                            });
                        }
                        Err(e) => {
                            let _ = job.reply.send(Err(e));
                        }
                    }
                }
                other => {
                    let result = execute_game_op(other, &job.args);
                    let _ = job.reply.send(result);
                }
            }
        }
    }
}

impl GodotAgentRuntime {
    fn advance_input_sequence(&mut self, delta: f64) {
        let Some(seq) = &mut self.active else { return };
        seq.countdown -= delta;
        while seq.countdown <= 0.0 {
            let Some((_, event)) = seq.timeline.pop_front() else { break };
            Input::singleton().parse_input_event(&event);
            seq.dispatched += 1;
            match seq.timeline.front() {
                Some((delay, _)) => seq.countdown += *delay,
                None => break,
            }
        }
        if seq.timeline.is_empty() {
            let done = self.active.take().unwrap();
            let _ = done
                .reply
                .send(Ok(json!(format!("Dispatched {} input events", done.dispatched))));
        }
    }
}

fn game_tool_definitions() -> Value {
    json!([
        {
            "name": "get_game_info",
            "description": "Get live state of the running game: FPS, current scene, window size.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "get_scene_tree",
            "description": "Get the live runtime node tree of the running game (includes autoloads and dynamically created nodes).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "max_depth": { "type": "number", "description": "Maximum tree depth to serialize (default 16)" }
                }
            }
        },
        {
            "name": "capture_screenshot",
            "description": "Capture the game's rendered frame and return it as an image (downscaled to max 1280px wide).",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "simulate_input",
            "description": "Inject a timed sequence of input events into the running game, as if a player performed them. Events: {type:'key', key:'Enter'|'A'|'Escape'|..., pressed?:bool} (omit pressed for tap), {type:'text', text:'hello'} (types characters), {type:'mouse_click', x, y, button?:'left'|'right'|'middle', double?:bool}, {type:'mouse_move', x, y}, {type:'action', action:'ui_accept', pressed?:bool} (omit pressed for tap, uses the project's input map), {type:'wait', ms:500}. Events are spaced across frames automatically; returns after the last event is dispatched.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "events": {
                        "type": "array",
                        "description": "Sequence of input events to dispatch in order",
                        "items": { "type": "object" }
                    }
                },
                "required": ["events"]
            }
        },
        {
            "name": "execute_script",
            "description": "Execute arbitrary GDScript inside the running game process. The code runs as the body of `func run():`; use Engine.get_main_loop() to reach the SceneTree (e.g. `return Engine.get_main_loop().current_scene.name`). The return value is serialized back.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "code": { "type": "string", "description": "GDScript statements to execute in the game" }
                },
                "required": ["code"]
            }
        }
    ])
}

fn scene_tree() -> Result<Gd<SceneTree>, String> {
    Engine::singleton()
        .get_main_loop()
        .and_then(|ml| ml.try_cast::<SceneTree>().ok())
        .ok_or("Main loop is not a SceneTree".to_string())
}

fn execute_game_op(name: &str, args: &Value) -> Result<Value, String> {
    match name {
        "get_game_info" => get_game_info(),
        "get_scene_tree" => {
            let max_depth = args.get("max_depth").and_then(Value::as_i64).unwrap_or(16);
            let root = scene_tree()?.get_root().ok_or("No root window")?;
            Ok(serialize_node(&root.upcast::<Node>(), max_depth))
        }
        "capture_screenshot" => capture_screenshot(),
        "execute_script" => {
            let code = args
                .get("code")
                .and_then(Value::as_str)
                .ok_or("execute_script requires a `code` argument")?;
            run_gdscript(code)
        }
        other => Err(format!("Unknown tool: {other}")),
    }
}

fn get_game_info() -> Result<Value, String> {
    let tree = scene_tree()?;
    let current = tree
        .get_current_scene()
        .map(|s| {
            json!({
                "name": s.get_name().to_string(),
                "path": s.get_scene_file_path().to_string(),
            })
        })
        .unwrap_or(Value::Null);
    let window_size = tree
        .get_root()
        .map(|w| json!([w.get_size().x, w.get_size().y]))
        .unwrap_or(Value::Null);

    Ok(json!({
        "fps": Engine::singleton().get_frames_per_second(),
        "current_scene": current,
        "window_size": window_size,
        "paused": tree.is_paused(),
    }))
}

fn capture_screenshot() -> Result<Value, String> {
    let root = scene_tree()?.get_root().ok_or("No root window")?;
    let texture = root.get_texture().ok_or("Viewport has no texture")?;
    let mut image = texture.get_image().ok_or("Failed to read viewport image")?;

    let width = image.get_width();
    if width > 1280 {
        let height = image.get_height() * 1280 / width;
        image.resize(1280, height);
    }

    let buffer = image.save_png_to_buffer();
    if buffer.is_empty() {
        return Err("Failed to encode PNG".to_string());
    }
    let b64 = Marshalls::singleton().raw_to_base64(&buffer).to_string();
    Ok(json!({ "_image_base64": b64, "_mime": "image/png" }))
}

/// Turn the `events` array into a dispatch timeline of (delay, event) pairs.
fn build_timeline(args: &Value) -> Result<VecDeque<(f64, Gd<InputEvent>)>, String> {
    let events = args
        .get("events")
        .and_then(Value::as_array)
        .ok_or("simulate_input requires an `events` array")?;

    let mut timeline: VecDeque<(f64, Gd<InputEvent>)> = VecDeque::new();
    let mut pending_delay = 0.0_f64;

    let push = |timeline: &mut VecDeque<(f64, Gd<InputEvent>)>,
                    delay: &mut f64,
                    event: Gd<InputEvent>,
                    gap_after: f64| {
        timeline.push_back((*delay, event));
        *delay = gap_after;
    };

    for (i, entry) in events.iter().enumerate() {
        let kind = entry
            .get("type")
            .and_then(Value::as_str)
            .ok_or(format!("events[{i}] is missing `type`"))?;
        match kind {
            "wait" => {
                let ms = entry.get("ms").and_then(Value::as_f64).unwrap_or(100.0);
                pending_delay += ms / 1000.0;
            }
            "key" => {
                let key_name = entry
                    .get("key")
                    .and_then(Value::as_str)
                    .ok_or(format!("events[{i}]: key events need a `key` name"))?;
                let keycode = Os::singleton().find_keycode_from_string(key_name);
                match entry.get("pressed").and_then(Value::as_bool) {
                    Some(pressed) => {
                        push(&mut timeline, &mut pending_delay, key_event(keycode, pressed), EVENT_GAP);
                    }
                    None => {
                        push(&mut timeline, &mut pending_delay, key_event(keycode, true), TAP_GAP);
                        push(&mut timeline, &mut pending_delay, key_event(keycode, false), EVENT_GAP);
                    }
                }
            }
            "text" => {
                let text = entry
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or(format!("events[{i}]: text events need `text`"))?;
                for ch in text.chars() {
                    push(&mut timeline, &mut pending_delay, char_event(ch, true), 0.02);
                    push(&mut timeline, &mut pending_delay, char_event(ch, false), 0.02);
                }
            }
            "mouse_click" => {
                let (x, y) = point(entry, i)?;
                let button = mouse_button(entry.get("button").and_then(Value::as_str));
                let double = entry.get("double").and_then(Value::as_bool).unwrap_or(false);
                // Move the pointer there first so hover states update.
                push(&mut timeline, &mut pending_delay, motion_event(x, y), TAP_GAP);
                push(&mut timeline, &mut pending_delay, click_event(x, y, button, true, double), TAP_GAP);
                push(&mut timeline, &mut pending_delay, click_event(x, y, button, false, false), EVENT_GAP);
            }
            "mouse_move" => {
                let (x, y) = point(entry, i)?;
                push(&mut timeline, &mut pending_delay, motion_event(x, y), EVENT_GAP);
            }
            "action" => {
                let action = entry
                    .get("action")
                    .and_then(Value::as_str)
                    .ok_or(format!("events[{i}]: action events need `action`"))?;
                match entry.get("pressed").and_then(Value::as_bool) {
                    Some(pressed) => {
                        push(&mut timeline, &mut pending_delay, action_event(action, pressed), EVENT_GAP);
                    }
                    None => {
                        push(&mut timeline, &mut pending_delay, action_event(action, true), TAP_GAP);
                        push(&mut timeline, &mut pending_delay, action_event(action, false), EVENT_GAP);
                    }
                }
            }
            other => return Err(format!("events[{i}]: unknown event type `{other}`")),
        }
    }

    Ok(timeline)
}

fn point(entry: &Value, i: usize) -> Result<(f32, f32), String> {
    let x = entry.get("x").and_then(Value::as_f64);
    let y = entry.get("y").and_then(Value::as_f64);
    match (x, y) {
        (Some(x), Some(y)) => Ok((x as f32, y as f32)),
        _ => Err(format!("events[{i}]: mouse events need numeric `x` and `y`")),
    }
}

fn mouse_button(name: Option<&str>) -> MouseButton {
    match name.unwrap_or("left") {
        "right" => MouseButton::RIGHT,
        "middle" => MouseButton::MIDDLE,
        _ => MouseButton::LEFT,
    }
}

fn key_event(keycode: godot::global::Key, pressed: bool) -> Gd<InputEvent> {
    let mut ev = InputEventKey::new_gd();
    ev.set_keycode(keycode);
    ev.set_physical_keycode(keycode);
    ev.set_pressed(pressed);
    ev.upcast()
}

fn char_event(ch: char, pressed: bool) -> Gd<InputEvent> {
    let mut ev = InputEventKey::new_gd();
    ev.set_unicode(ch as u32);
    ev.set_pressed(pressed);
    ev.upcast()
}

fn click_event(x: f32, y: f32, button: MouseButton, pressed: bool, double: bool) -> Gd<InputEvent> {
    let mut ev = InputEventMouseButton::new_gd();
    let pos = Vector2::new(x, y);
    ev.set_position(pos);
    ev.set_global_position(pos);
    ev.set_button_index(button);
    ev.set_pressed(pressed);
    ev.set_double_click(double);
    ev.upcast()
}

fn motion_event(x: f32, y: f32) -> Gd<InputEvent> {
    let mut ev = InputEventMouseMotion::new_gd();
    let pos = Vector2::new(x, y);
    ev.set_position(pos);
    ev.set_global_position(pos);
    ev.upcast()
}

fn action_event(action: &str, pressed: bool) -> Gd<InputEvent> {
    let mut ev = InputEventAction::new_gd();
    ev.set_action(&StringName::from(action));
    ev.set_pressed(pressed);
    ev.upcast()
}
