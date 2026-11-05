//! 自動アップデート機能(2026-08-19新設)。
//!
//! ユーザー指示「起動時にGitHub Releases APIで最新タグを確認し、
//! 現在のバージョンと比較→新版があればダウンロード→安全な入れ替え
//! (旧バイナリ退避→新バイナリ起動→ヘルスチェック→失敗時ロールバック)」
//! への対応。`rs-sync`側`src/self_update.rs`(同日、同じユーザー指示で
//! 移植)と実質同一の設計をこの`open-runo-router`バイナリ向けに適用した
//! (重複実装だが、両者は別リポジトリの別バイナリであり共有クレート化は
//! 今回のスコープ外——両ファイルのモジュールdocに互いへの相互参照を
//! 記載し、将来の保守で気づけるようにしてある)。
//!
//! ## 前提となる配布方式
//!
//! `.github/workflows/release.yml`がタグpushで
//! `open-runo-router-<target>.tar.gz`(Linux x86_64/aarch64)・
//! `open-runo-router-windows-x86_64.zip`をGitHub Releasesへ添付する。
//! `install.sh`(systemdサービス登録)は別経路——この自己更新機構は
//! `install.sh`で配置されたバイナリを対象に、実行中のバイナリ自身を
//! 差し替える形で動く(`install.sh`自体は変更していない、二重に同じ
//! ファイルへ書き込みに行かないよう、この機構は`current_exe()`基準の
//! 実パスのみを扱う)。
//!
//! ## 正直な開示(最重要)
//!
//! - **オプトイン方式**: `version.json`のようなインストール済み
//!   マーカーが無いため、ローカルバージョンは`env!("CARGO_PKG_
//!   VERSION")`(`Cargo.toml`の`workspace.package.version`)を使う。
//!   VPS本番運用と誤って競合しないよう、`OPEN_RUNO_ROUTER_SELF_UPDATE=
//!   true`を明示的に設定した場合のみ動作する(既定は無効)。
//! - **プラットフォーム**: release.ymlがビルドするLinux
//!   (x86_64/aarch64)・Windows(x86_64)のみ対象。
//! - **実機検証の範囲**: この開発機はWindowsのため、`cargo build
//!   --release -p open-runo-router`・単体テストまでを確認済み。
//!   `aon-co-jp/RPoem`に実際のリリースタグが存在するかは未確認——
//!   タグが無い場合は`fetch_latest_release`が404で失敗し、ログのみで
//!   サーバー起動は継続する(既存の「サービスを止めない」方針を踏襲)。
//!   「新版検出→ダウンロード→自己置換→ヘルスチェック→ロールバック」の
//!   一連のE2Eはこのセッションでは未検証。
//! - **ヘルスチェック**: 既存の`GET /healthz`(`hyper_compat::
//!   health_handler`)への到達可否のみで判定する。

// clippy: このモジュールの unwrap()/expect() は「固定ヘッダからのレスポンス構築」
// 「起動時に読む埋め込み定数のパース」等、構造上失敗しない箇所に限られる
// (各呼び出しに理由コメントあり)。business ロジック側の Quality Gate
// (workspace lints の unwrap_used/expect_used = warn) は維持したまま、
// この基盤モジュールだけ除外する。result_large_err は Result<T, Response>
// (エラー時に完成済み HTTP レスポンスで短絡する意図的な型)に対するもの。
#![allow(clippy::unwrap_used)]

use std::net::SocketAddr;
use std::time::Duration;

use serde::Deserialize;

const GITHUB_REPO: &str = "aon-co-jp/RPoem";

pub(crate) const HEALTH_CHECK_SECS: u64 = 12;

#[derive(Debug, Deserialize)]
pub(crate) struct ReleaseAsset {
    pub(crate) name: String,
    pub(crate) browser_download_url: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LatestRelease {
    pub(crate) tag_name: String,
    pub(crate) assets: Vec<ReleaseAsset>,
}

pub(crate) fn parse_version(raw: &str) -> (u64, u64, u64) {
    let trimmed = raw.trim().trim_start_matches('v');
    let mut parts = trimmed.split('.').map(|s| s.parse::<u64>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

pub(crate) fn is_newer(remote: &str, local: &str) -> bool {
    parse_version(remote) > parse_version(local)
}

async fn fetch_latest_release() -> anyhow::Result<LatestRelease> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");
    let res = client
        .get(&url)
        .header("User-Agent", "open-runo-router-self-updater")
        .send()
        .await?;
    if !res.status().is_success() {
        anyhow::bail!("GitHub releases API returned HTTP {}", res.status());
    }
    Ok(res.json::<LatestRelease>().await?)
}

fn windows_zip_asset(release: &LatestRelease) -> Option<&ReleaseAsset> {
    release.assets.iter().find(|a| {
        a.name.to_lowercase().ends_with(".zip") && a.name.to_lowercase().contains("windows")
    })
}

fn linux_tarball_asset(release: &LatestRelease) -> Option<&ReleaseAsset> {
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    };
    release.assets.iter().find(|a| {
        let name = a.name.to_lowercase();
        name.ends_with(".tar.gz") && name.contains("linux") && name.contains(arch)
    })
}

async fn download_to(url: &str, dest: &std::path::Path) -> anyhow::Result<()> {
    let client = reqwest::Client::builder().build()?;
    let bytes = client
        .get(url)
        .header("User-Agent", "open-runo-router-self-updater")
        .send()
        .await?
        .bytes()
        .await?;
    std::fs::write(dest, &bytes)?;
    Ok(())
}

#[cfg(unix)]
pub(crate) fn set_executable_permission(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
pub(crate) fn set_executable_permission(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

pub(crate) async fn health_check(addr: &SocketAddr) -> bool {
    let url = format!("http://{addr}/healthz");
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
    else {
        return false;
    };
    match client.get(&url).send().await {
        Ok(res) => res.status().is_success(),
        Err(_) => false,
    }
}

async fn health_check_with_wait(child: &mut tokio::process::Child, addr: &SocketAddr) -> bool {
    for _ in 0..HEALTH_CHECK_SECS {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if let Ok(Some(status)) = child.try_wait() {
            tracing::warn!(
                "open-runo-router self-update: new version exited immediately (status: {status})"
            );
            return false;
        }
    }
    health_check(addr).await
}

fn real_bind_addr() -> SocketAddr {
    std::env::var("OPEN_RUNO_ROUTER_BIND")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| "0.0.0.0:8081".parse().unwrap())
}

/// Linux版(x86_64/aarch64共通): tar.gzを展開し、現在の実行ファイルと
/// 同じディレクトリへ上書きコピーする。新バイナリはまず実ポート+1の
/// 一時プローブポートでヘルスチェックし、成功すれば実ポートで正式
/// 起動してこのプロセスは終了する。失敗すれば`.bak`から復元し、旧
/// プロセス(このプロセス自身)は一度も止めずに動作を継続する。
async fn apply_update_unix(tarball_path: &std::path::Path) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let install_dir = exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("could not resolve install directory"))?
        .to_path_buf();

    let backup_path = install_dir.join("open-runo-router.bak");
    std::fs::copy(&exe, &backup_path)?;

    let extract_dir = std::env::temp_dir().join("open-runo-router-self-update-extract");
    let _ = std::fs::remove_dir_all(&extract_dir);
    std::fs::create_dir_all(&extract_dir)?;

    let status = tokio::process::Command::new("tar")
        .args(["xzf", &tarball_path.to_string_lossy(), "-C"])
        .arg(&extract_dir)
        .status()
        .await?;
    if !status.success() {
        anyhow::bail!("tar extraction failed with status {status}");
    }

    let new_exe_extracted = extract_dir.join("open-runo-router");
    if !new_exe_extracted.exists() {
        anyhow::bail!("extracted tarball does not contain open-runo-router binary");
    }
    let new_exe = install_dir.join("open-runo-router");
    std::fs::copy(&new_exe_extracted, &new_exe)?;
    set_executable_permission(&new_exe)?;

    let real_addr = real_bind_addr();
    let probe_addr = SocketAddr::new(real_addr.ip(), real_addr.port().wrapping_add(1));

    let mut probe_child = tokio::process::Command::new(&new_exe)
        .current_dir(&install_dir)
        .env("OPEN_RUNO_ROUTER_BIND", probe_addr.to_string())
        .spawn()?;

    if health_check_with_wait(&mut probe_child, &probe_addr).await {
        let _ = probe_child.kill().await;
        tokio::process::Command::new(&new_exe)
            .current_dir(&install_dir)
            .env("OPEN_RUNO_ROUTER_BIND", real_addr.to_string())
            .spawn()?;
        tracing::info!("open-runo-router self-update: new version passed health check, switching over and exiting");
        std::process::exit(0);
    }

    let _ = probe_child.kill().await;
    tracing::warn!("open-runo-router self-update: new version failed health check — rolling back (this process was never stopped, no restart needed)");
    std::fs::copy(&backup_path, &new_exe)?;
    set_executable_permission(&new_exe)?;
    Ok(())
}

/// Windows版: デタッチしたバッチスクリプトが「少し待つ→旧バイナリを
/// `.bak`へ退避→新バイナリを展開・配置→起動→`/healthz`確認→失敗時は
/// `.bak`から復元して再起動」を行う間に、このプロセス自身は終了して
/// ファイルロックを解放する。
async fn apply_update_windows(zip_path: &std::path::Path) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let exe_name = exe
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("open-runo-router.exe")
        .to_string();
    let real_addr = real_bind_addr();

    let extract_dir = std::env::temp_dir().join("open-runo-router-self-update-extract");

    let script_path = std::env::temp_dir().join("open-runo-router-self-update.bat");
    let mut script = String::from("@echo off\r\nping 127.0.0.1 -n 3 > nul\r\n");
    script += &format!("taskkill /IM \"{exe_name}\" /F >nul 2>nul\r\n");
    script += "ping 127.0.0.1 -n 2 > nul\r\n";
    script += &format!(
        "copy /Y \"{}\" \"{}.bak\"\r\n",
        exe.display(),
        exe.display()
    );
    script += &format!("rd /S /Q \"{}\" >nul 2>nul\r\n", extract_dir.display());
    script += &format!(
        "powershell -NoProfile -Command \"Expand-Archive -Path '{}' -DestinationPath '{}' -Force\"\r\n",
        zip_path.display(),
        extract_dir.display()
    );
    script += &format!(
        "copy /Y \"{}\\open-runo-router.exe\" \"{}\"\r\n",
        extract_dir.display(),
        exe.display()
    );
    script += &format!("start \"\" \"{}\"\r\n", exe.display());
    script += &format!("ping 127.0.0.1 -n {} > nul\r\n", HEALTH_CHECK_SECS + 3);
    script += &format!(
        "powershell -NoProfile -Command \"try {{ $r = Invoke-WebRequest -UseBasicParsing -TimeoutSec 3 'http://{real_addr}/healthz'; if ($r.StatusCode -ne 200) {{ exit 1 }} }} catch {{ exit 1 }}\"\r\n"
    );
    script += "if %errorlevel% neq 0 (\r\n";
    script += &format!("  taskkill /IM \"{exe_name}\" /F >nul 2>nul\r\n");
    script += "  ping 127.0.0.1 -n 2 > nul\r\n";
    script += &format!(
        "  copy /Y \"{}.bak\" \"{}\"\r\n",
        exe.display(),
        exe.display()
    );
    script += &format!("  start \"\" \"{}\"\r\n", exe.display());
    script += ")\r\n";
    script += &format!("del \"{}\"\r\n", script_path.display());
    std::fs::write(&script_path, script)?;

    tokio::process::Command::new("cmd")
        .args(["/C", "start", "", script_path.to_string_lossy().as_ref()])
        .spawn()?;

    tracing::info!("open-runo-router self-update: launched replace+healthcheck script, exiting to release file locks");
    std::process::exit(0);
}

/// 起動時のメンテナンスチェックとして呼ぶエントリポイント。
/// `OPEN_RUNO_ROUTER_SELF_UPDATE=true`が設定されている場合のみ動作する
/// (既定オフ、モジュールdoc「正直な開示」参照)。
pub async fn check_and_apply_update() {
    let enabled = std::env::var("OPEN_RUNO_ROUTER_SELF_UPDATE")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);
    if !enabled {
        tracing::debug!("open-runo-router self-update: disabled (set OPEN_RUNO_ROUTER_SELF_UPDATE=true to enable)");
        return;
    }
    if !cfg!(any(target_os = "windows", target_os = "linux")) {
        tracing::info!("open-runo-router self-update: skipped (Windows and Linux only)");
        return;
    }

    let release = match fetch_latest_release().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("open-runo-router self-update: could not check GitHub releases ({e}) — continuing without updating");
            return;
        },
    };

    let local = env!("CARGO_PKG_VERSION");
    if !is_newer(&release.tag_name, local) {
        tracing::info!(
            "open-runo-router self-update: up to date (local {local}, latest release {})",
            release.tag_name
        );
        return;
    }

    let is_windows = cfg!(target_os = "windows");
    let asset = if is_windows {
        windows_zip_asset(&release)
    } else {
        linux_tarball_asset(&release)
    };
    let Some(asset) = asset else {
        tracing::warn!("open-runo-router self-update: newer release {} found but no matching asset attached — skipping", release.tag_name);
        return;
    };

    tracing::info!(
        "open-runo-router self-update: newer release {} found (local {local}), downloading {}",
        release.tag_name,
        asset.name
    );

    let dest = std::env::temp_dir().join(&asset.name);
    match download_to(&asset.browser_download_url, &dest).await {
        Ok(()) => {
            let result = if is_windows {
                apply_update_windows(&dest).await
            } else {
                apply_update_unix(&dest).await
            };
            if let Err(e) = result {
                tracing::warn!("open-runo-router self-update: failed to launch update ({e})");
            }
        },
        Err(e) => {
            tracing::warn!(
                "open-runo-router self-update: download failed ({e}) — continuing without updating"
            );
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_version_strings_with_and_without_v_prefix() {
        assert_eq!(parse_version("v0.5.1"), (0, 5, 1));
        assert_eq!(parse_version("0.5.1"), (0, 5, 1));
        assert_eq!(parse_version("garbage"), (0, 0, 0));
    }

    #[test]
    fn is_newer_compares_semver_correctly() {
        assert!(is_newer("v0.6.0", "0.5.1"));
        assert!(!is_newer("v0.5.1", "0.5.1"));
    }

    #[test]
    fn linux_asset_matches_release_yml_naming() {
        let release = LatestRelease {
            tag_name: "v0.2.0".into(),
            assets: vec![
                ReleaseAsset {
                    name: "open-runo-router-x86_64-unknown-linux-gnu.tar.gz".into(),
                    browser_download_url: "https://example.com/l".into(),
                },
                ReleaseAsset {
                    name: "open-runo-router-aarch64-unknown-linux-gnu.tar.gz".into(),
                    browser_download_url: "https://example.com/a".into(),
                },
                ReleaseAsset {
                    name: "open-runo-router-windows-x86_64.zip".into(),
                    browser_download_url: "https://example.com/w".into(),
                },
            ],
        };
        assert_eq!(
            windows_zip_asset(&release).unwrap().name,
            "open-runo-router-windows-x86_64.zip"
        );
        assert!(linux_tarball_asset(&release).is_some());
    }
}
