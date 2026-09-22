# -*- coding: utf-8 -*-
"""读取 fupan_config.ini 与主程序 config.ini 的配置，并解析产物存放路径。"""
import configparser
import os
import sys
from pathlib import Path

PROJECT_DIR = Path(__file__).resolve().parent.parent
CONFIG_DIR = PROJECT_DIR / 'config'
ENCODING = 'utf-8-sig'

# 产物路径配置所在的节名。与 Rust 侧 zunzhe-core/src/paths.rs 的 SECTION 必须一致：
# 两边读同一份 config.ini，任一侧改了节名就会各写各的目录。
PATHS_SECTION = '路径设置'

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
    ffmpeg_dir = str(PROJECT_DIR / 'ffmpeg')
    if ffmpeg_dir not in os.environ.get('PATH', ''):
        os.environ['PATH'] = ffmpeg_dir + os.pathsep + os.environ.get('PATH', '')


def venv_python() -> Path:
    """返回项目虚拟环境里的 Python 可执行文件（跨 Windows/macOS/Linux）。"""
    if sys.platform == 'win32':
        return PROJECT_DIR / '.venv' / 'Scripts' / 'python.exe'
    return PROJECT_DIR / '.venv' / 'bin' / 'python'


# ---------------------------------------------------------------------------
# 产物存放路径
#
# 「程序」和「产物」是两回事：程序（config/ .venv/ src/ fupan/）永远跟着
# PROJECT_DIR 走；产物（下载的短视频、转写文本、知识点、切片、封面、录像）
# 可以搬到任意目录，由 config.ini 的 [路径设置] 决定。
#
# 这里必须和 Rust 侧 zunzhe-core/src/paths.rs 给出**完全一致**的答案：两边都写
# 同一批目录，一旦算法不同（比如 `~` 展开规则、相对路径基准），daemon 写到 A、
# 复盘去 B 读，就会出现「明明录完了却找不到文件」这类静默错位。
# ---------------------------------------------------------------------------


def _expand_path(raw: str, base: Path):
    """把配置里写的路径文本展开成绝对 Path；空串返回 None（表示用默认值）。"""
    raw = (raw or '').strip()
    if not raw:
        return None
    if raw == '~':
        return Path.home()
    if raw.startswith('~/'):
        raw = str(Path.home()) + raw[1:]
    p = Path(raw).expanduser()
    return p if p.is_absolute() else base / p


def storage_paths() -> dict:
    """解析全部产物路径。读不到配置时退回旧行为（产物都在项目目录下）。"""
    cfg = main_config()
    media_root = _expand_path(get(cfg, PATHS_SECTION, '产物根目录'), PROJECT_DIR) or PROJECT_DIR
    downloads = _expand_path(get(cfg, PATHS_SECTION, '短视频下载目录'), PROJECT_DIR) or (media_root / 'downloads')
    fupan = _expand_path(get(cfg, PATHS_SECTION, '复盘输出目录'), PROJECT_DIR) or (media_root / 'fupan_output')
    # 与 Rust paths::resolve 保持一致：默认数据库跟随产物根。
    # 留空时退回项目源码目录会把 Python 与 Rust 指向两个不同的库，
    # 界面看到的是外置库，旧 Python 工具却读项目根旧库。
    database = _expand_path(get(cfg, PATHS_SECTION, '数据库文件'), PROJECT_DIR) or (
        media_root / 'database' / 'zunzhe-core.db'
    )
    live_save = _expand_path(get(cfg, '录制设置', '直播保存路径(不填则默认)'), PROJECT_DIR)
    return {
        'project_dir': PROJECT_DIR,
        'media_root': media_root,
        'downloads': downloads,
        'fupan_output': fupan,
        'covers': fupan / 'covers',
        'run': fupan / 'run',
        'database': database,
        # 直播录像落点：填了就用它，否则与旧行为一致落在 downloads 下。
        'recordings': live_save or downloads,
    }


def builds_paths_module_constant():
    """给模块级常量用：解析一次并缓存（流水线是短命进程，导入时解析即可）。"""
    return storage_paths()


_PATHS = storage_paths()

# 复盘输出目录（转写/知识点/切片/封面）。以前写死成 PROJECT_DIR / 'fupan_output'，
# 于是用户一旦把产物根换到别处，Python 流水线仍往老地方找，全部报「找不到文件」。
FUPAN_DIR = _PATHS['fupan_output']
# 短视频下载根目录。
DOWNLOADS_DIR = _PATHS['downloads']
# 运行态文件目录（pid/锁/协议文件）。
RUN_DIR = _PATHS['run']
# 直播录像落点。
RECORDINGS_DIR = _PATHS['recordings']
