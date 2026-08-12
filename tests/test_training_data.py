# -*- coding: utf-8 -*-
"""验证训练数据导出：合并转写 + 知识点为 JSONL，且带主播/日期/时间戳元数据。"""
import json
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import fupan.training_data as td

root = Path(tempfile.mkdtemp())
td.FUPAN_DIR = root
td.DEFAULT_OUTPUT = root / 'training_data.jsonl'

session = root / '主播甲' / '20260812_1015'
session.mkdir(parents=True)
stem = '20260812_1015'

(session / f'{stem}.转写.json').write_text(json.dumps({
    'source': '/tmp/demo.mp4',
    'language': 'zh',
    'segments': [
        {'start': 0.0, 'end': 3.2, 'text': '大家好，今天讲递归。'},
        {'start': 3.2, 'end': 8.0, 'text': '递归就是函数调用自己。'},
    ],
}, ensure_ascii=False), encoding='utf-8')
(session / f'{stem}.知识点.json').write_text(json.dumps([
    {'标题': '递归定义', '内容': '函数调用自己即递归', '开始秒': 3.2, '结束秒': 8.0, '价值': 4},
], ensure_ascii=False), encoding='utf-8')

result = td.export_training_data()
assert result['ok'], result
assert result['sessions'] == 1, result
assert result['knowledge'] == 1, result
assert result['transcript_segments'] == 2, result
assert result['records'] == 3, result

lines = result['output'] and Path(result['output']).read_text(encoding='utf-8').splitlines()
records = [json.loads(line) for line in lines]
k = next(r for r in records if r['type'] == 'knowledge')
assert k['anchor'] == '主播甲' and k['date'] and k['start_sec'] == 3.2
assert k['title'] == '递归定义' and k['value'] == 4
t = next(r for r in records if r['type'] == 'transcript')
assert t['anchor'] == '主播甲' and t['text'].startswith('大家好')

print('通过：转写+知识点正确合并导出 JSONL，元数据完整')
