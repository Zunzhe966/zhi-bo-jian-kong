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

$ConfigFile = Join-Path $Root 'config\config.ini'
if (-not (Test-Path $ConfigFile)) { $ConfigFile = Join-Path $Root 'config\config.ini.example' }
$MediaRoot = ''
$ConfiguredLogDir = ''
$ConfiguredFupanDir = ''
if (Test-Path $ConfigFile) {
  $InPaths = $false
  foreach ($line in Get-Content $ConfigFile) {
    if ($line -match '^\s*\[(.+?)\]\s*$') { $InPaths = ($Matches[1] -eq '路径设置'); continue }
    if ($InPaths -and $line -match '^\s*产物根目录\s*=\s*(.*)$') { $MediaRoot = $Matches[1].Trim(); continue }
    if ($InPaths -and $line -match '^\s*日志目录\s*=\s*(.*)$') { $ConfiguredLogDir = $Matches[1].Trim(); continue }
    if ($InPaths -and $line -match '^\s*复盘输出目录\s*=\s*(.*)$') { $ConfiguredFupanDir = $Matches[1].Trim(); continue }
  }
}
if ([string]::IsNullOrWhiteSpace($MediaRoot)) {
  $MediaRoot = Join-Path $HOME 'Documents\尊者产物'
} elseif ($MediaRoot -eq '~') {
  $MediaRoot = $HOME
} elseif ($MediaRoot.StartsWith('~/')) {
  $MediaRoot = Join-Path $HOME $MediaRoot.Substring(2)
} elseif (-not [IO.Path]::IsPathRooted($MediaRoot)) {
  $MediaRoot = Join-Path $Root $MediaRoot
}
if ([string]::IsNullOrWhiteSpace($ConfiguredLogDir)) {
  $LogDir = Join-Path $MediaRoot 'logs'
} elseif ($ConfiguredLogDir -eq '~') {
  $LogDir = $HOME
} elseif ($ConfiguredLogDir.StartsWith('~/')) {
  $LogDir = Join-Path $HOME $ConfiguredLogDir.Substring(2)
} elseif (-not [IO.Path]::IsPathRooted($ConfiguredLogDir)) {
  $LogDir = Join-Path $Root $ConfiguredLogDir
} else {
  $LogDir = $ConfiguredLogDir
}
if ([string]::IsNullOrWhiteSpace($ConfiguredFupanDir)) {
  $FupanDir = Join-Path $MediaRoot 'fupan_output'
} elseif ($ConfiguredFupanDir -eq '~') {
  $FupanDir = $HOME
} elseif ($ConfiguredFupanDir.StartsWith('~/')) {
  $FupanDir = Join-Path $HOME $ConfiguredFupanDir.Substring(2)
} elseif (-not [IO.Path]::IsPathRooted($ConfiguredFupanDir)) {
  $FupanDir = Join-Path $Root $ConfiguredFupanDir
} else {
  $FupanDir = $ConfiguredFupanDir
}
New-Item -ItemType Directory -Force -Path $LogDir | Out-Null
$Log = Join-Path $LogDir 'zunzhe-core.watchdog.log'
$AuditLog = Join-Path $LogDir 'zunzhe-core.audit.log'
function Rotate-IfLarge {
  param([string]$Path, [int]$MaxBytes = 5242880, [int]$Keep = 5)
  if (-not (Test-Path $Path)) { return }
  $item = Get-Item $Path
  if ($item.Length -lt $MaxBytes) { return }
  for ($i = $Keep; $i -ge 1; $i--) {
    $from = if ($i -eq 1) { $Path } else { "$Path.$($i-1)" }
    $to = "$Path.$i"
    if (Test-Path $from) { Move-Item -Force $from $to }
  }
}
Rotate-IfLarge $Log
Rotate-IfLarge $AuditLog
$PidFile = Join-Path $FupanDir 'run\zunzhe-core.pid'
$Pid = if (Test-Path $PidFile) { (Get-Content -Raw $PidFile).Trim() } else { 'none' }

$status = & $RustBin doctor 2>$null | Out-String
$daemonOk = $status -match '"rust_daemon":true'
$heartbeatOk = $status -match '"heartbeat_ok":true'
$now = Get-Date -Format 'yyyy-MM-dd HH:mm:ss zzz'
Add-Content -Path $Log -Value "$now pid=$Pid rust_daemon=$daemonOk heartbeat_ok=$heartbeatOk"

# 与 macOS/Linux 的 audit-longrun.sh 对齐：把 doctor 的 health_audit 指标也
# 写入 logs/zunzhe-core.audit.log，让 Windows 长跑同样有 RSS/心跳/断档证据。
try {
  $diag = $status | ConvertFrom-Json -ErrorAction Stop
  $audit = $diag.health_audit
  if ($null -eq $audit) {
    $audit = @{
      ok = [bool]$diag.health
      daemon_ok = [bool]($diag.daemon_ok -or $diag.rust_daemon)
      heartbeat_ok = [bool]$diag.heartbeat_ok
      pid = $Pid
      rss_kib = $null
      uptime_secs = [long]$diag.uptime_secs
      heartbeat_age_secs = $null
      heartbeat_lines = 0
      consecutive_heartbeats = 0
      log_gap_secs = $null
      source_total = 0
      source_failing = 0
      source_cooling = 0
      source_ok = $true
      notify_pending = 0
      notify_failed = 0
      notify_latest_failed_channel = $null
      notify_hint = $null
      notify_ok = $true
    }
  }
  $sourceHealth = $diag.source_health
  $notifyHealth = $diag.notify_health
  if ($null -ne $sourceHealth) {
    $audit.source_total = [int]$sourceHealth.total
    $audit.source_failing = [int]$sourceHealth.failing
    $audit.source_cooling = [int]$sourceHealth.cooling
    $audit.source_ok = [bool]$sourceHealth.ok
  }
  if ($null -ne $notifyHealth) {
    $audit.notify_pending = [int]$notifyHealth.pending
    $audit.notify_failed = [int]$notifyHealth.failed
    $audit.notify_latest_failed_channel = $notifyHealth.latest_failed_channel
    $audit.notify_hint = $notifyHealth.hint
    $audit.notify_ok = [bool]$notifyHealth.ok
  }
  $auditLine = [ordered]@{
    health = [bool]$audit.ok
    daemon_ok = [bool]$audit.daemon_ok
    heartbeat_ok = [bool]$audit.heartbeat_ok
    pid = if ($null -ne $audit.pid) { $audit.pid } else { $Pid }
    rss_kib = if ($null -ne $audit.rss_kib) { $audit.rss_kib } else { $null }
    uptime_secs = if ($null -ne $audit.uptime_secs) { [long]$audit.uptime_secs } else { 0 }
    heartbeat_age_secs = if ($null -ne $audit.heartbeat_age_secs) { $audit.heartbeat_age_secs } else { $null }
    heartbeat_lines = if ($null -ne $audit.heartbeat_lines) { [int]$audit.heartbeat_lines } else { 0 }
    consecutive_heartbeats = if ($null -ne $audit.consecutive_heartbeats) { [int]$audit.consecutive_heartbeats } else { 0 }
    log_gap_secs = if ($null -ne $audit.log_gap_secs) { $audit.log_gap_secs } else { $null }
    disk_free_mb = if ($null -ne $diag.disk_free_mb) { [long]$diag.disk_free_mb } else { $null }
    source_total = if ($null -ne $sourceHealth -and $null -ne $sourceHealth.total) { [int]$sourceHealth.total } else { 0 }
    source_failing = if ($null -ne $sourceHealth -and $null -ne $sourceHealth.failing) { [int]$sourceHealth.failing } else { 0 }
    source_cooling = if ($null -ne $sourceHealth -and $null -ne $sourceHealth.cooling) { [int]$sourceHealth.cooling } else { 0 }
    source_ok = if ($null -ne $sourceHealth -and $null -ne $sourceHealth.ok) { [bool]$sourceHealth.ok } else { $true }
    notify_pending = if ($null -ne $notifyHealth -and $null -ne $notifyHealth.pending) { [int]$notifyHealth.pending } else { 0 }
    notify_failed = if ($null -ne $notifyHealth -and $null -ne $notifyHealth.failed) { [int]$notifyHealth.failed } else { 0 }
    notify_latest_failed_channel = if ($null -ne $notifyHealth -and $null -ne $notifyHealth.latest_failed_channel) { $notifyHealth.latest_failed_channel } else { $null }
    notify_hint = if ($null -ne $notifyHealth -and $null -ne $notifyHealth.hint) { $notifyHealth.hint } else { $null }
    notify_ok = if ($null -ne $notifyHealth -and $null -ne $notifyHealth.ok) { [bool]$notifyHealth.ok } else { $true }
  }
  Add-Content -Path $AuditLog -Value "$now $(ConvertTo-Json -InputObject $auditLine -Compress -Depth 4)"
} catch {
  Add-Content -Path $AuditLog -Value "$now audit_unparseable"
}

if (-not $daemonOk -or -not $heartbeatOk) {
  Add-Content -Path $Log -Value "$now daemon unhealthy, calling ensure-services"
  & $RustBin ensure-services 2>$null | Out-Null
}
exit 0
