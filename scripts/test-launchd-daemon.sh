#!/bin/bash
# launchd 启动脚本隔离回归：即使继承的当前目录不可访问，也必须从
# 安装时固化的项目根启动，不能猜候选路径（尤其不能选中 ~/Projects 符号链接）。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DAEMON_SCRIPT="$ROOT/scripts/launchd/zunzhe-daemon.sh"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/zunzhe-launchd-test.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

TEST_HOME="$TMP/home"
PROJECT="$TEST_HOME/real project"
BIN_DIR="$PROJECT/zunzhe-core/target/release"
BIN="$BIN_DIR/zunzhe-core"
DELETED_CWD="$TMP/deleted-cwd"
LEGACY_LINK_PARENT="$TEST_HOME/Projects"
LEGACY_LINK="$LEGACY_LINK_PARENT/尊者监控直播短视频"

mkdir -p "$PROJECT/config" "$BIN_DIR" "$DELETED_CWD" "$PROJECT/scripts/launchd"
mkdir -p "$LEGACY_LINK_PARENT"
ln -s "$PROJECT" "$LEGACY_LINK"
cp "$ROOT/scripts/launchd/com.zunzhe.monitor.plist" "$PROJECT/scripts/launchd/"
cp "$ROOT/scripts/launchd/com.zunzhe.healthcheck.plist" "$PROJECT/scripts/launchd/"
CUSTOM_LOG_DIR="$TMP/custom-logs"
cat > "$PROJECT/config/config.ini.example" <<EOF
[路径设置]
日志目录 = $CUSTOM_LOG_DIR
EOF

cat > "$BIN" <<'EOF'
#!/bin/bash
set -euo pipefail
printf '%s\n' "$PWD" > "$TEST_HOME/observed-cwd"
printf '%s\n' "$@" > "$TEST_HOME/observed-args"
EOF
chmod +x "$BIN"

set +e
(
  cd "$DELETED_CWD"
  rmdir "$DELETED_CWD"
  HOME="$TEST_HOME" TEST_HOME="$TEST_HOME" ZUNZHE_PROJECT_ROOT="$PROJECT" \
    /bin/bash "$DAEMON_SCRIPT" --probe
) >"$TMP/stdout" 2>"$TMP/stderr"
daemon_rc=$?
set -e

if [ "$daemon_rc" -ne 0 ]; then
  echo "FAIL: daemon exited with status $daemon_rc" >&2
  cat "$TMP/stderr" >&2
  exit 1
fi

expected_root="$(cd "$PROJECT" && pwd -P)"
observed_cwd="$(cat "$TEST_HOME/observed-cwd" 2>/dev/null || true)"
observed_args="$(cat "$TEST_HOME/observed-args" 2>/dev/null || true)"

if [ "$observed_cwd" != "$expected_root" ]; then
  echo "FAIL: daemon cwd expected $expected_root, got $observed_cwd" >&2
  cat "$TMP/stderr" >&2
  exit 1
fi

if [ "$observed_args" != $'run-daemon\n--probe' ]; then
  echo "FAIL: unexpected daemon args: $observed_args" >&2
  exit 1
fi

LAUNCH_AGENTS="$TEST_HOME/Library/LaunchAgents"
INSTALLED_SCRIPT="$LAUNCH_AGENTS/zunzhe-daemon.sh"
mkdir -p "$LAUNCH_AGENTS"
cp "$DAEMON_SCRIPT" "$INSTALLED_SCRIPT"
chmod +x "$INSTALLED_SCRIPT"

# 安装脚本是 plist 渲染的唯一权威入口；从符号链接路径安装时，plist 必须钉住
# 物理真实根，避免 launchd 与配置解析再次落到两个项目根。
PROJECT_DIR="$PROJECT" HOME="$TEST_HOME" \
  /bin/bash "$ROOT/scripts/render-launchd-plists.sh" "$LEGACY_LINK" "$LAUNCH_AGENTS"

physical_root="$(cd "$PROJECT" && pwd -P)"

for plist in com.zunzhe.monitor.plist com.zunzhe.healthcheck.plist; do
  rendered="$LAUNCH_AGENTS/$plist"
  installed_root="$(plutil -extract EnvironmentVariables.ZUNZHE_PROJECT_ROOT raw "$rendered" 2>/dev/null || true)"
  if [ "$installed_root" != "$physical_root" ]; then
    echo "FAIL: $plist must pin physical ZUNZHE_PROJECT_ROOT=$physical_root, got '$installed_root'" >&2
    exit 1
  fi
done

if plutil -extract WorkingDirectory raw "$LAUNCH_AGENTS/com.zunzhe.monitor.plist" >/dev/null 2>&1; then
  echo "FAIL: launchd plist must not set WorkingDirectory" >&2
  exit 1
fi

# PATH 不能写死真实用户名；安装渲染后必须是当前用户 HOME，不能用仓库里的
# /Users/zhangxuetao 绝对路径污染其他机器。
program_arg="$(plutil -extract ProgramArguments.1 raw "$LAUNCH_AGENTS/com.zunzhe.monitor.plist")"
case "$program_arg" in
  "$TEST_HOME/Library/LaunchAgents/"*) ;;
  *) echo "FAIL: plist ProgramArguments must render from HOME, got $program_arg" >&2; exit 1 ;;
esac

path_value="$(plutil -extract EnvironmentVariables.PATH raw "$LAUNCH_AGENTS/com.zunzhe.monitor.plist")"
case "$path_value" in
  "$TEST_HOME/.cargo/bin:"*) ;;
  *) echo "FAIL: plist PATH must render from HOME, got $path_value" >&2; exit 1 ;;
esac

for key in StandardOutPath StandardErrorPath; do
  value="$(plutil -extract "$key" raw "$LAUNCH_AGENTS/com.zunzhe.monitor.plist")"
  case "$value" in
    "$CUSTOM_LOG_DIR/"*) ;;
    *) echo "FAIL: $key must follow [路径设置] 日志目录, got $value" >&2; exit 1 ;;
  esac
  if printf "%s" "$value" | grep -q "Library/Logs/zunzhe"; then
    echo "FAIL: $key still points at legacy user log dir: $value" >&2
    exit 1
  fi
done
if [ ! -d "$CUSTOM_LOG_DIR" ]; then
  echo "FAIL: configured log directory was not pre-created: $CUSTOM_LOG_DIR" >&2
  exit 1
fi

(
  cd "$TMP"
  HOME="$TEST_HOME" TEST_HOME="$TEST_HOME" ZUNZHE_PROJECT_ROOT="$PROJECT" \
    /bin/bash "$INSTALLED_SCRIPT" probe
) >"$TMP/installed-stdout" 2>"$TMP/installed-stderr"

installed_cwd="$(cat "$TEST_HOME/observed-cwd")"
if [ "$installed_cwd" != "$expected_root" ]; then
  echo "FAIL: installed daemon cwd expected $expected_root, got $installed_cwd" >&2
  cat "$TMP/installed-stderr" >&2
  exit 1
fi

echo "test-launchd-daemon: PASS"
