# Autoload entry point for the godot-agent in-game runtime.
# The implementation is the GodotAgentRuntime class from the godot_agent
# GDExtension. We instantiate it dynamically (instead of `extends`) so this
# script parses fine even before the extension's classes are visible to the
# GDScript parser.
extends Node

func _ready() -> void:
	if Engine.is_editor_hint():
		return
	# 安全:仅"从编辑器启动的运行实例"(F5 播放/编辑器构建)启动 MCP;导出模板(prod/web)
	# has_feature("editor")=false → 不启动,杜绝把任意-GDScript-执行(RCE)端点打进上线包。
	if not OS.has_feature("editor"):
		return
	var runtime = ClassDB.instantiate("GodotAgentRuntime")
	if runtime == null:
		push_error("[MCP] GodotAgentRuntime class not found — is the godot_agent extension loaded?")
		return
	add_child(runtime)
