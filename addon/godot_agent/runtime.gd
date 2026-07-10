# Autoload entry point for the godot-agent in-game runtime.
# The implementation is the GodotAgentRuntime class from the godot_agent
# GDExtension. We instantiate it dynamically (instead of `extends`) so this
# script parses fine even before the extension's classes are visible to the
# GDScript parser.
extends Node

func _ready() -> void:
	if Engine.is_editor_hint():
		return
	var runtime = ClassDB.instantiate("GodotAgentRuntime")
	if runtime == null:
		push_error("[MCP] GodotAgentRuntime class not found — is the godot_agent extension loaded?")
		return
	add_child(runtime)
