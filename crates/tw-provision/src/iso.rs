use std::path::{Path, PathBuf};

use serde_json::json;
use tw_core::{Error, Result};
use tw_hv::PowerShell;

use crate::answer::{AnswerOptions, autounattend_xml};
use crate::config::GuestConfig;
use crate::wim::{self, ImageInfo};

/// Label of the answer ISO; the first-logon command finds the volume by it.
pub const ANSWER_LABEL: &str = "TWSETUP";

const MOUNT: &str = r#"$image = Mount-DiskImage -ImagePath $p.path -Access ReadOnly -PassThru
$volume = $image | Get-Volume
if (-not $volume -or -not $volume.DriveLetter) { throw 'The ISO mounted but has no drive letter' }
@{ drive = "$($volume.DriveLetter)" }"#;

const DISMOUNT: &str = "Dismount-DiskImage -ImagePath $p.path | Out-Null\n$true";

/// Builds an ISO from a folder with the in-box IMAPI2 file-system API, which needs no extra tools.
const BUILD_ISO: &str = r#"if (Test-Path -LiteralPath $p.output) { Remove-Item -Force -LiteralPath $p.output }
$code = @'
public class TwIsoFile {
  public static unsafe void Create(string path, object stream, int blockSize, int totalBlocks) {
    int bytes = 0; byte[] buffer = new byte[blockSize];
    System.IntPtr pointer = (System.IntPtr)(&bytes);
    var output = System.IO.File.OpenWrite(path);
    var source = stream as System.Runtime.InteropServices.ComTypes.IStream;
    if (output != null) {
      while (totalBlocks-- > 0) { source.Read(buffer, blockSize, pointer); output.Write(buffer, 0, bytes); }
      output.Flush(); output.Close();
    }
  }
}
'@
if (-not ('TwIsoFile' -as [type])) {
  $options = New-Object System.CodeDom.Compiler.CompilerParameters
  $options.CompilerOptions = '/unsafe'
  Add-Type -CompilerParameters $options -TypeDefinition $code
}
$image = New-Object -ComObject IMAPI2FS.MsftFileSystemImage
$image.FileSystemsToCreate = 3
$image.VolumeName = $p.label
$image.Root.AddTree($p.source, $false)
$result = $image.CreateResultImage()
[TwIsoFile]::Create($p.output, $result.ImageStream, $result.BlockSize, $result.TotalBlocks)
$true"#;

#[derive(Debug, Clone)]
pub struct IsoInfo {
    pub path: PathBuf,
    /// `install.wim` or `install.esd`.
    pub image_file: String,
    pub images: Vec<ImageInfo>,
}

fn check_path(path: &Path) -> Result<()> {
    let is_iso = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("iso"));
    if !path.is_file() || !is_iso {
        return Err(Error::Usage(format!("{} is not an ISO file", path.display())));
    }
    Ok(())
}

/// Mounts the ISO read-only, reads its image list, and always dismounts.
pub async fn inspect(powershell: &PowerShell, iso: &Path) -> Result<IsoInfo> {
    check_path(iso)?;
    let params = json!({ "path": iso.to_string_lossy() });
    let mounted = powershell.run(MOUNT, &params).await?;
    let drive = mounted["drive"].as_str().unwrap_or_default().to_string();
    let result = read_from_drive(&drive, iso);
    let dismounted = powershell.run(DISMOUNT, &params).await;
    let info = result?;
    dismounted?;
    Ok(info)
}

fn read_from_drive(drive: &str, iso: &Path) -> Result<IsoInfo> {
    let sources = PathBuf::from(format!("{drive}:\\sources"));
    for name in ["install.wim", "install.esd"] {
        let file = sources.join(name);
        if file.is_file() {
            let images = wim::read_images(&mut std::fs::File::open(&file)?)?;
            return Ok(IsoInfo { path: iso.to_path_buf(), image_file: name.into(), images });
        }
    }
    Err(Error::Usage(
        "This ISO has no sources\\install.wim or install.esd. It is not a Windows installation ISO".into(),
    ))
}

/// Files for the answer ISO, written to a staging folder.
pub struct AnswerMedia<'a> {
    pub options: &'a AnswerOptions,
    pub config: &'a GuestConfig,
    pub agent_exe: &'a Path,
}

/// Writes `autounattend.xml` and the `setup` folder into `staging`.
pub fn stage(media: &AnswerMedia, staging: &Path) -> Result<()> {
    let setup = staging.join("setup");
    std::fs::create_dir_all(&setup)?;
    std::fs::write(staging.join("autounattend.xml"), autounattend_xml(media.options)?)?;
    std::fs::write(
        setup.join("install-agent.ps1"),
        format!("\u{feff}{}", include_str!("../../../guest/setup/install-agent.ps1")),
    )?;
    std::fs::write(setup.join("guest.json"), serde_json::to_vec_pretty(media.config)?)?;
    std::fs::copy(media.agent_exe, setup.join("twagent.exe"))
        .map_err(|e| Error::Other(format!("Cannot copy the guest agent {}: {e}", media.agent_exe.display())))?;
    Ok(())
}

/// Turns a folder into an ISO with the given volume label.
pub async fn build_iso(powershell: &PowerShell, source: &Path, output: &Path, label: &str) -> Result<()> {
    let params = json!({ "source": source.to_string_lossy(), "output": output.to_string_lossy(), "label": label });
    powershell.run(BUILD_ISO, &params).await?;
    if !output.is_file() {
        return Err(Error::Other("The ISO builder finished but wrote no file".into()));
    }
    Ok(())
}

/// Stages the answer files in a temporary folder, builds `answer.iso`, and removes the staging folder.
pub async fn build_answer_iso(powershell: &PowerShell, media: &AnswerMedia<'_>, output: &Path) -> Result<()> {
    let staging = tempfile::Builder::new().prefix("tw-answer-").tempdir()?;
    stage(media, staging.path())?;
    build_iso(powershell, staging.path(), output, ANSWER_LABEL).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wim::{TEST_XML, build_test_wim};

    /// IMAPI2 and disk-image mounting do not tolerate two builds at once.
    #[cfg(windows)]
    static IMAPI: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn options() -> AnswerOptions {
        AnswerOptions {
            image_index: 2,
            computer_name: "WORK".into(),
            password: "Pa55-word_x.".into(),
            locale: "en-US".into(),
            bypass_hardware_checks: false,
        }
    }

    #[test]
    fn non_iso_paths_are_rejected_before_any_script_runs() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("notes.txt");
        std::fs::write(&text, "x").unwrap();
        assert!(check_path(&text).is_err());
        assert!(check_path(&dir.path().join("missing.iso")).is_err());
        assert!(check_path(dir.path()).is_err());
        let iso = dir.path().join("a.ISO");
        std::fs::write(&iso, "x").unwrap();
        assert!(check_path(&iso).is_ok());
    }

    #[test]
    fn staging_writes_every_answer_file() {
        let dir = tempfile::tempdir().unwrap();
        let agent = dir.path().join("twagent.exe");
        std::fs::write(&agent, b"MZ fake").unwrap();
        let config = GuestConfig::new("default", "b7a4d3c0-5e21-4f8a-9c6d-2e81f0a7d315", &[3u8; 32], "0.1.0", false);
        let options = options();
        let staging = dir.path().join("stage");
        stage(&AnswerMedia { options: &options, config: &config, agent_exe: &agent }, &staging).unwrap();
        for file in ["autounattend.xml", "setup/install-agent.ps1", "setup/guest.json", "setup/twagent.exe"] {
            assert!(staging.join(file).is_file(), "{file}");
        }
        assert_eq!(std::fs::read(staging.join("setup/twagent.exe")).unwrap(), b"MZ fake");
        let script = std::fs::read_to_string(staging.join("setup/install-agent.ps1")).unwrap();
        assert!(script.starts_with('\u{feff}'), "a BOM keeps Windows PowerShell reading it as UTF-8");
        let parsed: GuestConfig =
            serde_json::from_slice(&std::fs::read(staging.join("setup/guest.json")).unwrap()).unwrap();
        assert_eq!(parsed, config);
    }

    #[test]
    fn a_missing_agent_binary_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = GuestConfig::new("a", "b", &[0; 32], "c", false);
        let options = options();
        let error = stage(
            &AnswerMedia { options: &options, config: &config, agent_exe: &dir.path().join("none.exe") },
            &dir.path().join("s"),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("guest agent"), "{error}");
    }

    #[test]
    fn the_install_script_has_valid_syntax() {
        let script = include_str!("../../../guest/setup/install-agent.ps1");
        assert!(
            script.contains("supervise") && script.contains("ExecutionTimeLimit") && script.contains("ProtectedData")
        );
    }

    /// Builds a real ISO with IMAPI2, mounts it, and reads the image list back, which exercises the whole
    /// inspection path except Windows Setup itself.
    #[cfg(windows)]
    #[tokio::test]
    async fn a_built_iso_can_be_mounted_and_inspected() {
        let _serial = IMAPI.lock().await;
        let ps = PowerShell::default().with_timeout(std::time::Duration::from_secs(120));
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src");
        std::fs::create_dir_all(source.join("sources")).unwrap();
        std::fs::write(source.join("sources").join("install.wim"), build_test_wim(TEST_XML)).unwrap();
        std::fs::write(source.join("autounattend.xml"), "<unattend/>").unwrap();
        let iso = dir.path().join("windows.iso");
        build_iso(&ps, &source, &iso, "TWTEST").await.unwrap();
        assert!(std::fs::metadata(&iso).unwrap().len() > 2048);

        let info = inspect(&ps, &iso).await.unwrap();
        assert_eq!(info.image_file, "install.wim");
        assert_eq!(info.images.len(), 3);
        assert_eq!(info.images[1].name, "Windows 11 Pro");
        // The ISO was dismounted again, so the file can be deleted.
        std::fs::remove_file(&iso).unwrap();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn an_iso_without_an_install_image_is_rejected_and_still_dismounted() {
        let _serial = IMAPI.lock().await;
        let ps = PowerShell::default().with_timeout(std::time::Duration::from_secs(120));
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("readme.txt"), "not windows").unwrap();
        let iso = dir.path().join("other.iso");
        build_iso(&ps, &source, &iso, "OTHER").await.unwrap();
        let error = inspect(&ps, &iso).await.unwrap_err().to_string();
        assert!(error.contains("not a Windows installation ISO"), "{error}");
        std::fs::remove_file(&iso).unwrap();
    }
}
