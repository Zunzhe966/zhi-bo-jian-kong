#!/usr/bin/env python3
"""历史产物目录迁移：旧布局 → 新布局（P1 目录合并）。

新布局（产品手册第 3 章，三条铁律）：
    <产物根>/<主播名>/直播/<日期> <场次>/<场次>.mp4 + .转写.txt + .转写.json
    <产物根>/<主播名>/短视频/<视频标题>/<视频标题>.mp4 + .转写.txt + .转写.json

旧布局（合并前）：
    <产物根>/downloads/短视频/<目录>/...          → {主播}/短视频/<目录>/
    <产物根>/downloads/<平台>直播/<主播>/...      → {主播}/直播/
    <产物根>/fupan_output/<标题>/...              → {主播}/短视频/<标题>/（并入）
    <产物根>/fupan_output/<主播>/<标题>/...       → {主播}/短视频/<标题>/（并入）
    <产物根>/fupan_output/<主播>/<场次>/...       → {主播}/直播/<场次>/（并入）

匹配算法（关键修正）：
    downloads/短视频、fupan_output 的目录名都已被 Rust clean_name() 洗过。
    所以必须先把 DB 标题、目录名都 clean_name() 再比，才能对上。
    命中不了时，若目录名恰好等于某个已知主播名（如「侯翠翠」「测试UP主」），
    说明这是「未知标题被兜底落到主播名目录」，按主播名处理。

安全约定（严禁违反）：
    - 默认 dry-run，只打印映射报告，绝不移动任何文件。
    - 必须显式传 --apply 才执行；先备份产物根，再执行。
    - 只 `shutil.move`，从不删除源目录里的内容；同名冲突自动加 _2 后缀。
    - 认不出的目录一律列进「未识别」，保持原地不动。
"""

from __future__ import annotations

import argparse
import json
import re
import shutil
import sqlite3
import sys
from dataclasses import dataclass, field
from pathlib import Path

# 对齐 Rust clean_name()：逐字符替换非法字符，再裁掉首尾下划线。
# 与 zunzhe-core/src/download.rs / live_monitor.rs 的 clean_name 完全一致。
UNSAFE_CHARS = set('/\\:*?"<>|&#.,，。~！· ')
RESERVED_NAMES = {
    "con", "prn", "aux", "nul",
    *(f"com{i}" for i in range(1, 10)),
    *(f"lpt{i}" for i in range(1, 10)),
}

# 各平台直播子目录名（downloads/<平台>直播/<主播>/ 的「<平台>直播」）。
LIVE_SUBDIR_RE = re.compile(r"^(B站|抖音|快手|斗鱼|虎牙|其他)直播$")
# fupan_output 下的系统目录，不属于任何主播。
FUPAN_SYSTEM_DIRS = {"run", "logs", "covers", "backup"}
# 下载文件名前缀 `[B站] `（download.rs 的 platform_tag 加的）。
PLATFORM_PREFIX_RE = re.compile(r"^\[(B站|抖音|快手|斗鱼|虎牙|其他)\]\s*")
# yt-dlp 画质后缀 `.f100113` / `.f30280` 等（音视频分流残留）。
QUALITY_SUFFIX_RE = re.compile(r"\.f\d+$")
# 媒体扩展名。
MEDIA_EXTS = (".mp4", ".mkv", ".webm", ".flv", ".m4a", ".mp3", ".aac", ".ts")


def _match_key(text: str) -> str:
    """统一比较键：去掉 [平台] 前缀、.fXXXXX 画质后缀、扩展名，再 clean_name。"""
    return clean_name(strip_media_name(text))


def strip_media_name(filename: str) -> str:
    """把下载文件名还原成「纯标题」：去掉扩展名 + 画质后缀 + [平台] 前缀。"""
    name = filename
    low = name.lower()
    for ext in MEDIA_EXTS:
        if low.endswith(ext):
            name = name[: -len(ext)]
            break
    name = QUALITY_SUFFIX_RE.sub("", name)
    name = PLATFORM_PREFIX_RE.sub("", name)
    return name.strip()


def clean_name(text: str) -> str:
    """对齐 Rust clean_name()：替换非法字符 + 裁首尾下划线 + 空值兜底。"""
    src = str(text or "").strip()
    out = "".join("_" if ch in UNSAFE_CHARS else ch for ch in src)
    trimmed = out.strip("_")
    return trimmed or "空白昵称"


def safe_dir_name(text: str) -> str:
    """对齐 Rust safe_dir_name()：clean_name 之上再躲开 Windows 保留设备名。"""
    cleaned = clean_name(text)
    return f"{cleaned}_" if cleaned.lower() in RESERVED_NAMES else cleaned


def unique_path(target: Path) -> Path:
    """目标已存在时加 _2/_3 后缀，绝不覆盖已有产物。"""
    if not target.exists():
        return target
    stem, suffix = target.stem, target.suffix
    for n in range(2, 1000):
        candidate = target.with_name(f"{stem}_{n}{suffix}")
        if not candidate.exists():
            return candidate
    raise RuntimeError(f"无法为 {target} 找到不冲突的名字")


@dataclass
class AnchorIndex:
    """标题/地址 → 主播名 的映射索引（清洗后 key）。"""

    # clean_name(title) → 主播名（只收 anchor 非空的）
    by_clean_title: dict[str, str] = field(default_factory=dict)
    # source_url → 主播名（来自 DB / 配置）
    by_source_url: dict[str, str] = field(default_factory=dict)
    # clean_name(title) → DB 原始标题（回填场次名用）
    original_title: dict[str, str] = field(default_factory=dict)
    # 所有已知主播名（clean 后），用于兜底判断
    known_anchors: set[str] = field(default_factory=set)

    def anchor_for_title(self, dir_name: str) -> str:
        """用 DB 里的标题反查主播名。"""
        return self.by_clean_title.get(clean_name(dir_name), "")

    def knows_title(self, dir_name: str) -> bool:
        key = clean_name(dir_name)
        return key in self.by_clean_title or key in self.original_title

    def is_known_anchor(self, dir_name: str) -> bool:
        return clean_name(dir_name) in self.known_anchors


def _alias_lookup(source_url: str, aliases: dict[str, str]) -> str:
    """按整条 url 或 url 末尾 UID 在 aliases 里找主播名。"""
    if not aliases:
        return ""
    if source_url in aliases:
        return aliases[source_url]
    uid = source_url.rstrip("/").split("/")[-1]
    return aliases.get(uid, "")


def _read_videos(db_path: Path) -> list[tuple[str, str, str, str]]:
    """只读 DB，返回 (title, anchor, source_url, platform)。"""
    if not db_path.exists():
        return []
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        return conn.execute(
            "SELECT title, anchor, source_url, platform FROM videos"
        ).fetchall()
    finally:
        conn.close()


def build_anchor_index(
    project_dir: Path, db_path: Path, aliases: dict[str, str] | None = None
) -> AnchorIndex:
    """从 DB + 配置文件收集 主播名 映射。DB 只读，绝不写入。

    aliases：{source_url 或 空间 UID: 主播名}，用于给「DB 里 anchor 为空、
    但确实属于某个主播」的源补名字（如一次性下载、没进监控列表的 UP主）。
    """
    index = AnchorIndex()
    aliases = aliases or {}

    for title, anchor, source_url, _platform in _read_videos(db_path):
        title = (title or "").strip()
        anchor = (anchor or "").strip()
        source_url = (source_url or "").strip()

        if title:
            index.original_title[clean_name(title)] = title
        # anchor 为空时，尝试用 alias 补名（按整条 url 或末尾 UID 匹配）。
        if not anchor and source_url:
            anchor = _alias_lookup(source_url, aliases)
        if title and anchor:
            index.by_clean_title[clean_name(title)] = anchor
        if source_url and anchor and source_url not in index.by_source_url:
            index.by_source_url[source_url] = anchor
        if anchor:
            index.known_anchors.add(clean_name(anchor))

    # 视频监控列表.ini：`url,备注名[,下载=是/否]`
    ini = project_dir / "config" / "视频监控列表.ini"
    if ini.exists():
        for line in ini.read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            parts = [p.strip() for p in line.split(",")]
            if len(parts) >= 2 and parts[0] and parts[1]:
                url, name = parts[0], parts[1]
                index.by_source_url.setdefault(url, name)
                index.known_anchors.add(clean_name(name))

    # URL_config.ini：`画质,直播地址,主播名`
    url_cfg = project_dir / "config" / "URL_config.ini"
    if url_cfg.exists():
        for line in url_cfg.read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if not line or line.startswith((";", "#")):
                continue
            parts = [p.strip() for p in line.split(",")]
            if len(parts) >= 3 and parts[1] and parts[2]:
                index.by_source_url.setdefault(parts[1], parts[2])
                index.known_anchors.add(clean_name(parts[2]))

    # 监控偏好.json：`{url: {name: ...}}`
    prefs = project_dir / "config" / "监控偏好.json"
    if prefs.exists():
        try:
            data = json.loads(prefs.read_text(encoding="utf-8"))
        except json.JSONDecodeError:
            data = {}
        for url, cfg in (data or {}).items():
            name = (cfg or {}).get("name") if isinstance(cfg, dict) else ""
            if url and name:
                index.by_source_url.setdefault(url, str(name))
                index.known_anchors.add(clean_name(str(name)))

    return index


@dataclass
class Move:
    src: Path
    dst: Path
    reason: str


@dataclass
class Plan:
    moves: list[Move] = field(default_factory=list)
    unknown: list[tuple[Path, str]] = field(default_factory=list)
    skipped: list[tuple[Path, str]] = field(default_factory=list)
    # 空目录（原样删除，不搬走）。
    empties: list[Path] = field(default_factory=list)


def _has_products(d: Path) -> bool:
    """目录里有转写/视频产物才算「有东西」。"""
    try:
        return any(p.is_file() for p in d.rglob("*"))
    except OSError:
        return False


def plan_short_videos(root: Path, index: AnchorIndex) -> Plan:
    """downloads/短视频/* → {产物根}/{主播}/短视频/{标题}/...

    旧布局有两种形态交织：
      A. downloads/短视频/{标题}/{标题}.mp4        ← 标题各占一个目录
      B. downloads/短视频/{主播}/{标题}.mp4 ...     ← 兜底把一堆视频平铺进主播目录
    统一按「每个视频一个标题目录」归位。标题目录名以磁盘上的视频名为准。
    """
    plan = Plan()
    src_root = root / "downloads" / "短视频"
    if not src_root.is_dir():
        return plan

    for d in sorted(p for p in src_root.iterdir() if p.is_dir()):
        name = d.name
        # 形态 B：目录名恰好是已知主播名且里面有平铺媒体文件 → 逐文件拆开。
        if index.is_known_anchor(name):
            anchor = clean_name(name)
            for f in sorted(d.iterdir()):
                if f.is_dir():
                    if f.name.startswith("_"):
                        # _残缺文件_待确认删除 之类，原样搬走，不拆。
                        plan.moves.append(
                            Move(f, root / safe_dir_name(anchor) / "短视频" / f.name,
                                 f"残留目录({anchor})")
                        )
                    continue
                if f.suffix.lower() not in MEDIA_EXTS:
                    continue
                title = strip_media_name(f.name) or f.stem
                dst = root / safe_dir_name(anchor) / "短视频" / title / f"{title}{f.suffix}"
                plan.moves.append(Move(f, dst, f"短视频({anchor}/{title})"))
            continue

        # 形态 A：目录名是视频标题。
        anchor = index.anchor_for_title(name)
        if not anchor:
            # 兜底：目录名对不上时，用目录里媒体文件名反查。
            anchor, title = _anchor_from_dir_files(index, d)
            if anchor:
                dst = root / safe_dir_name(anchor) / "短视频" / title
                plan.moves.append(Move(d, dst, f"短视频({anchor}/{title})"))
                continue
            plan.unknown.append((d, "短视频：DB/配置查不到主播名"))
            continue
        dst = root / safe_dir_name(anchor) / "短视频" / name
        if dst.resolve() == d.resolve():
            plan.skipped.append((d, "已在目标位置"))
            continue
        plan.moves.append(Move(d, dst, f"短视频({anchor}/{name})"))
    return plan


def planned_video_map(sv_plan: Plan) -> dict[tuple[str, str], Path]:
    """从短视频计划建立 {匹配键: 目标标题目录}，供 fupan 转写并入。

    匹配键 = (主播名, 标题去平台前缀/画质后缀/扩展名 + clean)。
    """
    m: dict[tuple[str, str], Path] = {}
    for mv in sv_plan.moves:
        if not mv.reason.startswith("短视频("):
            continue
        # dst = .../{主播}/短视频/{标题}[/{文件}]
        parts = mv.dst.parts
        try:
            i = parts.index("短视频")
        except ValueError:
            continue
        anchor = clean_name(parts[i - 1])
        title = parts[i + 1] if len(parts) > i + 1 else ""
        m[(anchor, _match_key(title))] = mv.dst.parent if mv.dst.suffix.lower() in MEDIA_EXTS else mv.dst
    return m


def _anchor_from_dir_files(index: AnchorIndex, d: Path) -> tuple[str, str]:
    """目录名对不上 DB 标题时，用目录内媒体文件名反查（标题=DB 原始标题）。"""
    try:
        files = sorted(p for p in d.iterdir() if p.is_file())
    except OSError:
        return "", ""
    for f in files:
        if f.suffix.lower() not in MEDIA_EXTS:
            continue
        stripped = strip_media_name(f.name)
        anchor = index.anchor_for_title(stripped)
        if anchor:
            return anchor, _canonical_title(index, stripped)
    return "", ""


def plan_lives(root: Path, index: AnchorIndex) -> Plan:
    """downloads/<平台>直播/<主播>/ → {产物根}/{主播}/直播/"""
    plan = Plan()
    src_root = root / "downloads"
    if not src_root.is_dir():
        return plan
    for platform_dir in sorted(p for p in src_root.iterdir() if p.is_dir()):
        if not LIVE_SUBDIR_RE.match(platform_dir.name):
            continue
        for anchor_dir in sorted(p for p in platform_dir.iterdir() if p.is_dir()):
            if not _has_products(anchor_dir):
                plan.empties.append(anchor_dir)
                continue
            anchor = clean_name(anchor_dir.name)
            dst = root / safe_dir_name(anchor) / "直播"
            if dst.resolve() == anchor_dir.resolve():
                plan.skipped.append((anchor_dir, "已在目标位置"))
                continue
            # 把「主播目录」整体挪成「{主播}/直播」，里面的场次目录跟着走。
            plan.moves.append(
                Move(anchor_dir, dst, f"直播({platform_dir.name}→{anchor})")
            )
    return plan


def plan_fupan(
    root: Path,
    index: AnchorIndex,
    video_map: dict[tuple[str, str], Path] | None = None,
) -> Plan:
    """fupan_output/* → 并入 {产物根}/{主播}/{直播|短视频}/"""
    plan = Plan()
    src_root = root / "fupan_output"
    if not src_root.is_dir():
        return plan
    video_map = video_map or {}

    for name_dir in sorted(p for p in src_root.iterdir() if p.is_dir()):
        if name_dir.name in FUPAN_SYSTEM_DIRS or name_dir.name.startswith("."):
            continue

        # 情形 A：这个一级目录本身就是一个短视频标题（旧布局的平铺写法）。
        anchor = index.anchor_for_title(name_dir.name)
        if not anchor and index.knows_title(name_dir.name):
            anchor = _anchor_for_known_title(index, name_dir.name)
        if anchor:
            _plan_one_fupan_session(
                plan, root, index, name_dir.name, name_dir, anchor, video_map
            )
            continue

        # 情形 B：这个一级目录是「主播名」，里面每个子目录才是一场/一个视频。
        if index.is_known_anchor(name_dir.name):
            for session_dir in sorted(p for p in name_dir.iterdir() if p.is_dir()):
                _plan_one_fupan_session(
                    plan, root, index, name_dir.name, session_dir,
                    clean_name(name_dir.name), video_map,
                )
            if not _has_products(name_dir):
                plan.empties.append(name_dir)
            continue

        # 认不出：整个一级目录进未识别清单（不动）。
        if not _has_products(name_dir):
            plan.empties.append(name_dir)
        else:
            plan.unknown.append((name_dir, "fupan_output：查不到主播名"))
    return plan


def _anchor_for_known_title(index: AnchorIndex, raw: str) -> str:
    """已知标题（能对上 DB）但 DB anchor 为空时，从别处推断主播名。"""
    key = clean_name(raw)
    # 优先用 strip 后的键（fupan 目录名常带 [B站] 前缀）再查一次。
    alt = _match_key(raw)
    for k in (key, alt):
        a = index.by_clean_title.get(k)
        if a:
            return a
    return ""


def _canonical_title(index: AnchorIndex, raw: str) -> str:
    """把一个（可能被清洗过的）名字还原成规范标题。

    优先用 DB 原始标题（保留真实空格/标点），否则退回 strip 后的目录名。
    """
    key = clean_name(raw)
    if key in index.original_title:
        return index.original_title[key]
    return strip_media_name(raw) or raw.strip()


def _transcript_files(session_dir: Path) -> list[Path]:
    """收集一个场次目录里的转写文件。

    兼容两种形态：
      A. 直接平铺：`{标题}/{标题}.转写.json`
      B. 多一层嵌套：`{标题}/[B站] {标题}/[B站] {标题}.转写.json`
    """
    direct = sorted(p for p in session_dir.iterdir() if p.is_file())
    if direct:
        return direct
    out: list[Path] = []
    for sub in sorted(p for p in session_dir.iterdir() if p.is_dir()):
        out.extend(sorted(p for p in sub.iterdir() if p.is_file()))
    return out


def _plan_one_fupan_session(
    plan: Plan,
    root: Path,
    index: AnchorIndex,
    parent_name: str,
    session_dir: Path,
    anchor: str,
    video_map: dict[tuple[str, str], Path],
) -> None:
    """规划单个 fupan 场次目录的落点（直播 or 短视频）。"""
    session = session_dir.name
    files = _transcript_files(session_dir)
    is_video = index.knows_title(session) or _match_key(session) in {
        k[1] for k in video_map if k[0] == clean_name(anchor)
    }

    # 空壳目录（无任何文件）→ 直接清掉，不搬。
    if not files:
        plan.empties.append(session_dir)
        return

    if is_video:
        # 目标标题目录优先跟磁盘上的视频走（R3：文字与视频同目录）。
        video_dir = video_map.get((clean_name(anchor), _match_key(session)))
        if video_dir is None:
            title = _canonical_title(index, session)
            video_dir = root / safe_dir_name(anchor) / "短视频" / title
            plan.moves.append(
                Move(session_dir, video_dir, f"fupan转写建目录({anchor}/{title})")
            )
            return
        # 已由短视频计划建立同名目录 → 把转写文件并进去（拍平嵌套）。
        for f in files:
            plan.moves.append(
                Move(f, video_dir / f.name, f"转写并入({anchor}/{video_dir.name})")
            )
        plan.empties.append(session_dir)
        return

    # 直播场次：整目录搬去 {主播}/直播/{场次}。
    out_name = session.strip() or "未命名场次"
    dst = root / safe_dir_name(anchor) / "直播" / out_name
    if dst.resolve() == session_dir.resolve():
        plan.skipped.append((session_dir, "已在目标位置"))
        return
    plan.moves.append(Move(session_dir, dst, f"fupan直播({parent_name}→{anchor})"))


def merge_plans(*plans: Plan) -> Plan:
    merged = Plan()
    for p in plans:
        merged.moves.extend(p.moves)
        merged.unknown.extend(p.unknown)
        merged.skipped.extend(p.skipped)
        merged.empties.extend(p.empties)
    # 去重空目录
    seen = set()
    uniq = []
    for e in merged.empties:
        if e not in seen:
            seen.add(e)
            uniq.append(e)
    merged.empties = uniq
    return merged


def _rel(p: Path, root: Path) -> str:
    try:
        return str(p.relative_to(root))
    except ValueError:
        return str(p)


def print_report(plan: Plan, root: Path, apply_mode: bool) -> None:
    title = "=== 迁移执行计划（APPLY）===" if apply_mode else "=== 迁移 dry-run 报告（不移动任何文件）==="
    print(title)
    print(f"产物根：{root}")
    print()
    print(f"【计划移动】{len(plan.moves)} 项")
    for m in plan.moves:
        print(f"  [移动] {_rel(m.src, root)}")
        print(f"       → {_rel(m.dst, root)}     ({m.reason})")
    print()
    print(f"【空目录（将被清理）】{len(plan.empties)} 项")
    for e in plan.empties:
        print(f"  [空] {_rel(e, root)}")
    print()
    print(f"【跳过】{len(plan.skipped)} 项")
    for path, why in plan.skipped:
        print(f"  [跳过] {_rel(path, root)} — {why}")
    print()
    print(f"【未识别，保持原地不动，需人工确认】{len(plan.unknown)} 项")
    for path, why in plan.unknown:
        print(f"  [未识别] {_rel(path, root)} — {why}")
    print()


def apply_plan(plan: Plan, root: Path) -> None:
    print("=== 开始执行迁移 ===")
    moved = 0
    # 先落短视频（建出标题目录），再把 fupan 转写并进去。
    def order_key(m: Move) -> int:
        if m.reason.startswith("短视频("):
            return 0
        if m.reason.startswith("直播("):
            return 1
        if m.reason.startswith("转写并入("):
            return 2
        return 1
    for m in sorted(plan.moves, key=order_key):
        m.dst.parent.mkdir(parents=True, exist_ok=True)
        target = unique_path(m.dst)
        print(f"  [移动] {_rel(m.src, root)} → {_rel(target, root)}")
        shutil.move(str(m.src), str(target))
        moved += 1
    print(f"=== 移动完成，共 {moved} 项 ===")
    print("清理空目录：")
    for e in sorted(plan.empties, key=lambda p: len(p.parts), reverse=True):
        try:
            e.rmdir()
            print(f"  [删空目录] {_rel(e, root)}")
        except OSError:
            pass
    _prune_empty_dirs(root / "downloads")
    _prune_empty_dirs(root / "fupan_output")


def _prune_empty_dirs(top: Path) -> None:
    """自底向上删掉空目录（只删空目录，永不递归删内容）。"""
    if not top.is_dir():
        return
    for path in sorted(top.rglob("*"), key=lambda p: len(p.parts), reverse=True):
        if path.is_dir():
            try:
                path.rmdir()
                print(f"  [删空目录] {_rel(path, top.parent)}")
            except OSError:
                pass


def main() -> int:
    parser = argparse.ArgumentParser(description="历史产物目录迁移（默认 dry-run）")
    parser.add_argument(
        "--root",
        default="/Volumes/A区/尊者产物",
        help="产物根目录（{路径设置} 产物根目录）",
    )
    parser.add_argument(
        "--project",
        default="/Volumes/A区/尊者监控直播短视频",
        help="项目目录（读 config/ 与 database/）",
    )
    parser.add_argument(
        "--apply",
        action="store_true",
        help="真正执行移动；不加只打印 dry-run 报告",
    )
    parser.add_argument(
        "--alias",
        action="append",
        default=[],
        metavar="UID=主播名",
        help="给 DB 里 anchor 为空的空间补主播名，可重复，如 --alias 360736302=老猫",
    )
    parser.add_argument(
        "--alias-file",
        default="",
        help="别名文件路径（每行 `UID=主播名`），与 --alias 合并",
    )
    args = parser.parse_args()

    aliases: dict[str, str] = {}
    if args.alias_file:
        af = Path(args.alias_file)
        if af.exists():
            for line in af.read_text(encoding="utf-8").splitlines():
                line = line.strip()
                if line and not line.startswith("#") and "=" in line:
                    k, v = line.split("=", 1)
                    aliases[k.strip()] = v.strip()
    for item in args.alias:
        if "=" in item:
            k, v = item.split("=", 1)
            aliases[k.strip()] = v.strip()

    root = Path(args.root).resolve()
    project = Path(args.project).resolve()
    db_path = root / "database" / "zunzhe-core.db"

    if not root.is_dir():
        print(f"产物根不存在：{root}", file=sys.stderr)
        return 1

    index = build_anchor_index(project, db_path, aliases)

    sv_plan = plan_short_videos(root, index)
    # 短视频计划里「每个视频的最终目录」，供 fupan 转写并入判断。
    vmap = planned_video_map(sv_plan)

    plan = merge_plans(
        sv_plan,
        plan_lives(root, index),
        plan_fupan(root, index, vmap),
    )
    print_report(plan, root, apply_mode=args.apply)

    if not args.apply:
        print("这是 dry-run：没有移动任何文件。")
        print("确认无误后加 --apply 执行（执行前请先备份产物根）。")
        return 0

    apply_plan(plan, root)
    return 0


if __name__ == "__main__":
    sys.exit(main())
