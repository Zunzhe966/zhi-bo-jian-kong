# -*- coding: utf-8 -*-
"""模拟"博主发新视频"场景，验证 发现->通知->按开关下载 的完整逻辑（不碰真实状态文件）。"""
import json
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import fupan.video_monitor as vm

calls = {'alert': [], 'download': []}

# 用临时状态文件，避免污染真实监控状态
tmp_state = Path(tempfile.mkdtemp()) / 'state.json'
vm.STATE_FILE = tmp_state

# 伪造视频源：老视频 v1，之后发布新视频 v2
fake_videos = [{'id': 'v1', 'title': '老视频', 'url': 'https://example.com/v1'}]
vm._fetch_videos = lambda url, cookie: list(fake_videos)
vm.send_new_video_alert = lambda name, title, url: calls['alert'].append((name, title))

def fake_download(video, name):
    calls['download'].append((name, video['title']))
    return None
vm._download_and_process = fake_download

watch = [
    {'url': 'https://example.com/a', 'name': '博主A_要下载', 'download': True},
    {'url': 'https://example.com/b', 'name': '博主B_只通知', 'download': False},
]
vm._load_watchlist = lambda: watch

print('== 第1轮：首次运行，只记基线 ==')
n = vm.check_once()
assert n == 0, f'首轮应为0，实际{n}'
print('通过：首轮记录基线，不通知不下载')

print('== 第2轮：两位博主各发布1条新视频 ==')
fake_videos.insert(0, {'id': 'v2', 'title': '新视频来了', 'url': 'https://example.com/v2'})
n = vm.check_once()
assert n == 2, f'应发现2条新视频，实际{n}'
assert len(calls['alert']) == 2, f'两位博主都应收到通知，实际{calls["alert"]}'
assert calls['download'] == [('博主A_要下载', '新视频来了')], f'只有博主A应触发下载，实际{calls["download"]}'
print('通过：都发了邮件通知；只有"下载=是"的博主A触发了下载')

print('== 第3轮：无新视频 ==')
n = vm.check_once()
assert n == 0 and len(calls['alert']) == 2
print('通过：无新视频时不重复通知')

print()
print('全部通过：新视频检测 / 邮件通知 / 每条监控独立下载开关 逻辑正确')
