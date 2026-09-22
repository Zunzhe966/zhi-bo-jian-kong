# -*- coding: utf-8 -*-
"""视频发布监控：轮询 B站UP主空间 / 任意RSS源，发现新视频 -> 邮件通知 -> (可选)下载并跑复盘流水线。

用法: python -m fupan.video_monitor
监控列表: config/视频监控列表.ini
"""
import json
import re
import shutil
import subprocess
import sys
import time
import xml.etree.ElementTree as ET
from pathlib import Path

import httpx

from .settings import (CONFIG_DIR, DOWNLOADS_DIR, FUPAN_DIR, PROJECT_DIR,
                       fupan_config, get, is_yes)
from .notify import send_new_video_alert
from .pipeline import run_pipeline
from . import bili_browser
from .bili_browser import is_bilibili
from .video_library import (upsert_video, mark_downloaded, attach_downloaded,
                            cache_cover, get_videos)

STATE_FILE = FUPAN_DIR / '视频监控状态.json'
# 短视频与直播录像同在 downloads 下：downloads/短视频/主播名/日期_标题.mp4
# 落点跟随 [路径设置]：以前写死 PROJECT_DIR/'downloads'，产物根改到别处后
# 这条链路会把下载/查重/去重全指向一个已经不存在的老目录。
VIDEO_DOWNLOAD_ROOT = DOWNLOADS_DIR / '短视频'
UA = ('Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 '
      '(KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36')


def _load_watchlist() -> list[dict]:
    """每行格式: 地址,名称[,下载=是/否]。第三段省略时跟随全局开关。"""
    path = CONFIG_DIR / '视频监控列表.ini'
    items = []
    if not path.exists():
        return items
    for line in path.read_text(encoding='utf-8-sig').splitlines():
        line = line.strip()
        if not line or line.startswith('#'):
            continue
        parts = [p.strip() for p in line.split(',')]
        url = parts[0]
        name = parts[1] if len(parts) > 1 and parts[1] else url
        download = None  # None = 跟随全局
        for extra in parts[2:]:
            if extra.startswith('下载'):
                download = is_yes(extra.split('=', 1)[-1])
        items.append({'url': url, 'name': name, 'download': download})
    return items


def _load_state() -> dict:
    if STATE_FILE.exists():
        try:
            return json.loads(STATE_FILE.read_text(encoding='utf-8'))
        except json.JSONDecodeError:
            pass
    return {}


def _save_state(state: dict) -> None:
    STATE_FILE.parent.mkdir(parents=True, exist_ok=True)
    STATE_FILE.write_text(json.dumps(state, ensure_ascii=False, indent=2), encoding='utf-8')


def _bilibili_title(bvid: str) -> str:
    try:
        d = httpx.get(f'https://api.bilibili.com/x/web-interface/view?bvid={bvid}',
                      headers={'User-Agent': UA}, timeout=15).json()
        return (d.get('data') or {}).get('title') or bvid
    except Exception:
        return bvid


def _bilibili_view(bvid: str) -> dict:
    """view 接口取标题/封面/发布时间/时长，风控较松，代理下也可用。"""
    try:
        d = httpx.get(f'https://api.bilibili.com/x/web-interface/view?bvid={bvid}',
                      headers={'User-Agent': UA, 'Referer': 'https://www.bilibili.com'},
                      timeout=15).json()
        data = d.get('data') or {}
        return {
            'id': bvid,
            'title': data.get('title') or bvid,
            'cover': (data.get('pic') or '').replace('http://', 'https://'),
            'url': f'https://www.bilibili.com/video/{bvid}',
            'date': time.strftime('%Y-%m-%d', time.localtime(data.get('pubdate', 0))) if data.get('pubdate') else '',
            'duration': data.get('duration', 0),
        }
    except Exception:
        return {'id': bvid, 'title': bvid, 'cover': '',
                'url': f'https://www.bilibili.com/video/{bvid}', 'date': '', 'duration': 0}


def _bilibili_upload_ids(mid: str, count: int) -> list[str]:
    """拉 UP主投稿 BV 号列表。优先官方 arc/search 接口，失败回退 yt-dlp。
    两个途径都带用户 cookie，避免 412 限流。"""
    bvids = _bilibili_upload_ids_api(mid, count)
    if not bvids:
        bvids = _bilibili_upload_ids_rsshub(mid, count)
    if bvids:
        return bvids
    ytdlp = _ytdlp_bin()
    cmd = [ytdlp, '--flat-playlist', '--playlist-end', str(count), '--print', '%(id)s',
           f'https://space.bilibili.com/{mid}/video']
    # 注意：匿名 buvid cookie 反而会触发 412，这里只使用用户配置或浏览器导出的 cookie
    cookies = _bilibili_user_cookies_file() or _bilibili_browser_cookies_file()
    if cookies:
        cmd[1:1] = ['--cookies', str(cookies)]
    result = subprocess.run(cmd, capture_output=True, text=True,
                            encoding='utf-8', errors='replace', timeout=120)
    return [ln.strip() for ln in result.stdout.splitlines() if ln.strip().startswith('BV')]


def _bilibili_upload_ids_api(mid: str, count: int) -> list[str]:
    """B站官方 arc/search 接口拉投稿 BV 号（需要 cookie，风控较稳）。"""
    cookie = get(fupan_config(), '视频监控', 'b站cookie')
    headers = {'User-Agent': UA, 'Referer': f'https://space.bilibili.com/{mid}/video'}
    if cookie:
        headers['Cookie'] = cookie
    try:
        data = httpx.get(
            f'https://api.bilibili.com/x/space/wbi/arc/search?mid={mid}&ps={min(count, 50)}&pn=1&order=pubdate',
            headers=headers, timeout=20).json()
        if data.get('code') == 0:
            vlist = (data.get('data') or {}).get('list', {}).get('vlist', [])
            return [v['bvid'] for v in vlist if v.get('bvid')]
    except Exception:
        pass
    return []


def _bilibili_upload_ids_rsshub(mid: str, count: int) -> list[str]:
    """RSSHub 公开实例拉 UP主投稿（不依赖 B站接口，绕开 412 限流）。
    RSSHub 返回含视频页链接，从中提取 BV 号。"""
    import urllib.parse
    rss_url = 'https://rsshub.app/bilibili/user/video/' + urllib.parse.quote(str(mid))
    try:
        resp = httpx.get(rss_url, headers={'User-Agent': UA}, timeout=20, follow_redirects=True)
        root = ET.fromstring(resp.content)
        bvids = []
        for item in root.iter('item'):
            link = (item.findtext('link') or '').strip()
            m = re.search(r'(BV[0-9A-Za-z]+)', link)
            if m and m.group(1) not in bvids:
                bvids.append(m.group(1))
            if len(bvids) >= count:
                break
        return bvids
    except Exception:
        return []


def list_blogger_videos(url: str, count: int = 30) -> list[dict]:
    """列出某博主已发布的视频（供批量选择下载）。
    返回每条含 id/title/cover/date/duration/url/downloaded/cover_cache。
    B站取投稿列表并补全标题/封面；RSS 直接解析（含 media:content 封面）。"""
    match = re.search(r'space\.bilibili\.com/(\d+)', url)
    if match:
        bvids = _bilibili_upload_ids(match.group(1), count)
        if not bvids:
            raise RuntimeError(
                'B站投稿列表拉取失败（可能被 412 风控）。请在「全局设置」里做一次 B站授权，'
                '或稍后再试。')
        items = [_bilibili_view(bv) for bv in bvids]
        for it in items:
            it['platform'] = 'bilibili'
        return attach_downloaded(items, source_url=url)
    # 非 B站：按 RSS 处理，条目自带标题
    items = _fetch_rss(url)[:count]
    for it in items:
        it.setdefault('date', '')
        it.setdefault('duration', 0)
        it.setdefault('platform', 'rss')
    return attach_downloaded(items, source_url=url)


def _fetch_bilibili_ytdlp(mid: str) -> list[dict]:
    """无 cookie 兜底：用 yt-dlp 拉 UP主最新投稿 ID，再补标题。"""
    ytdlp = _ytdlp_bin()
    result = subprocess.run(
        [ytdlp, '--flat-playlist', '--playlist-end', '10', '--print', '%(id)s',
         f'https://space.bilibili.com/{mid}/video'],
        capture_output=True, text=True, encoding='utf-8', errors='replace', timeout=120)
    if result.returncode != 0:
        raise RuntimeError(f'yt-dlp 获取投稿列表失败: {(result.stderr or "")[-300:]}')
    bvids = [line.strip() for line in result.stdout.splitlines() if line.strip().startswith('BV')]
    return [{'id': bvid, 'title': _bilibili_title(bvid),
             'url': f'https://www.bilibili.com/video/{bvid}'} for bvid in bvids]


def _fetch_bilibili(mid: str, cookie: str) -> list[dict]:
    """获取UP主最新投稿，返回 [{'id','title','url'}]。优先官方接口（需cookie），失败回退 yt-dlp。"""
    if cookie:
        headers = {'User-Agent': UA, 'Referer': f'https://space.bilibili.com/{mid}/video',
                   'Cookie': cookie}
        api = f'https://api.bilibili.com/x/space/wbi/arc/search?mid={mid}&ps=10&pn=1&order=pubdate'
        try:
            data = httpx.get(api, headers=headers, timeout=20).json()
            if data.get('code') == 0:
                vlist = data['data']['list']['vlist']
                return [{'id': v['bvid'], 'title': v['title'],
                         'url': f'https://www.bilibili.com/video/{v["bvid"]}'} for v in vlist]
        except Exception:
            pass
    return _fetch_bilibili_ytdlp(mid)


def _ytdlp_bin() -> str:
    """yt-dlp 可执行文件（跨平台：优先 PATH，其次 venv 内）。"""
    found = shutil.which('yt-dlp')
    if found:
        return found
    if sys.platform == 'win32':
        return str(Path(sys.executable).parent / 'yt-dlp.exe')
    return str(Path(sys.executable).parent / 'yt-dlp')


def _fetch_rss(url: str) -> list[dict]:
    resp = httpx.get(url, headers={'User-Agent': UA}, timeout=20, follow_redirects=True)
    root = ET.fromstring(resp.content)
    ns = {'media': 'http://search.yahoo.com/mrss/'}
    items = []
    for item in root.iter('item'):
        link = (item.findtext('link') or '').strip()
        title = (item.findtext('title') or '').strip()
        guid = (item.findtext('guid') or link).strip()
        cover = ''
        media_thumb = item.find('media:thumbnail', ns)
        media_content = item.find('media:content', ns)
        if media_thumb is not None and media_thumb.get('url'):
            cover = media_thumb.get('url')
        elif media_content is not None and media_content.get('url'):
            cover = media_content.get('url')
        elif item.findtext('enclosure'):
            cover = (item.find('enclosure').get('url') or '').strip()
        pub_date = (item.findtext('pubDate') or '').strip()
        if link:
            items.append({'id': guid, 'title': title, 'url': link,
                          'cover': cover, 'date': _rss_date(pub_date)})
    # Atom 格式兼容
    if not items:
        ns_a = {'a': 'http://www.w3.org/2005/Atom',
                'media': 'http://search.yahoo.com/mrss/'}
        for entry in root.findall('a:entry', ns):
            link_el = entry.find('a:link', ns_a)
            link = link_el.get('href') if link_el is not None else ''
            title = (entry.findtext('a:title', default='', namespaces=ns_a) or '').strip()
            guid = (entry.findtext('a:id', default=link, namespaces=ns_a) or '').strip()
            cover = ''
            media_thumb = entry.find('media:thumbnail', ns_a)
            media_content = entry.find('media:content', ns_a)
            if media_thumb is not None and media_thumb.get('url'):
                cover = media_thumb.get('url')
            elif media_content is not None and media_content.get('url'):
                cover = media_content.get('url')
            pub_date = (entry.findtext('a:published', default='', namespaces=ns_a) or '').strip()
            if link:
                items.append({'id': guid, 'title': title, 'url': link,
                              'cover': cover, 'date': _rss_date(pub_date)})
    return items


def _rss_date(raw: str) -> str:
    """把 RFC822 / ISO8601 时间转成 YYYY-MM-DD；失败返回空串。"""
    raw = (raw or '').strip()
    if not raw:
        return ''
    for fmt in ('%a, %d %b %Y %H:%M:%S %z', '%a, %d %b %Y %H:%M:%S %Z',
                '%Y-%m-%dT%H:%M:%S%z', '%Y-%m-%dT%H:%M:%S'):
        try:
            return time.strftime('%Y-%m-%d', time.strptime(raw[:31], fmt))
        except ValueError:
            continue
    return raw[:10]


def _fetch_videos(source_url: str, cookie: str) -> list[dict]:
    match = re.search(r'space\.bilibili\.com/(\d+)', source_url)
    if match:
        return _fetch_bilibili(match.group(1), cookie)
    return _fetch_rss(source_url)


def _bilibili_user_cookies_file() -> Path | None:
    """若用户在 [视频监控] b站cookie 里粘贴了浏览器 Cookie，转成 Netscape
    格式给 yt-dlp 用。B站 412 风控时这是最可靠的解法。"""
    cookie = get(fupan_config(), '视频监控', 'b站cookie')
    if not cookie:
        return None
    path = FUPAN_DIR / 'run' / 'bilibili_user_cookies.txt'
    path.parent.mkdir(parents=True, exist_ok=True)
    lines = ['# Netscape HTTP Cookie File']
    expire = int(time.time()) + 86400 * 180
    for pair in cookie.split(';'):
        key, _, value = pair.strip().partition('=')
        if key and value:
            lines.append(f'.bilibili.com\tTRUE\t/\tFALSE\t{expire}\t{key}\t{value}')
    path.write_text('\n'.join(lines) + '\n', encoding='utf-8')
    return path


def _bilibili_browser_cookies_file() -> Path | None:
    """桌面程序「B站登录/授权」导出的浏览器会话 Cookie（含登录态时质量更高）。"""
    path = FUPAN_DIR / 'run' / 'bilibili_browser_cookies.txt'
    return path if path.exists() else None


def _bilibili_anon_cookies_file() -> Path | None:
    """B站对无 cookie 请求常返回 412 风控。领取匿名设备标识(buvid)写成
    Netscape 格式 cookies 文件供 yt-dlp 使用，这不涉及任何账号凭据。"""
    try:
        d = httpx.get('https://api.bilibili.com/x/frontend/finger/spi',
                      headers={'User-Agent': UA}, timeout=15).json()['data']
        path = FUPAN_DIR / 'run' / 'bilibili_anon_cookies.txt'
        path.parent.mkdir(parents=True, exist_ok=True)
        lines = ['# Netscape HTTP Cookie File']
        expire = int(time.time()) + 86400 * 180
        for key, value in (('buvid3', d['b_3']), ('buvid4', d['b_4'])):
            lines.append(f'.bilibili.com\tTRUE\t/\tFALSE\t{expire}\t{key}\t{value}')
        path.write_text('\n'.join(lines) + '\n', encoding='utf-8')
        return path
    except Exception as e:
        print(f'获取B站匿名cookie失败(可忽略): {e}')
        return None


def _download_and_process(video: dict, name: str) -> Path | None:
    save_dir = VIDEO_DOWNLOAD_ROOT / name
    save_dir.mkdir(parents=True, exist_ok=True)
    date_prefix = time.strftime('%Y-%m-%d')
    video_id = str(video.get('id') or video.get('guid') or video.get('url'))

    # B站走 Electron 浏览器内核提取直链（绕过 412 风控），其余平台走 yt-dlp
    if is_bilibili(video['url']):
        media = bili_browser.download(video['url'], save_dir, filename_prefix=date_prefix)
        if media:
            mark_downloaded(video_id)
            run_pipeline(str(media), name)
            return media
        print('B站浏览器下载失败，回退 yt-dlp 再试一次')

    ytdlp = _ytdlp_bin()
    if not Path(ytdlp).exists():
        print('未安装 yt-dlp，跳过自动下载（pip install yt-dlp）')
        return None
    out_tpl = str(save_dir / f'{date_prefix}_%(title).60s.%(ext)s')
    print(f'开始下载: {video["title"]}')
    cmd = [ytdlp, '-o', out_tpl, '--no-playlist', '-f', 'bv*+ba/b', video['url']]
    if 'bilibili.com' in video['url']:
        cookies_file = _bilibili_user_cookies_file() or _bilibili_anon_cookies_file()
        if cookies_file:
            cmd[1:1] = ['--cookies', str(cookies_file)]
    result = subprocess.run(cmd, capture_output=True, text=True,
                            encoding='utf-8', errors='replace')
    if result.returncode != 0:
        print(f'下载失败: {result.stderr[-500:] if result.stderr else "未知错误"}')
        return None
    files = sorted(save_dir.glob('*'), key=lambda p: p.stat().st_mtime, reverse=True)
    media = next((f for f in files if f.suffix.lower() in ('.mp4', '.mkv', '.webm', '.flv')), None)
    if media:
        print(f'已保存: {media}')
        mark_downloaded(video_id)
        run_pipeline(str(media), name)
    return media


def check_once() -> int:
    """检查一轮所有监控源，返回发现的新视频数。"""
    cfg = fupan_config()
    cookie = get(cfg, '视频监控', 'b站cookie')
    auto_download = is_yes(get(cfg, '视频监控', '自动下载新视频(是/否)', '是'))
    notify_enabled = is_yes(get(cfg, '复盘邮件', '发现新视频时发送通知(是/否)', '是'))

    watchlist = _load_watchlist()
    if not watchlist:
        print('监控列表为空，请编辑 config/视频监控列表.ini')
        return 0

    state = _load_state()
    found = 0
    for item in watchlist:
        try:
            videos = _fetch_videos(item['url'], cookie)
        except Exception as e:
            print(f'[{item["name"]}] 获取视频列表失败: {e}')
            continue

        known = set(state.get(item['url'], []))
        first_run = item['url'] not in state
        new_videos = [v for v in videos if v['id'] not in known]
        # 把本轮拉到的视频元数据写入视频库（含封面/已下载状态）
        for v in videos:
            try:
                upsert_video(v, anchor=item['name'], source_url=item['url'])
            except Exception as e:
                print(f'[视频库] 写入失败 {v.get("id")}: {e}')
        state[item['url']] = [v['id'] for v in videos][:50] + list(known)[:100]

        if first_run:
            print(f'[{item["name"]}] 首次监控，记录 {len(videos)} 个已有视频作为基线')
            continue

        # 每条监控项可单独设置是否下载（下载=是/否），未设置则跟随全局开关
        item_download = auto_download if item.get('download') is None else item['download']
        for video in new_videos:
            found += 1
            print(f'[{item["name"]}] 发现新视频: {video["title"]} {video["url"]}')
            if notify_enabled:
                send_new_video_alert(item['name'], video['title'], video['url'], source_url=item['url'])
            if item_download:
                _download_and_process(video, item['name'])

    _save_state(state)
    return found


def main():
    cfg = fupan_config()
    interval = int(get(cfg, '视频监控', '检查间隔秒', '900') or 900)
    print(f'视频监控启动，每 {interval} 秒检查一次，Ctrl+C 退出')
    while True:
        try:
            check_once()
        except Exception as e:
            print(f'本轮检查异常: {e}')
        time.sleep(interval)


if __name__ == '__main__':
    main()
