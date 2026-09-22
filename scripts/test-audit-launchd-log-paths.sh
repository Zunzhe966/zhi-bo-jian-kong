#!/bin/bash
# 审计脚本隔离回归：不碰真实 launchd，验证一致与漂移两种状态。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
AUDIT="$ROOT/scripts/audit-launchd-log-paths.sh"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/zunzhe-launchd-log-audit.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

PROJECT="$TMP/project"
HOME_DIR="$TMP/home"
LAUNCH_AGENTS="$HOME_DIR/Library/LaunchAgents"
CONFIG_LOG="$TMP/product/logs"
mkdir -p "$PROJECT/config" "$LAUNCH_AGENTS" "$CONFIG_LOG"

cat > "$PROJECT/config/config.ini" <<EOF
[路径设置]
产物根目录 = $TMP/product
日志目录 = $CONFIG_LOG
EOF

cat > "$LAUNCH_AGENTS/com.zunzhe.monitor.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
  <key>StandardOutPath</key><string>$CONFIG_LOG/zunzhe-core.log</string>
  <key>StandardErrorPath</key><string>$CONFIG_LOG/zunzhe-core.err.log</string>
</dict></plist>
EOF

# 用假的 launchctl 注入“已加载目录”，审计脚本仍然只读真实文件与 print 输出。
FAKE_BIN="$TMP/bin"
mkdir -p "$FAKE_BIN"
cat > "$FAKE_BIN/launchctl" <<EOF
#!/bin/bash
if [ "\${1:-}" = "print" ]; then
  printf '%s\n' 'stdout path = $CONFIG_LOG/zunzhe-core.log'
  printf '%s\n' 'stderr path = $CONFIG_LOG/zunzhe-core.err.log'
  exit 0
fi
exit 1
EOF
chmod +x "$FAKE_BIN/launchctl"

PATH="$FAKE_BIN:$PATH" HOME="$HOME_DIR" ZUNZHE_PROJECT_ROOT="$PROJECT" \
  ZUNZHE_LAUNCH_AGENTS="$LAUNCH_AGENTS" bash "$AUDIT" >"$TMP/pass.out" 2>"$TMP/pass.err"
grep -q '^PASS:' "$TMP/pass.out"

# 把磁盘 plist 改成旧目录后必须稳定 FAIL；证明审计不是只看配置。
cat > "$LAUNCH_AGENTS/com.zunzhe.monitor.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
  <key>StandardOutPath</key><string>$PROJECT/logs/zunzhe-core.log</string>
  <key>StandardErrorPath</key><string>$PROJECT/logs/zunzhe-core.err.log</string>
</dict></plist>
EOF
if PATH="$FAKE_BIN:$PATH" HOME="$HOME_DIR" ZUNZHE_PROJECT_ROOT="$PROJECT" \
  ZUNZHE_LAUNCH_AGENTS="$LAUNCH_AGENTS" bash "$AUDIT" >"$TMP/fail.out" 2>"$TMP/fail.err"; then
  echo "FAIL: 磁盘 plist 漂移时审计必须失败" >&2
  exit 1
fi
grep -q '磁盘 plist 日志目录与配置不一致' "$TMP/fail.err"

echo "test-audit-launchd-log-paths: PASS"
