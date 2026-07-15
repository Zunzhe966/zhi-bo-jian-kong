# -*- coding: utf-8 -*-
"""One-click environment check before first run."""
from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

PROJECT_DIR = Path(__file__).resolve().parent.parent
VENV_PY = PROJECT_DIR / '.venv' / 'Scripts' / 'python.exe'
REQ = PROJECT_DIR / 'requirements-fupan.txt'


def main() -> int:
    issues = []
    if not VENV_PY.exists():
        issues.append('缺少 Python 虚拟环境 .venv，请在项目根目录运行: python -m venv .venv')
    elif not shutil.which(str(VENV_PY)):
        issues.append(f'Python 不可用: {VENV_PY}')
    if not (PROJECT_DIR / 'main.py').exists():
        issues.append('缺少 main.py')
    if not (PROJECT_DIR / 'config' / 'config.ini').exists():
        issues.append('缺少 config/config.ini')
    ffmpeg = shutil.which('ffmpeg')
    if not ffmpeg:
        issues.append('未检测到 ffmpeg（录制/合并视频需要，可从 ffmpeg.org 安装并加入 PATH）')
    if issues:
        print('环境检查未通过:')
        for i in issues:
            print(' -', i)
        return 1
    print('环境检查通过')
    return 0


if __name__ == '__main__':
    sys.exit(main())
