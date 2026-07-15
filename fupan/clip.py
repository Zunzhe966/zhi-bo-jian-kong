# -*- coding: utf-8 -*-
"""根据知识点时间戳，用 ffmpeg 无损切出精华片段。"""
import re
import subprocess
from pathlib import Path

from .settings import fupan_config, get, is_yes


def _safe_name(text: str) -> str:
    return re.sub(r'[\\/:*?"<>|\s]+', '_', text)[:50]


def cut_clips(media_path: str, points: list[dict], output_dir: str) -> list[str]:
    """按知识点切片，返回生成的文件列表。"""
    cfg = fupan_config()
    if not is_yes(get(cfg, '切片', '是否启用切片(是/否)', '是')):
        return []

    max_clips = int(get(cfg, '切片', '最多片段数', '8') or 8)
    padding = float(get(cfg, '切片', '片段缓冲秒', '3') or 3)

    clip_dir = Path(output_dir) / '精华切片'
    clip_dir.mkdir(parents=True, exist_ok=True)
    suffix = Path(media_path).suffix or '.mp4'

    created = []
    for idx, point in enumerate(points[:max_clips], start=1):
        try:
            start = max(0.0, float(point['开始秒']) - padding)
            end = float(point['结束秒']) + padding
        except (KeyError, TypeError, ValueError):
            continue
        if end - start < 5:
            end = start + 5

        out_file = clip_dir / f'{idx:02d}_{_safe_name(str(point.get("标题", "片段")))}{suffix}'
        cmd = [
            'ffmpeg', '-y', '-loglevel', 'error',
            '-ss', f'{start:.2f}', '-to', f'{end:.2f}',
            '-i', str(media_path),
            '-c', 'copy',
            str(out_file),
        ]
        result = subprocess.run(cmd, capture_output=True)
        if result.returncode == 0 and out_file.exists():
            created.append(str(out_file))
    return created
