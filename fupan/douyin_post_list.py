# -*- coding: utf-8 -*-
"""抖音短视频列表/单视频解析桥（免登录）。

为什么存在：抖音网页接口有 WAF，但 `aweme/v1/web/aweme/post`（用户视频列表）
与 `aweme/v1/web/aweme/detail`（单视频详情）只要带上项目自带的 a_bogus 签名
（src/ab_sign.py）+ 匿名 ttwid cookie 就能免登录访问（2026-09-25 实测，
两个在监账号均返回真实数据，play_addr 可直接匿名下载 MP4）。

架构约定（与 fupan/live_resolve.py 一致）：
- Rust 负责调度/超时/状态机/下载落盘；平台签名与网页解析留在 Python。
- 输出单行 JSON 契约，失败必须带 error 字段——绝不把失败伪装成空列表，
  否则风控/签名失效时用户看到的永远是「博主没发新视频」且零告警。
- 匿名通道失败时，若用户做过「抖音授权」（douyin_browser_cookies.txt），
  自动带授权 cookie 重试一次。

用法：
  python -m fupan.douyin_post_list --sec-uid MS4wLjABAAAA... [--count 30]
  python -m fupan.douyin_post_list --video-url https://www.douyin.com/video/7676...
"""

import argparse
import asyncio
import json
import os
import sys
import urllib.parse
from pathlib import Path

# 与 spider.py 抖音直播链路同一个 UA：a_bogus 签名与 UA 绑定，不能各写各的。
UA = (
    'Mozilla/5.0 (Windows NT 10.0; WOW64) AppleWebKit/537.36 (KHTML, like Gecko) '
    'Chrome/116.0.5845.97 Safari/537.36 Core/1.116.567.400 QQBrowser/19.7.6764.400'
)
# 上游借来的匿名 ttwid（与 src/spider.py:129 一致）。被风控时走授权 cookie 兜底。
ANON_TTWID = (
    'ttwid=1%7C2iDIYVmjzMcpZ20fcaFde0VghXAA3NaNXE_SLR68IyE%7C1761045455'
    '%7Cab35197d5cfb21df6cbb2fa7ef1c9262206b062c315b9d04da746d0b37dfbc7d'
)

_COMMON_PARAMS = {
    'device_platform': 'webapp',
    'aid': '6383',
    'channel': 'channel_pc_web',
    'pc_client_type': '1',
    'version_code': '290100',
    'version_name': '29.1.0',
    'cookie_enabled': 'true',
    'screen_width': '1920',
    'screen_height': '1080',
    'browser_language': 'zh-CN',
    'browser_platform': 'Win32',
    'browser_name': 'Chrome',
    'browser_version': '116.0.0.0',
    'browser_online': 'true',
    'engine_name': 'Blink',
    'engine_version': '116.0.0.0',
    'os_name': 'Windows',
    'os_version': '10',
    'cpu_core_num': '8',
    'device_memory': '8',
    'platform': 'PC',
    'downlink': '10',
    'msToken': '',
}


def _project_root() -> Path:
    return Path(__file__).resolve().parent.parent


def _ensure_src_on_path() -> None:
    root = _project_root()
    if str(root) not in sys.path:
        sys.path.insert(0, str(root))


def _signed_api(path: str, extra_params: dict) -> str:
    from src.ab_sign import ab_sign

    params = {**_COMMON_PARAMS, **extra_params}
    query = urllib.parse.urlencode(params)
    return f'https://www.douyin.com{path}?{query}&a_bogus={ab_sign(query, UA)}'


def _headers(referer: str, cookies: str) -> dict:
    return {
        'user-agent': UA,
        'referer': referer,
        'cookie': cookies or ANON_TTWID,
    }


def _pick_play_url(video: dict) -> str:
    play = (video or {}).get('play_addr') or {}
    for u in play.get('url_list') or []:
        if isinstance(u, str) and u.startswith('http'):
            return u
    return ''


def _video_item(aweme: dict) -> dict:
    aweme_id = str(aweme.get('aweme_id') or '')
    return {
        'aweme_id': aweme_id,
        'title': str(aweme.get('desc') or '').strip(),
        'create_time': int(aweme.get('create_time') or 0),
        'duration': int(aweme.get('duration') or 0) // 1000,  # 毫秒 → 秒
        'url': f'https://www.douyin.com/video/{aweme_id}',
        'download_url': _pick_play_url(aweme.get('video')),
        'cover': _pick_play_url({'play_addr': (aweme.get('video') or {}).get('cover')}),
    }


def _cookie_attempts() -> list:
    """匿名先试；有授权 cookie 时追加一次兜底。"""
    attempts = ['']
    try:
        from fupan.live_resolve import _load_douyin_cookies

        authorized = _load_douyin_cookies()
        if authorized:
            attempts.append(authorized)
    except Exception:
        pass
    return attempts


async def _fetch_json(api: str, referer: str, cookies: str) -> dict:
    import httpx

    async with httpx.AsyncClient(timeout=20) as client:
        resp = await client.get(api, headers=_headers(referer, cookies))
        if resp.status_code != 200:
            raise RuntimeError(f'HTTP {resp.status_code}')
        if not resp.text.strip():
            raise RuntimeError('接口返回空响应（可能触发风控）')
        return resp.json()


async def list_videos(sec_uid: str, count: int) -> dict:
    last_error = ''
    for cookies in _cookie_attempts():
        try:
            api = _signed_api('/aweme/v1/web/aweme/post/', {
                'sec_user_id': sec_uid,
                'count': str(min(max(count, 1), 50)),
                'max_cursor': '0',
                'locate_query': 'false',
                'show_live_replay_strategy': '1',
                'need_time_list': '1',
                'time_list_query': '0',
                'whale_cut_token': '',
                'cut_version': '1',
                'count_number': str(min(max(count, 1), 50)),
            })
            data = await _fetch_json(api, f'https://www.douyin.com/user/{sec_uid}', cookies)
            aweme_list = data.get('aweme_list') or []
            if not aweme_list:
                last_error = f'接口返回空视频列表（status_code={data.get("status_code")}，可能被风控或博主无作品）'
                continue
            return {
                'ok': True,
                'videos': [_video_item(a) for a in aweme_list][:count],
            }
        except Exception as exc:
            last_error = str(exc)
    return {'ok': False, 'error': f'抖音视频列表获取失败: {last_error or "接口无有效返回"}'}


async def video_detail(video_url: str) -> dict:
    aweme_id = ''
    if '/video/' in video_url:
        aweme_id = video_url.split('/video/')[-1].split('?')[0].split('/')[0].strip()
    if not aweme_id.isdigit():
        return {'ok': False, 'error': f'无法从链接解析 aweme_id: {video_url}（短链请先展开）'}
    last_error = ''
    for cookies in _cookie_attempts():
        try:
            api = _signed_api('/aweme/v1/web/aweme/detail/', {'aweme_id': aweme_id})
            data = await _fetch_json(api, f'https://www.douyin.com/video/{aweme_id}', cookies)
            detail = data.get('aweme_detail') or {}
            if not detail:
                last_error = f'接口返回空详情（status_code={data.get("status_code")}，可能被风控）'
                continue
            item = _video_item(detail)
            if not item['download_url']:
                last_error = '详情里没有可下载的 play_addr'
                continue
            return {'ok': True, 'video': item}
        except Exception as exc:
            last_error = str(exc)
    return {'ok': False, 'error': f'抖音视频详情获取失败: {last_error or "接口无有效返回"}'}


def main() -> int:
    parser = argparse.ArgumentParser()
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument('--sec-uid', help='用户主页 sec_uid（MS4wLjABAAAA 开头）')
    group.add_argument('--video-url', help='单视频链接（www.douyin.com/video/<id>）')
    parser.add_argument('--count', type=int, default=30)
    args = parser.parse_args()

    _ensure_src_on_path()
    # 与 live_resolve 同一约定：调试输出进 devnull，契约结果单独写原始 stdout。
    sys.stdout = open(os.devnull, 'w', encoding='utf-8')
    try:
        if args.sec_uid:
            result = asyncio.run(list_videos(args.sec_uid, args.count))
        else:
            result = asyncio.run(video_detail(args.video_url))
    except Exception as exc:
        result = {'ok': False, 'error': str(exc)}

    sys.__stdout__.write(json.dumps(result, ensure_ascii=False) + '\n')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
