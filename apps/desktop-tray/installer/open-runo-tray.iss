; Inno Setup script for the open-runo native tray companion.
; Poem/Tauri-parity gap: a real native Windows installer (.exe), matching
; the end-user experience Tauri's bundler provides, without depending on
; the `tauri` package or its bundler. Inno Setup is a long-established,
; freely licensed installer compiler (no commercial "maintenance fee"
; EULA, unlike WiX Toolset v7+ -- see docs/tauri-parity.md for why WiX was
; not used here) and is already present on this machine.
;
; Build: "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer\open-runo-tray.iss
; (run from apps/desktop-tray/, after `cargo build --release`)

#define MyAppName "open-runo Tray"
#define MyAppVersion "0.1.0"
#define MyAppPublisher "open-runo Contributors"
#define MyAppExeName "open-runo-tray.exe"

[Setup]
AppId={{8D72B372-F698-480C-BA8F-D97FA15285CE}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
DefaultDirName={autopf}\open-runo-tray
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
OutputDir=dist
; エコシステム全体の命名規則(2026-08-26統一、ユーザー指示「パワーシェル
; でインストールする関連リポジトリは全て、リポジトリ名-installer.exe
; ...に統一して」): <製品名>-installer.exe(旧`-setup`から改名)。
OutputBaseFilename=open-runo-tray-installer
Compression=lzma
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; No admin rights required -- installs per-user, matching the low-friction
; "no elevation prompt" experience of a Tauri-bundled app for a tray
; utility that talks only to localhost.
PrivilegesRequired=lowest

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "startupicon"; Description: "Start open-runo Tray automatically when Windows starts"; GroupDescription: "Additional options:"; Flags: unchecked

[Files]
Source: "..\target\release\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{userstartup}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: startupicon

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "Launch {#MyAppName} now"; Flags: nowait postinstall skipifsilent
