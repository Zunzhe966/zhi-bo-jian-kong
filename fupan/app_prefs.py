# -*- coding: utf-8 -*-
"""桌面应用偏好：守护、开机自启、向导完成标记等。"""
from __future__ import annotations

import json
from pathlib import Path

from .settings import CONFIG_DIR

PREFS_FILE = CONFIG_DIR / '桌面偏好.json'

DEFAULTS = {
    'watchdog': True,
    'autoStartServices': True,
    'openAtLogin': False,
    'wizardDone': False,
    'localNotify': True,
}


def _load() -> dict:
    data = dict(DEFAULTS)
    if PREFS_FILE.exists():
        try:
            raw = json.loads(PREFS_FILE.read_text(encoding='utf-8'))
            if isinstance(raw, dict):
                data.update(raw)
        except json.JSONDecodeError:
            pass
    return data


def _save(data: dict) -> None:
    PREFS_FILE.parent.mkdir(parents=True, exist_ok=True)
    PREFS_FILE.write_text(json.dumps(data, ensure_ascii=False, indent=2), encoding='utf-8')


def get_prefs() -> dict:
    return _load()


def set_prefs(**kwargs) -> dict:
    data = _load()
    for k, v in kwargs.items():
        if k in DEFAULTS:
            data[k] = v
    _save(data)
    return data


def mark_wizard_done() -> dict:
    return set_prefs(wizardDone=True)
