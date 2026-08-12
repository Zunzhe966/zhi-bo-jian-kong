# -*- coding: utf-8 -*-
"""QQ 邮箱配置助手：检查/写入 SMTP 配置，并支持从老项目迁移账号。

老项目 `zunzhe-dev` 的 `mcp_server/tools/email_service.py` 只保存了 QQ 邮箱
代码支持与加密密钥，没有保存过实际账号（accounts.json 不存在），因此无法
自动恢复授权码；这里提供账号/授权码导入与本地模拟发送，便于验证链路。
"""
from __future__ import annotations

import json
import smtplib
import sys
from pathlib import Path

from .config_api import get_config, set_config
from .settings import CONFIG_DIR, email_settings, is_yes

QQ_SMTP = 'smtp.qq.com'
QQ_PORT_SSL = 465
QQ_PORT_TLS = 587

LEGACY_EMAIL_SERVICE = (
    Path.home()
    / 'Projects'
    / 'owned'
    / 'zunzhe-dev'
    / 'mcp_server'
    / 'tools'
    / 'email_service.py'
)
LEGACY_ACCOUNTS = LEGACY_EMAIL_SERVICE.parent.parent / 'data' / 'email' / 'accounts.json'


def email_status() -> dict:
    """返回邮箱配置状态与可诊断的错误原因（不回显授权码）。"""
    cfg = email_settings()
    filled = {
        'host': bool(cfg['email_host']),
        'login': bool(cfg['login_email']),
        'auth': bool(cfg['email_pass']),
        'to': bool(cfg['to_email']),
    }
    missing = [k for k, v in filled.items() if not v]
    ok = not missing
    hint = '已配置'
    if missing:
        names = {
            'host': 'SMTP服务器',
            'login': '邮箱登录账号',
            'auth': '发件人密码(授权码)',
            'to': '收件人邮箱',
        }
        hint = '缺少: ' + '、'.join(names[k] for k in missing)
    return {
        'ok': ok,
        'configured': ok,
        'host': cfg['email_host'] or QQ_SMTP,
        'port': cfg['smtp_port'] or (QQ_PORT_SSL if is_yes(cfg['open_ssl']) else QQ_PORT_TLS),
        'ssl': is_yes(cfg['open_ssl']),
        'sender': cfg['sender_email'] or cfg['login_email'],
        'to': cfg['to_email'],
        'masked_login': _mask(cfg['login_email']),
        'masked_to': _mask(cfg['to_email']),
        'missing': missing,
        'hint': hint,
        'legacy_accounts_file': str(LEGACY_ACCOUNTS),
    }


def _mask(value: str) -> str:
    v = (value or '').strip()
    if not v:
        return ''
    if len(v) <= 3:
        return '*' * len(v)
    return v[:2] + '***' + v[-2:]


def import_legacy_account(legacy_file: str = '') -> dict:
    """从老项目 accounts.json 读取 QQ 邮箱账号并写入当前配置。

    注意：老项目密码是 AES-256-GCM 加密的，需要同目录 encryption_key.bin；
    若账号/密钥不存在则返回可读错误，绝不猜测授权码。
    """
    path = Path(legacy_file) if legacy_file else LEGACY_ACCOUNTS
    if not path.exists():
        return {
            'ok': False,
            'error': f'老项目邮箱账号文件不存在: {path}。老项目里只有代码支持，没有实际账号，需要你提供 QQ 邮箱授权码。',
            'file': str(path),
        }
    try:
        data = json.loads(path.read_text(encoding='utf-8'))
    except (OSError, ValueError) as e:
        return {'ok': False, 'error': f'账号文件解析失败: {e}', 'file': str(path)}

    accounts = data if isinstance(data, list) else data.get('accounts', [])
    if not accounts:
        return {'ok': False, 'error': '账号文件为空，没有可迁移的邮箱账号', 'file': str(path)}

    qq = None
    for acc in accounts:
        if not isinstance(acc, dict):
            continue
        addr = str(acc.get('email') or acc.get('email_addr') or '')
        if addr.lower().endswith('@qq.com'):
            qq = acc
            break
    if qq is None:
        qq = accounts[0]

    email = str(qq.get('email') or qq.get('email_addr') or '').strip()
    display = str(qq.get('display_name') or '').strip() or '尊者直播监控'
    encrypted = str(qq.get('_password') or '').strip()
    if not email or not encrypted:
        return {'ok': False, 'error': '账号记录缺少邮箱或密码，无法迁移', 'file': str(path)}

    # 老项目密码为 AES 加密：若能解密则迁移，否则让用户在设置面板填授权码。
    auth = _decrypt_legacy(encrypted)
    if auth is None:
        return {
            'ok': False,
            'error': '找到 QQ 邮箱账号，但授权码是加密存储且无法在本项目解密。请在桌面端「全局设置」重新填写授权码。',
            'email': email,
            'file': str(path),
        }

    _write_email_config(email=email, auth=auth, display=display, to=email)
    return {
        'ok': True,
        'email': email,
        'to': email,
        'hint': '已从老项目迁移 QQ 邮箱账号。',
    }


def _decrypt_legacy(encrypted: str) -> str | None:
    """尝试用老项目的 AES 密钥解密；无密钥或失败一律返回 None。"""
    key_file = (
        Path.home() / 'Projects' / 'owned' / 'zunzhe-dev' / 'backend' / 'data' / 'encryption_key.bin'
    )
    if not key_file.exists():
        return None
    try:
        sys.path.insert(0, str(Path.home() / 'Projects' / 'owned' / 'zunzhe-dev'))
        from backend.security.encrypt import decrypt_api_key
        return decrypt_api_key(encrypted)
    except Exception:
        return None


def write_email_config(
    email: str = '',
    auth: str = '',
    to: str = '',
    display: str = '尊者直播监控',
    host: str = QQ_SMTP,
    port: str = '',
    ssl: str = '是',
) -> dict:
    """写入 QQ 邮箱 SMTP 配置，并启用邮件通知渠道。"""
    email = (email or '').strip()
    auth = (auth or '').strip()
    if not email or not auth:
        return {'ok': False, 'error': '邮箱账号和授权码不能为空'}
    to = (to or email).strip()
    port = port or (str(QQ_PORT_SSL) if is_yes(ssl) else str(QQ_PORT_TLS))
    pairs = [
        ('直播状态推送渠道', '邮箱'),
        ('SMTP邮件服务器', host or QQ_SMTP),
        ('是否使用SMTP服务SSL加密(是/否)', ssl),
        ('SMTP邮件服务器端口', str(port)),
        ('邮箱登录账号', email),
        ('发件人密码(授权码)', auth),
        ('发件人邮箱', email),
        ('发件人显示昵称', display),
        ('收件人邮箱', to),
        ('开播推送开启(是/否)', '是'),
    ]
    errors = []
    for section, key, value in [('推送配置', k, v) for k, v in pairs]:
        try:
            set_config('config', section, key, value)
        except Exception as e:
            errors.append(f'{key}: {e}')
    if errors:
        return {'ok': False, 'error': '写入失败: ' + '; '.join(errors)}
    return {'ok': True, 'email': email, 'to': to, 'host': host, 'port': port}


def send_email_smtp(email: str, auth: str, to: str, title: str, content: str,
                    host: str = QQ_SMTP, port: str = '', ssl: bool = True,
                    display: str = '尊者直播监控') -> dict:
    """真正走 SMTP 发信；测试用（不落盘），成功后返回 sent=True。"""
    if not all([email, auth, to]):
        return {'ok': False, 'error': '邮箱/授权码/收件人不能为空'}
    from msg_push import send_email
    result = send_email(
        email_host=host,
        login_email=email,
        email_pass=auth,
        sender_email=email,
        sender_name=display,
        to_email=to,
        title=title,
        content=content,
        smtp_port=port or str(QQ_PORT_SSL if ssl else QQ_PORT_TLS),
        open_ssl=ssl,
    )
    if result.get('success'):
        return {'ok': True, 'sent': True, 'to': to}
    return {'ok': False, 'sent': False, 'error': 'SMTP 发送失败，请检查邮箱账号/授权码/端口或网络', 'to': to}


def _write_email_config(email: str, auth: str, display: str, to: str) -> None:
    """内部写入（供迁移函数使用，忽略单键失败时抛错）。"""
    write_email_config(email=email, auth=auth, to=to, display=display)
