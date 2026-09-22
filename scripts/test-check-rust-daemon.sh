#!/bin/bash
# 看门狗路径隔离回归：pid 必须跟随 [路径设置] 复盘输出目录，日志必须跟随
# [路径设置] 日志目录。测试把脚本放进隔离项目，并同时放置两个候选 pid：
# 配置目录里的正确 pid、旧硬编码目录里的错误 pid。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SCRIPT_UNDER_TEST="${ZUNZHE_CHECK_SCRIPT:-$ROOT/scripts/check-rust-daemon.sh}"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/zunzhe-check-rust-daemon-test.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

PROJECT="$TMP/project"
TEST_HOME="$TMP/home"
CUSTOM_FUPAN="$TMP/custom-fupan-output"
OTHER_FUPAN="$TMP/other-fupan-output"
CUSTOM_LOG_DIR="$TMP/custom-logs"
FAKE_CALL_LOG="$TMP/rust-bin-calls.log"

mkdir -p \
  "$PROJECT/config" \
  "$PROJECT/scripts" \
  "$PROJECT/zunzhe-core/target/release" \
  "$CUSTOM_FUPAN/run" \
  "$OTHER_FUPAN/run" \
  "$PROJECT/fupan_output/run" \
  "$TEST_HOME"

cp "$SCRIPT_UNDER_TEST" "$PROJECT/scripts/check-rust-daemon.sh"
chmod +x "$PROJECT/scripts/check-rust-daemon.sh"

cat > "$PROJECT/config/config.ini" <<EOF
[路径设置]
产物根目录 = $OTHER_FUPAN
复盘输出目录 = $CUSTOM_FUPAN
日志目录 = $CUSTOM_LOG_DIR
EOF

printf '%s\n' '424242' > "$CUSTOM_FUPAN/run/zunzhe-core.pid"
printf '%s\n' '111111' > "$PROJECT/fupan_output/run/zunzhe-core.pid"
printf '%s\n' '999999' > "$OTHER_FUPAN/run/zunzhe-core.pid"

cat > "$PROJECT/zunzhe-core/target/release/zunzhe-core" <<EOF
#!/bin/bash
printf '%s\n' "\$*" >> "$FAKE_CALL_LOG"
if [ "\${1:-}" = "doctor" ]; then
  printf '%s\n' '{"rust_daemon":true,"heartbeat_ok":true,"health_audit":{"ok":true,"pid":424242},"source_health":{"ok":true,"total":0,"failing":0,"cooling":0},"notify_health":{"ok":true,"pending":0,"failed":0}}'
  exit 0
fi
if [ "\${1:-}" = "ensure-services" ]; then
  printf '%s\n' 'unexpected ensure-services call' >&2
  exit 97
fi
exit 0
EOF
chmod +x "$PROJECT/zunzhe-core/target/release/zunzhe-core"

HOME="$TEST_HOME" \
  ZUNZHE_PROJECT_ROOT="$PROJECT" \
  /bin/bash "$PROJECT/scripts/check-rust-daemon.sh"

WATCHDOG_LOG="$CUSTOM_LOG_DIR/zunzhe-core.watchdog.log"
if [ ! -f "$WATCHDOG_LOG" ]; then
  echo "FAIL: watchdog log was not written to configured 日志目录: $WATCHDOG_LOG" >&2
  exit 1
fi

last_line="$(tail -n 1 "$WATCHDOG_LOG")"
if ! printf '%s\n' "$last_line" | grep -Eq ' pid=424242 rust_daemon=true heartbeat_ok=true$'; then
  echo "FAIL: watchdog did not report configured pid: $last_line" >&2
  exit 1
fi

if printf '%s\n' "$last_line" | grep -Eq 'pid=(111111|999999|none)'; then
  echo "FAIL: watchdog used a non-configured pid source: $last_line" >&2
  exit 1
fi

if [ -e "$PROJECT/logs/zunzhe-core.watchdog.log" ]; then
  echo "FAIL: watchdog fell back to project logs directory" >&2
  exit 1
fi

if [ ! -f "$CUSTOM_LOG_DIR/zunzhe-core.audit.log" ]; then
  echo "FAIL: audit log was not written to configured 日志目录" >&2
  exit 1
fi

if grep -q 'ensure-services' "$FAKE_CALL_LOG"; then
  echo "FAIL: healthy watchdog should not call ensure-services" >&2
  exit 1
fi

echo "test-check-rust-daemon: PASS"
