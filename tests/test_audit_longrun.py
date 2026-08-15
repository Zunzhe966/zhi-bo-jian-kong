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


def audit_line(seconds_ago: int, pid: int, health: bool = True) -> str:
    ts = datetime.datetime.now().astimezone() - datetime.timedelta(seconds=seconds_ago)
    return f'{ts.strftime("%Y-%m-%d %H:%M:%S %z")} ' + json.dumps(
        {
            "health": health,
            "daemon_ok": health,
            "heartbeat_ok": health,
            "pid": pid,
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

print("通过：audit-longrun --verify 未满/缺失场景判定正确")
