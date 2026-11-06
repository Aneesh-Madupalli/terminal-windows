$ErrorActionPreference = 'Stop'
# Runs inside the guest at first logon, from the answer ISO (label TWSETUP).
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$data = Join-Path $env:ProgramData 'terminal-windows'
New-Item -ItemType Directory -Force -Path $data | Out-Null
$log = Join-Path $data 'install.log'
function Log($message) { Add-Content -Path $log -Value ('{0:o} {1}' -f (Get-Date), $message) }

try {
  Log 'Installing the guest agent'
  $config = Get-Content -Raw -Path (Join-Path $here 'guest.json') | ConvertFrom-Json

  $install = Join-Path $env:ProgramFiles 'TerminalWindowsAgent'
  New-Item -ItemType Directory -Force -Path $install | Out-Null
  $exe = Join-Path $install 'twagent.exe'
  Copy-Item -Force -Path (Join-Path $here 'twagent.exe') -Destination $exe

  # The channel secret is protected for this user, the account the agent runs as.
  Add-Type -AssemblyName System.Security
  $secret = [Convert]::FromBase64String($config.channelSecret)
  $entropy = [Text.Encoding]::ASCII.GetBytes('terminal-windows channel secret v1')
  $blob = [Security.Cryptography.ProtectedData]::Protect($secret, $entropy, [Security.Cryptography.DataProtectionScope]::CurrentUser)
  $agent = @{ serviceGuid = $config.serviceGuid; secret = [Convert]::ToBase64String($blob); options = $config.options }
  Set-Content -Path (Join-Path $data 'agent.json') -Value ($agent | ConvertTo-Json -Depth 5) -Encoding UTF8
  & icacls.exe $data /inheritance:r /grant:r 'SYSTEM:(OI)(CI)F' 'Administrators:(OI)(CI)F' ($env:USERNAME + ':(OI)(CI)M') | Out-Null

  # A supervisor task at logon, with the highest privileges so input reaches elevated windows.
  $user = $env:COMPUTERNAME + '\' + $env:USERNAME
  $action = New-ScheduledTaskAction -Execute $exe -Argument 'supervise'
  $trigger = New-ScheduledTaskTrigger -AtLogOn -User $user
  $principal = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Highest
  $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable `
    -ExecutionTimeLimit ([TimeSpan]::Zero) -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1) -MultipleInstances IgnoreNew
  Register-ScheduledTask -TaskName 'TerminalWindowsAgent' -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force | Out-Null

  # Keep the desktop available: no sleep, no screen saver, no lock, no automatic restart while signed in.
  & powercfg.exe /change monitor-timeout-ac 0
  & powercfg.exe /change standby-timeout-ac 0
  & powercfg.exe /change hibernate-timeout-ac 0
  & powercfg.exe /hibernate off
  Set-ItemProperty -Path 'HKCU:\Control Panel\Desktop' -Name ScreenSaveActive -Value '0'
  $policies = 'HKLM:\SOFTWARE\Policies\Microsoft'
  New-Item -Path "$policies\Windows\Personalization" -Force | Out-Null
  Set-ItemProperty -Path "$policies\Windows\Personalization" -Name NoLockScreen -Value 1 -Type DWord
  New-Item -Path "$policies\Power\PowerSettings\0e796bdb-100d-47d6-a2d5-f7d2daa51f51" -Force | Out-Null
  Set-ItemProperty -Path "$policies\Power\PowerSettings\0e796bdb-100d-47d6-a2d5-f7d2daa51f51" -Name ACSettingIndex -Value 0 -Type DWord
  New-Item -Path "$policies\Windows\WindowsUpdate\AU" -Force | Out-Null
  Set-ItemProperty -Path "$policies\Windows\WindowsUpdate\AU" -Name NoAutoRebootWithLoggedOnUsers -Value 1 -Type DWord

  # VirtualBox has no Hyper-V sockets: the agent listens on TCP and the host's NAT forward arrives from 10.0.2.2.
  if ($config.options.channel -eq 'tcp') {
    $port = [int]$config.options.port
    Remove-NetFirewallRule -Name 'TerminalWindowsAgent' -ErrorAction SilentlyContinue
    New-NetFirewallRule -Name 'TerminalWindowsAgent' -DisplayName 'terminal-windows agent' -Direction Inbound -Protocol TCP `
      -LocalPort $port -RemoteAddress '10.0.2.2' -Action Allow -Profile Any | Out-Null
    if ($config.options.offline) {
      New-NetFirewallRule -Name 'TerminalWindowsOffline' -DisplayName 'terminal-windows offline' -Direction Outbound `
        -Action Block -RemoteAddress Internet -Profile Any | Out-Null
    }
  }

  # Remote Desktop would let Hyper-V's enhanced session take the console session away from the agent.
  Set-ItemProperty -Path 'HKLM:\SYSTEM\CurrentControlSet\Control\Terminal Server' -Name fDenyTSConnections -Value 1 -Type DWord

  if ($config.options.uac -eq 'off') {
    $system = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System'
    Set-ItemProperty -Path $system -Name PromptOnSecureDesktop -Value 0 -Type DWord
    Set-ItemProperty -Path $system -Name ConsentPromptBehaviorAdmin -Value 0 -Type DWord
  }

  # Setup keeps a copy of the answer file, which holds the account password.
  Remove-Item -Force -ErrorAction SilentlyContinue -Path "$env:SystemRoot\Panther\unattend.xml", "$env:SystemRoot\Panther\Unattend\unattend.xml"

  Start-ScheduledTask -TaskName 'TerminalWindowsAgent'
  Log 'Done'
} catch {
  Log ('FAILED: ' + $_.Exception.Message)
  throw
}
