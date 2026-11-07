#!/usr/bin/env bash
# 打包成 /Applications/Snipshot.app，并可选安装 LaunchAgent。
#
#   ./scripts/bundle-macos.sh            # 只打包到 target/Snipshot.app
#   ./scripts/bundle-macos.sh --install  # 同时拷到 /Applications 并注册 LaunchAgent
#
# 为什么要打 .app：裸二进制拿不到辅助功能授权（TCC 以签名为单位），
# 也无法以 LSUIElement 身份常驻。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

APP_NAME="Snipshot"
BIN_NAME="snipshot"
OUT_DIR="$ROOT/target"
APP="$OUT_DIR/$APP_NAME.app"

echo "==> cargo build --release"
cargo build --release

echo "==> 组装 $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$ROOT/target/release/$BIN_NAME" "$APP/Contents/MacOS/$BIN_NAME"
cp "$ROOT/packaging/macos/Info.plist" "$APP/Contents/Info.plist"

# 临时签名（ad-hoc）：本机自用足够；分发给别人要换成 Developer ID。
echo "==> codesign (ad-hoc)"
codesign --force --deep --sign - "$APP"
codesign -dv "$APP" 2>&1 | sed 's/^/    /'

echo "==> 完成：$APP"

if [[ "${1:-}" == "--install" ]]; then
  echo "==> 安装到 /Applications"
  rm -rf "/Applications/$APP_NAME.app"
  cp -R "$APP" "/Applications/$APP_NAME.app"

  PLIST="$HOME/Library/LaunchAgents/com.snipshot.daemon.plist"
  echo "==> 注册 LaunchAgent：$PLIST"
  mkdir -p "$(dirname "$PLIST")"
  cp "$ROOT/packaging/launchd/com.snipshot.daemon.plist" "$PLIST"
  launchctl unload "$PLIST" 2>/dev/null || true
  launchctl load "$PLIST"

  echo "==> 写入 espanso 触发定义"
  "/Applications/$APP_NAME.app/Contents/MacOS/$BIN_NAME" install

  cat <<'TIP'

接下来手动做两件事（系统不允许脚本代劳）：
  1. 系统设置 → 隐私与安全性 → 辅助功能 → 勾选 Snipshot
  2. espanso restart

然后 `snipshot doctor` 应显示四项全绿。
TIP
fi
