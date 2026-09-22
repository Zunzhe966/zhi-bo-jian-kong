# -*- coding: utf-8 -*-
"""回归测试：master.m3u8 变体码率解析。

线上真实故障：
1. `url_to_bandwidth[url]` 对缺失 BANDWIDTH 的变体抛 KeyError，导致直播间
   解析整体失败；
2. 用 `findall(BANDWIDTH)` 与 URL 列表整体 `zip`，一旦某个变体缺少 BANDWIDTH
   就会整体错位，选到错误画质；
3. `_pick_play_urls` 需要兼容调用方拼过 url_prefix 的情况（SOOP 等）。
"""
import sys
import re
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

# 只取纯函数，避免拉起 execjs / httpx 等重依赖。
_src = Path(__file__).resolve().parent.parent / 'src' / 'spider.py'
_text = _src.read_text(encoding='utf-8')
_start = _text.index('_BANDWIDTH_RE = re.compile')
_end = _text.index('async def get_play_url_list')
_ns = {'re': re, 'List': list, 'OptionalStr': str}
exec(_text[_start:_end], _ns)  # noqa: S102
parse_bandwidth = _ns['_parse_m3u8_bandwidth']
pick_play_urls = _ns['_pick_play_urls']


def test_missing_bandwidth_does_not_crash_and_ranks_last():
    """变体缺少 BANDWIDTH 时不得抛 KeyError，且该变体排在最后。"""
    resp = (
        '#EXTM3U\n'
        '#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360\n'
        'url1.m3u8\n'
        '#EXT-X-STREAM-INF:RESOLUTION=1280x720\n'
        'url2_no_bandwidth.m3u8\n'
        '#EXT-X-STREAM-INF:BANDWIDTH=2000000\n'
        'url3.m3u8\n'
    )
    urls = ['https://cdn/' + ln.strip()
            for ln in resp.split('\n') if ln.strip() and not ln.startswith('#')]
    out = pick_play_urls(urls, resp)
    assert out == [
        'https://cdn/url3.m3u8',
        'https://cdn/url1.m3u8',
        'https://cdn/url2_no_bandwidth.m3u8',
    ], out


def test_bandwidth_pairs_with_own_url_not_global_zip():
    """码率必须与自己的 URL 就近配对，不能被全局 zip 错位。"""
    resp = (
        '#EXTM3U\n'
        '#EXT-X-STREAM-INF:BANDWIDTH=500000\n'
        'low.m3u8\n'
        '#EXT-X-STREAM-INF:BANDWIDTH=3000000\n'
        'high.m3u8\n'
        '#EXT-X-STREAM-INF:BANDWIDTH=1500000\n'
        'mid.m3u8\n'
    )
    mapping = parse_bandwidth(resp)
    assert mapping['low.m3u8'] == 500000
    assert mapping['high.m3u8'] == 3000000
    assert mapping['mid.m3u8'] == 1500000


def test_sorted_descending():
    resp = (
        '#EXTM3U\n'
        '#EXT-X-STREAM-INF:BANDWIDTH=500000\n'
        'low.m3u8\n'
        '#EXT-X-STREAM-INF:BANDWIDTH=3000000\n'
        'high.m3u8\n'
    )
    urls = ['https://cdn/low.m3u8', 'https://cdn/high.m3u8']
    assert pick_play_urls(urls, resp) == ['https://cdn/high.m3u8', 'https://cdn/low.m3u8']


def test_url_prefix_still_matches_by_suffix():
    """调用方拼了 url_prefix 后仍要能通过后缀匹配拿到正确码率。"""
    resp = (
        '#EXTM3U\n'
        '#EXT-X-STREAM-INF:BANDWIDTH=900000\n'
        'a/stream.m3u8\n'
        '#EXT-X-STREAM-INF:BANDWIDTH=1800000\n'
        'b/stream.m3u8\n'
    )
    prefix = 'https://cdn.example.com'
    urls = [prefix + '/a/stream.m3u8', prefix + '/b/stream.m3u8']
    assert pick_play_urls(urls, resp)[0] == prefix + '/b/stream.m3u8'


def test_empty_input_is_safe():
    assert parse_bandwidth('') == {}
    assert pick_play_urls([], '') == []


if __name__ == '__main__':
    test_missing_bandwidth_does_not_crash_and_ranks_last()
    test_bandwidth_pairs_with_own_url_not_global_zip()
    test_sorted_descending()
    test_url_prefix_still_matches_by_suffix()
    test_empty_input_is_safe()
    print('通过：m3u8 码率解析不再 KeyError、不再错位、兼容 url_prefix 拼接')
