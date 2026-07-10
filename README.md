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

```
AI Chat dock ─→ claude CLI ─┐
                            ├─→ HTTP (MCP streamable JSON) ─→ Godot editor process
external MCP clients ───────┘
```

## Requirements

- Rust toolchain (1.85+)
- Godot 4.2+ (tested on 4.6.3)
- [Claude Code](https://claude.com/claude-code) CLI installed and logged in
  (only needed for the chat dock; the MCP server works without it)

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
can be done through it. Example prompt for your assistant:

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
