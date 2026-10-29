#!/bin/bash
# BonfireLink one-line installer for Steam Deck / Linux.
#   curl -fsSL https://github.com/clawdroppin/BonfireLink/releases/latest/download/install-steamdeck.sh | bash
# Installs (or updates) BonfireLink into ~/Applications/BonfireLink, verifies its checksum,
# adds a desktop + app-menu shortcut and starts it. Nothing needs root, and running it again
# keeps your settings and backups (they live in ~/Applications/BonfireLink/BonfireLink-data).
set -euo pipefail

REPO="clawdroppin/BonfireLink"
BASE="https://github.com/$REPO/releases/latest/download"
DEST="${BONFIRELINK_DIR:-$HOME/Applications/BonfireLink}"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

say() { printf '\033[1;33m>>\033[0m %s\n' "$*"; }

say "Downloading BonfireLink…"
curl -fL --progress-bar -o "$TMP/BonfireLink-SteamDeck.tar.gz" "$BASE/BonfireLink-SteamDeck.tar.gz"
curl -fsSL -o "$TMP/SHA256SUMS.txt" "$BASE/SHA256SUMS.txt"

say "Checking the download…"
( cd "$TMP" && grep ' BonfireLink-SteamDeck.tar.gz$' SHA256SUMS.txt | sha256sum -c --quiet - )

mkdir -p "$DEST"
tar -xzf "$TMP/BonfireLink-SteamDeck.tar.gz" -C "$TMP"
# Replace the app but never the data folder next to it.
install -m 755 "$TMP/BonfireLink/BonfireLink" "$DEST/BonfireLink"
install -m 644 "$TMP/BonfireLink/README-SteamDeck.txt" "$DEST/README-SteamDeck.txt"

DESKTOP_ENTRY="[Desktop Entry]
Type=Application
Name=BonfireLink
Comment=Seamless Co-op installer and companion
Exec=\"$DEST/BonfireLink\"
Path=$DEST
Icon=applications-games
Terminal=false
Categories=Game;Utility;"

mkdir -p "$HOME/.local/share/applications"
printf '%s\n' "$DESKTOP_ENTRY" > "$HOME/.local/share/applications/bonfirelink.desktop"
if [ -d "$HOME/Desktop" ]; then
  printf '%s\n' "$DESKTOP_ENTRY" > "$HOME/Desktop/BonfireLink.desktop"
  chmod +x "$HOME/Desktop/BonfireLink.desktop"
fi
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$HOME/.local/share/applications" >/dev/null 2>&1 || true

say "Installed to $DEST"
say "Starting BonfireLink…"
( cd "$DEST" && nohup ./BonfireLink >/dev/null 2>&1 & )
