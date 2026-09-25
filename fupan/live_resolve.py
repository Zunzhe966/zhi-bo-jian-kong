# -*- coding: utf-8 -*-
"""Rust 核心的多平台直播解析桥。

只做一个稳定 JSON 契约，平台签名/网页解析继续复用仓库旧的 `src/spider.py`
与 `src/stream.py`。这样 Rust 不重写 40+ 个平台的私有协议，也能把 daemon 的
调度、超时、录制状态机放在 Rust 侧。

输出字段与 Rust `LiveStatus` 对齐：
  is_live, title, anchor_name, stream_url
"""

import argparse
import asyncio
import json
import os
import sys
from pathlib import Path


def _project_root() -> Path:
    return Path(__file__).resolve().parent.parent


def _ensure_src_on_path() -> None:
    root = _project_root()
    if str(root) not in sys.path:
        sys.path.insert(0, str(root))


def _empty(anchor_name: str = '') -> dict:
    return {
        'is_live': False,
        'title': '',
        'anchor_name': anchor_name,
        'stream_url': '',
    }


def _load_douyin_cookies() -> str:
    """读取「抖音授权」保存的 cookie 文件（Netscape 格式），转成请求头字符串。

    与 Rust 短视频适配器读的是同一个文件：<产物根>/fupan_output/run/
    douyin_browser_cookies.txt。文件不存在（用户未授权）时返回空串，
    直播检测继续走匿名通道——授权只是兜底，不是前提。
    """
    try:
        from fupan.settings import RUN_DIR
        path = Path(RUN_DIR) / 'douyin_browser_cookies.txt'
    except Exception:
        return ''
    if not path.exists():
        return ''
    pairs = []
    try:
        for line in path.read_text(encoding='utf-8', errors='replace').splitlines():
            line = line.strip()
            if not line or line.startswith('#'):
                continue
            parts = line.split('\t')
            if len(parts) >= 7 and parts[5]:
                pairs.append(f'{parts[5]}={parts[6]}')
    except OSError:
        return ''
    return '; '.join(pairs)


async def _resolve_douyin(url: str) -> dict:
    from src.spider import get_douyin_web_stream_data, get_douyin_app_stream_data
    from src.stream import get_douyin_stream_url

    # 旧 Python 优先走 web，分享链接/用户页再走 app 解析。
    use_app = 'v.douyin.com' in url or '/user/' in url
    # 匿名（内置 ttwid）先试；失败时用「抖音授权」的 cookie 兜底一次。
    attempts = ['']
    if not use_app:
        authorized = _load_douyin_cookies()
        if authorized:
            attempts.append(authorized)

    raw = {}
    last_error = ''
    for cookies in attempts:
        try:
            if use_app:
                raw = await get_douyin_app_stream_data(url)
            else:
                raw = await get_douyin_web_stream_data(url, cookies=cookies or None)
        except Exception as exc:
            last_error = str(exc)
            raw = {}
            continue
        if raw.get('anchor_name'):
            break
        # spider.py 失败时会吞掉异常返回 {'anchor_name': ''}——这就是它的失败签名。
        last_error = '接口未返回主播信息（可能被风控、a_bogus 签名失效或房间不存在）'
        raw = {}
    if not raw.get('anchor_name'):
        # 关键约定：检测失败必须抛出，让上层写进 JSON 的 error 字段（Rust 桥只认
        # error 字段判失败）。绝不能把失败伪装成 is_live=False——否则风控/签名
        # 失效时用户看到的永远是「主播没开播」，监控坏了也没有任何告警。
        raise RuntimeError(f'抖音直播检测失败: {last_error or "接口无有效返回"}')

    result = await get_douyin_stream_url(raw, '原画', '')
    return {
        'is_live': bool(result.get('is_live')),
        'title': str(result.get('title') or ''),
        'anchor_name': str(result.get('anchor_name') or ''),
        'stream_url': str(result.get('record_url') or ''),
    }


async def _resolve_kuaishou(url: str) -> dict:
    from src.spider import get_kuaishou_stream_data, get_kuaishou_stream_data2
    from src.stream import get_kuaishou_stream_url

    # 优先用公开页面解析（__INITIAL_STATE__，免登录免签名、已验证稳定可用），
    # 再尝试 byUser API 作为补充（该 API 易被 IP 风控，页面解析更可靠）。
    if '/u/' in url:
        raw = await get_kuaishou_stream_data(url)
        if not raw or raw.get('type') != 2:
            try:
                raw = await get_kuaishou_stream_data2(url)
            except Exception:
                raw = raw or {'type': 2, 'is_live': False, 'anchor_name': ''}
    else:
        raw = await get_kuaishou_stream_data(url)
    result = await get_kuaishou_stream_url(raw, '原画')
    return {
        'is_live': bool(result.get('is_live')),
        'title': str(result.get('title') or ''),
        'anchor_name': str(result.get('anchor_name') or ''),
        'stream_url': str(result.get('record_url') or ''),
    }


async def _resolve_huya(url: str) -> dict:
    from src.spider import get_huya_stream_data
    from src.stream import get_huya_stream_url

    raw = await get_huya_stream_data(url)
    result = await get_huya_stream_url(raw, '原画')
    return {
        'is_live': bool(result.get('is_live')),
        'title': str(result.get('title') or ''),
        'anchor_name': str(result.get('anchor_name') or ''),
        'stream_url': str(result.get('record_url') or ''),
    }


async def _resolve_douyu(url: str) -> dict:
    from src.spider import get_douyu_info_data
    from src.stream import get_douyu_stream_url

    info = await get_douyu_info_data(url)
    result = await get_douyu_stream_url(info, '原画', '', '')
    return {
        'is_live': bool(result.get('is_live')),
        'title': str(result.get('title') or ''),
        'anchor_name': str(result.get('anchor_name') or ''),
        'stream_url': str(result.get('record_url') or ''),
    }


async def _resolve_youtube(url: str) -> dict:
    from src.spider import get_youtube_stream_url

    raw = await get_youtube_stream_url(url)
    return {
        'is_live': bool(raw.get('is_live')),
        'title': str(raw.get('title') or ''),
        'anchor_name': str(raw.get('anchor_name') or ''),
        'stream_url': str(raw.get('m3u8_url') or ''),
    }


async def _resolve_twitch(url: str) -> dict:
    from src.spider import get_twitchtv_stream_data

    raw = await get_twitchtv_stream_data(url)
    return {
        'is_live': bool(raw.get('is_live')),
        'title': str(raw.get('title') or ''),
        'anchor_name': str(raw.get('anchor_name') or ''),
        'stream_url': str(raw.get('m3u8_url') or raw.get('record_url') or ''),
    }


RESOLVERS = {
    'douyin': _resolve_douyin,
    'kuaishou': _resolve_kuaishou,
    'huya': _resolve_huya,
    'douyu': _resolve_douyu,
    'youtube': _resolve_youtube,
    'twitch': _resolve_twitch,
}


async def resolve(url: str, platform: str) -> dict:
    resolver = RESOLVERS.get(platform)
    if resolver is None:
        return _empty()
    try:
        return await asyncio.wait_for(resolver(url), timeout=22)
    except Exception as exc:
        return {
            ** _empty(),
            'error': str(exc),
        }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('--url', required=True)
    parser.add_argument('--platform', required=True)
    args = parser.parse_args()

    _ensure_src_on_path()
    # 旧解析器里有零散 `print` 调试输出，先重定向到 devnull；契约结果单独写入
    # 原始 stdout，保证 Rust 能拿到单行 JSON。
    sys.stdout = open(os.devnull, 'w', encoding='utf-8')
    try:
        result = asyncio.run(resolve(args.url, args.platform))
    except Exception as exc:
        result = {**_empty(), 'error': str(exc)}

    # 出错时不写 stderr 的 traceback，统一输出 JSON；退出码保持 0，让 Rust
    # 把“未开播/解析失败”视为一次明确的检测结果，而不是 daemon 崩溃。
    sys.__stdout__.write(json.dumps(result, ensure_ascii=False) + '\n')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
