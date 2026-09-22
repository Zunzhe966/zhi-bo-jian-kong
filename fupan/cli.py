# -*- coding: utf-8 -*-
"""尊者直播监控 — 智能体/自动化控制入口。每个子命令输出一行 JSON。

用法（在项目根目录）：
  .venv/bin/python -m fupan.cli <子命令> [参数]        # macOS/Linux
  .venv/Scripts/python.exe -m fupan.cli <子命令> [参数]  # Windows

子命令：
  status | add-live | remove-live | list-live | check-live
  add-video <地址> <名称> [是/否] | remove-video | list-video | check-videos
  download-video <视频链接> [名称]   立即下载单个视频并跑复盘
  blogger-videos <博主地址> [数量]   列出博主历史视频供批量勾选下载
  export-training-data [主播名] [输出文件]
                                  把转写+知识点导出为 JSONL 训练数据
  email-status                  查看邮箱配置状态（不显示授权码）
  email-setup <邮箱> <授权码> [收件人]
                                  写入 QQ 邮箱配置并启用邮件通知
  email-import-legacy           尝试从老项目 zunzhe-dev 迁移 QQ 邮箱账号
  start | stop | process | notes | test-email
  get-config | set-config | set-config-bulk | schema
  mcp-info                      打印 MCP 接入说明

MCP（推荐智能体连接方式）：
  .venv/bin/python -m fupan.mcp_server                 # macOS/Linux
  .venv/Scripts/python.exe -m fupan.mcp_server         # Windows
  配置见 mcp.json.example
"""
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

from .config_api import config_schema, get_config, set_config, set_config_bulk
from .app_prefs import get_prefs, set_prefs, mark_wizard_done
from .monitor_prefs import attach_to_items, delete_prefs, set_notify, get_notify, NOTIFY_OPTIONS
from .settings import (CONFIG_DIR, DOWNLOADS_DIR, FUPAN_DIR, PROJECT_DIR, RECORDINGS_DIR,
                       RUN_DIR as PATHS_RUN_DIR, ensure_project_on_path, storage_paths,
                       venv_python)
from .video_library import DB_PATH, init_db, get_videos, cache_cover, mark_downloaded

ensure_project_on_path()

RUN_DIR = PATHS_RUN_DIR
LOG_DIR = FUPAN_DIR / 'logs'
URL_CONFIG = CONFIG_DIR / 'URL_config.ini'
WATCHLIST = CONFIG_DIR / '视频监控列表.ini'
ENCODING = 'utf-8-sig'
PYTHON = str(venv_python())
QUALITIES = ('原画', '蓝光', '超清', '高清', '标清', '流畅')


def _out(data: dict) -> None:
    if 'ok' not in data:
        data = {**data, 'ok': 'error' not in data}
    print(json.dumps(data, ensure_ascii=False))


def _read_lines(path: Path) -> list[str]:
    return path.read_text(encoding=ENCODING).splitlines() if path.exists() else []


def _write_lines(path: Path, lines: list[str]) -> None:
    path.write_text('\n'.join(lines) + '\n', encoding=ENCODING)


def _contains_url(text: str) -> bool:
    t = text.strip().lower()
    return '://' in t or t.startswith('www.')


# ---------- 直播间监控列表 ----------

def _parse_live_line(line: str) -> dict:
    """解析 URL_config.ini 一行，格式与 main.py 一致。"""
    stripped = line.strip()
    enabled = not stripped.startswith('#')
    clean = stripped.lstrip('#').strip()
    parts = re.split(r'[,，]', clean) if re.search(r'[,，]', clean) else [clean]

    quality, url, remark = '', '', ''
    if len(parts) == 1:
        url = parts[0].strip()
    elif len(parts) == 2:
        if _contains_url(parts[0]):
            url, remark = parts[0].strip(), parts[1].strip()
        else:
            quality, url = parts[0].strip(), parts[1].strip()
    else:
        quality, url, remark = parts[0].strip(), parts[1].strip(), parts[2].strip()

    if quality and quality not in QUALITIES:
        if _contains_url(parts[0]):
            url = parts[0].strip()
            remark = ','.join(parts[1:]).strip()
            quality = ''
        else:
            quality = ''

    if url and '://' not in url:
        url = 'https://' + url

    return {'url': url, 'remark': remark, 'quality': quality or None, 'enabled': enabled}


def _format_live_line(url: str, remark: str = '', quality: str = '') -> str:
    q = quality if quality in QUALITIES else ''
    if q and remark:
        return f'{q},{url},{remark}'
    if q:
        return f'{q},{url}'
    if remark:
        return f'{url},{remark}'
    return url


def list_live() -> list[dict]:
    rooms = []
    for line in _read_lines(URL_CONFIG):
        stripped = line.strip()
        if not stripped or stripped.startswith('#'):
            continue
        parsed = _parse_live_line(line)
        if parsed['url'].startswith('http'):
            rooms.append(parsed)
    return attach_to_items(rooms, name_key='remark')


def add_live(url: str, remark: str = '', quality: str = '', notify: str = '') -> dict:
    line = _format_live_line(url, remark, quality)
    lines = _read_lines(URL_CONFIG)
    for i, line_old in enumerate(lines):
        existing = _parse_live_line(line_old)
        if existing['url'] == url or existing['url'].rstrip('/') == url.rstrip('/'):
            was_disabled = line_old.strip().startswith('#')
            lines[i] = line
            _write_lines(URL_CONFIG, lines)
            if notify:
                set_notify(url, notify, remark)
            return {'ok': True, 'action': '已重新启用' if was_disabled else '已更新',
                    'url': url, 'remark': remark, 'quality': quality or None,
                    'notify': notify or get_notify(url, remark),
                    'hint': '主程序运行中会在下一轮循环自动生效'}
    lines.append(line)
    _write_lines(URL_CONFIG, lines)
    if notify:
        set_notify(url, notify, remark)
    return {'ok': True, 'action': '已添加', 'url': url, 'remark': remark, 'quality': quality or None,
            'notify': notify or get_notify(url, remark),
            'hint': '主程序运行中会在下一轮循环自动生效，无需重启'}


def remove_live(url: str) -> dict:
    """暂停监控（注释掉，保留在列表）。"""
    lines = _read_lines(URL_CONFIG)
    changed = False
    for i, line in enumerate(lines):
        stripped = line.strip()
        if not stripped:
            continue
        existing = _parse_live_line(stripped)
        if existing['url'] == url and not stripped.startswith('#'):
            lines[i] = '#' + stripped
            changed = True
    if changed:
        _write_lines(URL_CONFIG, lines)
        return {'ok': True, 'action': '已暂停监控', 'url': url,
                'hint': '正在进行的录制会在几秒内停止；要从列表删除请用 delete-live'}
    return {'ok': False, 'error': '监控列表中没有这个地址', 'url': url}


def delete_live(url: str) -> dict:
    """从列表彻底删除（不再关注）。"""
    lines = _read_lines(URL_CONFIG)
    kept = []
    deleted = False
    for line in lines:
        stripped = line.strip()
        if not stripped:
            continue
        existing = _parse_live_line(stripped)
        if existing['url'] == url or existing['url'].rstrip('/') == url.rstrip('/'):
            deleted = True
            continue
        kept.append(line)
    if not deleted:
        return {'ok': False, 'error': '监控列表中没有这个地址', 'url': url}
    _write_lines(URL_CONFIG, kept)
    delete_prefs(url)
    return {'ok': True, 'action': '已删除', 'url': url}


def check_live(url: str) -> dict:
    room = None
    for r in list_live():
        if r['url'] == url or r['url'].rstrip('/') == url.rstrip('/'):
            room = r
            break
    pid = _service_pid('live_monitor')
    return {
        'url': url,
        'found': room is not None,
        'room': room,
        'live_monitor_running': pid is not None,
        'hint': '服务运行中，下一轮循环会自动检测开播' if pid else '请先运行 start 启动后台服务',
    }


# ---------- 视频发布监控列表 ----------

def list_video() -> list[dict]:
    """每行格式: 地址,名称[,下载=是/否]。download 为 None 表示跟随全局开关。"""
    items = []
    for line in _read_lines(WATCHLIST):
        stripped = line.strip()
        if not stripped or stripped.startswith('#'):
            continue
        parts = [p.strip() for p in stripped.split(',')]
        url = parts[0]
        name = parts[1] if len(parts) > 1 and parts[1] else url
        download = None
        for extra in parts[2:]:
            if extra.startswith('下载'):
                download = extra.split('=', 1)[-1].strip() in ('是', 'yes', 'true', '1')
        items.append({'url': url, 'name': name, 'download': download})
    return attach_to_items(items)


def add_video(url: str, name: str, download: str = '', notify: str = '') -> dict:
    """download: ''=跟随全局 / '是' / '否'；notify: 跟随全局/微信/邮箱/不通知"""
    suffix = f',下载={download}' if download in ('是', '否') else ''
    new_line = f'{url},{name}{suffix}'
    lines = _read_lines(WATCHLIST)
    for i, line in enumerate(lines):
        if line.strip().partition(',')[0].strip() == url and not line.strip().startswith('#'):
            lines[i] = new_line
            _write_lines(WATCHLIST, lines)
            if notify:
                set_notify(url, notify, name)
            return {'ok': True, 'action': '已更新', 'url': url, 'name': name,
                    'download': download or '跟随全局',
                    'notify': notify or get_notify(url, name)}
    lines.append(new_line)
    _write_lines(WATCHLIST, lines)
    if notify:
        set_notify(url, notify, name)
    return {'ok': True, 'action': '已添加', 'url': url, 'name': name,
            'download': download or '跟随全局',
            'notify': notify or get_notify(url, name),
            'hint': '首轮检查只记录基线，之后发布的新视频才会通知和下载'}


def download_video(url: str, name: str = '手动下载') -> dict:
    """立即下载单个视频到 downloads/短视频/<名称>/，并跑复盘流水线。"""
    from .video_monitor import _download_and_process
    media = _download_and_process({'title': url.rsplit('/', 1)[-1], 'url': url}, name)
    if media:
        return {'ok': True, 'file': str(media), 'name': name}
    return {'ok': False, 'error': '下载失败，请查看日志或确认链接可访问', 'url': url}


def blogger_videos(url: str, count: str = '30') -> dict:
    """列出某博主已发布的历史视频，供界面批量勾选下载。"""
    from .video_monitor import list_blogger_videos
    try:
        n = int(count)
    except (TypeError, ValueError):
        n = 30
    videos = list_blogger_videos(url, n)
    # 封面本地缓存，界面秒开
    for v in videos:
        vid = v.get('id') or v.get('guid') or v.get('url')
        try:
            from .video_library import upsert_video
            upsert_video(v, anchor=v.get('anchor') or v.get('name') or '', source_url=url)
        except Exception:
            pass
        if v.get('cover'):
            cached = cache_cover(v['cover'], str(vid))
            if cached and cached != v['cover']:
                v['cover_cache'] = cached
    from .video_library import attach_downloaded
    final = attach_downloaded(videos, source_url=url)
    return {'ok': True, 'url': url, 'count': len(final), 'videos': final,
            'library': {'db': str(DB_PATH)}}


def video_library(source_url: str = '', anchor: str = '', downloaded_only: str = '') -> dict:
    """查询视频库（已见过的视频 + 已下载标记）。"""
    rows = get_videos(source_url=source_url, anchor=anchor,
                      downloaded_only=downloaded_only in ('1', '是', 'true', 'yes'))
    return {'ok': True, 'videos': rows, 'count': len(rows), 'db': str(DB_PATH)}


def export_training_data(anchor: str = '', output: str = '') -> dict:
    """把转写+知识点导出为 JSONL 训练数据。"""
    from .training_data import export_training_data as _export
    return _export(anchor=anchor, output=output)


def email_status() -> dict:
    """查看邮箱配置状态（不回显授权码）。"""
    from .email_setup import email_status as _status
    return _status()


def email_setup(email: str, auth: str, to: str = '') -> dict:
    """写入 QQ 邮箱 SMTP 配置并启用邮件通知。"""
    from .email_setup import write_email_config
    return write_email_config(email=email, auth=auth, to=to)


def email_import_legacy(legacy_file: str = '') -> dict:
    """尝试从老项目 zunzhe-dev 迁移 QQ 邮箱账号。"""
    from .email_setup import import_legacy_account
    return import_legacy_account(legacy_file)


def remove_video(url: str) -> dict:
    lines = _read_lines(WATCHLIST)
    kept = [line for line in lines if line.strip().partition(',')[0].strip() != url]
    if len(kept) == len(lines):
        return {'ok': False, 'error': '监控列表中没有这个地址', 'url': url}
    _write_lines(WATCHLIST, kept)
    delete_prefs(url)
    return {'ok': True, 'action': '已删除', 'url': url}


def set_item_notify(url: str, channel: str, name: str = '') -> dict:
    return {'ok': True, **set_notify(url, channel, name)}


# ---------- 后台进程管理 ----------

_SERVICES = {
    'live_monitor': [PYTHON, '-u', 'main.py'],
    'video_monitor': [PYTHON, '-u', '-m', 'fupan.video_monitor'],
}


def _pid_file(service: str) -> Path:
    return RUN_DIR / f'{service}.pid'


def _service_pid(service: str) -> int | None:
    """返回存活的服务 pid，不存活则返回 None。"""
    import psutil
    path = _pid_file(service)
    if not path.exists():
        return None
    try:
        pid = int(path.read_text().strip())
        proc = psutil.Process(pid)
        if proc.is_running() and 'python' in proc.name().lower():
            return pid
    except (ValueError, psutil.NoSuchProcess, psutil.AccessDenied):
        pass
    return None


def _start_one(service: str) -> str:
    """启动单个后台服务，已在运行则跳过。"""
    if _service_pid(service):
        return '已在运行'
    RUN_DIR.mkdir(parents=True, exist_ok=True)
    LOG_DIR.mkdir(parents=True, exist_ok=True)
    cmd = _SERVICES[service]
    log_file = open(LOG_DIR / f'{service}.log', 'a', encoding='utf-8')
    popen_kwargs = {}
    if os.name == 'nt':
        popen_kwargs['creationflags'] = (
            subprocess.DETACHED_PROCESS | subprocess.CREATE_NEW_PROCESS_GROUP
        )
    else:
        # POSIX 下让后台服务脱离当前终端会话，避免随终端一起退出
        popen_kwargs['start_new_session'] = True
    proc = subprocess.Popen(
        cmd, cwd=str(PROJECT_DIR), stdout=log_file, stderr=subprocess.STDOUT,
        stdin=subprocess.DEVNULL, **popen_kwargs)
    _pid_file(service).write_text(str(proc.pid))
    time.sleep(2)
    if not _service_pid(service):
        return f'启动失败 pid={proc.pid}，请查看 logs/{service}.log'
    return f'已启动 pid={proc.pid}'


def start() -> dict:
    result = {}
    for service in _SERVICES:
        result[service] = _start_one(service)
    return {'ok': True, 'services': result, 'logs': str(LOG_DIR)}


def ensure_services() -> dict:
    """守护用：只拉起未运行的服务。"""
    result = {}
    restarted = []
    for service in _SERVICES:
        if _service_pid(service):
            result[service] = '运行中'
        else:
            msg = _start_one(service)
            result[service] = msg
            if '已启动' in msg:
                restarted.append(service)
    return {'ok': True, 'services': result, 'restarted': restarted}


def tail_logs(name: str = 'live_monitor', lines: int = 200) -> dict:
    """读取日志末尾若干行。name: live_monitor | video_monitor | desktop-start"""
    log = LOG_DIR / f'{name}.log'
    if name == 'desktop-start':
        log = LOG_DIR / 'desktop-start.log'
    if not log.exists():
        return {'ok': True, 'name': name, 'path': str(log), 'content': '（暂无日志）'}
    text = log.read_text(encoding='utf-8', errors='replace')
    tail = '\n'.join(text.splitlines()[-max(1, min(lines, 2000)):])
    return {'ok': True, 'name': name, 'path': str(log), 'content': tail, 'lines': len(tail.splitlines())}


def stop() -> dict:
    import psutil
    result = {}
    for service in _SERVICES:
        pid = _service_pid(service)
        if not pid:
            result[service] = '未在运行'
            continue
        try:
            proc = psutil.Process(pid)
            children = proc.children(recursive=True)
            # 先结束 ffmpeg 等子进程，避免留下孤儿进程继续占用文件
            for child in children:
                child.terminate()
            psutil.wait_procs(children, timeout=10)
            for child in children:
                if child.is_running():
                    child.kill()
            proc.terminate()
            proc.wait(timeout=10)
            result[service] = '已停止'
        except psutil.NoSuchProcess:
            result[service] = '已停止'
        except Exception as e:
            result[service] = f'停止失败: {e}'
        _pid_file(service).unlink(missing_ok=True)
    return {'ok': True, 'services': result}


# ---------- 状态与产物 ----------

def status() -> dict:
    # 录像落点可配置（[录制设置] 直播保存路径 / [路径设置] 产物根目录）。
    # 以前写死 PROJECT_DIR/'downloads'，用户把产物搬到别处后这里会扫到一个空目录，
    # 界面就显示「没有录制」。
    downloads = RECORDINGS_DIR
    recent_recordings = []
    if downloads.exists():
        files = sorted(downloads.rglob('*.*'), key=lambda p: p.stat().st_mtime, reverse=True)
        now = time.time()
        for f in files[:10]:
            # 录像可能落在项目目录之外（自定义产物根 / 外置盘），这时
            # relative_to 会抛 ValueError 把整个 status 命令带崩，必须兜住。
            try:
                shown = str(f.relative_to(PROJECT_DIR))
            except ValueError:
                shown = str(f)
            recent_recordings.append({
                'file': shown,
                'size_mb': round(f.stat().st_size / 1048576, 1),
                'minutes_ago': int((now - f.stat().st_mtime) / 60),
                'probably_recording': now - f.stat().st_mtime < 120,
            })
    resolved = storage_paths()
    return {
        'services': {s: ({'running': True, 'pid': pid} if (pid := _service_pid(s))
                         else {'running': False}) for s in _SERVICES},
        'live_watchlist': list_live(),
        'video_watchlist': list_video(),
        'recent_recordings': recent_recordings,
        'notes': notes(),
        # 键保持旧名不变（界面在用），另补 media_root/customized 等新字段，
        # 让「文件到底存哪」在界面上一目了然。
        'paths': {
            'recordings': str(downloads),
            'fupan_output': str(FUPAN_DIR),
            'logs': str(LOG_DIR),
            'downloads': str(DOWNLOADS_DIR),
            'media_root': str(resolved['media_root']),
            'covers': str(resolved['covers']),
            'run': str(resolved['run']),
            'database': str(resolved['database']),
            'customized': str(resolved['media_root']) != str(PROJECT_DIR),
        },
    }


def notes(name: str = '') -> list[dict]:
    results = []
    if not FUPAN_DIR.exists():
        return results
    for anchor_dir in FUPAN_DIR.iterdir():
        if not anchor_dir.is_dir() or anchor_dir.name in ('run', 'logs'):
            continue
        if name and anchor_dir.name != name:
            continue
        for session_dir in anchor_dir.iterdir():
            if not session_dir.is_dir() or session_dir.name == '视频下载':
                continue
            entry = {'anchor': anchor_dir.name, 'session': session_dir.name}
            for f in session_dir.iterdir():
                if f.name.endswith('.复盘笔记.md'):
                    entry['notes_file'] = str(f)
                elif f.name.endswith('.转写.txt'):
                    entry['transcript_file'] = str(f)
                elif f.name.endswith('.知识点.json'):
                    entry['points_file'] = str(f)
                elif f.is_dir() and f.name == '精华切片':
                    entry['clips'] = [str(c) for c in f.iterdir()]
            results.append(entry)
    return results


# ---------- 入口 ----------

def main() -> None:
    if hasattr(sys.stdout, 'reconfigure'):
        sys.stdout.reconfigure(encoding='utf-8')
    args = sys.argv[1:]
    if not args:
        print(__doc__)
        return
    cmd, rest = args[0], args[1:]
    try:
        if cmd == 'status':
            _out(status())
        elif cmd == 'add-live':
            _out(add_live(rest[0], rest[1] if len(rest) > 1 else '',
                           rest[2] if len(rest) > 2 else '', rest[3] if len(rest) > 3 else ''))
        elif cmd == 'remove-live':
            _out(remove_live(rest[0]))
        elif cmd == 'delete-live':
            _out(delete_live(rest[0]))
        elif cmd == 'set-notify':
            _out(set_item_notify(rest[0], rest[1] if len(rest) > 1 else '跟随全局',
                               rest[2] if len(rest) > 2 else ''))
        elif cmd == 'list-live':
            _out({'rooms': list_live()})
        elif cmd == 'check-live':
            _out(check_live(rest[0]))
        elif cmd == 'add-video':
            _out(add_video(rest[0], rest[1] if len(rest) > 1 else rest[0],
                           rest[2] if len(rest) > 2 else '', rest[3] if len(rest) > 3 else ''))
        elif cmd == 'download-video':
            _out(download_video(rest[0], rest[1] if len(rest) > 1 else '手动下载'))
        elif cmd == 'blogger-videos':
            _out(blogger_videos(rest[0], rest[1] if len(rest) > 1 else '30'))
        elif cmd == 'video-library':
            _out(video_library(rest[0] if len(rest) > 0 else '',
                               rest[1] if len(rest) > 1 else '',
                               rest[2] if len(rest) > 2 else ''))
        elif cmd == 'export-training-data':
            _out(export_training_data(rest[0] if len(rest) > 0 else '',
                                      rest[1] if len(rest) > 1 else ''))
        elif cmd == 'email-status':
            _out(email_status())
        elif cmd == 'email-setup':
            _out(email_setup(rest[0] if len(rest) > 0 else '',
                             rest[1] if len(rest) > 1 else '',
                             rest[2] if len(rest) > 2 else ''))
        elif cmd == 'email-import-legacy':
            _out(email_import_legacy(rest[0] if len(rest) > 0 else ''))
        elif cmd == 'remove-video':
            _out(remove_video(rest[0]))
        elif cmd == 'list-video':
            _out({'sources': list_video()})
        elif cmd == 'start':
            _out(start())
        elif cmd == 'ensure-services':
            _out(ensure_services())
        elif cmd == 'stop':
            _out(stop())
        elif cmd == 'tail-logs':
            _out(tail_logs(rest[0] if rest else 'live_monitor',
                           int(rest[1]) if len(rest) > 1 else 200))
        elif cmd == 'get-app-prefs':
            _out({'ok': True, 'prefs': get_prefs()})
        elif cmd == 'set-app-prefs':
            bulk = json.loads(rest[0]) if rest else {}
            _out({'ok': True, 'prefs': set_prefs(**bulk)})
        elif cmd == 'wizard-done':
            _out({'ok': True, 'prefs': mark_wizard_done()})
        elif cmd == 'doctor':
            from .doctor import main as doctor_main
            code = doctor_main()
            _out({'ok': code == 0})
            if code != 0:
                sys.exit(1)
        elif cmd == 'check-videos':
            from .video_monitor import check_once
            _out({'new_videos': check_once()})
        elif cmd == 'process':
            from .pipeline import run_pipeline
            _out(run_pipeline(rest[0], rest[1] if len(rest) > 1 else ''))
        elif cmd == 'notes':
            _out({'notes': notes(rest[0] if rest else '')})
        elif cmd in ('test-email', 'test-notify'):
            from .notify import send_test
            ok = send_test()
            _out({'ok': ok, 'sent': ok})
        elif cmd == 'get-config':
            _out(get_config(rest[0] if rest else 'all', rest[1] if len(rest) > 1 else '',
                             rest[2] if len(rest) > 2 else ''))
        elif cmd == 'set-config':
            _out(set_config(rest[0], rest[1], rest[2], rest[3]))
        elif cmd == 'set-config-bulk':
            _out(set_config_bulk(json.loads(rest[0])))
        elif cmd == 'schema':
            _out(config_schema())
        elif cmd == 'mcp-info':
            _out({
                'mcp_command': f'{PYTHON} -m fupan.mcp_server',
                'cwd': str(PROJECT_DIR),
                'config_example': str(PROJECT_DIR / 'mcp.json.example'),
                'hint': '在 Cursor / OpenCode 等智能体的 MCP 设置中添加 stdio 服务器',
            })
        else:
            _out({'error': f'未知子命令: {cmd}', 'hint': '运行 python -m fupan.cli 查看帮助'})
            sys.exit(2)
    except IndexError:
        _out({'error': f'{cmd} 缺少参数', 'hint': '运行 python -m fupan.cli 查看帮助'})
        sys.exit(2)
    except json.JSONDecodeError as e:
        _out({'error': f'JSON 解析失败: {e}'})
        sys.exit(2)
    except Exception as e:
        _out({'error': str(e)})
        sys.exit(1)


if __name__ == '__main__':
    main()
