# -*- coding: utf-8 -*-
"""通过 JSON 读写 config.ini / fupan_config.ini，供 CLI 与 MCP 使用。"""
from __future__ import annotations

import configparser
import re
from pathlib import Path

from .settings import CONFIG_DIR, ENCODING

MAIN_CONFIG = CONFIG_DIR / 'config.ini'
FUPAN_CONFIG = CONFIG_DIR / 'fupan_config.ini'

_SECRET_KEYS = (
    '发件人密码', '授权码', 'api密钥', 'cookie', 'Cookie', '令牌', 'token', '密钥', 'password', 'pass',
)
_SECRET_SECTIONS = ('Cookie',)

# 智能体可通过 MCP/CLI 修改的配置项说明（大模型由外部智能体自带，此处仅配置工具行为）
CONFIG_SCHEMA = {
    'config.ini': {
        '录制设置': {
            '原画|超清|高清|标清|流畅': '默认录制画质：原画/蓝光/超清/高清/标清/流畅',
            '循环时间(秒)': '监控轮询间隔',
            '视频保存格式ts|mkv|flv|mp4|mp3音频|m4a音频': '保存格式',
        },
        '推送配置': {
            '直播状态推送渠道': '填「邮箱」启用邮件通知',
            'SMTP邮件服务器': '如 smtp.qq.com',
            '是否使用SMTP服务SSL加密(是/否)': '是/否',
            'SMTP邮件服务器端口': 'SSL 通常 465',
            '邮箱登录账号': '发件邮箱',
            '发件人密码(授权码)': 'SMTP 授权码（secret）',
            '发件人邮箱': '发件邮箱',
            '发件人显示昵称': '邮件显示名',
            '收件人邮箱': '通知收件人',
            '开播推送开启(是/否)': '是/否',
            'server酱SendKey': 'Server酱³ SendKey，推微信，走443不怕代理（secret）',
            'pushplus_token': 'PushPlus token，走443不怕代理（secret）',
            'bark推送接口链接': 'iOS Bark 推送地址',
            'ntfy推送地址': 'ntfy 订阅地址',
        },
        'Cookie': {
            '抖音cookie': '录制抖音必填（secret）',
            'b站cookie': 'B站部分功能需要（secret）',
        },
    },
    'fupan_config.ini': {
        '转写': {
            '是否启用转写(是/否)': '是/否',
            'whisper模型': 'tiny/base/small/medium/large-v3',
            '运行设备': 'auto/cpu/cuda',
        },
        '知识提炼': {
            '是否启用知识提炼(是/否)': '是/否',
            'api地址': 'OpenAI 兼容 API（由连接的智能体配置）',
            'api密钥': 'API Key（secret，由智能体配置）',
            '模型': '模型名',
        },
        '切片': {
            '是否启用切片(是/否)': '是/否',
            '最多片段数': '整数',
        },
        '复盘邮件': {
            '录制完成后发送复盘笔记(是/否)': '是/否',
            '发现新视频时发送通知(是/否)': '是/否',
        },
        '视频监控': {
            '检查间隔秒': '轮询间隔',
            '自动下载新视频(是/否)': '是/否',
        },
    },
}


def _is_secret(section: str, key: str) -> bool:
    if section in _SECRET_SECTIONS:
        return True
    key_l = key.lower()
    return any(s.lower() in key_l for s in _SECRET_KEYS)


def _load(path: Path) -> configparser.RawConfigParser:
    parser = configparser.RawConfigParser()
    parser.optionxform = str  # 保留 ini 键名大小写（SMTP 等）
    if path.exists():
        parser.read(path, encoding=ENCODING)
    return parser


def _save(path: Path, parser: configparser.RawConfigParser) -> None:
    with path.open('w', encoding=ENCODING) as f:
        parser.write(f)


def _file_key(name: str) -> Path:
    if name in ('config', 'config.ini', 'main'):
        return MAIN_CONFIG
    if name in ('fupan', 'fupan_config.ini', 'fupan_config'):
        return FUPAN_CONFIG
    raise ValueError(f'未知配置文件: {name}，可用 config / fupan_config')


def get_config(file: str = 'all', section: str = '', key: str = '', mask_secrets: bool = True) -> dict:
    """读取配置，返回 JSON 友好结构。"""
    files = []
    if file in ('all', '', '*'):
        files = [('config.ini', MAIN_CONFIG), ('fupan_config.ini', FUPAN_CONFIG)]
    else:
        files = [(Path(_file_key(file)).name, _file_key(file))]

    result = {}
    for fname, path in files:
        parser = _load(path)
        sections = [section] if section else parser.sections()
        file_data = {}
        for sec in sections:
            if sec not in parser:
                continue
            sec_data = {}
            keys = [key] if key else parser.options(sec)
            for k in keys:
                if not parser.has_option(sec, k):
                    continue
                val = parser.get(sec, k).strip()
                if mask_secrets and _is_secret(sec, k) and val:
                    val = '***'
                sec_data[k] = val
            if sec_data:
                file_data[sec] = sec_data
        if file_data:
            result[fname] = file_data
    return result


def _match_existing_key(parser: configparser.RawConfigParser, section: str, key: str) -> str:
    """大小写不敏感地找已有键，避免同名不同大小写的重复键弄崩 main.py 的解析。"""
    if parser.has_section(section):
        for existing in parser.options(section):
            if existing.lower() == key.lower():
                return existing
    return key


def set_config(file: str, section: str, key: str, value: str) -> dict:
    path = _file_key(file)
    parser = _load(path)
    if not parser.has_section(section):
        parser.add_section(section)
    real_key = _match_existing_key(parser, section, key)
    parser.set(section, real_key, value)
    _save(path, parser)
    return {'file': path.name, 'section': section, 'key': real_key, 'set': True}


def set_config_bulk(updates: dict, mask_secrets: bool = True) -> dict:
    """updates: {"config.ini": {"推送配置": {"收件人邮箱": "..."}}}"""
    changed = []
    for fname, sections in updates.items():
        path = _file_key(fname.replace('.ini', ''))
        parser = _load(path)
        for section, pairs in sections.items():
            if not parser.has_section(section):
                parser.add_section(section)
            for key, value in pairs.items():
                if mask_secrets and value == '***':
                    continue  # 跳过占位符，不覆盖原密钥
                real_key = _match_existing_key(parser, section, key)
                parser.set(section, real_key, str(value))
                changed.append({'file': path.name, 'section': section, 'key': real_key})
        _save(path, parser)
    return {'changed': changed, 'count': len(changed)}


def config_schema() -> dict:
    return {
        'description': '尊者直播监控工具配置。大模型由外部智能体自带，通过 MCP/CLI 写入 [知识提炼] 即可。',
        'files': CONFIG_SCHEMA,
        'mcp_hint': '智能体用 set_config 写入 api地址/api密钥/模型，或用 bulk 一次写入。',
    }
