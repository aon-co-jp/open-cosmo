# open-runo-tray

Native system tray companion for the browser-based WASM admin UI
(`apps/desktop-wasm`). Closes the two Tauri-parity gaps a browser tab
structurally cannot: a system tray icon and genuine native OS
notifications. The actual admin UI stays exactly what it already is — a
browser-based WASM app served by `open-runo-router`/`open-runo-gateway` —
this binary has no UI of its own beyond the tray icon; clicking it (or
"Open open-runo" in its menu) opens the real app in the default browser.

**Does not depend on the `tauri` package.** Built from `tray-icon` (tray
icon abstraction) + `tao` (event loop / native message pump) + `notify-rust`
(native notifications). See the doc comment at the top of `src/main.rs` for
a note on `notify-rust`'s Windows backend transitively pulling in
`tauri-winrt-notification` — a narrow WinRT toast-notification binding
maintained under the Tauri GitHub org, not the `tauri` app framework itself.

## Build

```bash
cargo build --release
```

Produces `target/release/open-runo-tray.exe` (Windows) / `open-runo-tray`
(macOS/Linux).

## Run

```bash
cargo run --release
# or set a non-default target URL:
OPEN_RUNO_TRAY_URL=http://localhost:9090/ cargo run --release
```

Left-click the tray icon (or the "Open open-runo" menu item) to open the
admin UI in the default browser. Right-click for the menu (Open / Quit).

## Build a Windows installer

```bash
cargo build --release
"C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer\open-runo-tray.iss
```

Produces `installer\dist\open-runo-tray-installer.exe` — a real, per-user
(no admin elevation required), silently-uninstallable Windows installer.
[Inno Setup](https://jrsoftware.org/isinfo.php) was used instead of the WiX
Toolset: WiX v7+ requires accepting a commercial "Open Source Maintenance
Fee" EULA to build, which isn't something to accept on a user's behalf
without asking; Inno Setup has no such requirement and was already
available on the reference dev machine.

Verified end-to-end on a real Windows install: `/VERYSILENT` install places
the exe under `%LOCALAPPDATA%\Programs\open-runo-tray\`, registers a
correct `HKCU\...\Uninstall` entry (name, version, publisher, uninstall
string), and the generated uninstaller removes both.

## Build a Linux package (.deb)

```bash
# System libraries tray-icon/notify-rust/tao need on Debian/Ubuntu:
sudo apt install libgtk-3-dev libayatana-appindicator3-dev libxdo-dev \
                  libssl-dev pkg-config libdbus-1-dev

cargo install cargo-deb  # once
cargo build --release
cargo deb                # reads [package.metadata.deb] in Cargo.toml
```

Produces `target/debian/open-runo-desktop-tray_<version>-1_amd64.deb`.

Verified end-to-end on real Ubuntu 26.04 (WSL2): the binary compiles clean
with the libraries above, runs without crashing (confirmed alive under a
running-process check, not just "didn't error on launch"), and the `.deb`
installs cleanly via `dpkg -i` (binary lands at `/usr/bin/open-runo-tray`,
correct `Depends:` line auto-detected from the linked shared libraries) and
uninstalls cleanly via `dpkg -r`. The one thing *not* verified: the tray
icon's on-screen appearance, because WSLg (this environment's X11/Wayland
compositor) has no system-tray host panel to dock into — a property of
this specific sandboxed environment, not of the binary; the identical
tray-icon-rendering code path was already visually confirmed correct on
Windows (see above). A real desktop environment (GNOME with
AppIndicator/KStatusNotifierItem support, KDE, XFCE with the systray
plugin, etc.) has a tray host and should render it normally.

macOS packaging (`.dmg`/`.app` bundle) is not yet set up — no macOS
environment was available to build or verify it from.
