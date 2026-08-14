# 尊者 Rust 核心健康看门狗（Windows）：由桌面端每分钟调用。
# 每次记录 daemon 是否存活、心跳是否新鲜；异常时调用 ensure-services 拉起。
$ErrorActionPreference = 'SilentlyContinue'
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

$RustBin = Join-Path $Root 'zunzhe-core\target\release\zunzhe-core.exe'
if (-not (Test-Path $RustBin)) {
  $RustBin = Join-Path $Root 'zunzhe-core\target\debug\zunzhe-core.exe'
}
if (-not (Test-Path $RustBin)) {
  Write-Host "missing rust binary: $RustBin"
  exit 1
}

$LogDir = Join-Path $Root 'logs'
New-Item -ItemType Directory -Force -Path $LogDir | Out-Null
$Log = Join-Path $LogDir 'zunzhe-core.uptime.log'
$PidFile = Join-Path $Root 'fupan_output\run\zunzhe-core.pid'
$Pid = if (Test-Path $PidFile) { (Get-Content -Raw $PidFile).Trim() } else { 'none' }

$status = & $RustBin doctor 2>$null | Out-String
$daemonOk = $status -match '"rust_daemon":true'
$heartbeatOk = $status -match '"heartbeat_ok":true'
$now = Get-Date -Format 'yyyy-MM-dd HH:mm:ss zzz'
Add-Content -Path $Log -Value "$now pid=$Pid rust_daemon=$daemonOk heartbeat_ok=$heartbeatOk"

if (-not $daemonOk -or -not $heartbeatOk) {
  Add-Content -Path $Log -Value "$now daemon unhealthy, calling ensure-services"
  & $RustBin ensure-services 2>$null | Out-Null
}
exit 0
