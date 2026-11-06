#requires -Version 5.1
<#
.SYNOPSIS
  Installs terminal-windows for the current user and adds it to PATH. Run it from the unpacked release folder,
  or from the repository after scripts\build.ps1.
#>
param([string]$Source)
$ErrorActionPreference = 'Stop'
if (-not $Source) {
  $here = Split-Path -Parent $MyInvocation.MyCommand.Path
  $Source = if (Test-Path (Join-Path $here 'terminal-windows.exe')) { $here } else { Join-Path (Split-Path -Parent $here) 'target\release' }
}
foreach ($file in 'terminal-windows.exe', 'twagent.exe') {
  if (-not (Test-Path (Join-Path $Source $file))) { throw "Missing $file in $Source" }
}

$target = Join-Path $env:LOCALAPPDATA 'Programs\terminal-windows'
New-Item -ItemType Directory -Force -Path $target | Out-Null
foreach ($file in 'terminal-windows.exe', 'twagent.exe') {
  Copy-Item -Force (Join-Path $Source $file) $target
}
$skill = Join-Path $Source 'skills'
if (Test-Path $skill) { Copy-Item -Recurse -Force $skill $target }

$path = [Environment]::GetEnvironmentVariable('Path', 'User')
if (($path -split ';') -notcontains $target) {
  [Environment]::SetEnvironmentVariable('Path', ($path.TrimEnd(';') + ';' + $target), 'User')
  Write-Host "Added $target to your PATH. Open a new terminal to use it."
}
Write-Host "Installed to $target"
Write-Host 'Next: terminal-windows doctor'
