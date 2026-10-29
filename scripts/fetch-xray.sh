#!/usr/bin/env bash
# loov: 准备 xray sidecar 与 geo 数据（不进 git）。
#   scripts/fetch-xray.sh [target-triple] [本地解包目录]
# 不给本地目录时从 Xray 官方 release 下载；zip 与解包后的可执行文件都要和 src-tauri/xray.lock.json 一致。
# 依赖：bash、curl、python3（解 zip 与读 JSON，Windows 的 Git Bash 下也可用）。
set -euo pipefail
HERE="$(cd "$(dirname "$0")/.." && pwd)"
LOCK="$HERE/src-tauri/xray.lock.json"
# Windows 上 python3 可能是打开应用商店的空壳：逐个试跑，选第一个真能用的
PY=""
for c in python3 python py; do
  if "$c" -c 'import sys' >/dev/null 2>&1; then PY="$c"; break; fi
done
[ -n "$PY" ] || { echo "找不到可用的 python" >&2; exit 1; }

if [ -n "${1:-}" ]; then
  TRIPLE="$1"
else
  TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
fi
SRC="${2:-}"

field() { "$PY" -c 'import json,sys; t=json.load(open(sys.argv[1]))["targets"][sys.argv[2]]; print(t[sys.argv[3]])' "$LOCK" "$TRIPLE" "$1"; }
SOURCE_URL="$("$PY" -c 'import json,sys; print(json.load(open(sys.argv[1]))["source"])' "$LOCK")"
ASSET="$(field asset)"; ZIP_SHA="$(field zip_sha256)"; BIN="$(field bin)"; BIN_SHA="$(field bin_sha256)"

sha256() { "$PY" -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1],"rb").read()).hexdigest())' "$1"; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
if [ -z "$SRC" ]; then
  curl -fsSL --retry 3 -o "$WORK/$ASSET" "$SOURCE_URL$ASSET"
  [ "$(sha256 "$WORK/$ASSET")" = "$ZIP_SHA" ] || { echo "zip sha256 不匹配: $ASSET" >&2; exit 1; }
  "$PY" -c 'import zipfile,sys; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])' "$WORK/$ASSET" "$WORK/x"
  SRC="$WORK/x"
fi
[ "$(sha256 "$SRC/$BIN")" = "$BIN_SHA" ] || { echo "xray 可执行文件 sha256 不匹配: $SRC/$BIN" >&2; exit 1; }

EXT=""; case "$TRIPLE" in *windows*) EXT=".exe";; esac
mkdir -p "$HERE/src-tauri/binaries" "$HERE/src-tauri/xray-assets"
cp "$SRC/$BIN" "$HERE/src-tauri/binaries/xray-$TRIPLE$EXT"
chmod 0755 "$HERE/src-tauri/binaries/xray-$TRIPLE$EXT"
cp "$SRC/geoip.dat" "$SRC/geosite.dat" "$HERE/src-tauri/xray-assets/"
cp "$SRC/LICENSE" "$HERE/src-tauri/xray-assets/LICENSE-xray"
echo "ok: binaries/xray-$TRIPLE$EXT (xray $("$PY" -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "$LOCK")) + xray-assets/"
