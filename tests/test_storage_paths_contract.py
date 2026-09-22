# -*- coding: utf-8 -*-
"""Python 与 Rust 的产物路径解析必须在同一份 config.ini 上保持一致。"""

import configparser
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import fupan.settings as settings


root = Path(tempfile.mkdtemp())
media_root = root / "外置产物"
(root / "config").mkdir(parents=True)
(root / "config" / "config.ini").write_text(
    "[录制设置]\n"
    f"直播保存路径(不填则默认) = {root / '自定义录像'}\n"
    "[路径设置]\n"
    f"产物根目录 = {media_root}\n"
    "数据库文件 =\n",
    encoding="utf-8",
)

original_project = settings.PROJECT_DIR
original_config = settings.CONFIG_DIR
try:
    settings.PROJECT_DIR = root
    settings.CONFIG_DIR = root / "config"
    paths = settings.storage_paths()
finally:
    settings.PROJECT_DIR = original_project
    settings.CONFIG_DIR = original_config

expected = media_root / "database" / "zunzhe-core.db"
assert paths["database"] == expected, (
    "数据库文件留空时必须跟随产物根，不能退回项目源码目录："
    f"expected={expected}, actual={paths['database']}"
)
expected_paths = {
    "media_root": media_root,
    "downloads": media_root / "downloads",
    "fupan_output": media_root / "fupan_output",
    "covers": media_root / "fupan_output" / "covers",
    "run": media_root / "fupan_output" / "run",
    "database": expected,
    "recordings": root / "自定义录像",
}
for name, expected_path in expected_paths.items():
    assert paths[name] == expected_path, (
        f"{name} 与产物根/录制路径规则不一致："
        f"expected={expected_path}, actual={paths[name]}"
    )
print("通过：Python 全部关键路径跟随产物根与录制保存路径")
