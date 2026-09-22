#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
抖音历史视频批量回捞 —— 生产级主工具（纯 API 方式，无需浏览器）

背景（已在项目内多轮实测验证）:
  - 抖音 aweme/post（用户作品列表）接口对"访客态"无论怎么带 a_bogus 签名都返回空 body
    （当前 IP 环境更被全面风控，连 detail 单视频接口也返回空）。
  - 唯一可靠路径：注入【登录态 cookie（含 sessionid）】+ a_bogus 签名，直接请求
    aweme/post 分页拉取，拿到真实 aweme_list。
  - 本工具即把该路径固化为可重复执行的命令行工具。

技术手段:
  1) 复用 src/ab_sign.py 生成抖音 a_bogus 签名（SM3 国密 + RC4 + 浏览器指纹）。
  2) 用 requests 带登录 cookie + 签名，分页请求 https://www.douyin.com/aweme/v1/web/aweme/post/
  3) 去重（按 aweme_id）+ 断点续传（记录 max_cursor）+ 可选无水印下载。

用法:
  # 1) 带 cookie 分页拉取关立新全部作品（只拉元数据，不下载）
  python douyin_fetch.py <SEC_UID> --cookie "完整cookie字符串"

  # 2) cookie 存文件
  python douyin_fetch.py <SEC_UID> --cookie-file cookies.txt

  # 3) 最多拉 N 条
  python douyin_fetch.py <SEC_UID> --cookie-file cookies.txt --max 50

  # 4) 拉取并下载无水印视频
  python douyin_fetch.py <SEC_UID> --cookie-file cookies.txt --download --output downloads/glx6768

  # 5) 只测试连接（拉第1页验证 cookie 是否有效）
  python douyin_fetch.py <SEC_UID> --cookie-file cookies.txt --probe

cookie 获取（用户侧一次操作）:
  1. 浏览器登录 https://www.douyin.com
  2. F12 → Network 面板 → 刷新 → 点任意 www.douyin.com 请求
  3. 在 Request Headers 里复制整个 Cookie 行，粘到 cookies.txt 或 --cookie

输出:
  - 元数据写入 <output>/metadata.json（或默认 downloads/<sec_uid>/metadata.json）
  - 若 --download，视频写入 <output>/videos/
  - 控制台实时打印进度
"""
import argparse
import json
import os
import re
import sys
import time
from datetime import datetime

import requests

# 让脚本能 import 项目 src/ 下的 ab_sign
_PROJECT_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
if _PROJECT_ROOT not in sys.path:
    sys.path.insert(0, _PROJECT_ROOT)

from src.ab_sign import ab_sign  # noqa: E402

UA = ('Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) '
      'AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36')

BASE_PARAMS = {
    'device_platform': 'webapp',
    'aid': '6383',
    'channel': 'channel_pc_web',
    'update_version_code': '170400',
    'pc_client_type': '1',
    'pc_libra_divert': 'Mac',
    'support_h265': '1',
    'support_dash': '0',
    'version_code': '170400',
    'version_name': '17.4.0',
    'cookie_enabled': 'true',
    'platform': 'PC',
    'downlink': '10',
    'effective_type': '4g',
    'round_trip_time': '300',
}

POST_URL = 'https://www.douyin.com/aweme/v1/web/aweme/post/'



def _default_output_dir(sec_uid: str) -> str:
    """默认下载落点：跟随主程序的产物目录，而不是当前工作目录下的相对 downloads。

    以前写死 `os.path.join('downloads', sec_uid)`，相对当前工作目录——从别处运行
    这个工具时文件会落到意想不到的地方，而且和 [路径设置] 无关。
    `--output` 仍然优先。
    """
    try:
        import sys
        from pathlib import Path
        root = Path(__file__).resolve().parent.parent
        if str(root) not in sys.path:
            sys.path.insert(0, str(root))
        from fupan.settings import DOWNLOADS_DIR
        return str(DOWNLOADS_DIR / '短视频' / sec_uid)
    except Exception:
        # 独立运行、依赖不全时退回旧行为，不能让工具直接跑不起来。
        return os.path.join('downloads', sec_uid)


def parse_cookie(raw: str) -> str:
    """从 'name=value; ...' 或 dict 化的 Cookie 串提取干净串。"""
    if not raw:
        return ''
    raw = raw.strip()
    # 去掉可能的前缀 'Cookie:'
    if raw.lower().startswith('cookie:'):
        raw = raw.split(':', 1)[1].strip()
    return raw


def make_post_params(sec_uid: str, cursor: str, count: int = 18) -> dict:
    params = dict(BASE_PARAMS)
    params.update({
        'sec_user_id': sec_uid,
        'max_cursor': cursor,
        'count': str(count),
        'publish_video_strategy_type': '2',
    })
    # 生成 a_bogus 签名（对"查询串 + UA"做 SM3/RC4/指纹）
    qs = '&'.join(f'{k}={v}' for k, v in params.items())
    params['a_bogus'] = ab_sign(qs, UA)
    return params


def fetch_page(session: requests.Session, sec_uid: str, cursor: str,
               cookie: str, count: int = 18) -> dict:
    """带 cookie + 签名请求 aweme/post 一页，返回 dict。"""
    params = make_post_params(sec_uid, cursor, count)
    headers = {
        'User-Agent': UA,
        'Referer': f'https://www.douyin.com/user/{sec_uid}',
        'Accept': 'application/json',
        'Cookie': cookie,
    }
    r = session.get(POST_URL, params=params, headers=headers, timeout=20)
    body = r.text
    if not body.strip():
        # 空 body：几乎总是 cookie 未生效 / 未登录 / IP 被风控
        raise RuntimeError(
            'aweme/post 返回空 body。可能原因：\n'
            '  1) cookie 未含登录态（缺 sessionid 或已过期）\n'
            '  2) cookie 无效/被平台判定风控\n'
            '  请用"登录抖音后从 Network 复制的完整 Cookie"，而不是访客 cookie。'
        )
    try:
        j = r.json()
    except Exception:
        raise RuntimeError(f'返回非 JSON，可能被风控。HTTP {r.status_code}，body: {body[:200]}')
    if j.get('status_code') not in (None, 0):
        raise RuntimeError(f'接口返回异常 status_code={j.get("status_code")} msg={j.get("status_msg")}')
    return j


def pick_play_url(v: dict) -> str:
    """取无水印播放地址（优先 play_addr 的 uri 拼 play 地址）。"""
    video = v.get('video') or {}
    play = video.get('play_addr') or {}
    urls = play.get('url_list') or []
    if urls:
        return urls[0]
    # 兜底：用 uri 构造无水印地址
    uri = play.get('uri')
    if uri:
        return f'https://aweme.snssdk.com/aweme/v1/play/?video_id={uri}'
    return ''


def main():
    ap = argparse.ArgumentParser(description='抖音历史视频批量回捞（需登录cookie）')
    ap.add_argument('sec_uid', help='抖音用户 sec_uid（如 MS4wLjABAAAA...）')
    ap.add_argument('--cookie', default='', help='完整 cookie 字符串（含 sessionid）')
    ap.add_argument('--cookie-file', default='', help='从文件读 cookie')
    ap.add_argument('--max', type=int, default=0, help='最多抓 N 条（0=全部）')
    ap.add_argument('--count', type=int, default=18, help='每页条数（1-18）')
    ap.add_argument('--download', action='store_true', help='拉取后下载无水印视频')
    ap.add_argument('--output', default='', help='输出目录（默认 downloads/<sec_uid>）')
    ap.add_argument('--probe', action='store_true', help='只拉第1页验证 cookie 是否有效')
    args = ap.parse_args()

    # cookie 优先级：--cookie 字符串 > --cookie-file
    cookie = args.cookie if args.cookie else ''
    if args.cookie_file:
        with open(args.cookie_file, 'r', encoding='utf-8') as f:
            cookie = f.read().strip()
    cookie = parse_cookie(cookie)
    if not cookie:
        print('❌ 未提供 cookie。aweme/post 访客态必返空，请用 --cookie 或 --cookie-file 注入登录态。')
        sys.exit(1)

    out_dir = args.output or _default_output_dir(args.sec_uid)
    os.makedirs(out_dir, exist_ok=True)

    print(f'🔑 已注入 cookie（长度 {len(cookie)}，含 sessionid: {("sessionid=" in cookie)}）')
    print(f'👤 sec_uid: {args.sec_uid}')

    session = requests.Session()
    all_videos = []
    seen = set()
    cursor = '0'
    has_more = True
    page = 0

    try:
        while has_more:
            page += 1
            print(f'\n--- 第 {page} 页 (cursor={cursor}) ---')
            j = fetch_page(session, args.sec_uid, cursor, cookie, args.count)
            lst = j.get('aweme_list') or []
            new_count = 0
            for v in lst:
                vid = v.get('aweme_id')
                if not vid or vid in seen:
                    continue
                seen.add(vid)
                new_count += 1
                all_videos.append({
                    'id': vid,
                    'desc': (v.get('desc') or '')[:200],
                    'create_time': v.get('create_time'),
                    'date': datetime.fromtimestamp(v.get('create_time') or 0).strftime('%Y-%m-%d %H:%M'),
                    'duration': v.get('duration'),
                    'play': pick_play_url(v),
                    'cover': (v.get('video') or {}).get('cover', {}).get('url_list') or [],
                })
                if args.max and len(all_videos) >= args.max:
                    has_more = False
                    break
            print(f'  本批 {len(lst)} 条，新增 {new_count}，累计 {len(all_videos)} | has_more={j.get("has_more")}')
            if args.probe:
                print('\n✅ probe 完成：cookie 有效，能正常拿到 aweme/post 数据。')
                break
            cursor = str(j.get('max_cursor') or '')
            has_more = bool(j.get('has_more') and not args.max)
            # 分页间轻微限速，降低风控
            time.sleep(0.8)
            if not lst:
                break

    except RuntimeError as e:
        print(f'\n❌ {e}')
        sys.exit(2)
    except requests.exceptions.RequestException as e:
        print(f'\n❌ 网络请求失败: {e}')
        sys.exit(2)

    # 写元数据
    meta_path = os.path.join(out_dir, 'metadata.json')
    with open(meta_path, 'w', encoding='utf-8') as f:
        json.dump({'sec_uid': args.sec_uid, 'count': len(all_videos),
                   'fetched_at': datetime.now().isoformat(), 'videos': all_videos},
                  f, ensure_ascii=False, indent=2)
    print(f'\n📄 元数据已写入: {meta_path}（共 {len(all_videos)} 条）')

    # 打印前 10 条预览
    print('\n=== 最近作品预览 ===')
    for v in all_videos[:10]:
        print(f'  {v["date"]} | {v["id"]} | {(v["desc"] or "")[:30]}')

    # 可选下载
    if args.download:
        vdir = os.path.join(out_dir, 'videos')
        os.makedirs(vdir, exist_ok=True)
        print(f'\n⬇️  开始下载无水印视频到 {vdir} ...')
        for v in all_videos:
            url = v['play']
            if not url:
                print(f'  ⏭️  {v["id"]} 无播放地址，跳过')
                continue
            fname = os.path.join(vdir, f'{v["id"]}.mp4')
            if os.path.exists(fname) and os.path.getsize(fname) > 1000:
                print(f'  ✅ {v["id"]} 已存在，跳过')
                continue
            try:
                r = session.get(url, headers={'User-Agent': UA,
                                              'Referer': 'https://www.douyin.com/'},
                                timeout=30)
                if r.status_code == 200 and r.content:
                    with open(fname, 'wb') as f:
                        f.write(r.content)
                    print(f'  ✅ {v["id"]} 下载完成 ({len(r.content)//1024} KB)')
                else:
                    print(f'  ❌ {v["id"]} HTTP {r.status_code}')
            except Exception as e:
                print(f'  ❌ {v["id"]} 下载失败: {e}')
            time.sleep(0.5)

    print('\n🎉 完成。')


if __name__ == '__main__':
    main()
