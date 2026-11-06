#requires -Version 5.1
<#
.SYNOPSIS
  Unpacks the release zip into a temporary folder and checks that it is complete and that the binaries start.
  Run scripts\build.ps1 first.
#>
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$zip = Get-ChildItem (Join-Path $root 'dist') -Filter 'terminal-windows-*-x64.zip' | Select-Object -First 1
if (-not $zip) { throw 'No zip in dist\. Run scripts\build.ps1 first.' }

$expected = ((Get-Content "$($zip.FullName).sha256" -Raw) -split '\s+')[0].ToLower()
$actual = (Get-FileHash $zip.FullName -Algorithm SHA256).Hash.ToLower()
if ($expected -ne $actual) { throw "SHA-256 mismatch: expected $expected, got $actual" }

$work = Join-Path ([IO.Path]::GetTempPath()) ('tw-package-' + [Guid]::NewGuid().ToString('N'))
try {
  Expand-Archive -Path $zip.FullName -DestinationPath $work
  foreach ($file in 'terminal-windows.exe', 'twagent.exe', 'install.ps1', 'LICENSE', 'THIRD-PARTY-NOTICES.md', 'skills\terminal-windows\SKILL.md') {
    if (-not (Test-Path (Join-Path $work $file))) { throw "The zip is missing $file" }
  }

  $cli = Join-Path $work 'terminal-windows.exe'
  $version = (& $cli version) -join "`n"
  if ($LASTEXITCODE -ne 0 -or $version -notmatch '^terminal-windows \d+\.\d+\.\d+') { throw "Unexpected version output: $version" }

  $help = (& $cli help) -join "`n"
  if ($help -notmatch 'up \[name\]') { throw 'help does not describe the up command' }

  $skill = (& $cli skills get) -join "`n"
  if ($skill -notmatch '(?m)^name: terminal-windows') { throw 'skills get did not print the skill' }

  $config = (& $cli mcp --config) -join "`n" | ConvertFrom-Json
  if ($config.mcpServers.'terminal-windows'.command -ne 'terminal-windows') { throw 'mcp --config prints the wrong server entry' }

  $plan = (& $cli up --plan) -join "`n"
  if ($plan -notmatch 'create-hyper-v-vm') { throw 'up --plan did not list the steps' }

  # The agent is a windows-subsystem program, so only its exit code is checked.
  & (Join-Path $work 'twagent.exe') --version | Out-Null
  if ($LASTEXITCODE -ne 0) { throw 'twagent --version failed' }

  Write-Host "Package OK: $($zip.Name)"
} finally {
  Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
