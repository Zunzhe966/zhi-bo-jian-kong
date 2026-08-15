#!/bin/bash
# 24×30 长跑体检：从 doctor 的 health_audit 提取可审计指标，按分钟落盘。
# 由 launchd/桌面端每分钟调用一次，配合 zunzhe-core.uptime.log 一起证明
# “持续运行、心跳连续、RSS 不增长、无日志断档”。任何一步失败只记一行
# 异常，不让审计脚本本身阻塞健康看门狗。
set -u

VERIFY="${1:-}"
VERIFY_LOG="${2:-}"

ROOT="${ZUNZHE_PROJECT_ROOT:-}"
if [ -z "$ROOT" ]; then
  for candidate in "$HOME/Projects/尊者监控直播短视频" "$HOME/Documents/尊者监控直播短视频" "$HOME/Desktop/尊者监控直播短视频"; do
    if [ -f "$candidate/config/config.ini.example" ] || [ -f "$candidate/config/config.ini" ]; then
      ROOT="$candidate"
      break
    fi
  done
fi
if [ -z "$ROOT" ]; then
  exit 0
fi

LOG_DIR="$ROOT/logs"
LOG="$LOG_DIR/zunzhe-core.audit.log"
RUST_BIN="$ROOT/zunzhe-core/target/release/zunzhe-core"
if [ ! -x "$RUST_BIN" ]; then
  RUST_BIN="$ROOT/zunzhe-core/target/debug/zunzhe-core"
fi

# --verify：只读审计日志，判断是否已满足 24×30 验收（同一 pid 连续心跳、
# 日志无断档、最近心跳新鲜）。不做任何写入，可被 CI/手动验收重复调用。
if [ "$VERIFY" = "--verify" ]; then
  AUDIT_LOG="${VERIFY_LOG:-$LOG}"
  [ -f "$AUDIT_LOG" ] || { echo '{"ok":false,"error":"审计日志不存在"}' ; exit 0; }
  python3 - "$AUDIT_LOG" "$RUST_BIN" <<'PY'
import json, subprocess, sys, datetime
path = sys.argv[1]
rust_bin = sys.argv[2]
records = []
with open(path, encoding="utf-8") as f:
    for line in f:
        line = line.strip()
        if not line:
            continue
        # 行格式：时间 JSON；异常行/轮转前的旧格式不参与判定。
        try:
            # 实际行是 "2026-08-15 09:24:25 +0800 {...}"，时间占三段，
            # JSON 从第一个 "{" 开始，不能按单个空格拆分。
            ts = line[:25]
            raw = line[25:].strip()
            # 日志时间格式是 "2026-08-15 09:24:25 +0800"，
            # Python 的 fromisoformat 需要把时区空格换成 "T" 才能解析。
            ts = datetime.datetime.fromisoformat(ts.replace(" ", "T", 1))
            rec = json.loads(raw)
            records.append((ts, rec))
        except Exception:
            continue
if not records:
    print('{"ok":false,"error":"审计日志无可解析记录"}')
    sys.exit(0)

now = datetime.datetime.now().astimezone()
last_ts, last = records[-1]
last_age = (now - last_ts).total_seconds()
pid = last.get("pid")
same_pid = [r for r in records if r[1].get("pid") == pid and r[1].get("heartbeat_ok")]
first_ts = same_pid[0][0] if same_pid else None
running_secs = (last_ts - first_ts).total_seconds() if first_ts else 0
target_secs = 24 * 30 * 60
same_pid_records = [r for r in records if r[1].get("pid") == pid and r[1].get("heartbeat_ok")]
# 断档判定只看同一 pid 的连续心跳：不同 pid 之间的时间间隔是版本热切换/
# 正常重启，不代表当前 daemon 断档；同一 pid 相邻记录间隔超过 180 秒才判
# 断档，避免历史日志永久卡住 24×30 验收。
gap_ok = all(
    (same_pid_records[i][0] - same_pid_records[i - 1][0]).total_seconds() <= 180
    for i in range(1, len(same_pid_records))
)
recent_ok = last_age <= 300 and bool(last.get("heartbeat_ok")) and bool(last.get("health"))
# 源/通知健康按“最近 30 分钟 + 最新快照”判定，而不是全历史累加：
# 一条早已修复的历史异常（例如用户清理测试通知、平台恢复）不应永久卡住
# 24×30 验收；但最近 30 分钟出现过冷却/通知失败仍会被判定为不健康。
recent_window = datetime.timedelta(minutes=30)
source_ok = bool(last.get("source_ok", True)) and all(
    rec.get("source_ok", True) for ts, rec in records if ts >= last_ts - recent_window
)
notify_ok = bool(last.get("notify_ok", True)) and all(
    rec.get("notify_ok", True) for ts, rec in records if ts >= last_ts - recent_window
)
uptime_secs = 0
try:
    diag = json.loads(subprocess.check_output(
        [rust_bin, "doctor"], cwd=path if False else None,
        stderr=subprocess.DEVNULL, timeout=10,
    ).decode("utf-8", "replace"))
    uptime_secs = int(diag.get("uptime_secs") or 0)
except Exception:
    pass
# 审计日志会按 5MB 轮转，首条可能不在当前文件里；daemon 自报 uptime 是
# 同一 pid 的连续运行时长，作为轮转后的辅助证据。
duration_ok = running_secs >= target_secs or uptime_secs >= target_secs
ok = bool(pid) and duration_ok and gap_ok and recent_ok and source_ok and notify_ok
print(json.dumps({
    "ok": ok,
    "pid": pid,
    "running_secs": int(running_secs),
    "uptime_secs": uptime_secs,
    "target_secs": target_secs,
    "records_same_pid": len(same_pid),
    "gap_ok": gap_ok,
    "recent_ok": recent_ok,
    "source_ok": source_ok,
    "notify_ok": notify_ok,
    "source_cooling": last.get("source_cooling", 0),
    "source_failing": last.get("source_failing", 0),
    "notify_pending": last.get("notify_pending", 0),
    "notify_failed": last.get("notify_failed", 0),
    "notify_hint": last.get("notify_hint"),
    "health_window_secs": int(recent_window.total_seconds()),
    "last_age_secs": int(last_age),
}, ensure_ascii=False))
PY
  exit 0
fi

[ -x "$RUST_BIN" ] || exit 0

mkdir -p "$LOG_DIR"

# 与健康脚本同样按 5MB 轮转，保留 5 份，避免 24×30 审计日志无限膨胀。
rotate_if_large() {
  local file="$1"
  local size
  if [ -f "$file" ]; then
    # macOS 用 -f%z，Linux 用 -c%s；两者取不到时按 0 处理，不阻塞轮转。
    size="$(stat -f%z "$file" 2>/dev/null || stat -c%s "$file" 2>/dev/null || echo 0)"
  else
    size=0
  fi
  if [ "$size" -ge 5242880 ]; then
    for i in 5 4 3 2 1; do
      if [ -f "$file.$i" ]; then
        mv -f "$file.$i" "$file.$((i+1))" 2>/dev/null || true
      fi
    done
    if [ -f "$file.1" ]; then
      mv -f "$file.1" "$file.2" 2>/dev/null || true
    fi
    mv -f "$file" "$file.1" 2>/dev/null || true
  fi
}
rotate_if_large "$LOG"

status="$(cd "$ROOT" && "$RUST_BIN" doctor 2>/dev/null || true)"
if [ -z "$status" ]; then
  echo "$(date '+%Y-%m-%d %H:%M:%S %z') doctor_unavailable" >> "$LOG"
  exit 0
fi

# 优先取新增 health_audit 的机器字段；旧二进制缺失时回退到原有字段，
# 保证脚本对老版本也能记录基础指标。
health="$(printf '%s' "$status" | python3 -c 'import json,sys
d=json.load(sys.stdin)
h=d.get("health_audit") or {}
sh=d.get("source_health") or {}
nh=d.get("notify_health") or {}
print(json.dumps({
  "health": bool(d.get("health", h.get("ok", False))),
  "daemon_ok": bool(d.get("daemon_ok", d.get("rust_daemon", h.get("daemon_ok", False)))),
  "heartbeat_ok": bool(d.get("heartbeat_ok", h.get("heartbeat_ok", False))),
  "pid": h.get("pid") or d.get("rust_pid"),
  "rss_kib": h.get("rss_kib"),
  "uptime_secs": h.get("uptime_secs", d.get("uptime_secs", 0)),
  "heartbeat_age_secs": h.get("last_heartbeat_age_secs"),
  "heartbeat_lines": h.get("heartbeat_lines", 0),
  "consecutive_heartbeats": h.get("consecutive_heartbeats", 0),
  "log_gap_secs": h.get("log_gap_secs"),
  "source_total": sh.get("total", 0),
  "source_failing": sh.get("failing", 0),
  "source_cooling": sh.get("cooling", 0),
  "source_ok": bool(sh.get("ok", True)),
  "notify_pending": nh.get("pending", 0),
  "notify_failed": nh.get("failed", 0),
  "notify_latest_failed_channel": nh.get("latest_failed_channel"),
  "notify_hint": nh.get("hint"),
  "notify_ok": bool(nh.get("ok", True)),
}, ensure_ascii=False))' 2>/dev/null || true)"
if [ -z "$health" ]; then
  echo "$(date '+%Y-%m-%d %H:%M:%S %z') audit_unparseable" >> "$LOG"
  exit 0
fi

printf '%s %s\n' "$(date '+%Y-%m-%d %H:%M:%S %z')" "$health" >> "$LOG"
exit 0
