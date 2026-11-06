#requires -Version 5.1
<#
.SYNOPSIS
  Builds release binaries and packs dist\terminal-windows-<version>-x64.zip.
#>
param([switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$version = (Select-String -Path 'Cargo.toml' -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
if (-not $SkipBuild) {
  cargo build --release --locked -p tw-cli -p tw-agent
  if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
}

$stage = Join-Path $root "dist\terminal-windows-$version-x64"
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $stage | Out-Null
Copy-Item (Join-Path $root 'target\release\terminal-windows.exe') $stage
Copy-Item (Join-Path $root 'target\release\twagent.exe') $stage
Copy-Item (Join-Path $root 'LICENSE'), (Join-Path $root 'THIRD-PARTY-NOTICES.md') $stage
New-Item -ItemType Directory -Force -Path (Join-Path $stage 'skills') | Out-Null
Copy-Item -Recurse (Join-Path $root 'skills\terminal-windows') (Join-Path $stage 'skills')
Copy-Item (Join-Path $root 'scripts\install.ps1') $stage

$zip = Join-Path $root "dist\terminal-windows-$version-x64.zip"
Remove-Item -Force $zip -ErrorAction SilentlyContinue
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip
$hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
Set-Content -Path "$zip.sha256" -Value "$hash  $(Split-Path -Leaf $zip)" -Encoding ascii
Write-Host "Built $zip"
Write-Host "SHA-256 $hash"
