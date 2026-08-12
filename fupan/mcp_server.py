# -*- coding: utf-8 -*-
"""尊者直播监控 MCP Server — 任何支持 MCP 的智能体（Cursor、OpenCode 等）均可连接。

启动（stdio，供 Cursor 等配置）:
  .venv/bin/python -m fupan.mcp_server          # macOS/Linux
  .venv/Scripts/python.exe -m fupan.mcp_server  # Windows

Cursor 配置见项目根目录 mcp.json.example
"""
from __future__ import annotations

import json
from typing import Any

from mcp.server.fastmcp import FastMCP

from . import cli as _cli
from .config_api import config_schema, get_config, set_config, set_config_bulk

mcp = FastMCP(
    'live-monitor',
    instructions=(
        '尊者直播监控工具：自动监控直播开播/发视频、录制、转写、复盘、邮件通知。'
        '支持 40+ 直播平台。大模型由调用方智能体自带，用 set_config 写入 [知识提炼] 的 api地址/api密钥/模型。'
        '所有工具返回 JSON 字符串。'
    ),
)


def _json(data: Any) -> str:
    if isinstance(data, dict) and 'ok' not in data:
        data = {**data, 'ok': True}
    return json.dumps(data, ensure_ascii=False)


@mcp.tool()
def live_status() -> str:
    """查看整体状态：后台进程、监控列表、最近录制、复盘产物、路径。"""
    return _json(_cli.status())


@mcp.tool()
def live_add(url: str, name: str = '', quality: str = '') -> str:
    """添加直播监控。url 为直播间链接；name 为主播备注；quality 可选：原画/蓝光/超清/高清/标清/流畅。"""
    remark = f'主播: {name}' if name else ''
    return _json(_cli.add_live(url, remark, quality))


@mcp.tool()
def live_remove(url: str) -> str:
    """暂停指定直播间的监控（不删除，可重新启用）。"""
    return _json(_cli.remove_live(url))


@mcp.tool()
def live_list() -> str:
    """列出所有直播监控项。"""
    return _json({'rooms': _cli.list_live()})


@mcp.tool()
def live_check(url: str) -> str:
    """检查单个直播间的监控状态（是否在列表、是否启用、服务是否运行）。"""
    return _json(_cli.check_live(url))


@mcp.tool()
def video_add(url: str, name: str, download: str = '') -> str:
    """添加博主发视频监控（B站 space 或 RSS 地址）。download: ''=跟随全局 / '是'=发现新视频自动下载 / '否'=只通知不下载。"""
    return _json(_cli.add_video(url, name, download))


@mcp.tool()
def video_download(url: str, name: str = '手动下载') -> str:
    """立即下载单个视频链接到 downloads/短视频/<名称>/ 并跑复盘流水线。"""
    return _json(_cli.download_video(url, name))


@mcp.tool()
def blogger_videos(url: str, count: int = 30) -> str:
    """列出某博主已发布的历史视频（B站空间或RSS地址），返回 [{id,title,url,date,duration}]，供批量选择下载。"""
    return _json(_cli.blogger_videos(url, str(count)))


@mcp.tool()
def video_remove(url: str) -> str:
    """移除发视频监控。"""
    return _json(_cli.remove_video(url))


@mcp.tool()
def video_list() -> str:
    """列出所有发视频监控。"""
    return _json({'sources': _cli.list_video()})


@mcp.tool()
def video_check_now() -> str:
    """立即检查一轮是否有新视频发布。"""
    from .video_monitor import check_once
    return _json({'ok': True, 'new_videos': check_once()})


@mcp.tool()
def service_start() -> str:
    """启动直播监控 + 发视频监控后台服务。"""
    return _json(_cli.start())


@mcp.tool()
def service_stop() -> str:
    """停止全部后台服务。"""
    return _json(_cli.stop())


@mcp.tool()
def config_get(file: str = 'all', section: str = '', key: str = '') -> str:
    """读取配置。file: all/config/fupan_config；密钥字段返回 ***。"""
    return _json(get_config(file, section, key, mask_secrets=True))


@mcp.tool()
def config_set(file: str, section: str, key: str, value: str) -> str:
    """写入单个配置项。file: config 或 fupan_config；section/key 见 config_schema。"""
    try:
        return _json(set_config(file, section, key, value))
    except Exception as e:
        return _json({'ok': False, 'error': str(e)})


@mcp.tool()
def config_set_bulk(updates_json: str) -> str:
    """批量写入配置。updates_json 为 JSON 字符串，格式见 config_schema 工具。"""
    try:
        updates = json.loads(updates_json)
        return _json(set_config_bulk(updates))
    except Exception as e:
        return _json({'ok': False, 'error': str(e)})


@mcp.tool()
def config_schema_tool() -> str:
    """返回所有可配置项的结构说明（含邮箱、Cookie、转写、知识提炼等）。"""
    return _json(config_schema())


@mcp.tool()
def notes_list(name: str = '') -> str:
    """列出复盘产物（转写、笔记、切片路径）。"""
    return _json({'notes': _cli.notes(name)})


@mcp.tool()
def process_media(file_path: str, anchor_name: str = '') -> str:
    """对任意音视频文件跑转写/提炼/切片流水线。"""
    from .pipeline import run_pipeline
    return _json(run_pipeline(file_path, anchor_name))


@mcp.tool()
def test_email() -> str:
    """发送测试邮件，验证 SMTP 配置。"""
    from .notify import _send
    ok = _send('【尊者直播监控】MCP 邮箱测试', '收到此邮件说明 SMTP 配置正确。')
    return _json({'ok': ok})


def main() -> None:
    mcp.run(transport='stdio')


if __name__ == '__main__':
    main()
