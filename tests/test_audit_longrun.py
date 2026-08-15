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


def run_verify(log: Path, doctor_pid: int | None = None):
    env = os.environ.copy()
    env["ZUNZHE_PROJECT_ROOT"] = str(ROOT)
    if doctor_pid is not None:
        env["ZUNZHE_VERIFY_DOCTOR_PID"] = str(doctor_pid)
    else:
        env.pop("ZUNZHE_VERIFY_DOCTOR_PID", None)
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
    rss_kib: int | None = None,
    disk_free_mb: int | None = None,
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
            "rss_kib": rss_kib,
            "disk_free_mb": disk_free_mb,
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
result = run_verify(log, doctor_pid=123)
assert result["ok"] is False, result
assert result["pid"] == 123, result
assert result["doctor_pid"] == 123, result
assert result["pid_aligned"] is True, result
assert result["running_secs"] < 24 * 3600 * 30, result
assert result["gap_ok"] is True, result

# 24×30 是“24 小时 × 30 天”，目标时长必须是 2,592,000 秒，而不是 43200 秒
# （后者只是 12 小时）。跑满 30 天且健康时 --verify 必须判成功。
full_30d = tmp / "full-30d.log"
with full_30d.open("w", encoding="utf-8") as f:
    # 真实 30 天每分钟一条心跳：首条在 30 天前，末条 60 秒前，共 43201 条。
    for i in range(24 * 60 * 30 + 1):
        f.write(audit_line(24 * 3600 * 30 - i * 60, 123) + "\n")
result = run_verify(full_30d, doctor_pid=123)
assert result["ok"] is True, result
assert result["target_secs"] == 24 * 3600 * 30, result
assert result["running_secs"] >= 24 * 3600 * 30, result

# 审计日志按 5MB 轮转会拆成 .1/.2/.3 + 当前文件；--verify 必须合并这些
# 轮转文件并按时间排序，否则同一 pid 的 30 天证据在轮转后会被误判为未满。
rot_current = tmp / "rot-30d.log"
rot_1 = tmp / "rot-30d.log.1"
rot_2 = tmp / "rot-30d.log.2"
rot_3 = tmp / "rot-30d.log.3"
chunk = 14_400  # 10 天每分钟一条
with rot_3.open("w", encoding="utf-8") as f:
    for i in range(chunk):
        f.write(audit_line(24 * 3600 * 30 + 120 - i * 60, 125) + "\n")
with rot_2.open("w", encoding="utf-8") as f:
    for i in range(chunk):
        f.write(audit_line(24 * 3600 * 20 - i * 60, 125) + "\n")
with rot_1.open("w", encoding="utf-8") as f:
    for i in range(chunk):
        f.write(audit_line(24 * 3600 * 10 - i * 60, 125) + "\n")
rot_current.write_text(audit_line(60, 125) + "\n", encoding="utf-8")
result = run_verify(rot_current, doctor_pid=125)
assert result["ok"] is True, result
assert result["pid"] == 125, result
assert result["running_secs"] >= 24 * 3600 * 30, result

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
result = run_verify(source_bad, doctor_pid=123)
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
result = run_verify(old_notify_bad, doctor_pid=123)
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
result = run_verify(recent_notify_bad, doctor_pid=123)
assert result["notify_ok"] is False, result

# 不同 pid 之间的间隔（正常热切换）不应让 gap_ok 永久失败；同一 pid 的
# 相邻记录超过 180 秒仍必须判断档。
pid_switch = tmp / "pid-switch.log"
pid_switch.write_text(
    audit_line(6000, 100, notify_ok=True)
    + "\n"
    + audit_line(120, 101, notify_ok=True)
    + "\n"
    + audit_line(60, 101, notify_ok=True)
    + "\n",
    encoding="utf-8",
)
result = run_verify(pid_switch, doctor_pid=101)
assert result["gap_ok"] is True, result
assert result["pid"] == 101, result

same_pid_gap = tmp / "same-pid-gap.log"
same_pid_gap.write_text(
    audit_line(400, 102, notify_ok=True)
    + "\n"
    + audit_line(60, 102, notify_ok=True)
    + "\n",
    encoding="utf-8",
)
result = run_verify(same_pid_gap, doctor_pid=102)
assert result["gap_ok"] is False, result

# RSS 增长超过阈值必须拦截验收，防止内存泄漏被“进程活着 + 心跳连续”掩盖。
rss_leak = tmp / "rss-leak.log"
rss_leak.write_text(
    audit_line(120, 124, rss_kib=20_000)
    + "\n"
    + audit_line(60, 124, rss_kib=20_000 + 300 * 1024)
    + "\n",
    encoding="utf-8",
)
result = run_verify(rss_leak, doctor_pid=124)
assert result["rss_ok"] is False, result
assert result["rss_growth_mib"] > 256, result

# RSS 正常波动不应误伤：增长小于阈值时 rss_ok 必须保持 true。
rss_ok_file = tmp / "rss-ok.log"
rss_ok_file.write_text(
    audit_line(120, 124, rss_kib=20_000)
    + "\n"
    + audit_line(60, 124, rss_kib=21_000)
    + "\n",
    encoding="utf-8",
)
result = run_verify(rss_ok_file, doctor_pid=124)
assert result["rss_ok"] is True, result

# 磁盘余量过低必须拦截验收，防止写满后被“进程活着”掩盖。
disk_low = tmp / "disk-low.log"
disk_low.write_text(
    audit_line(120, 124, disk_free_mb=300)
    + "\n"
    + audit_line(60, 124, disk_free_mb=200)
    + "\n",
    encoding="utf-8",
)
result = run_verify(disk_low, doctor_pid=124)
assert result["disk_ok"] is False, result
assert result["disk_free_mb"] == 200, result

# 磁盘余量充足时不能误伤验收。
disk_ok_file = tmp / "disk-ok.log"
disk_ok_file.write_text(
    audit_line(120, 124, disk_free_mb=1024)
    + "\n"
    + audit_line(60, 124, disk_free_mb=1020)
    + "\n",
    encoding="utf-8",
)
result = run_verify(disk_ok_file, doctor_pid=124)
assert result["disk_ok"] is True, result

# 审计日志 pid 与当前 doctor pid 不一致时，不能把旧实例的累计时长当成
# 当前实例已跑满：即使审计日志看起来满 30 天，也应判未通过。
stale_30d = tmp / "stale-30d.log"
with stale_30d.open("w", encoding="utf-8") as f:
    for i in range(24 * 60 * 30 + 1):
        f.write(audit_line(24 * 3600 * 30 - i * 60, 200) + "\n")
result = run_verify(stale_30d, doctor_pid=201)
assert result["ok"] is False, result
assert result["doctor_pid"] == 201, result
assert result["pid_aligned"] is False, result

# Windows 看门狗把 pid 写成字符串、doctor 返回整数；同实例的跨类型 pid
# 必须归一化后判对齐，不能误报 pid_aligned=false。
string_pid = tmp / "string-pid.log"
line = audit_line(
    60,
    81364,
    rss_kib=20000,
    disk_free_mb=1024,
)
line = line.replace('"pid": 81364', '"pid": "81364"', 1)
string_pid.write_text(line + "\n", encoding="utf-8")
result = run_verify(string_pid, doctor_pid=81364)
assert result["pid"] == 81364, result
assert result["doctor_pid"] == 81364, result
assert result["pid_aligned"] is True, result

print("通过：audit-longrun --verify 未满/满30天/跨轮转30天/缺失/源冷却/历史坏行/最近坏行/pid切换/同pid断档/RSS增长/RSS正常波动/磁盘不足/磁盘充足场景判定正确")
