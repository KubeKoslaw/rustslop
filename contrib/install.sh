#!/bin/sh
# Install / uninstall the rusted-wm login session.
#
#   cargo build --release
#   doas contrib/install.sh            # or sudo
#   doas contrib/install.sh uninstall
#
# Environment: PREFIX (default /usr/local), SESSIONS (default /usr/share/wayland-sessions),
# DESTDIR (staging root, for packaging/testing), BIN (binary to install).
set -eu
cd "$(dirname "$0")/.."

PREFIX=${PREFIX:-/usr/local}
SESSIONS=${SESSIONS:-/usr/share/wayland-sessions}
DESTDIR=${DESTDIR:-}
BIN=${BIN:-target/release/rusted-wm}

case "${1:-install}" in
install)
    if [ ! -x "$BIN" ]; then
        echo "error: $BIN not found - run 'cargo build --release' first (as your normal user)" >&2
        exit 1
    fi
    install -Dm755 "$BIN" "$DESTDIR$PREFIX/bin/rusted-wm"
    install -Dm755 contrib/rusted-session "$DESTDIR$PREFIX/bin/rusted-session"
    install -d "$DESTDIR$SESSIONS"
    sed "s|@PREFIX@|$PREFIX|g" contrib/rusted.desktop.in > "$DESTDIR$SESSIONS/rusted.desktop"
    chmod 644 "$DESTDIR$SESSIONS/rusted.desktop"
    install -Dm644 config.example.toml "$DESTDIR$PREFIX/share/doc/rusted-wm/config.example.toml"
    echo "installed to $DESTDIR$PREFIX/bin and $DESTDIR$SESSIONS/rusted.desktop"
    echo "pick 'Rusted' in your display manager's session menu."
    echo "optional: mkdir -p ~/.config/rusted-wm && cp $PREFIX/share/doc/rusted-wm/config.example.toml ~/.config/rusted-wm/config.toml"
    ;;
uninstall)
    rm -f "$DESTDIR$PREFIX/bin/rusted-wm" "$DESTDIR$PREFIX/bin/rusted-session" \
        "$DESTDIR$SESSIONS/rusted.desktop" "$DESTDIR$PREFIX/share/doc/rusted-wm/config.example.toml"
    rmdir "$DESTDIR$PREFIX/share/doc/rusted-wm" 2>/dev/null || true
    echo "removed rusted-wm session files"
    ;;
*)
    echo "usage: $0 [install|uninstall]" >&2
    exit 2
    ;;
esac
