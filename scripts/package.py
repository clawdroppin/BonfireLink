"""Packages the release builds into dist/:
  BonfireLink-windows-x64.exe      portable Windows app
  BonfireLink-SteamDeck.tar.gz     Steam Deck / Linux app (folder with the binary + readme)
  BonfireLink-linux-x64            raw Linux binary (used by the in-app updater)
  install-steamdeck.sh             one-line installer (curl ... | bash)
  SHA256SUMS.txt
Run from the repo root after build.ps1 has compiled both targets."""
import hashlib
import io
import os
import shutil
import tarfile
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DIST = os.path.join(ROOT, "dist")
WIN = os.path.join(ROOT, "target", "release", "BonfireLink.exe")
LINUX = os.path.join(ROOT, "target", "x86_64-unknown-linux-gnu", "release", "BonfireLink")

DECK_README = """BonfireLink for Steam Deck
==========================

1. In Desktop Mode, double-click "BonfireLink" in this folder.
   (If nothing happens: right-click it > Properties > Permissions > tick "Is executable".)
2. Install Seamless Co-op for your games and press "Set launch options" on the Home page.
   Steam restarts once.
3. Press "Add BonfireLink to Steam" so you can open it from Gaming Mode.

From then on Steam's normal Play button starts Seamless Co-op (also in Gaming Mode), and the mod
updates itself right before the game starts.

Keep this folder where it is: BonfireLink-data/ next to the app holds your settings, save backups
and rollback copies. Moving the folder is fine; open BonfireLink once afterwards so the games
learn the new location.

More help: https://github.com/clawdroppin/BonfireLink
"""


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def add_bytes(tar, name, data, mode):
    info = tarfile.TarInfo(name)
    info.size = len(data)
    info.mode = mode
    info.mtime = int(time.time())
    tar.addfile(info, io.BytesIO(data))


def main():
    for p in (WIN, LINUX):
        if not os.path.isfile(p):
            raise SystemExit(f"missing build: {p}")
    shutil.rmtree(DIST, ignore_errors=True)
    os.makedirs(DIST)
    outputs = []

    win_out = os.path.join(DIST, "BonfireLink-windows-x64.exe")
    shutil.copy2(WIN, win_out)
    outputs.append(win_out)

    raw_out = os.path.join(DIST, "BonfireLink-linux-x64")
    shutil.copy2(LINUX, raw_out)
    outputs.append(raw_out)

    deck_out = os.path.join(DIST, "BonfireLink-SteamDeck.tar.gz")
    with tarfile.open(deck_out, "w:gz") as tar:
        add_bytes(tar, "BonfireLink/BonfireLink", open(LINUX, "rb").read(), 0o755)
        add_bytes(tar, "BonfireLink/README-SteamDeck.txt", DECK_README.encode(), 0o644)
    outputs.append(deck_out)

    script = os.path.join(ROOT, "scripts", "install-steamdeck.sh")
    script_out = os.path.join(DIST, "install-steamdeck.sh")
    with open(script, "rb") as src, open(script_out, "wb") as dst:
        dst.write(src.read().replace(b"\r\n", b"\n"))
    outputs.append(script_out)

    with open(os.path.join(DIST, "SHA256SUMS.txt"), "w", newline="\n") as f:
        for p in outputs:
            f.write(f"{sha256(p)}  {os.path.basename(p)}\n")
    for p in outputs:
        print(f"{os.path.getsize(p) / 1e6:7.1f} MB  {os.path.basename(p)}")


main()
