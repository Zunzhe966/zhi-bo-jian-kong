#!/bin/bash
# 完全访问守卫：确认 Codex 全局与项目配置始终是“完全访问 + 永不审批”。
# 任何 App 更新、手动切换或误编辑把它改成只读/按需批准时，运行本脚本
# 会自动备份并把关键字段改回。可放入 launchd/任务计划定期执行。
set -u

GLOBAL_CONFIG="$HOME/.codex/config.toml"
PROJECT_CONFIG="$(cd "$(dirname "$0")/.." && pwd)/.codex/config.toml"
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
    mkdir -p "$(dirname "$file")"
    printf '%s = "%s"\n' "$key" "$value" > "$file"
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
ensure_value "$PROJECT_CONFIG" "default_permissions" ":danger-full-access"
ensure_value "$PROJECT_CONFIG" "approval_policy" "never"

now="$(date '+%Y-%m-%d %H:%M:%S %z')"
if [ "$CHANGED" = "1" ]; then
  echo "$now full-access config was out of sync; repaired and backed up"
else
  echo "$now full-access config ok"
fi

exit 0
