#requires -Version 5.1
<#
.SYNOPSIS
  Downloads a terminal-windows release, verifies its SHA-256, and installs it.
  Set TERMINAL_WINDOWS_REPO to OWNER/NAME of the GitHub repository that publishes the releases.
#>
param([string]$Version = 'latest')
$ErrorActionPreference = 'Stop'
$repo = $env:TERMINAL_WINDOWS_REPO
if (-not $repo) { throw 'Set TERMINAL_WINDOWS_REPO to OWNER/NAME first, for example: $env:TERMINAL_WINDOWS_REPO = "owner/terminal-windows"' }
if ($repo -notmatch '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$') { throw 'TERMINAL_WINDOWS_REPO must look like OWNER/NAME' }

[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$headers = @{ 'User-Agent' = 'terminal-windows-installer' }
$api = if ($Version -eq 'latest') { "https://api.github.com/repos/$repo/releases/latest" } else { "https://api.github.com/repos/$repo/releases/tags/$Version" }
$release = Invoke-RestMethod -Uri $api -Headers $headers
$zipAsset = $release.assets | Where-Object { $_.name -like 'terminal-windows-*-x64.zip' } | Select-Object -First 1
$hashAsset = $release.assets | Where-Object { $_.name -eq "$($zipAsset.name).sha256" } | Select-Object -First 1
if (-not $zipAsset -or -not $hashAsset) { throw 'The release has no zip with a .sha256 file' }

$work = Join-Path ([IO.Path]::GetTempPath()) ("terminal-windows-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work | Out-Null
try {
  $zip = Join-Path $work $zipAsset.name
  Invoke-WebRequest -Uri $zipAsset.browser_download_url -OutFile $zip -Headers $headers
  $expected = ((Invoke-RestMethod -Uri $hashAsset.browser_download_url -Headers $headers) -split '\s+')[0].ToLower()
  $actual = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
  if ($expected -ne $actual) { throw "SHA-256 mismatch: expected $expected, got $actual" }
  Expand-Archive -Path $zip -DestinationPath (Join-Path $work 'pkg')
  & (Join-Path $work 'pkg\install.ps1') -Source (Join-Path $work 'pkg')
} finally {
  Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
