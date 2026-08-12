# -*- coding: utf-8 -*-
"""验证 QQ 邮箱配置助手：状态诊断、写入配置、SMTP 发送（模拟，不发真实邮件）。"""
import json
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import fupan.email_setup as es
import fupan.config_api as api
import fupan.settings as settings

tmp = Path(tempfile.mkdtemp())
es.CONFIG_DIR = tmp
api.MAIN_CONFIG = tmp / 'config.ini'
settings.CONFIG_DIR = tmp

# 未配置时能指出缺什么
status = es.email_status()
assert not status['configured'], status
assert '授权码' in status['hint'], status

# 写入 QQ 邮箱模板
result = es.write_email_config(email='test@qq.com', auth='fake-auth', to='me@qq.com')
assert result['ok'], result
status = es.email_status()
assert status['configured'], status
assert status['host'] == 'smtp.qq.com' and status['to'] == 'me@qq.com'
assert 'fake-auth' not in json.dumps(status, ensure_ascii=False), '状态里不能泄露授权码'

# 用模拟 SMTP 验证发送函数参数（不真正联网）
sent = {}
def fake_send_email(**kwargs):
    sent.update(kwargs)
    return {'success': ['me@qq.com'], 'error': []}
import fupan.notify as notify
notify._send_email = fake_send_email

# 配置读取正常
cfg = settings.email_settings()
assert cfg['email_host'] == 'smtp.qq.com'
assert cfg['login_email'] == 'test@qq.com'
assert cfg['email_pass'] == 'fake-auth'

# 缺授权码不能写入
bad = es.write_email_config(email='test@qq.com', auth='')
assert not bad['ok'] and '授权码' in bad['error']

print('通过：邮箱状态诊断、QQ SMTP 模板写入、敏感字段不回显 全部正常')
