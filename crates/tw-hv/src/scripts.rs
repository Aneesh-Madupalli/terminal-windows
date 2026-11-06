//! Fixed PowerShell bodies for the Hyper-V backend. Each reads `$p` (JSON parameters) and returns one value.
//! Cmdlets that can emit objects are piped to `Out-Null` so only the final value is returned.

/// One VM by name; refuses to act when the name is ambiguous.
const GET_VM: &str = r#"$vms = @(Get-VM -Name $p.hvName)
if ($vms.Count -ne 1) { throw "Expected exactly one VM named $($p.hvName) but found $($vms.Count)" }
$vm = $vms[0]
"#;

fn with_vm(rest: &str) -> String {
    format!("{GET_VM}{rest}")
}

pub fn preflight() -> String {
    r#"$os = Get-CimInstance Win32_OperatingSystem
$cs = Get-CimInstance Win32_ComputerSystem
$id = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($id)
$elevated = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
$inGroup = $id.Groups | Where-Object { $_.Value -eq 'S-1-5-32-578' } | Select-Object -First 1
$vmms = Get-Service vmms -ErrorAction SilentlyContinue
$module = [bool](Get-Module -ListAvailable -Name Hyper-V)
$key = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Virtualization\GuestCommunicationServices\' + $p.serviceGuid
@{
  windows = "$($os.Version)"
  caption = "$($os.Caption)"
  hypervisorPresent = [bool]$cs.HypervisorPresent
  vmmsStatus = $(if ($vmms) { "$($vmms.Status)" } else { 'missing' })
  hyperVModule = $module
  elevated = [bool]$elevated
  inHyperVAdministrators = [bool]$inGroup
  serviceRegistered = [bool](Test-Path $key)
  vmconnect = [bool](Test-Path (Join-Path $env:SystemRoot 'System32\vmconnect.exe'))
  logicalProcessors = [int]$cs.NumberOfLogicalProcessors
  ramBytes = [int64]$cs.TotalPhysicalMemory
}"#
    .into()
}

pub fn create() -> String {
    r#"$name = $p.hvName
if (@(Get-VM -Name $name -ErrorAction SilentlyContinue).Count -gt 0) { throw "A VM named $name already exists" }
New-Item -ItemType Directory -Force -Path $p.vmPath | Out-Null
$newArgs = @{
  Name = $name
  Generation = 2
  MemoryStartupBytes = [int64]$p.memoryBytes
  NewVHDPath = $p.vhdPath
  NewVHDSizeBytes = [int64]$p.diskBytes
  Path = $p.vmPath
}
if ($p.network) {
  if (-not (Get-VMSwitch -Name 'Default Switch' -ErrorAction SilentlyContinue)) {
    throw 'The Hyper-V Default Switch does not exist. Create a virtual switch, or use --offline.'
  }
  $newArgs.SwitchName = 'Default Switch'
}
$vm = New-VM @newArgs
Set-VMProcessor -VMName $name -Count ([int]$p.cpu)
Set-VMMemory -VMName $name -DynamicMemoryEnabled $false -StartupBytes ([int64]$p.memoryBytes)
Set-VMFirmware -VMName $name -EnableSecureBoot On -SecureBootTemplate MicrosoftWindows
Set-VMKeyProtector -VMName $name -NewLocalKeyProtector
Enable-VMTPM -VMName $name
Set-VM -Name $name -AutomaticCheckpointsEnabled $false -CheckpointType Standard -AutomaticStopAction ShutDown
$warning = $null
try {
  Set-VMVideo -VMName $name -HorizontalResolution ([int]$p.width) -VerticalResolution ([int]$p.height) -ResolutionType Single
} catch { $warning = "The display size was not applied: $($_.Exception.Message)" }
@{ id = "$($vm.Id)"; warning = $warning }"#
        .into()
}

pub fn attach_dvd() -> String {
    "Add-VMDvdDrive -VMName $p.hvName -Path $p.path | Out-Null\n$true".into()
}

pub fn set_boot_dvd() -> String {
    r#"$dvd = Get-VMDvdDrive -VMName $p.hvName | Where-Object { $_.Path -eq $p.path } | Select-Object -First 1
if (-not $dvd) { throw 'The install media is not attached' }
Set-VMFirmware -VMName $p.hvName -FirstBootDevice $dvd
$true"#
        .into()
}

pub fn detach_dvds() -> String {
    r#"Get-VMDvdDrive -VMName $p.hvName | Remove-VMDvdDrive
$hd = Get-VMHardDiskDrive -VMName $p.hvName | Select-Object -First 1
if ($hd) { Set-VMFirmware -VMName $p.hvName -FirstBootDevice $hd }
$true"#
        .into()
}

pub fn start() -> String {
    with_vm("Start-VM -VM $vm | Out-Null\n$true")
}

pub fn stop_graceful() -> String {
    with_vm("Stop-VM -VM $vm -Confirm:$false\n$true")
}

pub fn stop_force() -> String {
    with_vm("Stop-VM -VM $vm -TurnOff -Force\n$true")
}

pub fn pause() -> String {
    with_vm("Suspend-VM -VM $vm\n$true")
}

pub fn resume() -> String {
    with_vm("Resume-VM -VM $vm\n$true")
}

pub fn state() -> String {
    with_vm("@{ state = \"$($vm.State)\"; id = \"$($vm.Id)\" }")
}

pub fn keyboard() -> String {
    r#"$name = $p.hvName.Replace("'", "''")
$cs = Get-CimInstance -Namespace root\virtualization\v2 -ClassName Msvm_ComputerSystem -Filter "ElementName='$name'"
if (-not $cs) { throw 'The VM was not found' }
$kb = Get-CimAssociatedInstance -InputObject $cs -ResultClassName Msvm_Keyboard | Select-Object -First 1
if (-not $kb) { throw 'The VM has no keyboard device' }
function Check($r) { if ($r.ReturnValue -ne 0) { throw "The keyboard call failed (code $($r.ReturnValue))" } }
switch ($p.kind) {
  'ctrl-alt-del' { Check (Invoke-CimMethod -InputObject $kb -MethodName TypeCtrlAltDel) }
  'text' { Check (Invoke-CimMethod -InputObject $kb -MethodName TypeText -Arguments @{ asciiText = [string]$p.text }) }
  'key' {
    Check (Invoke-CimMethod -InputObject $kb -MethodName PressKey -Arguments @{ keyCode = [uint32]$p.code })
    Check (Invoke-CimMethod -InputObject $kb -MethodName ReleaseKey -Arguments @{ keyCode = [uint32]$p.code })
  }
  default { throw "Unknown keyboard action $($p.kind)" }
}
$true"#
        .into()
}

pub fn checkpoint_create() -> String {
    with_vm("Checkpoint-VM -VM $vm -SnapshotName $p.label\n$true")
}

pub fn checkpoint_list() -> String {
    with_vm(
        r#"@(Get-VMSnapshot -VM $vm | ForEach-Object { @{ name = $_.Name; created = $_.CreationTime.ToString('o') } })"#,
    )
}

pub fn checkpoint_restore() -> String {
    with_vm("Restore-VMSnapshot -VMSnapshot (Get-VMSnapshot -VM $vm -Name $p.label) -Confirm:$false\n$true")
}

pub fn checkpoint_remove() -> String {
    with_vm("Remove-VMSnapshot -VMSnapshot (Get-VMSnapshot -VM $vm -Name $p.label) -Confirm:$false\n$true")
}

pub fn close_console() -> String {
    r#"Get-CimInstance Win32_Process -Filter "Name='vmconnect.exe'" |
  Where-Object { $_.CommandLine -like "*$($p.hvName)*" } |
  ForEach-Object { Stop-Process -Id $_.ProcessId -Force }
$true"#
        .into()
}

pub fn remove() -> String {
    with_vm("if ($vm.State -ne 'Off') { Stop-VM -VM $vm -TurnOff -Force }\nRemove-VM -VM $vm -Force\n$true")
}

/// Host facts for backends that do not need the Hyper-V module.
pub fn host_info() -> String {
    r#"$os = Get-CimInstance Win32_OperatingSystem
$cs = Get-CimInstance Win32_ComputerSystem
$id = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($id)
@{
  windows = "$($os.Version)"
  caption = "$($os.Caption)"
  hypervisorPresent = [bool]$cs.HypervisorPresent
  elevated = [bool]$principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
  logicalProcessors = [int]$cs.NumberOfLogicalProcessors
  ramBytes = [int64]$cs.TotalPhysicalMemory
}"#
    .into()
}

/// Registers the control-channel service ID and adds the current user to Hyper-V Administrators.
pub fn setup_host() -> String {
    r#"$id = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($id)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
  throw 'ELEVATION Run this command from an elevated (administrator) prompt.'
}
$restart = $false
if ($p.enableHyperV) {
  $feature = Enable-WindowsOptionalFeature -Online -FeatureName Microsoft-Hyper-V-All -NoRestart
  $restart = [bool]$feature.RestartNeeded
}
$sid = $id.User.Value
$member = $false
try { $member = [bool](Get-LocalGroupMember -SID 'S-1-5-32-578' -ErrorAction Stop | Where-Object { $_.SID.Value -eq $sid }) } catch { }
$added = $false
if (-not $member) {
  try { Add-LocalGroupMember -SID 'S-1-5-32-578' -Member $sid -ErrorAction Stop; $added = $true }
  catch { if ($_.Exception.Message -notmatch 'already a member') { throw } }
}
$key = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Virtualization\GuestCommunicationServices\' + $p.serviceGuid
New-Item -Path $key -Force | Out-Null
New-ItemProperty -Path $key -Name ElementName -Value 'terminal-windows' -PropertyType String -Force | Out-Null
@{ addedToGroup = $added; registered = $true; restartRequired = $restart }"#
        .into()
}

/// Every script, for the syntax check in the tests.
pub fn all() -> Vec<(&'static str, String)> {
    vec![
        ("preflight", preflight()),
        ("create", create()),
        ("attach_dvd", attach_dvd()),
        ("set_boot_dvd", set_boot_dvd()),
        ("detach_dvds", detach_dvds()),
        ("start", start()),
        ("stop_graceful", stop_graceful()),
        ("stop_force", stop_force()),
        ("pause", pause()),
        ("resume", resume()),
        ("state", state()),
        ("keyboard", keyboard()),
        ("checkpoint_create", checkpoint_create()),
        ("checkpoint_list", checkpoint_list()),
        ("checkpoint_restore", checkpoint_restore()),
        ("checkpoint_remove", checkpoint_remove()),
        ("close_console", close_console()),
        ("remove", remove()),
        ("setup_host", setup_host()),
        ("host_info", host_info()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parses every script with PowerShell's own parser, so syntax errors fail here instead of on a Hyper-V host.
    #[cfg(windows)]
    #[test]
    fn every_script_parses() {
        for (name, body) in all() {
            let script = crate::powershell::wrap(&body);
            let encoded = crate::powershell::encode_command(&script);
            let check = "$s = [Text.Encoding]::Unicode.GetString([Convert]::FromBase64String($env:TW_SCRIPT)); \
                         $e = $null; $t = $null; [void][Management.Automation.Language.Parser]::ParseInput($s, [ref]$t, [ref]$e); \
                         if ($e.Count -gt 0) { $e | ForEach-Object { $_.Message }; exit 1 }";
            let output = std::process::Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command", check])
                .env("TW_SCRIPT", encoded)
                .output()
                .expect("powershell.exe runs");
            assert!(output.status.success(), "{name}: {}", String::from_utf8_lossy(&output.stdout));
        }
    }

    #[test]
    fn scripts_never_interpolate_parameters() {
        for (name, body) in all() {
            assert!(!body.contains("{{") && !body.contains("%s"), "{name}");
            assert!(name == "host_info" || body.contains("$p."), "{name} reads its parameters from $p");
        }
    }

    #[test]
    fn vm_scripts_refuse_ambiguous_names() {
        for body in [start(), stop_graceful(), stop_force(), pause(), resume(), state(), remove()] {
            assert!(body.contains("Expected exactly one VM"));
        }
    }
}
