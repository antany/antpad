#!/usr/bin/env bash
# Install antpad for the current user (no root needed):
#   binary  -> ~/.local/bin/antpad
#   icon    -> ~/.local/share/icons/hicolor/scalable/apps/antpad.svg
#   launcher-> ~/.local/share/applications/antpad.desktop
#
#   ./install.sh              install (builds first if needed)
#   ./install.sh --uninstall  remove everything
set -euo pipefail
cd "$(dirname "$0")"

PREFIX="${XDG_DATA_HOME:-$HOME/.local/share}"
BIN_DIR="$HOME/.local/bin"
APP_DIR="$PREFIX/applications"
ICON_DIR="$PREFIX/icons/hicolor/scalable/apps"

refresh() {
  command -v update-desktop-database >/dev/null && update-desktop-database "$APP_DIR" 2>/dev/null || true
  command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t "$PREFIX/icons/hicolor" 2>/dev/null || true
}

if [[ "${1:-}" == "--uninstall" ]]; then
  rm -fv "$BIN_DIR/antpad" "$APP_DIR/antpad.desktop" "$ICON_DIR/antpad.svg"
  refresh
  echo "Uninstalled."
  exit 0
fi

# --- find or build the binary ---
if [[ -x dist/antpad ]]; then
  BIN=dist/antpad
elif [[ -x target/release/antpad ]]; then
  BIN=target/release/antpad
elif command -v cargo >/dev/null; then
  cargo build --release
  BIN=target/release/antpad
elif command -v podman >/dev/null; then
  ./build.sh
  BIN=dist/antpad
else
  echo "No binary found and neither cargo nor podman is available." >&2
  echo "Install Rust (sudo pacman -S rust) or podman, then re-run." >&2
  exit 1
fi

mkdir -p "$BIN_DIR" "$APP_DIR" "$ICON_DIR"
install -Dm755 "$BIN" "$BIN_DIR/antpad"
install -Dm644 assets/antpad.svg "$ICON_DIR/antpad.svg"

# Terminal=true makes the desktop environment open it in your default terminal.
cat > "$APP_DIR/antpad.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Antpad
GenericName=Text Editor
Comment=Tiny Notepad-style text editor for the terminal
Exec=$BIN_DIR/antpad %f
Icon=antpad
Terminal=true
Categories=Utility;TextEditor;
MimeType=text/plain;
Keywords=notepad;text;editor;tui;
StartupNotify=false
EOF
chmod 644 "$APP_DIR/antpad.desktop"

refresh

echo "Installed:"
echo "  $BIN_DIR/antpad"
echo "  $APP_DIR/antpad.desktop"
echo "  $ICON_DIR/antpad.svg"
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "Note: $BIN_DIR is not in your PATH; add it to run 'antpad' from a shell." ;;
esac
