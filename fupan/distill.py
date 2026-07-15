# -*- coding: utf-8 -*-
"""调用 OpenAI 兼容大模型接口，从转写文本中提炼知识点和精华片段。"""
import json
import re
from pathlib import Path

from .settings import fupan_config, get, is_yes

_SYSTEM_PROMPT = (
    '你是一名直播/视频内容复盘助手。用户会给你一段带时间戳的直播转写文本，'
    '直播中有大量寒暄、闲聊、重复的废话，你的任务是把真正有价值的知识提炼出来。'
)

_CHUNK_PROMPT = """以下是直播转写文本的一部分（格式为 [开始秒,结束秒] 内容）：

{text}

请只输出 JSON（不要输出其他文字），结构如下：
{{
  "知识点": [
    {{"标题": "一句话概括", "内容": "详细讲了什么，观点/方法/结论", "开始秒": 123.0, "结束秒": 456.0, "价值": 1到5的整数}}
  ]
}}
只保留有实际信息量的内容（方法论、经验、数据、观点、干货教学），寒暄/带货口播/无意义互动一律丢弃。"""

_SUMMARY_PROMPT = """以下是同一场直播提炼出的全部知识点（JSON）：

{points}

请写一份中文复盘笔记（Markdown 格式），包含：
1. 「本场核心内容」：3~5 句话总结这场直播讲了什么；
2. 「知识点清单」：按价值从高到低列出每个知识点，标注对应的时间段（时:分:秒）；
3. 「一句话行动建议」：如果观众只能记住一件事，是什么。
直接输出 Markdown，不要额外解释。"""


def _client():
    cfg = fupan_config()
    base_url = get(cfg, '知识提炼', 'api地址')
    api_key = get(cfg, '知识提炼', 'api密钥')
    if not is_yes(get(cfg, '知识提炼', '是否启用知识提炼(是/否)', '是')) or not base_url or not api_key:
        return None, None
    from openai import OpenAI
    return OpenAI(base_url=base_url, api_key=api_key), get(cfg, '知识提炼', '模型', 'deepseek-chat')


def _chat(client, model: str, system: str, user: str) -> str:
    resp = client.chat.completions.create(
        model=model,
        messages=[{'role': 'system', 'content': system}, {'role': 'user', 'content': user}],
        temperature=0.3,
    )
    return resp.choices[0].message.content or ''


def _parse_json(text: str) -> dict:
    match = re.search(r'\{.*\}', text, re.S)
    if not match:
        return {}
    try:
        return json.loads(match.group(0))
    except json.JSONDecodeError:
        return {}


def distill_transcript(transcript: dict, output_dir: str, title: str = '') -> dict | None:
    """输入 transcribe 的结果，输出 知识点.json 和 复盘笔记.md。"""
    client, model = _client()
    if client is None:
        return None

    cfg = fupan_config()
    chunk_size = int(get(cfg, '知识提炼', '分块大小', '6000') or 6000)

    lines = [f'[{s["start"]},{s["end"]}] {s["text"]}' for s in transcript['segments']]
    chunks, current = [], ''
    for line in lines:
        if len(current) + len(line) > chunk_size and current:
            chunks.append(current)
            current = ''
        current += line + '\n'
    if current:
        chunks.append(current)

    points = []
    for chunk in chunks:
        data = _parse_json(_chat(client, model, _SYSTEM_PROMPT, _CHUNK_PROMPT.format(text=chunk)))
        for p in data.get('知识点', []):
            if isinstance(p, dict) and p.get('标题'):
                points.append(p)

    points.sort(key=lambda p: -int(p.get('价值', 3)))
    out_dir = Path(output_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    stem = Path(transcript['source']).stem

    (out_dir / f'{stem}.知识点.json').write_text(
        json.dumps(points, ensure_ascii=False, indent=2), encoding='utf-8')

    notes = _chat(client, model, _SYSTEM_PROMPT,
                  _SUMMARY_PROMPT.format(points=json.dumps(points, ensure_ascii=False)))
    header = f'# {title or stem} 复盘笔记\n\n'
    (out_dir / f'{stem}.复盘笔记.md').write_text(header + notes, encoding='utf-8')

    return {'points': points, 'notes': header + notes}
