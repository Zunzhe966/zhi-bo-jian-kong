# Restart Zunzhe Live Monitor desktop app
$ErrorActionPreference = 'SilentlyContinue'
$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$Desktop = Join-Path $Root 'desktop'
$Electron = Join-Path $Desktop 'node_modules\electron\dist\electron.exe'
$Icon = Join-Path $Desktop 'assets\icon.ico'
$LogDir = Join-Path $Root 'fupan_output\logs'
$Log = Join-Path $LogDir 'desktop-start.log'

New-Item -ItemType Directory -Force -Path $LogDir | Out-Null

Get-CimInstance Win32_Process -Filter "Name='electron.exe'" |
  Where-Object { $_.CommandLine -like '*直播复盘助手*desktop*' -and $_.CommandLine -notlike '*--type=*' } |
  ForEach-Object { Stop-Process -Id $_.ProcessId -Force }

Start-Sleep -Milliseconds 800

Set-Location $Desktop
Write-Host '[restart] building UI...'
npm run build 2>&1 | Tee-Object -FilePath $Log -Append | Out-Null
if ($LASTEXITCODE -ne 0) {
  Write-Host "[restart] build failed, see $Log"
  exit 1
}

$Desk = [Environment]::GetFolderPath('Desktop')
$names = @('尊者直播监控.lnk', '直播复盘录制工具.lnk', '直播录屏软件.lnk')
foreach ($name in $names) {
  $lnk = Join-Path $Desk $name
  $w = New-Object -ComObject WScript.Shell
  $s = $w.CreateShortcut($lnk)
  $s.TargetPath = $Electron
  $s.Arguments = "`"$Desktop`""
  $s.WorkingDirectory = $Desktop
  if (Test-Path $Icon) { $s.IconLocation = "$Icon,0" }
  $s.Description = 'Zunzhe Live Monitor'
  $s.Save()
}

Write-Host '[restart] starting electron...'
Start-Process -FilePath $Electron -ArgumentList "`"$Desktop`"" -WorkingDirectory $Desktop
Write-Host '[restart] done'
