#!/bin/bash
# 完全访问守卫：确认 Codex 全局与项目配置始终是“完全访问 + 永不审批”。
# 任何 App 更新、手动切换或误编辑把它改成只读/按需批准时，运行本脚本
# 会自动备份并把关键字段改回。可放入 launchd/任务计划定期执行。
set -u

GLOBAL_CONFIG="$HOME/.codex/config.toml"
# 项目根优先取显式环境变量；没有时按仓库内位置（scripts/..）探测。
# 独立副本装在 ~/Library/LaunchAgents 时不能按 dirname 反推项目根，否则会
# 把 ~/Library/.codex 误当成项目配置目录。
PROJECT_ROOT="${ZUNZHE_PROJECT_ROOT:-}"
if [ -z "$PROJECT_ROOT" ]; then
  PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
fi
PROJECT_CONFIG="$PROJECT_ROOT/.codex/config.toml"
CHANGED=0

backup() {
  local file="$1"
  local stamp
  stamp="$(date '+%Y%m%d-%H%M%S')"
  cp -p "$file" "$file.bak-before-full-access-$stamp" 2>/dev/null || true
}

ensure_value() {
  local file="$1"
  local key="$2"
  local value="$3"
  local pattern="$key *= *"

  if [ ! -f "$file" ]; then
    # 绝不凭空创建配置目录：独立副本装在 LaunchAgents 时若拿不到项目根，
    # 宁可跳过也不要误建 ~/Library/.codex 之类的无关文件。
    if [ ! -d "$(dirname "$file")" ]; then
      return
    fi
    printf '%s = "%s"\n' "$key" "$value" >> "$file"
    CHANGED=1
    return
  fi

  if grep -Eq "^$pattern" "$file"; then
    if ! grep -Eq "^$key = \"$value\"\$" "$file"; then
      backup "$file"
      # 只替换该键的整行，保留其余配置与注释。
      sed -i.bak "s|^$pattern.*|$key = \"$value\"|" "$file"
      rm -f "$file.bak"
      CHANGED=1
    fi
  else
    backup "$file"
    printf '\n%s = "%s"\n' "$key" "$value" >> "$file"
    CHANGED=1
  fi
}

ensure_value "$GLOBAL_CONFIG" "sandbox_mode" "danger-full-access"
ensure_value "$GLOBAL_CONFIG" "approval_policy" "never"
if [ -d "$PROJECT_ROOT/.codex" ]; then
  ensure_value "$PROJECT_CONFIG" "default_permissions" ":danger-full-access"
  ensure_value "$PROJECT_CONFIG" "approval_policy" "never"
fi

# 桌面端每个任务的权限由 ~/.codex/.codex-global-state.json 持久化。若任务被
# 误切到只读/按需批准，配置级守卫修不到这里。这里用原子替换把当前宿主任务、
# 以及所有工作目录属于本项目的任务固定回完全访问；同时钉死 host 级默认
# “完全访问 + 永不审批”，保证新任务默认继承，而不是每次新建都回到只读。
GLOBAL_STATE="$HOME/.codex/.codex-global-state.json"
THREAD_ID="${CODEX_THREAD_ID:-}"
if [ -f "$GLOBAL_STATE" ]; then
  TMP_STATE="$(mktemp "$HOME/.codex/.codex-global-state.json.XXXXXX")"
  if python3 - "$GLOBAL_STATE" "$TMP_STATE" "$THREAD_ID" "$PROJECT_ROOT" <<'PY' 2>/dev/null
import json
import os
import sys

state_path, tmp_path, thread_id, project_root = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]

try:
    with open(state_path, "r", encoding="utf-8") as f:
        state = json.load(f)
except (OSError, json.JSONDecodeError):
    os.unlink(tmp_path)
    sys.exit(2)

container = state.get("electron-persisted-atom-state")
if not isinstance(container, dict):
    os.unlink(tmp_path)
    sys.exit(0)

changed = False


def full_access_entry():
    return {
        "activePermissionProfile": {"id": ":danger-full-access", "extends": None},
        "approvalPolicy": "never",
        "approvalsReviewer": "user",
        "sandboxPolicy": {"type": "dangerFullAccess"},
    }


def repair_thread(tid):
    global changed
    key = "heartbeat-thread-permissions-by-id." + tid
    entry = container.get(key)
    if not isinstance(entry, dict):
        return
    current = entry.get("sandboxPolicy", {}).get("type")
    profile = entry.get("activePermissionProfile", {}).get("id")
    approval = entry.get("approvalPolicy")
    if current == "dangerFullAccess" and profile == ":danger-full-access" and approval == "never":
        return
    container[key] = full_access_entry()
    changed = True


if thread_id:
    repair_thread(thread_id)

# 从桌面端进程台账找出属于本项目的任务，一并修复。这样即使 launchd 环境
# 拿不到 CODEX_THREAD_ID，守卫仍能覆盖“本项目里被切到只读”的历史任务。
process_file = os.path.expanduser("~/.codex/process_manager/chat_processes.json")
try:
    with open(process_file, "r", encoding="utf-8") as f:
        processes = json.load(f)
except (OSError, json.JSONDecodeError):
    processes = []
for process in processes:
    cwd = process.get("cwd") or ""
    if os.path.abspath(cwd).startswith(os.path.abspath(project_root)):
        repair_thread(process.get("conversationId") or "")

# host 级默认：新任务从“完全访问 + 永不审批”继承，避免只有当前任务被修好、
# 下次新建任务又退回只读/按需批准。
host_defaults = {
    "agent-mode-by-host-id": {"local": "full-access"},
    "preferred-non-full-access-agent-mode-by-host-id": {"local": None},
    "permission-selection-by-host-id:local": {
        "kind": "agent-mode",
        "agentMode": "full-access",
    },
}
for key, value in host_defaults.items():
    if container.get(key) != value:
        container[key] = value
        changed = True

if not changed:
    os.unlink(tmp_path)
    sys.exit(0)

with open(tmp_path, "w", encoding="utf-8") as f:
    json.dump(state, f, ensure_ascii=False, separators=(",", ":"))
    f.flush()
    os.fsync(f.fileno())
PY
  then
    if [ -s "$TMP_STATE" ]; then
      backup "$GLOBAL_STATE"
      chmod --reference="$GLOBAL_STATE" "$TMP_STATE" 2>/dev/null || chmod 600 "$TMP_STATE"
      mv -f "$TMP_STATE" "$GLOBAL_STATE"
      CHANGED=1
    else
      rm -f "$TMP_STATE"
    fi
  else
    rm -f "$TMP_STATE"
  fi
fi

now="$(date '+%Y-%m-%d %H:%M:%S %z')"
if [ "$CHANGED" = "1" ]; then
  echo "$now full-access config was out of sync; repaired and backed up"
else
  echo "$now full-access config ok"
fi

exit 0
