$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$exePath = Join-Path $repoRoot "src-tauri\target\release\secret-tunnel.exe"
if (!(Test-Path -LiteralPath $exePath)) {
  throw "Release executable not found at $exePath"
}

$tempRoot = Join-Path $env:TEMP ("secret-tunnel-launch-smoke-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Force -Path $tempRoot | Out-Null

$oldAppData = $env:APPDATA
$oldLocalAppData = $env:LOCALAPPDATA
$process = $null
try {
  $env:APPDATA = Join-Path $tempRoot "AppData\Roaming"
  $env:LOCALAPPDATA = Join-Path $tempRoot "AppData\Local"
  New-Item -ItemType Directory -Force -Path $env:APPDATA, $env:LOCALAPPDATA | Out-Null

  $process = Start-Process -FilePath $exePath -PassThru -WindowStyle Hidden
  Start-Sleep -Seconds 5

  if (!(Get-Process -Id $process.Id -ErrorAction SilentlyContinue)) {
    throw "Secret Tunnel release executable exited during launch smoke."
  }

  Write-Host "Secret Tunnel release launch smoke passed. pid=$($process.Id)"
} finally {
  if ($process -and (Get-Process -Id $process.Id -ErrorAction SilentlyContinue)) {
    Stop-Process -Id $process.Id -Force
  }
  $env:APPDATA = $oldAppData
  $env:LOCALAPPDATA = $oldLocalAppData
}
