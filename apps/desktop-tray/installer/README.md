# このフォルダについて / About this folder

`open-runo-tray.iss` はこのインストーラーを**作るための** [Inno Setup](https://jrsoftware.org/isinfo.php)
ビルドスクリプトです(このスクリプト自体はRPoemリポジトリ内の`apps/desktop-tray/`サブディレクトリに
属するもので、対象は`open-runo-desktop-tray`アプリ)。`open-runo-tray-installer.exe` はこのフォルダ内に
実体として置いてあります(ユーザー指示により、ローカルビルドしたバイナリを直接コミット)。

**⬇ 今すぐダウンロード**: [open-runo-tray-installer.exe](open-runo-tray-installer.exe)

**正直な開示**: このファイルはビルド成果物であり、`open-runo-tray.iss`やソースコードを変更しても
自動的には更新されません(手動での再ビルド・再コミットが必要)。現時点でこのインストーラーを
自動ビルド・公開するCIは無いため、このファイルが唯一の配布経路です。

---

`open-runo-tray.iss` is the [Inno Setup](https://jrsoftware.org/isinfo.php) build script used to
**create** this installer (this script lives under the `apps/desktop-tray/` subdirectory of the
RPoem repository, and packages the `open-runo-desktop-tray` app). `open-runo-tray-installer.exe`
is committed directly into this folder as a real file (per user instruction, a locally-built
binary is committed as-is).

**⬇ Download now**: [open-runo-tray-installer.exe](open-runo-tray-installer.exe)

**Honest disclosure**: this file is a build artifact. Changes to `open-runo-tray.iss` or the source
code do not automatically update it (manual rebuild and recommit required). There is currently no CI
that automatically builds and publishes this installer, so this file is the only distribution channel.
