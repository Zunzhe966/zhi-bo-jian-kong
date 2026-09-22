# -*- coding: utf-8 -*-
"""回归测试：复盘流水线的媒体文件发现与结果输出。

线上真实故障：
1. `_find_candidates` 在「请求的路径本身就是 .mp4」时，会把同一个路径重复
   加进候选列表（media 与 media.with_suffix('.mp4') 是同一文件）；
2. 分段录制产生多个分片时，只静默取第一个，其余分片永不进入复盘且无任何提示；
3. `pipeline.main()` 丢弃 `run_pipeline` 的返回值，导致「三个开关全关」这类
   提前返回分支不打印任何内容，Rust 侧只看到空 output_tail，无法诊断。
"""
import json
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from fupan.pipeline import _find_candidates


def test_exact_match_not_duplicated():
    """请求 .mp4 时不能被重复添加两次。"""
    tmp = Path(tempfile.mkdtemp())
    exact = tmp / '视频.mp4'
    exact.touch()
    out = _find_candidates(exact)
    assert out == [exact], out


def test_multi_segment_returns_all_candidates():
    """分段录制模板应返回全部分片，供调用方判断并提示，而不是只留一个。"""
    tmp = Path(tempfile.mkdtemp())
    for i in range(3):
        (tmp / f'直播_000{i}.mp4').touch()
    out = _find_candidates(tmp / '直播_%04d.ts')
    assert len(out) == 3, out
    assert [p.name for p in out] == ['直播_0000.mp4', '直播_0001.mp4', '直播_0002.mp4']


def test_mp4_preferred_over_ts():
    """转码完成后 mp4 应优先于 ts。"""
    tmp = Path(tempfile.mkdtemp())
    (tmp / 'a.mp4').touch()
    (tmp / 'a.ts').touch()
    out = _find_candidates(tmp / 'a.ts')
    assert out[0].suffix == '.mp4', out


def test_missing_file_returns_empty():
    tmp = Path(tempfile.mkdtemp())
    assert _find_candidates(tmp / '不存在.mp4') == []


def test_main_emits_json_summary_when_all_switches_off():
    """三个开关全关时，stdout 必须给出可解析的结果，而不是空输出。"""
    proc = subprocess.run(
        [sys.executable, '-m', 'fupan.pipeline',
         '--save_file_path', '/nonexistent-media-for-test.mp4',
         '--record_name', '回归测试',
         '--transcribe', '否', '--distill', '否', '--clips', '否'],
        cwd=str(ROOT), capture_output=True, text=True, timeout=120,
    )
    assert proc.returncode == 0, proc.stderr
    last = [ln for ln in proc.stdout.strip().splitlines() if ln.strip()][-1]
    payload = json.loads(last)
    assert 'pipeline_summary' in payload, proc.stdout
    assert '关闭' in payload['pipeline_summary']['status']
    print('main() stdout:', last)


if __name__ == '__main__':
    test_exact_match_not_duplicated()
    test_multi_segment_returns_all_candidates()
    test_mp4_preferred_over_ts()
    test_missing_file_returns_empty()
    test_main_emits_json_summary_when_all_switches_off()
    print('通过：媒体发现去重/分段可见/画质优先级/缺失安全/流水线结果可解析')
