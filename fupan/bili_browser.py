# -*- coding: utf-8 -*-
"""用 Electron 内置浏览器内核下载 B站视频，绕过 yt-dlp 遇到的 412 风控。

原理与浏览器"下载插件"相同：真实浏览器内核加载视频页、跑完风控 JS，
从网页内嵌的 window.__playinfo__ 拿到 DASH 直链，再用 ffmpeg 下载+合流。
"""
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

from .settings import PROJECT_DIR, RUN_DIR as _PATHS_RUN_DIR

def _electron_binary() -> str:
    """返回 Electron 可执行文件（跨 Windows/macOS/Linux）。"""
    base = PROJECT_DIR / 'desktop' / 'node_modules' / 'electron' / 'dist'
    if sys.platform == 'win32':
        candidates = [base / 'electron.exe']
    elif sys.platform == 'darwin':
        candidates = [
            base / 'Electron.app' / 'Contents' / 'MacOS' / 'Electron',
            base / 'electron',
        ]
    else:
        candidates = [base / 'electron']
    for candidate in candidates:
        if candidate.exists():
            return str(candidate)
    return shutil.which('electron') or str(candidates[0])


ELECTRON = _electron_binary()
EXTRACT_SCRIPT = PROJECT_DIR / 'desktop' / 'electron' / 'extract-run.cjs'
RUN_DIR = _PATHS_RUN_DIR
UA = ('Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 '
      '(KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36')


def is_bilibili(url: str) -> bool:
    return 'bilibili.com' in url or 'b23.tv' in url


def _ffmpeg() -> str:
    if sys.platform == 'win32':
        bundled = PROJECT_DIR / 'ffmpeg' / 'ffmpeg.exe'
    else:
        bundled = PROJECT_DIR / 'ffmpeg' / 'ffmpeg'
    if bundled.exists():
        return str(bundled)
    return shutil.which('ffmpeg') or 'ffmpeg'


def extract_streams(video_url: str, timeout: int = 70) -> dict:
    """驱动 Electron 提取 B站视频的 DASH 直链。返回 {ok,title,video,audio}。"""
    if not Path(ELECTRON).exists():
        return {'ok': False, 'error': f'未找到 Electron，请先启动过桌面程序：{ELECTRON}'}
    RUN_DIR.mkdir(parents=True, exist_ok=True)
    out_file = RUN_DIR / f'bili_streams_{int(time.time()*1000)}.json'
    out_file.unlink(missing_ok=True)
    env = {**os.environ, 'FUPAN_VIDEO_URL': video_url, 'FUPAN_OUT_FILE': str(out_file)}
    try:
        subprocess.run([str(ELECTRON), str(EXTRACT_SCRIPT)], env=env,
                       capture_output=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return {'ok': False, 'error': '浏览器提取超时'}
    if not out_file.exists():
        return {'ok': False, 'error': '提取器未产出结果（Electron 可能启动失败）'}
    try:
        data = json.loads(out_file.read_text(encoding='utf-8'))
    finally:
        out_file.unlink(missing_ok=True)
    return data


def download(video_url: str, save_dir: Path, filename_prefix: str = '') -> Path | None:
    """下载 B站视频到 save_dir，返回最终 mp4 路径；失败返回 None。"""
    info = extract_streams(video_url)
    if not info.get('ok'):
        print(f'B站浏览器提取失败: {info.get("error")}')
        return None
    save_dir.mkdir(parents=True, exist_ok=True)
    safe_title = ''.join(c for c in (info.get('title') or 'bilibili') if c not in r'\/:*?"<>|')[:60]
    prefix = f'{filename_prefix}_' if filename_prefix else ''
    out = save_dir / f'{prefix}{safe_title}.mp4'
    headers = f'Referer: https://www.bilibili.com/\r\nUser-Agent: {UA}'
    ff = _ffmpeg()
    if info.get('audio'):
        cmd = [ff, '-y', '-headers', headers, '-i', info['video'],
               '-headers', headers, '-i', info['audio'],
               '-map', '0:v', '-map', '1:a', '-c', 'copy', str(out)]
    else:
        cmd = [ff, '-y', '-headers', headers, '-i', info['video'], '-c', 'copy', str(out)]
    print(f'开始下载(浏览器直链): {safe_title}')
    result = subprocess.run(cmd, capture_output=True, text=True,
                            encoding='utf-8', errors='replace')
    if result.returncode != 0 or not out.exists():
        print(f'ffmpeg 下载失败: {(result.stderr or "")[-400:]}')
        return None
    print(f'已保存: {out}')
    return out
