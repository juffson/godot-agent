# Godot Agent

An AI agent embedded directly inside the Godot editor, written in Rust.

Install a single GDExtension into your project and you get two things:

1. **An MCP server inside the editor** — AI assistants (Claude Code, or any
   MCP client) connect over HTTP and drive the **live editor** in real time:
   inspect the scene tree, open scenes, run the game, and execute arbitrary
   editor-side GDScript. No external bridge process, no per-operation engine
   startup, millisecond latency.
2. **An "AI Chat" dock panel** — chat with Claude without leaving the editor.
   The panel drives a headless [Claude Code](https://claude.com/claude-code)
   session whose MCP config points back at this same editor, so the assistant
   you chat with can directly see and modify what you're working on.

And when you run the game, a third piece comes alive:

3. **An MCP server inside the running game** (port 6011) — input simulation
   (clicks, keys, actions from the project's input map), screenshots of the
   rendered frame, the live runtime scene tree, and script execution in the
   game context. The AI can play the game: press buttons, read state, look at
   the screen.

```
AI Chat dock ─→ claude CLI ─┐
                            ├─→ :6010/mcp ─→ Godot editor process
external MCP clients ───────┤
                            └─→ :6011/mcp ─→ running game process
```

## Requirements

- Rust toolchain (1.85+) — build time only
- Godot 4.2+ (tested on 4.6.3)
- [Claude Code](https://claude.com/claude-code) CLI installed and logged in
  (only needed for the chat dock; the MCP servers work without it)

**No Node.js, npm, or external server process.** The whole thing is one Rust
dylib; MCP is served natively from inside the Godot processes over plain HTTP.

## How it works — end-to-end flow

1. `./install.sh <project>` builds the extension and copies
   `addons/godot_agent/` (gdextension manifest + dylib + autoload stub) into
   your project.
2. **Open the project in the Godot editor.** The GDExtension loads
   automatically (no plugin enabling needed) and the `EditorPlugin`:
   - starts the editor MCP server on `127.0.0.1:6010/mcp`,
   - adds the **AI Chat** dock,
   - registers the `GodotAgentRuntime` autoload in project settings (once).
3. **Run the game** (F5, or let the AI call `play_scene`). The autoload
   starts the game MCP server on `127.0.0.1:6011/mcp`.
4. **Connect an AI:**
   - the built-in AI Chat dock spawns a headless Claude Code session
     preconfigured with both servers, or
   - any external MCP client connects to either port over HTTP.

A typical loop the AI can drive end-to-end: edit a scene (6010) → play it
(6010) → screenshot the rendered frame (6011) → click buttons / type text
(6011) → read the runtime scene tree to verify (6011) → stop (6010).

## Install

```bash
git clone https://github.com/juffson/godot-agent.git
cd godot-agent
./install.sh /path/to/your/godot/project --release
```

This builds the extension and copies it to `addons/godot_agent/` in your
project. Open the project in the Godot editor — the plugin loads automatically
(GDExtension editor plugins need no manual enabling) and prints:

```
[MCP] Editor MCP server listening on http://127.0.0.1:6010/mcp
```

Connect Claude Code:

```bash
claude mcp add --transport http godot-editor http://127.0.0.1:6010/mcp
```

Set `GODOT_MCP_HTTP_PORT` before launching the editor to change the port.

## Tools

| Tool | Description |
|---|---|
| `get_editor_info` | Godot version, project name, edited/open scenes, play state |
| `get_scene_tree` | Live node tree of the currently edited scene |
| `open_scene` | Open a `.tscn` in the editor |
| `save_all_scenes` | Save all open scenes |
| `play_scene` / `stop_playing` | Run the main scene (or a specific one) / stop |
| `execute_editor_script` | Run arbitrary GDScript in the editor (full `EditorInterface` access); the code becomes the body of `func run():` and its return value is serialized back |

`execute_editor_script` is the universal escape hatch — anything the editor
API can do (create nodes, edit resources, inspect selection, trigger imports)
can be done through it.

### Game-side tools (port 6011, while the game runs)

The editor plugin auto-registers a `GodotAgentRuntime` autoload in your
project settings. When the game starts, it serves MCP on
`http://127.0.0.1:6011/mcp` (override with `GODOT_AGENT_GAME_PORT`):

| Tool | Description |
|---|---|
| `get_game_info` | FPS, current scene, window size, pause state |
| `get_scene_tree` | Live runtime node tree (autoloads + dynamic nodes) |
| `capture_screenshot` | Rendered frame as an image (max 1280px wide) |
| `simulate_input` | Timed input sequences: `key`, `text`, `mouse_click`, `mouse_move`, `action` (input-map actions), `wait` — spaced across frames like real input |
| `execute_script` | Arbitrary GDScript in the game process (`Engine.get_main_loop()` reaches the SceneTree) |

Connect an external client with:

```bash
claude mcp add --transport http godot-game http://127.0.0.1:6011/mcp
```

(The chat dock includes both servers automatically; start the game before
pressing New if you want the session to see the game-side tools.) Example prompt for your assistant:

> "Add a CharacterBody2D named Player with a Sprite2D child to the current
> scene, then save."

## AI Chat dock

After installing, a dock panel named **AI Chat** appears on the right side of
the editor. Type a message and press Enter — the first message spawns a
persistent headless Claude Code session rooted at your project directory, with
tool access restricted to this editor's MCP server (`--strict-mcp-config`,
`--permission-mode acceptEdits`). Tool calls appear inline as `⚙ tool_name`.
Press **New** to end the conversation and start fresh.

The `claude` binary is resolved via a login shell (Homebrew paths work even
when the editor is launched from Finder); override with the
`GODOT_AGENT_CLAUDE_BIN` environment variable.

## Usage examples

With Claude Code connected (`claude mcp add --transport http godot-editor
http://127.0.0.1:6010/mcp`), just describe what you want:

> "Open the login scene and show me its node tree"
> "Run the game, screenshot the title screen, and check if the buttons overlap"
> "Click the Login button, wait, and verify the scene changed"

Or drive it from any HTTP client — the servers speak plain JSON-RPC:

```bash
# What is the editor looking at right now?
curl -s -X POST http://127.0.0.1:6010/mcp -H 'Content-Type: application/json' -d '{
  "jsonrpc":"2.0","id":1,"method":"tools/call",
  "params":{"name":"get_editor_info","arguments":{}}}'

# Find every button in the running game, with clickable coordinates
curl -s -X POST http://127.0.0.1:6011/mcp -H 'Content-Type: application/json' -d '{
  "jsonrpc":"2.0","id":2,"method":"tools/call",
  "params":{"name":"execute_script","arguments":{"code":
    "var out = []\nfor b in Engine.get_main_loop().current_scene.find_children(\"*\", \"Button\", true, false):\n\tvar r = b.get_global_rect()\n\tout.append({\"text\": b.text, \"center\": [r.get_center().x, r.get_center().y], \"disabled\": b.disabled})\nreturn out"}}}'

# Click one of them like a real player would
curl -s -X POST http://127.0.0.1:6011/mcp -H 'Content-Type: application/json' -d '{
  "jsonrpc":"2.0","id":3,"method":"tools/call",
  "params":{"name":"simulate_input","arguments":{"events":[
    {"type":"mouse_click","x":640,"y":461}]}}}'
```

A typical debugging session: read the UI structure (`execute_script`) →
reproduce the player's actions (`simulate_input`) → verify logic state
(`get_game_info` / `execute_script`) → verify rendering (`capture_screenshot`).
Structured reads are precise and cheap; screenshots catch what data can't
(overlapping layout, missing textures, theme issues).

See [docs/USAGE.zh-CN.md](docs/USAGE.zh-CN.md) for a Chinese usage guide.

## Architecture

- `src/server.rs` — HTTP thread. Minimal MCP streamable-HTTP implementation
  (JSON-RPC over POST, stateless mode). Never touches Godot APIs.
- `src/ops.rs` — tool schemas and the job type passed between threads.
- `src/chat.rs` — chat backend. Spawns the headless `claude` CLI
  (stream-json in/out) and parses its event stream on a reader thread.
- `src/lib.rs` — the `EditorPlugin` and the chat dock UI. Its `process()`
  drains queued MCP jobs and chat events every frame on the main thread,
  where editor APIs are safe to call.

The core constraint shaping the design: Godot editor APIs are only safe on the
main thread. The HTTP thread parses MCP requests and queues jobs; the plugin
executes them between frames and replies through a channel.

## Security

The server binds to 127.0.0.1 only. `execute_editor_script` runs arbitrary
code with full editor privileges — do not port-forward or expose this
endpoint.

## License

MIT
