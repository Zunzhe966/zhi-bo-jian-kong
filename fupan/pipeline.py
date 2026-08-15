# -*- coding: utf-8 -*-
"""复盘流水线：录制完成的文件 -> 转写 -> 知识提炼 -> 切片 -> 邮件。

两种触发方式：
1. 主程序 config.ini 开启「录制完成后执行自定义脚本」，命令填：
   .venv/bin/python -m fupan.pipeline           # macOS/Linux
   .venv\\Scripts\\python.exe -m fupan.pipeline  # Windows
   （主程序会自动附加 --record_name / --save_file_path 等参数）
2. 手动/智能体调用：python -m fupan.pipeline --save_file_path "视频路径" --record_name "主播名"
"""
import argparse
import json
import re
from pathlib import Path

from .settings import FUPAN_DIR, fupan_config, get, is_yes, ensure_project_on_path

ensure_project_on_path()
from .transcribe import transcribe_file
from .distill import distill_transcript
from .clip import cut_clips
from .notify import send_fupan_notes


_MEDIA_SUFFIXES = ('.mp4', '.ts', '.mkv', '.flv', '.mov', '.webm', '.mp3', '.m4a', '.wav')


def _find_candidates(media: Path) -> list[Path]:
    """按路径找实际录制文件。分段录制路径是 ffmpeg 模板（如 xxx_%03d.ts），
    实际文件是 xxx_000.ts；转码又会把 ts 换成 mp4，都要兼容。"""
    import re
    # ffmpeg 分段模板占位符 %03d / %d 换成通配符
    stem_pattern = re.sub(r'%0?\d*d', '*', media.stem)
    glob_pattern = re.sub(r'\*+', '*', f'{stem_pattern}*.*')
    candidates = []
    for path in (media, media.with_suffix('.mp4')):
        if '%' not in path.name and path.exists():
            candidates.append(path)
    if not candidates and media.parent.exists():
        candidates = [p for p in media.parent.glob(glob_pattern)
                      if p.suffix.lower() in _MEDIA_SUFFIXES]
    # mp4 优先（ts 转码完成后原文件会被删）
    candidates.sort(key=lambda p: (p.suffix.lower() != '.mp4', p.name))
    return candidates


def _resolve_media(media_path: str, timeout: int = 900) -> Path:
    """录制的 ts 可能正在被异步转成 mp4（转完删原文件），等文件落定后返回实际路径。"""
    import time
    media = Path(media_path)
    deadline = time.time() + timeout
    last = (None, -1)
    while time.time() < deadline:
        candidates = _find_candidates(media)
        if candidates:
            target = candidates[0]
            size = target.stat().st_size
            if size > 0 and (str(target), size) == last:
                return target
            last = (str(target), size)
        time.sleep(5)
    candidates = _find_candidates(media)
    if candidates:
        return candidates[0]
    raise FileNotFoundError(f'找不到录制文件: {media_path}')


def run_pipeline(media_path: str, record_name: str = '',
                 transcribe: str = '', distill: str = '', clips: str = '') -> dict:
    """Rust 核心显式传入三个独立开关；为空时读取配置，兼容旧 CLI 直接调用。"""
    cfg = fupan_config()
    switches = {
        'transcribe': is_yes(transcribe or get(cfg, '转写', '是否启用转写(是/否)', '是')),
        'distill': is_yes(distill or get(cfg, '知识提炼', '是否启用知识提炼(是/否)', '是')),
        'clips': is_yes(clips or get(cfg, '切片', '是否启用切片(是/否)', '是')),
    }
    if not any(switches.values()):
        # 三个环节全部关闭：不解析媒体、不加载模型，直接返回，符合
        # “关闭 = 该环节完全不运行”。调用方通常也不会在这种状态入队。
        return {'status': '复盘流水线全部关闭，跳过'}
    media = _resolve_media(media_path)

    record_name = record_name or media.stem
    out_dir = FUPAN_DIR / record_name / media.stem
    out_dir.mkdir(parents=True, exist_ok=True)
    summary = {'media': str(media), 'output_dir': str(out_dir)}

    transcript = None
    if switches['transcribe']:
        print(f'[1/4] 转写: {media.name}')
        transcript = transcribe_file(str(media), str(out_dir))
    else:
        summary['transcribe'] = '跳过'
    if not transcript or not transcript['segments']:
        summary['status'] = '转写为空或未启用，流水线结束'
        print(summary['status'])
        return summary
    summary['segments'] = len(transcript['segments'])

    distilled = None
    if switches['distill']:
        print('[2/4] 知识提炼')
        distilled = distill_transcript(transcript, str(out_dir), title=record_name)
    else:
        summary['distill'] = '跳过'
    if not distilled:
        summary['status'] = '未配置大模型接口，已保留转写文本，流水线结束'
        print(summary['status'])
        return summary
    summary['points'] = len(distilled['points'])

    clips = []
    if switches['clips']:
        print('[3/4] 精华切片')
        clips = cut_clips(str(media), distilled['points'], str(out_dir))
    else:
        summary['clips'] = '跳过'
    summary['clips'] = clips

    print('[4/4] 邮件通知')
    cfg = fupan_config()
    if is_yes(get(cfg, '复盘邮件', '录制完成后发送复盘笔记(是/否)', '是')):
        summary['email_sent'] = send_fupan_notes(record_name, distilled['notes'], clips)

    summary['status'] = '完成'
    (out_dir / '流水线结果.json').write_text(
        json.dumps(summary, ensure_ascii=False, indent=2), encoding='utf-8')
    print(f'复盘完成，输出目录: {out_dir}')
    return summary


def main():
    parser = argparse.ArgumentParser(description='直播复盘流水线')
    parser.add_argument('--save_file_path', required=True, help='录制文件路径')
    parser.add_argument('--record_name', default='', help='主播名/任务名')
    # 主程序自定义脚本钩子还会传这些参数，接收但不使用
    parser.add_argument('--save_type', default='')
    parser.add_argument('--split_video_by_time', default='')
    parser.add_argument('--converts_to_mp4', default='')
    parser.add_argument('--transcribe', default='')
    parser.add_argument('--distill', default='')
    parser.add_argument('--clips', default='')
    args, _unknown = parser.parse_known_args()
    # 主程序传来的 record_name 形如 "序号2 失控进化"，去掉序号前缀
    record_name = re.sub(r'^序号\d+\s*', '', args.record_name).strip()
    summary = run_pipeline(args.save_file_path, record_name,
                           transcribe=args.transcribe, distill=args.distill,
                           clips=args.clips)


if __name__ == '__main__':
    main()
