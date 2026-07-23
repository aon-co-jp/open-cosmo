#!/usr/bin/env bash
# RPoem <-> RCosmo の「Cosmo共通コア」クレート群のドリフト検知・同期スクリプト
#
# 背景(2026-07-23調査): RPoem/RCosmoの両CLAUDE.mdが謳う「共通コア」
# (WunderGraph Cosmo有料版相当機能の自前実装、Federation/VersionlessAPI/
# SCIM/Security/Cache等)は、これまで各セッションが手作業でファイルを
# 逐一コピーして「ミラー」する運用だった。本スクリプトはその手作業を
# 機械的に検証・実行できるようにする——crateへのCargo path依存は使わず
# (このエコシステムの既存方針「別リポジトリのcrateへ直接のCargo依存は
# しない」を踏襲)、あくまでファイルコピー方式のままだが、
# 「どのクレートが同期しているか/いないか」を毎回grepで手動確認する
# 手間を無くす。
#
# 使い方:
#   scripts/sync-cosmo-core.sh check          共通コアクレートの同期状態を報告(既定)
#   scripts/sync-cosmo-core.sh diff <crate>   指定クレートの差分を表示
#   scripts/sync-cosmo-core.sh push <crate>   RPoem側の内容をRCosmo側へコピー
#   scripts/sync-cosmo-core.sh pull <crate>   RCosmo側の内容をRPoem側へコピー
#
# 前提: RPoemとRCosmoが兄弟ディレクトリ(F:\runo\RPoem, F:\runo\RCosmo)に
# 配置されていること。どちらのリポジトリからスクリプトを実行しても動作する
# (自分自身の場所から相対的に相方を探す)。

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SELF_REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
SELF_NAME="$(basename "${SELF_REPO_ROOT}")"

if [ "${SELF_NAME}" = "RPoem" ]; then
    OTHER_NAME="RCosmo"
elif [ "${SELF_NAME}" = "RCosmo" ]; then
    OTHER_NAME="RPoem"
else
    echo "warning: unexpected repo dir name '${SELF_NAME}' (expected RPoem or RCosmo), assuming RPoem" >&2
    SELF_NAME="RPoem"
    OTHER_NAME="RCosmo"
fi

OTHER_REPO_ROOT="$(cd "${SELF_REPO_ROOT}/.." && pwd)/${OTHER_NAME}"

if [ ! -d "${OTHER_REPO_ROOT}" ]; then
    echo "error: sibling repo not found at ${OTHER_REPO_ROOT}" >&2
    exit 1
fi

# 「Cosmo共通コア」として両リポジトリで完全一致を維持すべきクレート一覧。
# RPoem固有(open-runo-poem-compat*)・意図的に分岐させたクレート
# (open-runo-db/gateway/router — RPoem固有のPoem/Tauri再現機能・
# appserver_tenants/udp_notice等の実装差分を含むため)はここに含めない。
SHARED_CRATES=(
    open-runo-ai-routing
    open-runo-api-types
    open-runo-appserver
    open-runo-backup
    open-runo-cache
    open-runo-cli
    open-runo-core
    open-runo-feature-flags
    open-runo-federation
    open-runo-history
    open-runo-observability
    open-runo-persisted-queries
    open-runo-rustjson
    open-runo-schema-registry
    open-runo-scim
    open-runo-security
    open-runo-versionless-api
    open-runo-view
)

cmd="${1:-check}"

case "$cmd" in
    check)
        echo "Shared Cosmo core crate drift check: ${SELF_NAME} <-> ${OTHER_NAME}"
        echo
        in_sync=0
        drifted=0
        for c in "${SHARED_CRATES[@]}"; do
            a="${SELF_REPO_ROOT}/crates/${c}/src"
            b="${OTHER_REPO_ROOT}/crates/${c}/src"
            if [ ! -d "$a" ] || [ ! -d "$b" ]; then
                echo "  ?? ${c}: missing in one repo (self=$( [ -d "$a" ] && echo yes || echo no ), other=$( [ -d "$b" ] && echo yes || echo no ))"
                continue
            fi
            n=0
            if ! diff -rq "$a" "$b" > /tmp/sync-cosmo-core-diff.$$ 2>&1; then
                n=$(wc -l < /tmp/sync-cosmo-core-diff.$$)
            fi
            rm -f /tmp/sync-cosmo-core-diff.$$
            if [ "$n" -eq 0 ]; then
                echo "  OK ${c}: in sync"
                in_sync=$((in_sync + 1))
            else
                echo "  XX ${c}: DRIFTED (${n} differing file(s) — run 'diff ${c}' for detail)"
                drifted=$((drifted + 1))
            fi
        done
        echo
        echo "summary: ${in_sync} in sync, ${drifted} drifted (out of ${#SHARED_CRATES[@]} shared crates)"
        [ "$drifted" -eq 0 ]
        ;;
    diff)
        c="${2:?usage: sync-cosmo-core.sh diff <crate>}"
        diff -ru "${SELF_REPO_ROOT}/crates/${c}/src" "${OTHER_REPO_ROOT}/crates/${c}/src" || true
        ;;
    push)
        c="${2:?usage: sync-cosmo-core.sh push <crate>}"
        echo "Copying ${SELF_NAME}/crates/${c} -> ${OTHER_NAME}/crates/${c} (src/ only)"
        rm -rf "${OTHER_REPO_ROOT}/crates/${c}/src"
        cp -r "${SELF_REPO_ROOT}/crates/${c}/src" "${OTHER_REPO_ROOT}/crates/${c}/src"
        echo "Done. Review with 'git diff' in ${OTHER_NAME}, then cargo test before committing."
        ;;
    pull)
        c="${2:?usage: sync-cosmo-core.sh pull <crate>}"
        echo "Copying ${OTHER_NAME}/crates/${c} -> ${SELF_NAME}/crates/${c} (src/ only)"
        rm -rf "${SELF_REPO_ROOT}/crates/${c}/src"
        cp -r "${OTHER_REPO_ROOT}/crates/${c}/src" "${SELF_REPO_ROOT}/crates/${c}/src"
        echo "Done. Review with 'git diff' in ${SELF_NAME}, then cargo test before committing."
        ;;
    *)
        echo "usage: $0 {check|diff <crate>|push <crate>|pull <crate>}" >&2
        exit 1
        ;;
esac
