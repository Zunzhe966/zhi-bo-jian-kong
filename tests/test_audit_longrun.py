# -*- coding: utf-8 -*-
"""验证 24×30 长跑审计的 --verify 判定逻辑，不依赖真实 daemon。"""
import datetime
import json
import os
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "audit-longrun.sh"


def run_verify(log: Path):
    env = os.environ.copy()
    env["ZUNZHE_PROJECT_ROOT"] = str(ROOT)
    out = subprocess.run(
        ["bash", str(SCRIPT), "--verify", str(log)],
        capture_output=True,
        text=True,
        timeout=20,
        env=env,
        check=True,
    ).stdout.strip()
    return json.loads(out)


def audit_line(
    seconds_ago: int,
    pid: int,
    health: bool = True,
    *,
    source_ok: bool = True,
    notify_ok: bool = True,
) -> str:
    ts = datetime.datetime.now().astimezone() - datetime.timedelta(seconds=seconds_ago)
    return f'{ts.strftime("%Y-%m-%d %H:%M:%S %z")} ' + json.dumps(
        {
            "health": health,
            "daemon_ok": health,
            "heartbeat_ok": health,
            "pid": pid,
            "source_ok": source_ok,
            "source_cooling": 0,
            "source_failing": 0,
            "notify_ok": notify_ok,
            "notify_pending": 0,
            "notify_failed": 0,
        },
        ensure_ascii=False,
    )


tmp = Path(tempfile.mkdtemp())
log = tmp / "audit.log"

# 未满 24×30：两条同 pid 记录，间隔 60 秒，时长远小于目标。
log.write_text(
    audit_line(120, 123) + "\n" + audit_line(60, 123) + "\n",
    encoding="utf-8",
)
result = run_verify(log)
assert result["ok"] is False, result
assert result["pid"] == 123, result
assert result["running_secs"] < 24 * 30 * 60, result
assert result["gap_ok"] is True, result

# 审计日志缺失：返回 ok=false 而不是崩溃。
missing = run_verify(tmp / "missing.log")
assert missing["ok"] is False and "error" in missing, missing

# 源进入冷却（source_ok=false）时，即使 pid/心跳/断档都正常，也不能判验收通过。
source_bad = tmp / "source-bad.log"
source_bad.write_text(
    audit_line(120, 123) + "\n" + audit_line(60, 123) + "\n",
    encoding="utf-8",
)
source_bad.write_text(
    source_bad.read_text(encoding="utf-8").replace(
        '"source_ok": true', '"source_ok": false', 1
    ),
    encoding="utf-8",
)
result = run_verify(source_bad)
assert result["ok"] is False, result
assert result["source_ok"] is False, result
assert result["source_cooling"] == 0, result

# 历史坏行（已修复/清理）不应永久卡住验收：坏行滚出最近 30 分钟窗口后，
# 即使审计文件里仍保留旧记录，notify_ok 也应恢复为 true。
old_notify_bad = tmp / "old-notify-bad.log"
old_notify_bad.write_text(
    audit_line(60 * 60 * 2, 123, notify_ok=False)
    + "\n"
    + audit_line(60 * 60, 123, notify_ok=True)
    + "\n",
    encoding="utf-8",
)
result = run_verify(old_notify_bad)
assert result["ok"] is False, result  # 仍未跑满 24×30，但健康维度应通过
assert result["notify_ok"] is True, result

# 最近 30 分钟内的通知失败仍然不能判健康，防止长期卡住的问题变成“永久掩盖”。
recent_notify_bad = tmp / "recent-notify-bad.log"
recent_notify_bad.write_text(
    audit_line(120, 123, notify_ok=True)
    + "\n"
    + audit_line(60, 123, notify_ok=False)
    + "\n",
    encoding="utf-8",
)
result = run_verify(recent_notify_bad)
assert result["notify_ok"] is False, result

print("通过：audit-longrun --verify 未满/缺失/源冷却/历史坏行/最近坏行场景判定正确")
