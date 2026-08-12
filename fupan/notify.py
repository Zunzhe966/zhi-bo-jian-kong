# -*- coding: utf-8 -*-
"""开播/新视频/复盘通知。支持多通道并带自动兜底：

邮件(SMTP) 在启用了 TUN 模式的代理（如 FlClash）下会被拦截，因此额外支持
走 443 HTTPS 的推送通道（Server酱→微信、PushPlus、Bark、ntfy），这些在代理
环境下依然可达。任一通道成功即视为通知成功。
"""
import json
import re
import urllib.request
import urllib.error
import urllib.parse

from .settings import email_settings, ensure_project_on_path, main_config, get, is_yes, FUPAN_DIR
from .monitor_prefs import get_notify, NOTIFY_OPTIONS

ensure_project_on_path()
from msg_push import send_email, pushplus, bark, ntfy  # noqa: E402

NOTIFY_QUEUE = FUPAN_DIR / 'run' / 'pending_notify.json'

# 不走系统代理，规避应用层代理设置；但 TUN 模式仍在网络层拦截，故 HTTPS 通道更可靠
_no_proxy_opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def _clean(value: str) -> str:
    """过滤基础项目里遗留的占位符，但保留真实的 Server酱/PushPlus 密钥。"""
    v = (value or '').strip()
    if not v:
        return ''
    low = v.lower()
    # 明显是示例占位符（整串 x、或 ntfy/bark 示例 URL）
    if low in ('sctxxxxxxxx', 'sctxxxxxxxx...') or re.fullmatch(r'sct[x*]+', low):
        return ''
    if 'xxx' in low and (low.count('x') > 6 or 'xxxxx' in low):
        return ''
    if 'key/' in low and 'xxx' in low:
        return ''
    return v


def _push_channels() -> dict:
    """从 [推送配置] 读取各 HTTPS 推送通道的凭据。"""
    cfg = main_config()
    return {
        'serverchan': _clean(get(cfg, '推送配置', 'server酱SendKey')),
        # 与 main.py（直播开播推送）共用同一个 key，配一次两处都生效
        'pushplus': _clean(get(cfg, '推送配置', 'pushplus推送token')),
        'bark': _clean(get(cfg, '推送配置', 'bark推送接口链接')),
        'ntfy': _clean(get(cfg, '推送配置', 'ntfy推送地址')),
    }


def _serverchan(sendkey: str, title: str, content: str) -> bool:
    """Server酱³：推送到微信，走 443 HTTPS，代理环境下可达。"""
    try:
        url = f'https://sctapi.ftqq.com/{sendkey}.send'
        data = urllib.parse.urlencode({'title': title[:32], 'desp': content}).encode('utf-8')
        req = urllib.request.Request(url, data=data)
        resp = _no_proxy_opener.open(req, timeout=15)
        result = json.loads(resp.read().decode('utf-8'))
        ok = result.get('code') == 0 or result.get('data', {}).get('pushid')
        if not ok:
            print(f'Server酱推送失败: {result}')
        return bool(ok)
    except Exception as e:
        print(f'Server酱推送失败: {e}')
        return False


def _send_email(title: str, content: str) -> bool:
    cfg = email_settings()
    if not (cfg['email_host'] and cfg['login_email'] and cfg['email_pass'] and cfg['to_email']):
        return False
    result = send_email(
        email_host=cfg['email_host'], login_email=cfg['login_email'],
        email_pass=cfg['email_pass'], sender_email=cfg['sender_email'] or cfg['login_email'],
        sender_name=cfg['sender_name'], to_email=cfg['to_email'],
        title=title, content=content, smtp_port=cfg['smtp_port'], open_ssl=cfg['open_ssl'],
    )
    return bool(result['success'])


def _queue_local(title: str, content: str) -> None:
    """写入待推送队列，由 Electron 桌面端弹出系统通知（代理/额度失败时的兜底）。"""
    try:
        from .app_prefs import get_prefs
        if not get_prefs().get('localNotify', True):
            return
    except Exception:
        pass
    import time
    NOTIFY_QUEUE.parent.mkdir(parents=True, exist_ok=True)
    items = []
    if NOTIFY_QUEUE.exists():
        try:
            items = json.loads(NOTIFY_QUEUE.read_text(encoding='utf-8'))
        except json.JSONDecodeError:
            items = []
    if not isinstance(items, list):
        items = []
    items.append({
        'id': f'{time.time():.3f}',
        'title': (title or '尊者直播监控')[:64],
        'body': (content or '')[:500],
    })
    NOTIFY_QUEUE.write_text(json.dumps(items[-30:], ensure_ascii=False), encoding='utf-8')


def _send_channels(title: str, content: str, channels: set[str]) -> bool:
    """按指定通道集合发送，channels 元素：微信/邮箱/PushPlus/Bark/ntfy"""
    sent = False
    tried = []
    ch = _push_channels()

    if '微信' in channels and ch['serverchan']:
        tried.append('微信')
        sent = _serverchan(ch['serverchan'], title, content) or sent
    if 'PushPlus' in channels and ch['pushplus']:
        tried.append('PushPlus')
        sent = bool(pushplus(ch['pushplus'], title, content)['success']) or sent
    if 'Bark' in channels and ch['bark']:
        tried.append('Bark')
        sent = bool(bark(ch['bark'], title=title, content=content)['success']) or sent
    if 'ntfy' in channels and ch['ntfy']:
        tried.append('ntfy')
        sent = bool(ntfy(ch['ntfy'], title=title, content=content)['success']) or sent
    if '邮箱' in channels:
        tried.append('邮箱')
        sent = _send_email(title, content) or sent

    if not tried:
        print('未配置所选通知通道，跳过发送')
    elif not sent:
        print(f'所选通道均发送失败：{"/".join(tried)}')
    return sent


def _channels_for_pref(pref: str) -> set[str] | None:
    """返回要使用的通道集合；None 表示不通知。"""
    pref = (pref or '跟随全局').strip()
    if pref == '不通知':
        return None
    if pref == '微信':
        return {'微信'}
    if pref == '邮箱':
        return {'邮箱'}
    if pref == 'PushPlus':
        return {'PushPlus'}
    # 跟随全局：邮箱优先（用户默认用邮箱），其余已配通道作为兜底
    ch = _push_channels()
    configured = {
        '微信' if ch['serverchan'] else None,
        'PushPlus' if ch['pushplus'] else None,
        'Bark' if ch['bark'] else None,
        'ntfy' if ch['ntfy'] else None,
    }
    out = {c for c in configured if c}
    if is_yes(get(main_config(), '推送配置', '直播状态推送渠道', '')) or '邮箱' in get(main_config(), '推送配置', '直播状态推送渠道', ''):
        out.add('邮箱')
    return out or {'邮箱'}


def send_notification(title: str, content: str, source_url: str = '', anchor_name: str = '') -> bool:
    """按该监控项的「通知=」偏好发送（可在监控偏好.json 按 URL 配置）。"""
    pref = get_notify(source_url, anchor_name)
    channels = _channels_for_pref(pref)
    if channels is None:
        print(f'[{anchor_name or source_url}] 通知已设为「不通知」，跳过')
        return False
    _queue_local(title, content)
    return _send_channels(title, content, channels)


def _send(title: str, content: str) -> bool:
    """测试/无 URL 时：走全局（所有已配通道）。"""
    channels = _channels_for_pref('跟随全局')
    return _send_channels(title, content, channels or set())


def send_fupan_notes(anchor_name: str, notes: str, clip_files: list[str], source_url: str = '') -> bool:
    clip_text = '\n'.join(f'- {p}' for p in clip_files) if clip_files else '（未生成切片）'
    content = f'{notes}\n\n---\n精华切片文件：\n{clip_text}'
    return send_notification(f'【直播复盘】{anchor_name} 的复盘笔记已生成', content, source_url, anchor_name)


def send_new_video_alert(anchor_name: str, video_title: str, video_url: str, source_url: str = '') -> bool:
    content = f'{anchor_name} 发布了新视频：\n\n标题：{video_title}\n链接：{video_url}\n\n复盘流水线将自动处理。'
    return send_notification(f'【新视频】{anchor_name} 发布了新内容', content, source_url or video_url, anchor_name)


def send_live_alert(anchor_name: str, live_url: str, content: str | None = None) -> bool:
    body = content or f'{anchor_name} 开播了！\n\n直播间：{live_url}\n\n正在自动录制。'
    return send_notification(f'【开播】{anchor_name} 开始直播', body, live_url, anchor_name)


def send_test() -> bool:
    _queue_local('【尊者直播监控】通知测试', '收到这条消息说明通知通道配置成功。')
    return _send('【尊者直播监控】通知测试', '收到这条消息说明通知通道配置成功。')


if __name__ == '__main__':
    ok = send_test()
    print('测试通知发送成功' if ok else '测试通知发送失败，请检查推送配置')
