#!/bin/bash
# 批量下载某主播未下载的历史视频（分批入队，防 B站 412 风控）
# 用法: bash scripts/download-anchor-history.sh <source_url> <主播名> [每批数量]
# 示例: bash scripts/download-anchor-history.sh https://space.bilibili.com/1458143131 侯翠翠 10
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

SOURCE_URL="${1:?缺少监控源地址}"
ANCHOR="${2:?缺少主播名}"
BATCH="${3:-10}"
DB="$ROOT/zunzhe-core.db"
CORE="$ROOT/zunzhe-core/target/release/zunzhe-core"

if [ ! -f "$DB" ]; then
  echo "数据库不存在: $DB" >&2
  exit 1
fi
if [ ! -x "$CORE" ]; then
  echo "未找到 zunzhe-core 二进制: $CORE" >&2
  exit 1
fi

# 未下载且从未入队过的视频（含 failed 也不重复入队，避免失败项死循环）
pending_sql() {
  sqlite3 "$DB" "SELECT v.url FROM videos v
    WHERE v.source_url='$SOURCE_URL' AND v.downloaded=0
      AND NOT EXISTS (SELECT 1 FROM tasks t WHERE t.kind='download' AND t.url=v.url)
    ORDER BY v.published_at DESC;"
}

round=0
while :; do
  pending_sql > /tmp/zunzhe_pending_urls.txt
  total=$(wc -l < /tmp/zunzhe_pending_urls.txt | tr -d ' ')
  if [ "$total" -eq 0 ]; then
    echo "[完成] 没有未下载且未入队的视频了"
    break
  fi
  round=$((round + 1))
  echo "[第 $round 批] 剩余待下载 $total 条，本批入队 ${BATCH} 条"
  head -n "$BATCH" /tmp/zunzhe_pending_urls.txt | while IFS= read -r url; do
    [ -z "$url" ] && continue
    echo "  入队: ${url: -14}"
    "$CORE" download-video "$url" "$ANCHOR" >/dev/null 2>&1 || echo "  入队失败: $url"
  done
  echo "  等待本批下载完成（每 30 秒检查一次）..."
  while :; do
    active=$(sqlite3 "$DB" "SELECT count(*) FROM tasks t
      WHERE t.kind='download' AND t.status IN ('pending','processing')
        AND t.url IN (SELECT url FROM videos WHERE source_url='$SOURCE_URL');")
    if [ "$active" -eq 0 ]; then
      break
    fi
    sleep 30
  done
  done_count=$(sqlite3 "$DB" "SELECT count(*) FROM tasks t
    WHERE t.kind='download' AND t.status='done'
      AND t.url IN (SELECT url FROM videos WHERE source_url='$SOURCE_URL');")
  fail_count=$(sqlite3 "$DB" "SELECT count(*) FROM tasks t
    WHERE t.kind='download' AND t.status='failed'
      AND t.url IN (SELECT url FROM videos WHERE source_url='$SOURCE_URL');")
  echo "  本批结束：成功 $done_count，失败 $fail_count"
  # 失败过多说明被风控，等待 5 分钟再继续
  if [ "$fail_count" -gt 0 ] && [ "$fail_count" -ge "$BATCH" ]; then
    echo "  本批全部失败，疑似风控，等待 300 秒降温..."
    sleep 300
  fi
done

echo "[全部完成] 下载结果："
sqlite3 -header "$DB" "SELECT count(*) as 已下载 FROM videos WHERE source_url='$SOURCE_URL' AND downloaded=1;"
