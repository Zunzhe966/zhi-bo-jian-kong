# -*- coding: utf-8 -*-
"""每个监控项的独立偏好（通知通道等），按 URL 存储，不破坏 main.py 的 URL_config 格式。"""
from __future__ import annotations

import json
from pathlib import Path

from .settings import CONFIG_DIR

PREFS_FILE = CONFIG_DIR / '监控偏好.json'
# 通知=跟随全局 | 微信 | 邮箱 | 不通知
NOTIFY_OPTIONS = ('跟随全局', '微信', '邮箱', '不通知')


def _load() -> dict:
    if PREFS_FILE.exists():
        try:
            return json.loads(PREFS_FILE.read_text(encoding='utf-8'))
        except json.JSONDecodeError:
            pass
    return {}


def _save(data: dict) -> None:
    PREFS_FILE.parent.mkdir(parents=True, exist_ok=True)
    PREFS_FILE.write_text(json.dumps(data, ensure_ascii=False, indent=2), encoding='utf-8')


def get_notify(url: str, name: str = '') -> str:
    data = _load()
    entry = data.get(url.strip(), {})
    if isinstance(entry, dict) and entry.get('notify'):
        return entry['notify']
    if name:
        for u, v in data.items():
            if isinstance(v, dict) and v.get('name') == name and v.get('notify'):
                return v['notify']
    return '跟随全局'


def set_notify(url: str, channel: str, name: str = '') -> dict:
    channel = channel.strip() or '跟随全局'
    data = _load()
    if channel == '跟随全局':
        data.pop(url.strip(), None)
    elif channel not in NOTIFY_OPTIONS:
        data.pop(url.strip(), None)
    else:
        data[url.strip()] = {'notify': channel, 'name': name or data.get(url.strip(), {}).get('name', '')}
    _save(data)
    return {'url': url, 'notify': channel, 'name': name}


def delete_prefs(url: str) -> None:
    data = _load()
    data.pop(url.strip(), None)
    _save(data)


def attach_to_items(items: list[dict], url_key: str = 'url', name_key: str = 'name') -> list[dict]:
    """给 list_live / list_video 结果附加 notify 字段。"""
    out = []
    for it in items:
        row = dict(it)
        row['notify'] = get_notify(row.get(url_key, ''), row.get(name_key, '') or row.get('remark', ''))
        out.append(row)
    return out
