#!/bin/bash
# =============================================================================
# 双击我即可把「尊者直播监控」升级到 1.7.17
#
# 为什么要用这个文件：开发助手环境对 /Applications 的写入会被安全策略拦成
# "写一半"（留下临时文件、破坏签名、版本换不上去）。这个脚本由你在自己的
# 终端里跑，不受那层拦截，能一次干净完成。
#
# 它会：退出旧版 → 清掉残留 → 替换成 1.7.17 → 重新签名 → 校验 → 重新打开。
#
# 1.7.17 相比 1.7.16 修了什么：
#   - 抖音「列出历史」修好：以前点刷新永远拿的是数据库里的旧列表，还被标成
#     "刚抓的"，看着有新作品其实没更新；现在会真的去取，取不到就如实说原因。
#   - 抖音和 B站 一样，启动时自动把浏览器登录态同步给后台程序（以前只同步 B站）。
# =============================================================================
set -uo pipefail

ROOT="/Volumes/A区/尊者监控直播短视频"
SRC="$ROOT/desktop/release/mac-arm64/尊者直播监控.app"
TARGET="/Applications/尊者直播监控.app"
NAME="尊者直播监控"

echo "=============================================="
echo " 尊者直播监控 → 1.7.17 安装程序"
echo "=============================================="
echo

if [ ! -d "$SRC" ]; then
  echo "❌ 找不到新版程序：$SRC"
  echo "   请确认移动硬盘已挂载、且项目目录没被移动。"
  echo
  read -n 1 -s -r -p "按任意键关闭…"
  exit 1
fi

SRC_VER="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$SRC/Contents/Info.plist" 2>/dev/null || echo '?')"
echo "待安装版本：$SRC_VER"
echo

# ---- 1. 退出旧版 ----------------------------------------------------------
echo "[1/6] 正在退出旧版…"
osascript -e "tell application \"$NAME\" to quit" >/dev/null 2>&1 || true
for _ in $(seq 1 10); do
  pgrep -f "Applications/${NAME}.app/Contents/MacOS/" >/dev/null 2>&1 || break
  sleep 1
done
if pgrep -f "Applications/${NAME}.app/Contents/MacOS/" >/dev/null 2>&1; then
  echo "      正常退出没生效，强制结束（不影响你的数据）"
  pkill -TERM -f "Applications/${NAME}.app/Contents/MacOS/" >/dev/null 2>&1 || true
  sleep 3
  pkill -KILL -f "Applications/${NAME}.app/Contents/MacOS/" >/dev/null 2>&1 || true
  pkill -KILL -f "${NAME} Helper" >/dev/null 2>&1 || true
  sleep 2
fi
echo "      已退出"
echo

# ---- 1b. 收掉多余的后台进程 -----------------------------------------------
# 有些 daemon 可能不是从应用程序包跑起来的（例如直接从项目目录启动过），
# 它们会用旧二进制同时读写同一个数据库，跟新版自带的那个抢活，
# 表现为「同一条新视频被下载两次」或监控状态错乱。这里一并收掉，
# 重开软件后它会自己拉起正确的那一个。
STRAY="$(pgrep -f 'zunzhe-core run-daemon' 2>/dev/null | wc -l | tr -d ' ')"
if [ "$STRAY" != "0" ]; then
  echo "      收掉 $STRAY 个后台进程（旧版残留）"
  pkill -TERM -f 'zunzhe-core run-daemon' >/dev/null 2>&1 || true
  sleep 3
  pkill -KILL -f 'zunzhe-core run-daemon' >/dev/null 2>&1 || true
  sleep 1
fi
echo "      后台进程已清空"
echo

# ---- 2. 清理上次被拦下留下的临时文件 --------------------------------------
echo "[2/6] 清理残留临时文件…"
BEFORE="$(find "$TARGET" -name '.BC.*' 2>/dev/null | wc -l | tr -d ' ')"
find "$TARGET" -name '.BC.*' -delete 2>/dev/null || true
echo "      清掉 $BEFORE 个"
echo

# ---- 3. 替换 --------------------------------------------------------------
echo "[3/6] 替换为新版…"
/usr/bin/ditto "$SRC" "$TARGET"
find "$TARGET" -name '.BC.*' -delete 2>/dev/null || true
echo "      完成"
echo

# ---- 4. 重新签名（未签名/坏签名会导致打不开、联网被拒）--------------------
echo "[4/6] 重新签名…"
/usr/bin/codesign --force --deep --sign - --identifier com.zunzhe.livemonitor "$TARGET" 2>&1 | tail -2
echo

# ---- 5. 校验 --------------------------------------------------------------
echo "[5/6] 校验…"
OK=1
if /usr/bin/codesign --verify --deep --strict "$TARGET" >/dev/null 2>&1; then
  echo "      ✅ 签名通过"
else
  echo "      ⚠️ 签名未通过，再签一次…"
  find "$TARGET" -name '.BC.*' -delete 2>/dev/null || true
  /usr/bin/codesign --force --deep --sign - --identifier com.zunzhe.livemonitor "$TARGET" >/dev/null 2>&1
  if /usr/bin/codesign --verify --deep --strict "$TARGET" >/dev/null 2>&1; then
    echo "      ✅ 签名通过"
  else
    echo "      ❌ 签名仍不通过，请把上面的输出截图发我"
    OK=0
  fi
fi

NEW_VER="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$TARGET/Contents/Info.plist" 2>/dev/null || echo '?')"
DAEMON_VER="$("$TARGET/Contents/Resources/bin/zunzhe-core" --version 2>/dev/null || echo '?')"
STRAYS="$(find "$TARGET" -name '.BC.*' 2>/dev/null | wc -l | tr -d ' ')"
echo "      应用版本：$NEW_VER"
echo "      后台版本：$DAEMON_VER"
echo "      残留临时文件：$STRAYS 个"
[ "$NEW_VER" = "$SRC_VER" ] && echo "      ✅ 版本已就位" || { echo "      ❌ 版本不对（期望 $SRC_VER）"; OK=0; }
echo

# ---- 6. 重新打开 ----------------------------------------------------------
echo "[6/6] 重新打开软件…"
open -a "$TARGET" >/dev/null 2>&1 || open "$TARGET" >/dev/null 2>&1 || true
sleep 2
echo "      已启动"
echo

if [ "$OK" = "1" ]; then
  echo "=============================================="
  echo " ✅ 完成！现在是 $NEW_VER"
  echo "=============================================="
  echo
  echo " 建议顺手看一眼："
  echo "   1) B站 老猫AI创业班 → 列出历史，应该还是 46 条。"
  echo "   2) 抖音 关立新 / 侯翠翠 → 列出历史，应该会真的去取一次"
  echo "      （比以前多出几条），等几秒属正常。"
else
  echo "=============================================="
  echo " ⚠️ 有步骤没通过，请把上面的内容发给我"
  echo "=============================================="
fi
echo
read -n 1 -s -r -p "按任意键关闭这个窗口…"
echo
