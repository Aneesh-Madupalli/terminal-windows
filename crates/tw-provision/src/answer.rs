use tw_core::{Error, Result};

/// The guest account the agent runs as.
pub const AGENT_USER: &str = "agent";

pub struct AnswerOptions {
    pub image_index: u32,
    pub computer_name: String,
    pub password: String,
    /// A Windows locale tag such as `en-US`.
    pub locale: String,
    /// Lets Windows 11 install on virtual hardware without a TPM, Secure Boot or enough RAM checks.
    pub bypass_hardware_checks: bool,
}

/// A random 24-character password with upper case, lower case and digits, using only characters that need
/// no XML or command-line escaping.
pub fn generate_password() -> String {
    const UPPER: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ";
    const LOWER: &[u8] = b"abcdefghijkmnopqrstuvwxyz";
    const DIGIT: &[u8] = b"23456789";
    let all: Vec<u8> = [UPPER, LOWER, DIGIT, b"-_." as &[u8]].concat();
    let mut random = [0u8; 24];
    getrandom::fill(&mut random).expect("the operating system random source is available");
    let mut password: Vec<u8> = random.iter().map(|b| all[*b as usize % all.len()]).collect();
    // One of each class, at random positions, so the result always satisfies the complexity rules.
    for (slot, class) in [(0usize, UPPER), (1, LOWER), (2, DIGIT)] {
        password[slot] = class[random[slot + 3] as usize % class.len()];
    }
    let shift = random[7] as usize % password.len();
    password.rotate_left(shift);
    String::from_utf8(password).expect("ASCII")
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

pub fn validate_locale(locale: &str) -> Result<()> {
    let bytes = locale.as_bytes();
    let valid = (5..=10).contains(&bytes.len())
        && locale.split('-').count() >= 2
        && locale.split('-').all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric()));
    if valid { Ok(()) } else { Err(Error::Usage(format!("Invalid --locale '{locale}'. Example: en-US"))) }
}

const COMPONENT: &str = r#"processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance""#;

/// The first-logon command: find the answer ISO by its label and run the installer from it.
const INSTALL_COMMAND: &str = r#"powershell.exe -NoProfile -ExecutionPolicy Bypass -Command "$d = (Get-Volume -FileSystemLabel TWSETUP).DriveLetter; & \"${d}:\setup\install-agent.ps1\"""#;

const BYPASS_XML: &str = r#"      <RunSynchronous>
        <RunSynchronousCommand wcm:action="add"><Order>1</Order><Path>reg add HKLM\SYSTEM\Setup\LabConfig /v BypassTPMCheck /t REG_DWORD /d 1 /f</Path></RunSynchronousCommand>
        <RunSynchronousCommand wcm:action="add"><Order>2</Order><Path>reg add HKLM\SYSTEM\Setup\LabConfig /v BypassSecureBootCheck /t REG_DWORD /d 1 /f</Path></RunSynchronousCommand>
        <RunSynchronousCommand wcm:action="add"><Order>3</Order><Path>reg add HKLM\SYSTEM\Setup\LabConfig /v BypassRAMCheck /t REG_DWORD /d 1 /f</Path></RunSynchronousCommand>
      </RunSynchronous>
"#;

/// `autounattend.xml`: partitions the disk, installs the chosen image, creates the agent account, signs it in
/// automatically and runs the agent installer.
pub fn autounattend_xml(options: &AnswerOptions) -> Result<String> {
    validate_locale(&options.locale)?;
    if options.password.is_empty() || options.image_index == 0 {
        return Err(Error::Other("The answer file needs a password and an image index".into()));
    }
    let locale = escape(&options.locale);
    let password = escape(&options.password);
    let computer = escape(&options.computer_name);
    let intl = format!(
        "<InputLocale>{locale}</InputLocale><SystemLocale>{locale}</SystemLocale><UILanguage>{locale}</UILanguage><UserLocale>{locale}</UserLocale>"
    );
    let command = escape(INSTALL_COMMAND);
    let bypass = if options.bypass_hardware_checks { BYPASS_XML } else { "" };
    Ok(format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<unattend xmlns="urn:schemas-microsoft-com:unattend">
  <settings pass="windowsPE">
    <component name="Microsoft-Windows-International-Core-WinPE" {COMPONENT}>
      <SetupUILanguage><UILanguage>{locale}</UILanguage></SetupUILanguage>
      {intl}
    </component>
    <component name="Microsoft-Windows-Setup" {COMPONENT}>
      <DiskConfiguration>
        <Disk wcm:action="add">
          <DiskID>0</DiskID>
          <WillWipeDisk>true</WillWipeDisk>
          <CreatePartitions>
            <CreatePartition wcm:action="add"><Order>1</Order><Type>EFI</Type><Size>100</Size></CreatePartition>
            <CreatePartition wcm:action="add"><Order>2</Order><Type>MSR</Type><Size>16</Size></CreatePartition>
            <CreatePartition wcm:action="add"><Order>3</Order><Type>Primary</Type><Extend>true</Extend></CreatePartition>
          </CreatePartitions>
          <ModifyPartitions>
            <ModifyPartition wcm:action="add"><Order>1</Order><PartitionID>1</PartitionID><Format>FAT32</Format><Label>System</Label></ModifyPartition>
            <ModifyPartition wcm:action="add"><Order>2</Order><PartitionID>2</PartitionID></ModifyPartition>
            <ModifyPartition wcm:action="add"><Order>3</Order><PartitionID>3</PartitionID><Format>NTFS</Format><Label>Windows</Label><Letter>C</Letter></ModifyPartition>
          </ModifyPartitions>
        </Disk>
      </DiskConfiguration>
      <ImageInstall>
        <OSImage>
          <InstallFrom><MetaData wcm:action="add"><Key>/IMAGE/INDEX</Key><Value>{index}</Value></MetaData></InstallFrom>
          <InstallTo><DiskID>0</DiskID><PartitionID>3</PartitionID></InstallTo>
        </OSImage>
      </ImageInstall>
{bypass}      <UserData><AcceptEula>true</AcceptEula></UserData>
    </component>
  </settings>
  <settings pass="specialize">
    <component name="Microsoft-Windows-Shell-Setup" {COMPONENT}>
      <ComputerName>{computer}</ComputerName>
    </component>
  </settings>
  <settings pass="oobeSystem">
    <component name="Microsoft-Windows-International-Core" {COMPONENT}>
      {intl}
    </component>
    <component name="Microsoft-Windows-Shell-Setup" {COMPONENT}>
      <OOBE>
        <HideEULAPage>true</HideEULAPage>
        <HideOnlineAccountScreens>true</HideOnlineAccountScreens>
        <HideWirelessSetupInOOBE>true</HideWirelessSetupInOOBE>
        <HideLocalAccountScreen>true</HideLocalAccountScreen>
        <ProtectYourPC>3</ProtectYourPC>
      </OOBE>
      <UserAccounts>
        <LocalAccounts>
          <LocalAccount wcm:action="add">
            <Name>{user}</Name>
            <Group>Administrators</Group>
            <Password><Value>{password}</Value><PlainText>true</PlainText></Password>
          </LocalAccount>
        </LocalAccounts>
      </UserAccounts>
      <AutoLogon>
        <Enabled>true</Enabled>
        <LogonCount>999999</LogonCount>
        <Username>{user}</Username>
        <Password><Value>{password}</Value><PlainText>true</PlainText></Password>
      </AutoLogon>
      <FirstLogonCommands>
        <SynchronousCommand wcm:action="add">
          <Order>1</Order>
          <CommandLine>{command}</CommandLine>
          <Description>Install the terminal-windows guest agent</Description>
        </SynchronousCommand>
      </FirstLogonCommands>
    </component>
  </settings>
</unattend>
"#,
        index = options.image_index,
        bypass = bypass,
        user = AGENT_USER,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> AnswerOptions {
        AnswerOptions {
            image_index: 2,
            computer_name: "WORK".into(),
            password: "Pa55-word_x.".into(),
            locale: "en-US".into(),
            bypass_hardware_checks: false,
        }
    }

    fn doc(xml: &str) -> roxmltree::Document<'_> {
        roxmltree::Document::parse(xml).expect("the answer file is well-formed XML")
    }

    fn text<'a>(document: &'a roxmltree::Document, name: &str) -> Vec<&'a str> {
        document.descendants().filter(|n| n.has_tag_name(name)).filter_map(|n| n.text()).collect()
    }

    #[test]
    fn the_answer_file_is_well_formed_and_complete() {
        let xml = autounattend_xml(&options()).unwrap();
        let document = doc(&xml);
        let passes: Vec<_> =
            document.descendants().filter(|n| n.has_tag_name("settings")).filter_map(|n| n.attribute("pass")).collect();
        assert_eq!(passes, ["windowsPE", "specialize", "oobeSystem"]);
        assert_eq!(text(&document, "Value"), ["2", "Pa55-word_x.", "Pa55-word_x."]);
        assert_eq!(text(&document, "ComputerName"), ["WORK"]);
        assert_eq!(text(&document, "Name").last(), Some(&AGENT_USER));
        assert_eq!(text(&document, "Group"), ["Administrators"]);
        assert_eq!(text(&document, "Enabled"), ["true"]);
        assert_eq!(text(&document, "InputLocale"), ["en-US", "en-US"]);
        assert!(
            !xml.contains("{") || xml.contains("${d}"),
            "no unreplaced placeholders: only the PowerShell variable remains"
        );
    }

    #[test]
    fn partitions_match_a_uefi_install() {
        let document_text = autounattend_xml(&options()).unwrap();
        let document = doc(&document_text);
        assert_eq!(text(&document, "Type"), ["EFI", "MSR", "Primary"]);
        assert_eq!(text(&document, "Format"), ["FAT32", "NTFS"]);
        assert_eq!(text(&document, "WillWipeDisk"), ["true"]);
    }

    #[test]
    fn the_first_logon_command_survives_xml_escaping() {
        let xml = autounattend_xml(&options()).unwrap();
        let document = doc(&xml);
        let command = text(&document, "CommandLine");
        assert_eq!(command, [INSTALL_COMMAND]);
        assert!(command[0].contains("TWSETUP") && command[0].contains("install-agent.ps1"));
    }

    #[test]
    fn hostile_values_are_escaped_not_interpreted() {
        let mut o = options();
        o.password = "a&b<c>\"d'e".into();
        o.computer_name = "</ComputerName><Evil/>".into();
        let xml = autounattend_xml(&o).unwrap();
        let document = doc(&xml);
        assert_eq!(text(&document, "Value")[1], "a&b<c>\"d'e");
        assert_eq!(text(&document, "ComputerName"), ["</ComputerName><Evil/>"]);
        assert!(!document.descendants().any(|n| n.has_tag_name("Evil")));
    }

    #[test]
    fn the_hardware_check_bypass_is_optional_and_placed_before_user_data() {
        let plain = autounattend_xml(&options()).unwrap();
        assert!(!plain.contains("LabConfig"));
        let mut o = options();
        o.bypass_hardware_checks = true;
        let xml = autounattend_xml(&o).unwrap();
        let document = doc(&xml);
        assert_eq!(text(&document, "Path").len(), 3);
        assert!(
            xml.contains("BypassTPMCheck") && xml.contains("BypassSecureBootCheck") && xml.contains("BypassRAMCheck")
        );
        assert!(xml.find("RunSynchronous").unwrap() < xml.find("<UserData>").unwrap());
    }

    #[test]
    fn locales_are_validated() {
        for ok in ["en-US", "de-DE", "pt-BR", "zh-Hans-CN"] {
            assert!(validate_locale(ok).is_ok(), "{ok}");
        }
        for bad in ["", "english", "en_US", "en-", "-US", "en-US<x>", "e", "en-US-aaaaaaaaaaaa"] {
            assert!(validate_locale(bad).is_err(), "{bad}");
        }
        let mut o = options();
        o.locale = "bad".into();
        assert!(autounattend_xml(&o).is_err());
    }

    #[test]
    fn missing_inputs_are_rejected() {
        let mut o = options();
        o.password.clear();
        assert!(autounattend_xml(&o).is_err());
        let mut o = options();
        o.image_index = 0;
        assert!(autounattend_xml(&o).is_err());
    }

    #[test]
    fn passwords_are_complex_and_xml_safe() {
        for _ in 0..200 {
            let p = generate_password();
            assert_eq!(p.len(), 24);
            assert!(p.bytes().any(|b| b.is_ascii_uppercase()), "{p}");
            assert!(p.bytes().any(|b| b.is_ascii_lowercase()), "{p}");
            assert!(p.bytes().any(|b| b.is_ascii_digit()), "{p}");
            assert!(p.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)), "{p}");
        }
        assert_ne!(generate_password(), generate_password());
    }
}
