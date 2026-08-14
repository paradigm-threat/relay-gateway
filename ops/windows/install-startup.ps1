# Install relay-ops into the current user's Startup folder.
# Usage (PowerShell): .\install-startup.ps1 -Exe C:\path\relay-ops.exe -Gitlab https://gitlab.com/org/relay-gateway
param(
  [Parameter(Mandatory = $true)][string]$Exe,
  [Parameter(Mandatory = $true)][string]$Gitlab
)
$startup = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Startup"
New-Item -ItemType Directory -Force -Path $startup | Out-Null
$cmd = Join-Path $startup "relay-ops.cmd"
@"
@echo off
start "relay-ops" /min "$Exe" serve --gitlab $Gitlab
"@ | Set-Content -Path $cmd -Encoding ASCII
Write-Host "wrote $cmd"
