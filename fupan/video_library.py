# -*- coding: utf-8 -*-
"""博主视频库：用 SQLite 记录每个视频的元数据（标题/封面/日期/时长）与
「是否已下载」状态。界面据此显示封面缩略图和"已下载"标记，下载成功即打标，
不靠扫描文件名判断，换机器也不丢记录。
"""
from __future__ import annotations

import json
import sqlite3
import time
from pathlib import Path

from .settings import FUPAN_DIR

DB_PATH = FUPAN_DIR / 'video_library.db'

SCHEMA = '''
CREATE TABLE IF NOT EXISTS videos (
    id            TEXT PRIMARY KEY,   -- 平台内唯一ID（BV号/视频ID/guid）
    platform      TEXT NOT NULL,      -- bilibili / douyin / kuaishou / rss ...
    url           TEXT NOT NULL,
    title         TEXT NOT NULL DEFAULT '',
    cover         TEXT NOT NULL DEFAULT '',
    cover_cache   TEXT NOT NULL DEFAULT '',  -- 本地封面缓存文件路径
    published_at  TEXT NOT NULL DEFAULT '',  -- 发布日期 YYYY-MM-DD
    duration      INTEGER NOT NULL DEFAULT 0,
    anchor        TEXT NOT NULL DEFAULT '',  -- 博主名
    source_url    TEXT NOT NULL DEFAULT '',  -- 监控源地址
    downloaded    INTEGER NOT NULL DEFAULT 0,
    downloaded_at TEXT NOT NULL DEFAULT '',
    first_seen    TEXT NOT NULL DEFAULT '',
    updated_at    TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_videos_source ON videos(source_url);
CREATE INDEX IF NOT EXISTS idx_videos_anchor ON videos(anchor);
'''

COVER_CACHE_DIR = FUPAN_DIR / 'covers'

# 建表只需成功执行一次；之后每次调用都重跑 executescript(SCHEMA) 纯属浪费
# （CREATE TABLE IF NOT EXISTS 仍要走一遍解析+取写锁，在这个库被 CLI/下载/
# 列表多处并发访问时，无谓的写锁会抬高 database is locked 的概率）。
_schema_ready = False


def _connect() -> sqlite3.Connection:
    DB_PATH.parent.mkdir(parents=True, exist_ok=True)
    # 这个库会被旧 Python CLI 与下载/列表调用并发访问；默认 5 秒等待太短，
    # 且 DELETE 日志模式在读写交错时更容易让用户看到 database is locked。
    # 与 Rust 主库保持一致：等待 15 秒，并让 SQLite 使用 WAL。
    conn = sqlite3.connect(DB_PATH, timeout=15)
    conn.execute('PRAGMA journal_mode=WAL')
    conn.execute('PRAGMA synchronous=NORMAL')
    conn.row_factory = sqlite3.Row
    return conn


def init_db() -> None:
    global _schema_ready
    COVER_CACHE_DIR.mkdir(parents=True, exist_ok=True)
    if _schema_ready:
        return
    with _connect() as conn:
        conn.executescript(SCHEMA)
    _schema_ready = True


def _now() -> str:
    return time.strftime('%Y-%m-%d %H:%M:%S')


def upsert_video(video: dict, anchor: str = '', source_url: str = '') -> dict:
    """把一条视频元数据写入视频库；返回带 downloaded 标记的记录。"""
    init_db()
    vid = video.get('id') or video.get('guid') or video.get('url')
    if not vid:
        return dict(video, downloaded=0)
    platform = video.get('platform') or 'rss'
    cover_cache = video.get('cover_cache') or ''
    if video.get('cover') and not cover_cache:
        cover_cache = cache_cover(video['cover'], str(vid))
    row = {
        'id': str(vid),
        'platform': platform,
        'url': video.get('url', ''),
        'title': video.get('title', ''),
        'cover': video.get('cover', ''),
        'cover_cache': cover_cache,
        'published_at': video.get('date', video.get('published_at', '')),
        'duration': int(video.get('duration') or 0),
        'anchor': video.get('anchor', anchor),
        'source_url': video.get('source_url', source_url),
        'updated_at': _now(),
    }
    with _connect() as conn:
        existing = conn.execute('SELECT downloaded, downloaded_at, first_seen FROM videos WHERE id=?',
                                (row['id'],)).fetchone()
        if existing:
            row['downloaded'] = existing['downloaded']
            row['downloaded_at'] = existing['downloaded_at']
            row['first_seen'] = existing['first_seen']
            conn.execute(
                '''UPDATE videos SET platform=?, url=?, title=?, cover=?, cover_cache=?,
                   published_at=?, duration=?, anchor=?, source_url=?, updated_at=?
                   WHERE id=?''',
                (row['platform'], row['url'], row['title'], row['cover'], row['cover_cache'],
                 row['published_at'], row['duration'], row['anchor'], row['source_url'],
                 row['updated_at'], row['id']))
        else:
            row['downloaded'] = 0
            row['downloaded_at'] = ''
            row['first_seen'] = _now()
            conn.execute(
                '''INSERT INTO videos (id, platform, url, title, cover, cover_cache,
                   published_at, duration, anchor, source_url, downloaded, downloaded_at,
                   first_seen, updated_at)
                   VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)''',
                (row['id'], row['platform'], row['url'], row['title'], row['cover'],
                 row['cover_cache'], row['published_at'], row['duration'], row['anchor'],
                 row['source_url'], row['downloaded'], row['downloaded_at'],
                 row['first_seen'], row['updated_at']))
    return row


def mark_downloaded(video_id: str, downloaded: bool = True) -> None:
    init_db()
    with _connect() as conn:
        conn.execute('UPDATE videos SET downloaded=?, downloaded_at=?, updated_at=? WHERE id=?',
                     (1 if downloaded else 0, _now() if downloaded else '', _now(), video_id))


def is_downloaded(video_id: str) -> bool:
    init_db()
    with _connect() as conn:
        row = conn.execute('SELECT downloaded FROM videos WHERE id=?', (video_id,)).fetchone()
    return bool(row and row['downloaded'])


def get_videos(source_url: str = '', anchor: str = '', downloaded_only: bool = False) -> list[dict]:
    init_db()
    sql = 'SELECT * FROM videos WHERE 1=1'
    params: list = []
    if source_url:
        sql += ' AND source_url=?'
        params.append(source_url)
    if anchor:
        sql += ' AND anchor=?'
        params.append(anchor)
    if downloaded_only:
        sql += ' AND downloaded=1'
    sql += ' ORDER BY published_at DESC, updated_at DESC'
    with _connect() as conn:
        rows = conn.execute(sql, params).fetchall()
    return [dict(r) for r in rows]


def cache_cover(cover_url: str, video_id: str) -> str:
    """把封面下载到本地缓存，返回本地路径；失败返回原 URL。"""
    if not cover_url:
        return ''
    init_db()
    ext = '.jpg'
    if '.png' in cover_url.lower():
        ext = '.png'
    dest = COVER_CACHE_DIR / f'{video_id}{ext}'
    if dest.exists() and dest.stat().st_size > 0:
        return str(dest)
    try:
        import httpx
        resp = httpx.get(cover_url, headers={'User-Agent': 'Mozilla/5.0'}, timeout=15, follow_redirects=True)
        if resp.status_code == 200 and resp.content:
            dest.write_bytes(resp.content)
            return str(dest)
    except Exception:
        pass
    return cover_url


def attach_downloaded(videos: list[dict], source_url: str = '', anchor: str = '') -> list[dict]:
    """给列表批量附上 downloaded 标记（与库中记录比对）。

    封面缓存路径也用 dict 一次建好：旧实现对每个视频都 next(...) 线性扫描
    整个 known 列表，视频一多（几百条）就是 O(n²)，而这里恰好在「点开主播
    详情/刷新列表」的热路径上，会肉眼可见地卡顿。
    """
    init_db()
    known = get_videos(source_url=source_url, anchor=anchor)
    map_id = {str(v['id']): v['downloaded'] for v in known}
    map_cover = {str(v['id']): v['cover_cache'] for v in known}
    out = []
    for v in videos:
        vid = str(v.get('id') or v.get('guid') or v.get('url') or '')
        row = dict(v)
        row['downloaded'] = 1 if map_id.get(vid) else 0
        row['cover_cache'] = map_cover.get(vid, '')
        out.append(row)
    return out
