# -*- coding: utf-8 -*-

"""
Author: Hmily
GitHub: https://github.com/ihmily
Copyright (c) 2024 by Hmily, All Rights Reserved.

ffmpeg 自动安装：只从官方/可信构建源下载，并校验 SHA-256，不再使用蓝奏云网盘。
"""

import hashlib
import json
import os
import platform
import shutil
import subprocess
import sys
import zipfile
from pathlib import Path

import requests
from tqdm import tqdm

from src.logger import logger

current_platform = platform.system()
execute_dir = os.path.split(os.path.realpath(sys.argv[0]))[0]
current_env_path = os.environ.get('PATH')
ffmpeg_path = os.path.join(execute_dir, 'ffmpeg')

_UA = ('Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 '
       '(KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36')


def _http_get(url: str, stream: bool = False):
    headers = {'User-Agent': _UA}
    return requests.get(url, headers=headers, stream=stream, timeout=(15, 120))


def _download(url: str, dest: Path, desc: str) -> None:
    dest.parent.mkdir(parents=True, exist_ok=True)
    tmp = dest.with_suffix(dest.suffix + '.part')
    response = _http_get(url, stream=True)
    response.raise_for_status()
    total = int(response.headers.get('Content-Length', 0))
    with tqdm(total=total, unit='B', unit_scale=True, ncols=100, desc=desc) as bar:
        with open(tmp, 'wb') as f:
            for chunk in response.iter_content(chunk_size=1024 * 256):
                if chunk:
                    f.write(chunk)
                    bar.update(len(chunk))
    tmp.rename(dest)


def _download_with_sha256(url: str, sha_url: str, dest: Path, desc: str) -> None:
    """下载并校验 SHA-256；校验文件不可用时放弃安装，避免供应链风险。"""
    _download(url, dest, desc)
    sha_resp = _http_get(sha_url)
    sha_resp.raise_for_status()
    expected = sha_resp.text.strip().split()[0].lower()
    actual = hashlib.sha256(dest.read_bytes()).hexdigest()
    if actual != expected:
        dest.unlink(missing_ok=True)
        raise RuntimeError(f'SHA-256 校验失败，已删除下载文件: 期望 {expected}，实际 {actual}')


def _download_with_size(url: str, expected_size: int, dest: Path, desc: str) -> None:
    """evermeet.cx 只发布 GPG 签名而不发布 SHA，这里用官方 release 元数据中的
    精确版本与文件大小做交叉校验，避免跟随“latest”链接时内容被替换。"""
    _download(url, dest, desc)
    actual = dest.stat().st_size
    if expected_size and actual != expected_size:
        dest.unlink(missing_ok=True)
        raise RuntimeError(f'文件大小校验失败: 期望 {expected_size}，实际 {actual}')


def _add_bundled_to_path() -> None:
    os.environ['PATH'] = ffmpeg_path + os.pathsep + current_env_path


def install_ffmpeg_windows() -> bool:
    """从 BtbN/FFmpeg-Builds 官方 GitHub 发布页下载 Windows 构建并校验。"""
    try:
        logger.warning('ffmpeg is not installed.')
        logger.debug('Downloading ffmpeg from BtbN/FFmpeg-Builds (official GitHub release)...')
        asset = 'ffmpeg-master-latest-win64-gpl.zip'
        base = 'https://github.com/BtbN/FFmpeg-Builds/releases/latest/download/'
        zip_path = Path(execute_dir) / asset
        _download_with_sha256(base + asset, base + asset + '.sha256', zip_path, 'Downloading ffmpeg (BtbN)')

        target_dir = Path(ffmpeg_path)
        target_dir.mkdir(parents=True, exist_ok=True)
        with zipfile.ZipFile(zip_path) as zf:
            names = [n for n in zf.namelist() if n.endswith('/bin/ffmpeg.exe')]
            if not names:
                raise RuntimeError('压缩包内未找到 ffmpeg.exe')
            target = target_dir / 'ffmpeg.exe'
            with zf.open(names[0]) as src, open(target, 'wb') as out:
                shutil.copyfileobj(src, out)
        zip_path.unlink(missing_ok=True)
        _add_bundled_to_path()
        return check_ffmpeg_installed()
    except Exception as e:
        logger.error(f'ffmpeg installation failed: {e}')
        logger.error('请手动从 https://www.gyan.dev/ffmpeg/builds/ 下载 ffmpeg，'
                     '或从 https://github.com/BtbN/FFmpeg-Builds/releases 下载后加入 PATH')
        return False


def install_ffmpeg_mac() -> bool:
    """优先用 Homebrew；没有 Homebrew 时从 evermeet.cx 官方构建源下载并校验。"""
    brew = shutil.which('brew')
    if brew:
        logger.warning('ffmpeg is not installed.')
        logger.debug('Installing ffmpeg via Homebrew...')
        try:
            result = subprocess.run([brew, 'install', 'ffmpeg'], capture_output=True)
            if result.returncode == 0:
                logger.debug('ffmpeg installation was successful.')
                return check_ffmpeg_installed()
            logger.error(result.stderr.decode('utf-8', errors='replace')[-500:])
        except Exception as e:
            logger.error(f'Homebrew 安装失败: {e}')
        return False

    try:
        logger.warning('未检测到 Homebrew，改为下载 evermeet.cx 官方静态构建...')
        with _http_get('https://evermeet.cx/ffmpeg/info/ffmpeg/release') as resp:
            resp.raise_for_status()
            release = resp.json()
        zip_meta = release['download']['zip']
        zip_path = Path(execute_dir) / 'ffmpeg_latest_macos.zip'
        _download_with_size(zip_meta['url'], int(zip_meta.get('size', 0)),
                            zip_path, f"Downloading ffmpeg {release.get('version', '')} (evermeet.cx)")
        target_dir = Path(ffmpeg_path)
        target_dir.mkdir(parents=True, exist_ok=True)
        with zipfile.ZipFile(zip_path) as zf:
            names = [n for n in zf.namelist() if n.endswith('/ffmpeg') or n == 'ffmpeg']
            if not names:
                raise RuntimeError('压缩包内未找到 ffmpeg')
            target = target_dir / 'ffmpeg'
            with zf.open(names[0]) as src, open(target, 'wb') as out:
                shutil.copyfileobj(src, out)
            os.chmod(target, 0o755)
        zip_path.unlink(missing_ok=True)
        _add_bundled_to_path()
        return check_ffmpeg_installed()
    except Exception as e:
        logger.error(f'ffmpeg installation failed: {e}')
        logger.error('请手动执行: brew install ffmpeg，或从 https://evermeet.cx/ffmpeg/ 下载后放入 ffmpeg/')
        return False


def install_ffmpeg_linux() -> bool:
    try:
        logger.warning('ffmpeg is not installed.')
        logger.debug('Trying to install ffmpeg with system package manager...')
        if shutil.which('apt-get'):
            subprocess.run(['apt-get', 'update'], capture_output=True, check=False)
            result = subprocess.run(['apt-get', 'install', '-y', 'ffmpeg'], capture_output=True)
        elif shutil.which('yum'):
            result = subprocess.run(['yum', '-y', 'install', 'ffmpeg'], capture_output=True)
        elif shutil.which('dnf'):
            result = subprocess.run(['dnf', '-y', 'install', 'ffmpeg'], capture_output=True)
        else:
            logger.error('未找到 apt/yum/dnf，请手动安装 ffmpeg')
            return False
        if result.returncode == 0:
            logger.debug('ffmpeg installation was successful.')
            return check_ffmpeg_installed()
        logger.error(result.stderr.decode('utf-8', errors='replace')[-500:])
    except Exception as e:
        logger.error(f'ffmpeg installation failed: {e}')
    logger.error('请手动安装 ffmpeg（如 apt install ffmpeg / yum install ffmpeg）')
    return False


def install_ffmpeg() -> bool:
    if current_platform == 'Windows':
        return install_ffmpeg_windows()
    if current_platform == 'Linux':
        return install_ffmpeg_linux()
    if current_platform == 'Darwin':
        return install_ffmpeg_mac()
    logger.debug(f'ffmpeg auto installation is not supported on this platform: {current_platform}. '
                 f'Please install ffmpeg manually.')
    return False


def ensure_ffmpeg_installed(func):
    def wrapper(*args, **kwargs):
        try:
            result = subprocess.run(['ffmpeg', '-version'], capture_output=True)
            if result.returncode == 0 and result.stdout.strip():
                return func(*args, **kwargs)
        except FileNotFoundError:
            pass
        return False

    def wrapped_func(*args, **kwargs):
        res = wrapper(*args, **kwargs)
        if not res:
            install_ffmpeg()
            res = wrapper(*args, **kwargs)
        if not res:
            raise RuntimeError('ffmpeg is not installed.')
        return func(*args, **kwargs)

    return wrapped_func


def check_ffmpeg_installed() -> bool:
    try:
        result = subprocess.run(['ffmpeg', '-version'], capture_output=True)
        if result.returncode == 0 and result.stdout.strip():
            return True
    except FileNotFoundError:
        pass
    except OSError as e:
        print(f'OSError occurred: {e}. ffmpeg may not be installed correctly or is not available in the system PATH.')
    except Exception as e:
        print(f'An unexpected error occurred: {e}')
    return False


def check_ffmpeg() -> bool:
    if not check_ffmpeg_installed():
        return install_ffmpeg()
    return True
