# -*- coding: utf-8 -*-
import json
import io
import os
import random
import shutil
import string
import tempfile
from pathlib import Path
import functools
import hashlib
import re
import traceback
from typing import Any
from urllib.parse import parse_qs, urlparse
from collections import OrderedDict
import execjs
from .logger import logger
import configparser

OptionalStr = str | None
OptionalDict = dict | None


class Color:
    RED = "\033[31m"
    GREEN = "\033[32m"
    YELLOW = "\033[33m"
    BLUE = "\033[34m"
    MAGENTA = "\033[35m"
    CYAN = "\033[36m"
    WHITE = "\033[37m"
    RESET = "\033[0m"

    @staticmethod
    def print_colored(text, color):
        print(f"{color}{text}{Color.RESET}")


def trace_error_decorator(func: callable) -> callable:
    @functools.wraps(func)
    def wrapper(*args: list, **kwargs: dict) -> Any:
        try:
            return func(*args, **kwargs)
        except execjs.ProgramError:
            logger.warning('Failed to execute JS code. Please check if the Node.js environment')
        except Exception as e:
            error_line = traceback.extract_tb(e.__traceback__)[-1].lineno
            error_info = f"message: type: {type(e).__name__}, {str(e)} in function {func.__name__} at line: {error_line}"
            logger.error(error_info)
            return []

    return wrapper


def check_md5(file_path: str | Path) -> str:
    with open(file_path, 'rb') as fp:
        file_md5 = hashlib.md5(fp.read()).hexdigest()
    return file_md5


def dict_to_cookie_str(cookies_dict: dict) -> str:
    cookie_str = '; '.join([f"{key}={value}" for key, value in cookies_dict.items()])
    return cookie_str


def read_config_value(file_path: str | Path, section: str, key: str) -> str | None:
    config = configparser.ConfigParser()

    try:
        config.read(file_path, encoding='utf-8-sig')
    except Exception as e:
        print(f"Error occurred while reading the configuration file: {e}")
        return None

    if section in config:
        if key in config[section]:
            return config[section][key]
        else:
            print(f"Key [{key}] does not exist in section [{section}].")
    else:
        print(f"Section [{section}] does not exist in the file.")

    return None


def atomic_write_text(file_path: str | Path, text: str, encoding: str = 'utf-8-sig') -> None:
    """原子写文本：先写同目录临时文件，fsync 后 os.replace 顶替。

    直接 `open(path, 'w')` 覆盖有两个问题：
      1. 写到一半进程被杀（daemon 被 kill -9、机器断电、看门狗强杀），
         文件就被截断 —— 监控列表、Cookie、配置整份丢；
      2. 并发读方可能读到写了一半的内容（部分行）。
    `os.replace` 在同一文件系统内是原子的：读方要么看到旧内容，要么看到新内容。
    临时文件必须放在同目录，跨盘 rename 不保证原子。
    """
    path = Path(file_path)
    parent = path.parent if str(path.parent) else Path('.')
    parent.mkdir(parents=True, exist_ok=True)
    fd, tmp_path = tempfile.mkstemp(prefix=f'.{path.name}.tmp.', dir=str(parent))
    try:
        with os.fdopen(fd, 'w', encoding=encoding) as handle:
            handle.write(text)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(tmp_path, path)
    except BaseException:
        try:
            os.unlink(tmp_path)
        except OSError:
            pass
        raise


def update_config(file_path: str | Path, section: str, key: str, new_value: str) -> None:
    config = configparser.ConfigParser()

    try:
        config.read(file_path, encoding='utf-8-sig')
    except Exception as e:
        print(f"An error occurred while reading the configuration file: {e}")
        return

    if section not in config:
        print(f"Section [{section}] does not exist in the file.")
        return

    # 转义%字符
    escaped_value = new_value.replace('%', '%%')
    config[section][key] = escaped_value

    try:
        # 原子写：旧实现直接 open('w') 覆盖，写一半被杀就把整份配置截断。
        buffer = io.StringIO()
        config.write(buffer)
        atomic_write_text(file_path, buffer.getvalue(), encoding='utf-8-sig')
        print(f"The value of {key} under [{section}] in the configuration file has been updated.")
    except Exception as e:
        print(f"Error occurred while writing to the configuration file: {e}")


def get_file_paths(directory: str) -> list:
    file_paths = []
    for root, dirs, files in os.walk(directory):
        for file in files:
            file_paths.append(os.path.join(root, file))
    return file_paths


def remove_emojis(text: str, replace_text: str = '') -> str:
    emoji_pattern = re.compile(
        "["
        "\U0001F1E0-\U0001F1FF"  # flags (iOS)
        "\U0001F300-\U0001F5FF"  # symbols & pictographs
        "\U0001F600-\U0001F64F"  # emoticons
        "\U0001F680-\U0001F6FF"  # transport & map symbols
        "\U0001F700-\U0001F77F"  # alchemical symbols
        "\U0001F780-\U0001F7FF"  # Geometric Shapes Extended
        "\U0001F800-\U0001F8FF"  # Supplemental Arrows-C
        "\U0001F900-\U0001F9FF"  # Supplemental Symbols and Pictographs
        "\U0001FA00-\U0001FA6F"  # Chess Symbols
        "\U0001FA70-\U0001FAFF"  # Symbols and Pictographs Extended-A
        "\U00002702-\U000027B0"  # Dingbats
        "]+",
        flags=re.UNICODE
    )
    return emoji_pattern.sub(replace_text, text)


def remove_duplicate_lines(file_path: str | Path) -> None:
    unique_lines = OrderedDict()
    text_encoding = 'utf-8-sig'
    with open(file_path, 'r', encoding=text_encoding) as input_file:
        for line in input_file:
            unique_lines[line.strip()] = None
    # 监控列表是「唯一权威」文件，截断=丢源，必须原子写。
    atomic_write_text(
        file_path, ''.join(line + '\n' for line in unique_lines), encoding=text_encoding)


def check_disk_capacity(file_path: str | Path, show: bool = False) -> float:
    absolute_path = os.path.abspath(file_path)
    directory = os.path.dirname(absolute_path)
    disk_usage = shutil.disk_usage(directory)
    disk_root = Path(directory).anchor
    free_space_gb = disk_usage.free / (1024 ** 3)
    if show:
        print(f"{disk_root} Total: {disk_usage.total / (1024 ** 3):.2f} GB "
              f"Used: {disk_usage.used / (1024 ** 3):.2f} GB "
              f"Free: {free_space_gb:.2f} GB\n")
    return free_space_gb


def handle_proxy_addr(proxy_addr):
    if proxy_addr:
        if not proxy_addr.startswith('http'):
            proxy_addr = 'http://' + proxy_addr
    else:
        proxy_addr = None
    return proxy_addr


def generate_random_string(length: int) -> str:
    characters = string.ascii_uppercase + string.digits
    random_string = ''.join(random.choices(characters, k=length))
    return random_string


def jsonp_to_json(jsonp_str: str) -> OptionalDict:
    pattern = r'(\w+)\((.*)\);?$'
    match = re.search(pattern, jsonp_str)

    if match:
        _, json_str = match.groups()
        json_obj = json.loads(json_str)
        return json_obj
    else:
        raise Exception("No JSON data found in JSONP response.")


def replace_url(file_path: str | Path, old: str, new: str) -> None:
    with open(file_path, 'r', encoding='utf-8-sig') as f:
        content = f.read()
    if old in content:
        atomic_write_text(file_path, content.replace(old, new), encoding='utf-8-sig')


def get_query_params(url: str, param_name: OptionalStr) -> dict | list[str]:
    parsed_url = urlparse(url)
    query_params = parse_qs(parsed_url.query)

    if param_name is None:
        return query_params
    else:
        values = query_params.get(param_name, [])
        return values
    