#!/bin/bash
# 完全访问守卫隔离回归：用临时 HOME 模拟桌面端“只读任务”，验证守卫能
# 从带 host 前缀的路由记录、进程台账和任务归属表里找到项目任务并修回
# 完全访问，且异常 JSON 结构不会让守卫崩溃或误报。
set -u

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
GUARD="$ROOT/scripts/ensure-full-access.sh"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/zunzhe-full-access-test.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

mkdir -p "$TMP/.codex/process_manager" "$TMP/project/.codex" "$TMP/project/scripts"

cat > "$TMP/project/.codex/config.toml" <<'EOF'
approval_policy = "never"
default_permissions = ":danger-full-access"
EOF

cat > "$TMP/.codex/config.toml" <<'EOF'
sandbox_mode = "danger-full-access"
approval_policy = "never"
EOF

cat > "$TMP/.codex/.codex-global-state.json" <<EOF
{"electron-persisted-atom-state":{
  "agent-mode-by-host-id":{"local":"full-access"},
  "preferred-non-full-access-agent-mode-by-host-id":{"local":null},
  "permission-selection-by-host-id:local":{"kind":"agent-mode","agentMode":"full-access"},
  "heartbeat-thread-permissions-by-id":{"test-tid-123":{
    "activePermissionProfile":{"id":":read-only","extends":null},
    "approvalPolicy":"on-request",
    "approvalsReviewer":"user",
    "sandboxPolicy":{"type":"readOnly"}}},
  "thread-tab-routes-v1:local%3Atest-tid-123":{"routes":[{"kind":"chat","params":{"conversationId":"test-tid-123","cwd":"$TMP/project"}}]},
  "thread-tab-routes-v1:bad":{"routes":"not-a-list"},
  "thread-tab-routes-v1:also-bad":"not-a-dict"
}}
EOF

# 进程台账允许是异常结构：守卫必须忽略而不是崩溃。
cat > "$TMP/.codex/process_manager/chat_processes.json" <<'EOF'
{"processes": "unexpected-shape"}
EOF

cp "$GUARD" "$TMP/project/scripts/ensure-full-access.sh"

run_guard() {
  HOME="$TMP" ZUNZHE_PROJECT_ROOT="$TMP/project" \
    bash "$TMP/project/scripts/ensure-full-access.sh"
}

first="$(run_guard)"
echo "$first"
case "$first" in
  *"repaired and backed up"*) ;;
  *) echo "FAIL: 第一次运行应报告修复" >&2; exit 1 ;;
esac

python3 - "$TMP" <<'PY'
import json
import sys

root = sys.argv[1]
path = root + "/.codex/.codex-global-state.json"
with open(path, encoding="utf-8") as f:
    state = json.load(f)
entry = state["electron-persisted-atom-state"]["heartbeat-thread-permissions-by-id"]["test-tid-123"]
assert entry["sandboxPolicy"]["type"] == "dangerFullAccess", entry
assert entry["activePermissionProfile"]["id"] == ":danger-full-access", entry
assert entry["approvalPolicy"] == "never", entry
print("permission entry repaired to dangerFullAccess + never")
PY

second="$(run_guard)"
echo "$second"
case "$second" in
  *"full-access config ok"*) ;;
  *) echo "FAIL: 第二次运行应报告配置正常" >&2; exit 1 ;;
esac

echo "test-ensure-full-access: PASS"
