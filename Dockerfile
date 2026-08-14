FROM rust:1-bookworm AS builder

WORKDIR /app

COPY . /app

RUN apt-get update && \
    apt-get install -y --no-install-recommends pkg-config libssl-dev && \
    rm -rf /var/lib/apt/lists/*

# Rust 核心依赖全部来自 crates.io，网络失败会直接让构建失败，避免生成
# “看起来能跑但没有 zunzhe-core 可执行文件”的镜像。
RUN cd /app/zunzhe-core && cargo build --release

FROM python:3.11-slim

WORKDIR /app

# 只复制运行时需要的目录，不把 .git/桌面端 node_modules/构建缓存带进镜像。
COPY --from=builder /app/zunzhe-core/target/release/zunzhe-core /usr/local/bin/zunzhe-core
COPY config config
COPY fupan fupan
COPY src src
COPY scripts scripts
COPY requirements.txt requirements.txt
COPY requirements-fupan.txt requirements-fupan.txt

RUN apt-get update && \
    apt-get install -y --no-install-recommends ffmpeg tzdata && \
    ln -fs /usr/share/zoneinfo/Asia/Shanghai /etc/localtime && \
    dpkg-reconfigure -f noninteractive tzdata && \
    rm -rf /var/lib/apt/lists/*

# Python 层保留给复盘桥接（转写/提炼/切片）与 yt-dlp 兜底，Rust daemon
# 统一调度直播/视频监控；默认入口是 Rust 核心，不再直接跑旧 Python main.py。
RUN pip install --no-cache-dir -r requirements.txt -r requirements-fupan.txt

ENV RUST_LOG=info

# 容器内没有桌面端/launchd，直接用 daemon 主入口；config/、logs/、
# fupan_output/、downloads/ 由用户通过 volume 持久化。
CMD ["bash", "/app/scripts/container-run.sh", "--interval", "900"]
