# -*- coding: utf-8 -*-
"""用 faster-whisper 把录制的音视频转成带时间戳的文字。"""
import json
import os
from pathlib import Path

from .settings import fupan_config, get, is_yes


def _format_ts(seconds: float) -> str:
    h, rem = divmod(int(seconds), 3600)
    m, s = divmod(rem, 60)
    return f'{h:02d}:{m:02d}:{s:02d}'


def transcribe_file(media_path: str, output_dir: str) -> dict | None:
    """转写单个文件，输出 转写.json / 转写.txt，返回 segments 数据。"""
    cfg = fupan_config()
    if not is_yes(get(cfg, '转写', '是否启用转写(是/否)', '是')):
        return None

    mirror = get(cfg, '转写', 'hf镜像')
    if mirror:
        os.environ.setdefault('HF_ENDPOINT', mirror)

    from faster_whisper import WhisperModel

    model_name = get(cfg, '转写', 'whisper模型', 'small')
    device = get(cfg, '转写', '运行设备', 'auto')
    language = get(cfg, '转写', '语言', 'zh') or None

    compute_type = 'int8' if device in ('auto', 'cpu') else 'float16'
    try:
        model = WhisperModel(model_name, device=device, compute_type=compute_type)
    except Exception:
        model = WhisperModel(model_name, device='cpu', compute_type='int8')

    segments_iter, info = model.transcribe(
        media_path, language=language, vad_filter=True,
        initial_prompt='以下是普通话的简体中文内容。' if (language or '').startswith('zh') else None)

    segments = []
    for seg in segments_iter:
        text = seg.text.strip()
        if text:
            segments.append({'start': round(seg.start, 2), 'end': round(seg.end, 2), 'text': text})

    out_dir = Path(output_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    stem = Path(media_path).stem

    result = {
        'source': str(media_path),
        'language': info.language,
        'duration': round(info.duration, 2),
        'segments': segments,
    }
    (out_dir / f'{stem}.转写.json').write_text(
        json.dumps(result, ensure_ascii=False, indent=2), encoding='utf-8')

    lines = [f'[{_format_ts(s["start"])} - {_format_ts(s["end"])}] {s["text"]}' for s in segments]
    (out_dir / f'{stem}.转写.txt').write_text('\n'.join(lines), encoding='utf-8')
    return result
