# -*- coding: utf-8 -*-
"""抖音直播解析桥的行为契约测试。

核心约定（2026-09-25 修复）：检测失败必须透出 error 字段，绝不能伪装成
is_live=False——否则风控/签名失效时用户看到的永远是「主播没开播」，
监控坏了也没有任何告警（Rust 桥 live_python_bridge.rs 只认 error 字段判失败）。
"""
import asyncio
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from fupan import live_resolve  # noqa: E402
import src.spider as spider  # noqa: E402
import src.stream as stream  # noqa: E402


def _run(coro):
    return asyncio.run(coro)


def _patch_stream(monkeypatch):
    async def fake_stream(raw, quality, extra):
        return {'is_live': False, 'anchor_name': raw.get('anchor_name', '')}

    monkeypatch.setattr(stream, 'get_douyin_stream_url', fake_stream)


def test_douyin_swallowed_failure_becomes_error(monkeypatch):
    """spider 吞异常返回空 anchor（它的失败签名）→ 契约必须带 error 字段。"""
    async def fake_web(url, proxy_addr=None, cookies=None):
        return {'anchor_name': ''}

    monkeypatch.setattr(spider, 'get_douyin_web_stream_data', fake_web)
    _patch_stream(monkeypatch)
    monkeypatch.setattr(live_resolve, '_load_douyin_cookies', lambda: '')

    result = _run(live_resolve.resolve('https://live.douyin.com/abc', 'douyin'))
    assert result['is_live'] is False
    assert 'error' in result, f'失败必须带 error 字段，实际: {result}'
    assert '抖音直播检测失败' in result['error']


def test_douyin_exception_becomes_error(monkeypatch):
    """spider 直接抛异常（网络炸/签名炸）→ 同样必须带 error 字段。"""
    async def fake_web(url, proxy_addr=None, cookies=None):
        raise RuntimeError('a_bogus sign broken')

    monkeypatch.setattr(spider, 'get_douyin_web_stream_data', fake_web)
    _patch_stream(monkeypatch)
    monkeypatch.setattr(live_resolve, '_load_douyin_cookies', lambda: '')

    result = _run(live_resolve.resolve('https://live.douyin.com/abc', 'douyin'))
    assert 'error' in result and 'a_bogus sign broken' in result['error']


def test_douyin_offline_room_is_not_error(monkeypatch):
    """正常未开播（接口返回了主播信息）→ is_live=False 且无 error。"""
    async def fake_web(url, proxy_addr=None, cookies=None):
        return {'anchor_name': '侯翠翠', 'status': 4}

    monkeypatch.setattr(spider, 'get_douyin_web_stream_data', fake_web)
    _patch_stream(monkeypatch)
    monkeypatch.setattr(live_resolve, '_load_douyin_cookies', lambda: '')

    result = _run(live_resolve.resolve('https://live.douyin.com/xiguahaochi96', 'douyin'))
    assert result['is_live'] is False
    assert 'error' not in result
    assert result['anchor_name'] == '侯翠翠'


def test_douyin_authorized_cookie_fallback(monkeypatch):
    """匿名失败 → 有授权 cookie 时自动兜底重试，第二次带 cookie。"""
    calls = []

    async def fake_web(url, proxy_addr=None, cookies=None):
        calls.append(cookies)
        if cookies:
            return {'anchor_name': '某人', 'status': 4}
        return {'anchor_name': ''}

    monkeypatch.setattr(spider, 'get_douyin_web_stream_data', fake_web)
    _patch_stream(monkeypatch)
    monkeypatch.setattr(live_resolve, '_load_douyin_cookies', lambda: 'ttwid=abc')

    result = _run(live_resolve.resolve('https://live.douyin.com/abc', 'douyin'))
    assert 'error' not in result
    assert result['anchor_name'] == '某人'
    assert calls == [None, 'ttwid=abc']


def test_load_douyin_cookies_netscape_parsing(monkeypatch, tmp_path):
    """Netscape cookie 文件正确转成请求头字符串；无文件返回空串。"""
    import fupan.settings as settings

    monkeypatch.setattr(settings, 'RUN_DIR', str(tmp_path))
    assert live_resolve._load_douyin_cookies() == ''

    cookie_file = tmp_path / 'douyin_browser_cookies.txt'
    cookie_file.write_text(
        '# Netscape HTTP Cookie File\n'
        '.douyin.com\tTRUE\t/\tTRUE\t1893456000\tttwid\tabc123\n'
        '.douyin.com\tTRUE\t/\tFALSE\t1893456000\tsessionid\txyz\n'
        'bad line\n',
        encoding='utf-8',
    )
    assert live_resolve._load_douyin_cookies() == 'ttwid=abc123; sessionid=xyz'
