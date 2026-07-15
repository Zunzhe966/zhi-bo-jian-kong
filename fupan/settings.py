# -*- coding: utf-8 -*-
"""读取 fupan_config.ini 与主程序 config.ini 的配置。"""
import configparser
import sys
from pathlib import Path

PROJECT_DIR = Path(__file__).resolve().parent.parent
CONFIG_DIR = PROJECT_DIR / 'config'
FUPAN_DIR = PROJECT_DIR / 'fupan_output'
ENCODING = 'utf-8-sig'

_YES = ('是', 'yes', 'true', '1', 'on')


def _load(path: Path) -> configparser.ConfigParser:
    # 保持默认的键名小写转换：读取时大小写不敏感，兼容 main.py 的读取方式
    parser = configparser.RawConfigParser()
    if path.exists():
        parser.read(path, encoding=ENCODING)
    return parser


def fupan_config() -> configparser.RawConfigParser:
    return _load(CONFIG_DIR / 'fupan_config.ini')


def main_config() -> configparser.RawConfigParser:
    return _load(CONFIG_DIR / 'config.ini')


def is_yes(value: str) -> bool:
    return str(value).strip().lower() in _YES


def get(cfg, section: str, key: str, default: str = '') -> str:
    try:
        return cfg.get(section, key).strip()
    except (configparser.NoSectionError, configparser.NoOptionError):
        return default


def email_settings() -> dict:
    """复用主程序 [推送配置] 里的 SMTP 账号。"""
    cfg = main_config()
    return {
        'email_host': get(cfg, '推送配置', 'SMTP邮件服务器'),
        'open_ssl': is_yes(get(cfg, '推送配置', '是否使用SMTP服务SSL加密(是/否)', '是')),
        'smtp_port': get(cfg, '推送配置', 'SMTP邮件服务器端口'),
        'login_email': get(cfg, '推送配置', '邮箱登录账号'),
        'email_pass': get(cfg, '推送配置', '发件人密码(授权码)'),
        'sender_email': get(cfg, '推送配置', '发件人邮箱'),
        'sender_name': get(cfg, '推送配置', '发件人显示昵称') or '尊者直播监控',
        'to_email': get(cfg, '推送配置', '收件人邮箱'),
    }


def ensure_project_on_path() -> None:
    if str(PROJECT_DIR) not in sys.path:
        sys.path.insert(0, str(PROJECT_DIR))
    # 让独立运行的流水线也能找到项目自带的 ffmpeg
    import os
    ffmpeg_dir = str(PROJECT_DIR / 'ffmpeg')
    if ffmpeg_dir not in os.environ.get('PATH', ''):
        os.environ['PATH'] = ffmpeg_dir + os.pathsep + os.environ.get('PATH', '')
