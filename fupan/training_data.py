# -*- coding: utf-8 -*-
"""训练数据沉淀：把每场复盘的转写 + 知识点合并导出为 JSONL。

每行一条记录，统一带 anchor（主播）、session（场次）、date、时间戳等元数据，
便于后续训练/微调/检索直接消费：
  {"type": "knowledge", ...}  知识提炼产出的要点
  {"type": "transcript", ...} 转写片段（无知识点的场次也有原始语料）
"""
from __future__ import annotations

import json
import time
from pathlib import Path

from .settings import FUPAN_DIR

DEFAULT_OUTPUT = FUPAN_DIR / 'training_data.jsonl'
_SKIP_DIRS = {'run', 'logs', 'covers', 'training_data'}


def _fmt_date(ts: float) -> str:
    return time.strftime('%Y-%m-%d', time.localtime(ts))


def _read_json(path: Path) -> dict | list | None:
    try:
        data = json.loads(path.read_text(encoding='utf-8'))
    except (OSError, ValueError):
        return None
    return data if isinstance(data, (dict, list)) else None


def export_training_data(anchor: str = '', output: str = '') -> dict:
    """扫描 fupan_output/<主播>/<场次>/，把转写与知识点导出为 JSONL。"""
    out_path = Path(output) if output else DEFAULT_OUTPUT
    out_path.parent.mkdir(parents=True, exist_ok=True)

    records: list[dict] = []
    sessions = 0
    knowledge = 0
    transcript_segments = 0
    errors: list[str] = []

    if FUPAN_DIR.exists():
        for anchor_dir in sorted(p for p in FUPAN_DIR.iterdir() if p.is_dir()):
            if anchor_dir.name in _SKIP_DIRS:
                continue
            if anchor and anchor_dir.name != anchor:
                continue
            for session_dir in sorted(p for p in anchor_dir.iterdir() if p.is_dir()):
                if session_dir.name == '视频下载':
                    continue
                points_file = next(session_dir.glob('*.知识点.json'), None)
                transcript_file = next(session_dir.glob('*.转写.json'), None)
                if not points_file and not transcript_file:
                    continue
                sessions += 1
                date = _fmt_date(session_dir.stat().st_mtime)
                base = {
                    'anchor': anchor_dir.name,
                    'session': session_dir.name,
                    'date': date,
                    'output_dir': str(session_dir),
                }

                points = _read_json(points_file) if points_file else None
                if isinstance(points, list):
                    for p in points:
                        if not isinstance(p, dict) or not p.get('标题'):
                            continue
                        knowledge += 1
                        records.append({
                            **base,
                            'type': 'knowledge',
                            'title': p.get('标题', ''),
                            'content': p.get('内容', ''),
                            'start_sec': p.get('开始秒', 0),
                            'end_sec': p.get('结束秒', 0),
                            'value': int(p.get('价值', 3) or 3),
                        })

                transcript = _read_json(transcript_file) if transcript_file else None
                if isinstance(transcript, dict):
                    source = transcript.get('source', '')
                    for seg in transcript.get('segments', []) or []:
                        if not isinstance(seg, dict) or not seg.get('text'):
                            continue
                        transcript_segments += 1
                        records.append({
                            **base,
                            'type': 'transcript',
                            'source': source,
                            'language': transcript.get('language', ''),
                            'text': seg.get('text', ''),
                            'start_sec': seg.get('start', 0),
                            'end_sec': seg.get('end', 0),
                        })

                if points_file and not points:
                    errors.append(f'{points_file}: 知识点文件无法解析或为空')

    with out_path.open('w', encoding='utf-8') as f:
        for record in records:
            f.write(json.dumps(record, ensure_ascii=False) + '\n')

    return {
        'ok': True,
        'output': str(out_path),
        'records': len(records),
        'sessions': sessions,
        'knowledge': knowledge,
        'transcript_segments': transcript_segments,
        'errors': errors,
    }
