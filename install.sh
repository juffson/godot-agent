#!/usr/bin/env bash
# Build the GDExtension and install it into a Godot project.
# Usage: ./install.sh /path/to/godot/project [--release]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT="${1:?Usage: ./install.sh /path/to/godot/project [--release]}"
PROFILE="debug"
if [[ "${2:-}" == "--release" ]]; then
  PROFILE="release"
fi

if [[ ! -f "$PROJECT/project.godot" ]]; then
  echo "error: $PROJECT does not contain a project.godot file" >&2
  exit 1
fi

cd "$SCRIPT_DIR"
if [[ "$PROFILE" == "release" ]]; then
  cargo build --release
else
  cargo build
fi

DEST="$PROJECT/addons/godot_agent"
mkdir -p "$DEST/lib"
cp addon/godot_agent/godot_agent.gdextension "$DEST/"

case "$(uname -s)" in
  Darwin) LIB="libgodot_agent.dylib" ;;
  Linux)  LIB="libgodot_agent.so" ;;
  *)      LIB="godot_agent.dll" ;;
esac
cp "target/$PROFILE/$LIB" "$DEST/lib/"

echo "Installed godot_agent ($PROFILE) to $DEST"
echo "Open the project in the Godot editor; the MCP server starts automatically."
echo "Connect Claude Code with:"
echo "  claude mcp add --transport http godot-editor http://127.0.0.1:6010/mcp"
