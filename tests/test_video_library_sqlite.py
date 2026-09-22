# -*- coding: utf-8 -*-
"""验证旧 Python 视频库的连接参数：多人/多进程访问时不能沿用短锁
默认值，必须与 Rust 主库一样显式等待并启用 WAL，避免下载/列表互相
把对方判成 `database is locked`。
"""
import sqlite3
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import fupan.video_library as vl


root = Path(tempfile.mkdtemp())
vl.DB_PATH = root / "video_library.db"

conn = vl._connect()
try:
    busy_timeout = conn.execute("PRAGMA busy_timeout").fetchone()[0]
    journal_mode = conn.execute("PRAGMA journal_mode").fetchone()[0]
finally:
    conn.close()

assert busy_timeout == 15_000, f"busy_timeout 应为 15 秒，实际 {busy_timeout}ms"
assert journal_mode.lower() == "wal", f"journal_mode 应为 wal，实际 {journal_mode}"

print("通过：Python 视频库显式启用 15 秒 busy_timeout 与 WAL")
