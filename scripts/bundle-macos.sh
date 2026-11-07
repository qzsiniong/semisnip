#!/usr/bin/env bash
# 打包成 /Applications/Semisnip.app，并可选安装 LaunchAgent。
#
#   ./scripts/bundle-macos.sh            # 只打包到 target/Semisnip.app
#   ./scripts/bundle-macos.sh --install  # 同时拷到 /Applications 并注册 LaunchAgent
#
# 为什么要打 .app：裸二进制拿不到辅助功能授权（TCC 以签名为单位），
# 也无法以 LSUIElement 身份常驻。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

APP_NAME="Semisnip"
BIN_NAME="semisnip"
OUT_DIR="$ROOT/target"
APP="$OUT_DIR/$APP_NAME.app"

echo "==> cargo build --release"
cargo build --release

# 图标由 build.rs 在构建期生成（形状定义在 src/mark.rs），这里只做存在性检查。
# 缺了说明 build.rs 没跑成——通常是换了 CARGO_TARGET_DIR 或改了包结构。
if [[ ! -f "$ROOT/packaging/macos/AppIcon.icns" ]]; then
  echo "❌ 没找到 packaging/macos/AppIcon.icns，build.rs 应该会生成它" >&2
  exit 1
fi

echo "==> 组装 $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$ROOT/target/release/$BIN_NAME" "$APP/Contents/MacOS/$BIN_NAME"
cp "$ROOT/packaging/macos/Info.plist" "$APP/Contents/Info.plist"
cp "$ROOT/packaging/macos/AppIcon.icns" "$APP/Contents/Resources/AppIcon.icns"

# 临时签名（ad-hoc）：本机自用足够；分发给别人要换成 Developer ID。
echo "==> codesign (ad-hoc)"
codesign --force --deep --sign - "$APP"
codesign -dv "$APP" 2>&1 | sed 's/^/    /'

echo "==> 完成：$APP"

# 把 .app 里的二进制暴露成 PATH 上的 `semisnip`。
# 已存在同名的普通文件时直接报错退出，不覆盖用户自己装的东西。
link_cli() {
  local target="/Applications/$APP_NAME.app/Contents/MacOS/$BIN_NAME"
  local link="/usr/local/bin/$BIN_NAME"

  if [[ ! -w "$(dirname "$link")" ]]; then
    echo "==> 跳过 CLI 软链：$(dirname "$link") 不可写，请手动执行"
    echo "    sudo ln -sf \"$target\" \"$link\""
    return 0
  fi

  if [[ -e "$link" && ! -L "$link" ]]; then
    echo "==> 跳过 CLI 软链：$link 已存在且不是软链（可能是你自己装的）"
    return 0
  fi

  ln -sf "$target" "$link"
  echo "==> CLI 软链：$link -> $target"
}

if [[ "${1:-}" == "--install" ]]; then
  echo "==> 安装到 /Applications"
  rm -rf "/Applications/$APP_NAME.app"
  cp -R "$APP" "/Applications/$APP_NAME.app"

  PLIST="$HOME/Library/LaunchAgents/com.semisnip.daemon.plist"
  echo "==> 注册 LaunchAgent：$PLIST"
  mkdir -p "$(dirname "$PLIST")"
  cp "$ROOT/packaging/launchd/com.semisnip.daemon.plist" "$PLIST"
  launchctl unload "$PLIST" 2>/dev/null || true
  launchctl load "$PLIST" 2>/dev/null || true
  # `launchctl load` 在非登录会话里会打印 "Load failed: 5" 却仍然返回 0，
  # 所以只能回查状态判断到底有没有接管。
  if launchctl print "gui/$(id -u)/com.semisnip.daemon" >/dev/null 2>&1; then
    echo "==> LaunchAgent 已接管"
  else
    echo "⚠️  daemon 没能注册到 launchd（多见于非登录会话 / CI）。"
    echo "    重新登录，或在终端里再执行一次本脚本即可。"
    echo "    临时可用：另开一个终端跑 \`$BIN_NAME daemon\`"
  fi

  # CLI 软链：二进制在 .app 里，不链一下 shell 就找不到 `semisnip`。
  # 选 /usr/local/bin 而不是 ~/.local/bin，是因为前者在所有 macOS 的默认 PATH 里。
  # 放在 `install` 之前：`install` 会检查这条链接是否存在来决定要不要提示补链。
  link_cli

  echo "==> 写入 espanso 触发定义"
  "/Applications/$APP_NAME.app/Contents/MacOS/$BIN_NAME" install

  # 补全：子命令十来个，装一次省得记。失败不算安装失败，用户随时可以重跑。
  if [[ -e "/usr/local/bin/$BIN_NAME" ]]; then
    echo "==> 安装 shell 补全"
    "/usr/local/bin/$BIN_NAME" completion install || echo "    （跳过：稍后可手动执行 semisnip completion）"
  fi

  cat <<'TIP'

接下来手动做两件事（系统不允许脚本代劳）：
  1. 系统设置 → 隐私与安全性 → 辅助功能 → 勾选 Semisnip
  2. espanso restart

然后 `semisnip doctor` 应当每项都打勾。
TIP
fi
