@echo off
chcp 65001 >nul
REM =============================================================================
REM 尊者直播监控 —— Windows 一键打包发布脚本
REM
REM 作用：把 Electron UI + Rust daemon + ffmpeg + 配置 打包成便携版 .exe
REM 产物：desktop\release\尊者直播监控-<version>-便携版.exe
REM
REM 用法：双击运行，或在项目根目录命令行执行  build.bat
REM
REM 前置条件（Windows）：
REM   - 已安装 Rust 工具链：https://rustup.rs
REM   - 已安装 Node.js：https://nodejs.org
REM   - ffmpeg.exe 已放入 .\ffmpeg\ffmpeg.exe
REM   - 已执行过：cd desktop && npm install
REM =============================================================================
setlocal
cd /d "%~dp0"

echo [打包] 编译 Rust daemon release ...
pushd zunzhe-core
call cargo build --release
if errorlevel 1 ( echo [打包] Rust 编译失败 & exit /b 1 )
popd

echo [打包] 构建前端 UI ...
pushd desktop
call npx vite build
if errorlevel 1 ( echo [打包] 前端构建失败 & exit /b 1 )

echo [打包] 清理旧产物 ...
if exist release\win-unpacked rmdir /s /q release\win-unpacked
if exist release\*.blockmap del /q release\*.blockmap 2>nul
if exist release\*.yml del /q release\*.yml 2>nul

echo [打包] electron-builder 打包 Windows 便携版 ...
call npx electron-builder --win portable
if errorlevel 1 ( echo [打包] 打包失败 & exit /b 1 )

echo.
echo ============================================================================
echo [打包] 完成！产物：%cd%\release\尊者直播监控-*-便携版.exe
echo ============================================================================
popd
endlocal
