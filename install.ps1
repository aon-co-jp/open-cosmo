# RCosmo(open-runo-router)インストールスクリプト(Windows / Windows Server 共通)。
#
# RPoem(open-runo-router、姉妹プロジェクト)と同じバイナリ名・同じ役割
# (open-web-server=第二のApache+NginxとSETで使うアプリケーションサーバー、
# 第二のTomcat相当)を持つ。
#
# 使い方(管理者権限のPowerShellで):
#   Invoke-WebRequest -Uri "https://github.com/aon-co-jp/open-cosmo/releases/latest/download/open-runo-router-windows-x86_64.zip" -OutFile open-runo-router.zip
#   Expand-Archive open-runo-router.zip -DestinationPath open-runo-router
#   cd open-runo-router
#   .\install.ps1

#Requires -RunAsAdministrator

$ErrorActionPreference = "Stop"

$InstallDir = "C:\Program Files\RCosmo"
$ServiceName = "RCosmoRouter"

Write-Host "==> インストール先: $InstallDir"
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

$BinSrc = Join-Path $PSScriptRoot "open-runo-router.exe"
if (-not (Test-Path $BinSrc)) {
    Write-Error "open-runo-router.exe が見つかりません($BinSrc)。zipを展開したディレクトリで実行してください。"
    exit 1
}
Copy-Item $BinSrc -Destination $InstallDir -Force

$existing = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
if ($existing) {
    Write-Host "==> 既存のWindowsサービスが見つかったため、バイナリのみ更新しました(再起動は行いません)"
    Write-Host "    手動で再起動する場合: Restart-Service $ServiceName"
} else {
    Write-Host "==> Windowsサービスとして登録する場合の手順:"
    Write-Host "      [Environment]::SetEnvironmentVariable('OPEN_RUNO_ROUTER_BIND', '0.0.0.0:8081', 'Machine')"
    Write-Host "      New-Service -Name $ServiceName -BinaryPathName '$InstallDir\open-runo-router.exe' -DisplayName 'RCosmo (open-runo-router)' -StartupType Automatic"
    Write-Host "      Start-Service $ServiceName"
    Write-Host "==> open-web-server(第二のApache+Nginx)へこのポートを登録する場合:"
    Write-Host "      POST /admin/tenants { backend_addr: `"<このマシンのIP>:8081`" }"
}

Write-Host "==> 完了。"
