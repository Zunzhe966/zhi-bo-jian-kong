# -*- coding: utf-8 -*-
"""验证通知偏好：默认邮箱优先，每主播可覆盖为微信/不通知。"""
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import fupan.notify as notify
import fupan.monitor_prefs as prefs

tmp = Path(tempfile.mkdtemp())
prefs.PREFS_FILE = tmp / '监控偏好.json'
notify._push_channels = lambda: {'serverchan': '', 'pushplus': '', 'bark': '', 'ntfy': ''}  # 不配微信等通道
notify.main_config = lambda: None  # 无全局渠道配置
notify.get = lambda cfg, section, key, default='': default

# 默认跟随全局：邮箱
assert notify._channels_for_pref('跟随全局') == {'邮箱'}, notify._channels_for_pref('跟随全局')
# 每主播覆盖
assert notify._channels_for_pref('微信') == {'微信'}
assert notify._channels_for_pref('邮箱') == {'邮箱'}
assert notify._channels_for_pref('不通知') is None

prefs.set_notify('https://example.com/live', '邮箱', '博主甲')
assert prefs.get_notify('https://example.com/live') == '邮箱'
prefs.set_notify('https://example.com/live', '微信', '博主甲')
assert prefs.get_notify('https://example.com/live') == '微信'

print('通过：默认邮箱，每主播可覆盖为微信/不通知')
