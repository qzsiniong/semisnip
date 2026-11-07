#!/usr/bin/env bash
#
# Semisnip 发布新版：改版本号 → 同步 Cargo.lock → 跑本地门禁 →（可选）提交 / 打 tag / 推送。
#
# 版本号的真源是 **Cargo.toml** 的 `[package].version`——就这一处：
#   · .app 的 Info.plist 在打包时由 scripts/bundle-macos.sh 从它注入
#   · 二进制里 clap 的 `--version` 也来自它
#   · release 工作流拿 tag 与它比对
# 所以脚本只改这两处：Cargo.toml 的 version，和 Cargo.lock 里根包那一行（cargo 想改的也正是它）。
#
# 用法:
#   scripts/release.sh <版本|patch|minor|major> [选项]
#
# 例子:
#   scripts/release.sh patch                      # 0.1.0 → 0.1.1，跑门禁，然后交互问要不要提交/打 tag/推送
#   scripts/release.sh 0.2.0-beta.1 --dry-run     # 只打印将要改什么
#   scripts/release.sh minor --commit             # 只提交，不打 tag、不推
#   scripts/release.sh 0.2.0 --commit --tag --push # 全自动（会触发 CI 与发版）
#
# 选项:
#   --commit        提交版本号改动（commit message: chore(release): vX.Y.Z）
#   --tag           打注释 tag vX.Y.Z（需要同时 --commit）
#   --push          推当前分支与 tag（需要同时 --commit --tag；会触发 CI 与发版）
#   --yes, -y       等价于 --commit --tag --push（谨慎）
#   --no-check      跳过本地门禁（默认跑与 CI 相同的那套）
#   --dist          门禁里顺带打 universal 的 dmg/zip 到 target/（发版前想先装一遍用）
#   --dry-run       只打印将要改什么，不写任何文件
#   --allow-dirty   工作区不干净时也继续（默认拒绝，避免把无关改动混进发版提交）
#   -h, --help      显示本帮助
#
# 不给 --commit/--tag/--push 时：如果是在终端里跑，就逐条交互询问；否则只改版本号并打印下一步命令。
# 非 TTY 环境（IDE 任务、CI 包装脚本）也想走交互：设 SEMISNIP_RELEASE_INTERACTIVE=1。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# 只有这一个文件记录版本号（Cargo.lock 由 cargo 同步）
VERSION_FILE="Cargo.toml"

usage() {
  sed -n '2,33p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

target=""
do_commit=0
do_tag=0
do_push=0
yes_all=0
no_check=0
with_dist=0
dry_run=0
allow_dirty=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h | --help)
      usage
      exit 0
      ;;
    --commit) do_commit=1 ;;
    --tag) do_tag=1 ;;
    --push) do_push=1 ;;
    --yes | -y) yes_all=1 ;;
    --no-check) no_check=1 ;;
    --dist) with_dist=1 ;;
    --dry-run) dry_run=1 ;;
    --allow-dirty) allow_dirty=1 ;;
    -*)
      echo "✗ 未知选项：${1}" >&2
      usage >&2
      exit 2
      ;;
    *)
      if [[ -n "${target}" ]]; then
        echo "✗ 只接受一个版本参数（收到：${target} 与 ${1}）" >&2
        exit 2
      fi
      target="$1"
      ;;
  esac
  shift
done

if [[ -z "${target}" ]]; then
  usage >&2
  exit 2
fi

if [[ $yes_all -eq 1 ]]; then
  do_commit=1
  do_tag=1
  do_push=1
fi
if [[ $do_tag -eq 1 && $do_commit -eq 0 ]]; then
  echo "✗ --tag 需要同时给 --commit：否则 tag 会指向「还没包含版本号改动」的提交" >&2
  exit 2
fi
if [[ $do_push -eq 1 && $do_tag -eq 0 ]]; then
  echo "✗ --push 需要同时给 --commit --tag" >&2
  exit 2
fi

# ── 当前版本 → 目标版本 ──────────────────────────────────────────────────────
current="$(awk -F'"' '/^version = "/{print $2; exit}' "${VERSION_FILE}")"
if [[ -z "${current}" ]]; then
  echo "✗ 读不到 ${VERSION_FILE} 的 [package].version" >&2
  exit 1
fi

case "${target}" in
  patch | minor | major)
    base="${current%%-*}"
    IFS=. read -r major_ver minor_ver patch_ver <<<"$base"
    case "${target}" in
      major)
        major_ver=$((major_ver + 1))
        minor_ver=0
        patch_ver=0
        ;;
      minor)
        minor_ver=$((minor_ver + 1))
        patch_ver=0
        ;;
      patch) patch_ver=$((patch_ver + 1)) ;;
    esac
    new="${major_ver}.${minor_ver}.${patch_ver}"
    ;;
  *) new="${target}" ;;
esac

if [[ ! "${new}" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]]; then
  echo "✗ 版本号不合法：${new}（示例：0.2.0 / 0.2.0-beta.1 / patch）" >&2
  exit 2
fi
if [[ "${new}" == "${current}" ]]; then
  echo "✗ 当前已经是 ${current}。" >&2
  echo "  要重打 tag 就直接用 git；要把文件改回来用：git checkout -- ${VERSION_FILE} Cargo.lock" >&2
  exit 1
fi
if git rev-parse -q --verify "refs/tags/v${new}" >/dev/null; then
  echo "✗ 本地已存在 tag v${new}：换个版本号，或先 git tag -d v${new}" >&2
  exit 1
fi
# 远端 tag 只在**真要推送**时查：这是网络调用（可能几十秒甚至卡住），
# 平时不发版不该被它拖住。顺带给 git 一个低速超时，避免链接被黑洞时无限等。
if [[ $do_push -eq 1 ]] && git -c http.lowSpeedLimit=1000 -c http.lowSpeedTime=10 \
  ls-remote --exit-code --tags origin "v${new}" >/dev/null 2>&1; then
  echo "✗ 远端已存在 tag v${new}：换个版本号，或先删掉远端 tag" >&2
  exit 1
fi

if [[ $allow_dirty -eq 0 ]]; then
  dirty="$(git status --porcelain)"
  if [[ -n "$dirty" ]]; then
    echo "✗ 工作区不干净 —— 发版提交里会混进无关改动。先提交/stash，或加 --allow-dirty：" >&2
    echo "$dirty" >&2
    exit 1
  fi
fi

echo "== Semisnip 发布 =="
echo "  当前版本：v${current}"
echo "  目标版本：v${new}"
echo "  将改动：  ${VERSION_FILE} + Cargo.lock"
if [[ $do_commit -eq 1 ]]; then
  suffix=""
  if [[ $do_tag -eq 1 ]]; then suffix=" → 打 tag v${new}"; fi
  if [[ $do_push -eq 1 ]]; then suffix="${suffix} → 推送（触发 CI 与发版）"; fi
  echo "  之后：    提交${suffix}"
fi

if [[ $dry_run -eq 1 ]]; then
  echo
  echo "（--dry-run：没有写任何文件）"
  exit 0
fi

restore_hint() {
  echo >&2
  echo "回退这次改动：git checkout -- ${VERSION_FILE} Cargo.lock" >&2
}
trap 'echo "✗ 出错中断（版本号可能已被改动）" >&2; restore_hint' ERR

# ── 改版本号 ────────────────────────────────────────────────────────────────
# 只替换文件里第一条 `version = "..."`（即 [package] 的那条），
# 依赖都是 `name = "..."` 或 `name = { version = ... }`，不会误伤。
# 用 awk 而不是 `sed 0,/re/`：那是 GNU 扩展，macOS 自带的 BSD sed 不认。
tmp="$(mktemp -t semisnip-version)"
awk -v v="${new}" '
  !done && /^version = "/ { print "version = \"" v "\""; done = 1; next }
  { print }
' "${VERSION_FILE}" >"${tmp}"
# 用 cat 覆盖而不是 mv：mktemp 建的是 0600，mv 会把这份权限带到 Cargo.toml 上。
cat "${tmp}" >"${VERSION_FILE}"
rm -f "${tmp}"

got="$(awk -F'"' '/^version = "/{print $2; exit}' "${VERSION_FILE}")"
if [[ "${got}" != "${new}" ]]; then
  echo "✗ 改完读回来是 ${got}，期望 ${new}" >&2
  restore_hint
  exit 1
fi
echo "✓ ${VERSION_FILE}：v${current} → v${new}"

# ── 同步 Cargo.lock ─────────────────────────────────────────────────────────
# `cargo metadata` 本来能做这件事，但它会为了组装依赖信息去下载 crate 源码：
# 断网（或网络很慢）时会卡在 "Downloading crates ..." 上，而这里要改的只是
# **根包自己那一行版本号**。所以直接改——cargo 想改的也正是这一行。
#
# 改错了也不会静默漏过：紧跟着的门禁会跑 clippy / build，cargo 发现 lock 与
# manifest 不一致会当场重写它（`--no-check` 跳过门禁时也不会脏，因为它下次
# 构建就会自己修正）。
awk -v v="${new}" '
  /^name = "semisnip"$/ { print; in_root = 1; next }
  in_root && /^version = "/ { print "version = \"" v "\""; in_root = 0; next }
  { print }
' Cargo.lock >"${tmp}"
cat "${tmp}" >Cargo.lock
rm -f "${tmp}"
lock_ver="$(awk '/^name = "semisnip"$/{ getline; if ($0 ~ /^version = "/) { gsub(/version = "|"/, ""); print; exit } }' Cargo.lock)"
if [[ "${lock_ver}" != "${new}" ]]; then
  echo "✗ Cargo.lock 里的 semisnip 版本是「${lock_ver}」，期望 ${new}" >&2
  restore_hint
  exit 1
fi
echo "✓ Cargo.lock 已同步"

# ── 本地门禁（与 CI 相同） ───────────────────────────────────────────────────
if [[ $no_check -eq 1 ]]; then
  echo "→ 已跳过本地门禁（--no-check）"
else
  echo "== 本地门禁（与 CI 相同；跳过用 --no-check）=="
  for step in \
    "cargo fmt --all --check" \
    "cargo clippy --all-targets -- -D warnings" \
    "cargo test --no-fail-fast"; do
    echo "→ ${step}"
    # shellcheck disable=SC2086 # 这里就是要按空格拆成命令与参数
    $step
  done

  # 打包冒烟：CI 的 bundle job 做的就是这一步。它验证 check 验证不到的东西——
  # build.rs 有没有生成图标、Info.plist 能不能被 PlistBuddy 改、ad-hoc 签名会不会失败。
  if [[ $with_dist -eq 1 ]]; then
    echo "→ ./scripts/bundle-macos.sh --universal --dist"
    ./scripts/bundle-macos.sh --universal --dist
  else
    echo "→ ./scripts/bundle-macos.sh"
    ./scripts/bundle-macos.sh
  fi
  echo "✓ 本地门禁通过"
fi

# ── 可选的提交 / 打 tag / 推送 ──────────────────────────────────────────────
explicit=0
if [[ $do_commit -eq 1 || $do_tag -eq 1 || $do_push -eq 1 ]]; then
  explicit=1
fi

if [[ $explicit -eq 0 ]]; then
  # 想在非 TTY 环境（IDE 任务、CI 包装脚本）里也走询问，就设 SEMISNIP_RELEASE_INTERACTIVE=1
  if [[ -t 0 && -t 1 ]] || [[ "${SEMISNIP_RELEASE_INTERACTIVE:-}" == "1" ]]; then
    echo
    echo "== 接下来（直接回车 = 用方括号里的默认值）=="
    read -r -p "提交这次版本号改动？[Y/n] " ans || true
    if [[ ! "$ans" =~ ^[Nn]$ ]]; then
      do_commit=1
      read -r -p "打 tag v${new}？[Y/n] " ans || true
      if [[ ! "$ans" =~ ^[Nn]$ ]]; then
        do_tag=1
        read -r -p "推送分支与 tag（会触发 CI 与发版流程）？[y/N] " ans || true
        if [[ "$ans" =~ ^[Yy]$ ]]; then do_push=1; fi
      fi
    fi
  else
    echo
    echo "（非交互环境：只改了版本号，下面是剩下的命令）"
  fi
fi

if [[ $do_commit -eq 1 ]]; then
  git add -- "${VERSION_FILE}" Cargo.lock
  git commit -m "chore(release): v${new}"
  echo "✓ 已提交：chore(release): v${new}"
fi
if [[ $do_tag -eq 1 ]]; then
  git tag -a "v${new}" -m "Semisnip v${new}"
  echo "✓ 已打 tag：v${new}"
fi
if [[ $do_push -eq 1 ]]; then
  branch="$(git rev-parse --abbrev-ref HEAD)"
  if [[ "${branch}" != "main" ]]; then
    echo "⚠ 当前分支是 ${branch}（不是 main）—— 确认这是你想发布的提交"
  fi
  git push
  git push origin "v${new}"
  echo "✓ 已推送分支与 tag"
fi

# ── 收尾提示 ────────────────────────────────────────────────────────────────
web_url="$(git remote get-url origin 2>/dev/null | sed -E 's#^git@([^:]+):#https://\1/#; s#\.git$##' || true)"

echo
echo "✓ 版本号：v${current} → v${new}"
if [[ $do_commit -eq 0 ]]; then
  echo "下一步（或重跑：scripts/release.sh ${new} --commit --tag --push 需先 git checkout -- .）："
  echo "  git add -- ${VERSION_FILE} Cargo.lock"
  echo "  git commit -m \"chore(release): v${new}\""
fi
if [[ $do_tag -eq 0 && $do_commit -eq 1 ]]; then
  echo "  git tag -a v${new} -m \"Semisnip v${new}\""
fi
if [[ $do_push -eq 0 && $do_tag -eq 1 ]]; then
  echo "  git push && git push origin v${new}"
fi
if [[ $do_push -eq 1 ]]; then
  echo
  echo "已推送：Release 工作流会自动跑 ci → verify → build（universal dmg + zip）"
  if [[ -n "${web_url}" ]]; then
    echo "  看进度：${web_url}/actions"
    echo "  检查草稿并 Publish：${web_url}/releases"
  fi
else
  echo
  echo "提示：推送 tag 才会触发发版；想先试跑流水线可用 Actions 页面的 Run workflow。"
fi
