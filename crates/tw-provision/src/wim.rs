//! Reads the image list from a Windows installation image (`install.wim` or `install.esd`) without mounting it
//! and without elevation: the WIM header points at an XML description stored uncompressed in the file.

use std::io::{Read, Seek, SeekFrom};

use tw_core::{Error, Result};

const MAGIC: &[u8; 8] = b"MSWIM\0\0\0";
/// `RESHDR_DISK_SHORT` of the XML data starts here in the header.
const XML_RESOURCE_OFFSET: u64 = 72;
const FLAG_COMPRESSED: u8 = 0x04;
/// The XML of a real image is a few hundred kilobytes at most.
const MAX_XML: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Architecture {
    X86,
    Arm,
    X64,
    Arm64,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageInfo {
    pub index: u32,
    pub name: String,
    pub description: String,
    pub display_name: String,
    pub edition_id: String,
    pub architecture: Architecture,
    pub major: u32,
    pub minor: u32,
    pub build: u32,
}

impl ImageInfo {
    /// A note for the user when the image is not a Windows version this tool is tested with.
    pub fn support_warning(&self) -> Option<String> {
        let supported = self.major == 10 && (self.build == 19045 || self.build >= 22000);
        (!supported).then(|| {
            format!(
                "{} (build {}) is not a tested guest. Windows 10 22H2 and Windows 11 are supported",
                self.name, self.build
            )
        })
    }
}

/// The text of the first `<tag>...</tag>` in `block`.
fn tag<'a>(block: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let start = block.find(&open)? + open.len();
    let end = block[start..].find(&format!("</{name}>"))? + start;
    Some(block[start..end].trim())
}

fn number(block: &str, name: &str) -> u32 {
    tag(block, name).and_then(|v| v.parse().ok()).unwrap_or(0)
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

pub fn parse_xml(xml: &str) -> Vec<ImageInfo> {
    let mut images = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<IMAGE ") {
        let after = &rest[start..];
        let Some(end) = after.find("</IMAGE>") else { break };
        let block = &after[..end];
        rest = &after[end + "</IMAGE>".len()..];
        let index =
            block.split("INDEX=\"").nth(1).and_then(|v| v.split('"').next()).and_then(|v| v.parse().ok()).unwrap_or(0);
        let text = |name: &str| unescape(tag(block, name).unwrap_or_default());
        let architecture = match number(block, "ARCH") {
            0 => Architecture::X86,
            5 => Architecture::Arm,
            9 => Architecture::X64,
            12 => Architecture::Arm64,
            _ => Architecture::Unknown,
        };
        images.push(ImageInfo {
            index,
            name: text("NAME"),
            description: text("DESCRIPTION"),
            display_name: text("DISPLAYNAME"),
            edition_id: text("EDITIONID"),
            architecture,
            major: number(block, "MAJOR"),
            minor: number(block, "MINOR"),
            build: number(block, "BUILD"),
        });
    }
    images
}

fn decode_utf16(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    String::from_utf16_lossy(&units).trim_start_matches('\u{feff}').to_string()
}

pub fn read_images<R: Read + Seek>(reader: &mut R) -> Result<Vec<ImageInfo>> {
    let invalid = || Error::Other("This is not a valid Windows installation image (install.wim)".into());
    let mut magic = [0u8; 8];
    reader.read_exact(&mut magic).map_err(|_| invalid())?;
    if &magic != MAGIC {
        return Err(invalid());
    }
    reader.seek(SeekFrom::Start(XML_RESOURCE_OFFSET))?;
    let mut entry = [0u8; 24];
    reader.read_exact(&mut entry).map_err(|_| invalid())?;
    let mut size_bytes = [0u8; 8];
    size_bytes[..7].copy_from_slice(&entry[..7]);
    let size = u64::from_le_bytes(size_bytes);
    let flags = entry[7];
    let offset = u64::from_le_bytes(entry[8..16].try_into().expect("8 bytes"));
    if flags & FLAG_COMPRESSED != 0 {
        return Err(Error::Other(
            "The image list is compressed in this file; run the check from an elevated prompt so DISM can read it"
                .into(),
        ));
    }
    if size == 0 || size > MAX_XML {
        return Err(invalid());
    }
    reader.seek(SeekFrom::Start(offset))?;
    let mut xml = vec![0u8; size as usize];
    reader.read_exact(&mut xml).map_err(|_| invalid())?;
    let images = parse_xml(&decode_utf16(&xml));
    if images.is_empty() {
        return Err(invalid());
    }
    Ok(images)
}

/// Chooses the image to install: by index, by edition name, or the only one.
pub fn select<'a>(images: &'a [ImageInfo], index: Option<u32>, edition: Option<&str>) -> Result<&'a ImageInfo> {
    let list = || images.iter().map(|i| format!("  {}: {}", i.index, i.name)).collect::<Vec<_>>().join("\n");
    let chosen = match (index, edition) {
        (Some(_), Some(_)) => return Err(Error::Usage("Use either --image-index or --edition, not both".into())),
        (Some(wanted), None) => images
            .iter()
            .find(|i| i.index == wanted)
            .ok_or_else(|| Error::Usage(format!("The ISO has no image {wanted}. Available images:\n{}", list())))?,
        (None, Some(wanted)) => {
            let wanted = wanted.to_lowercase();
            images
                .iter()
                .find(|i| [&i.name, &i.display_name, &i.edition_id].iter().any(|n| n.to_lowercase() == wanted))
                .ok_or_else(|| {
                    Error::Usage(format!("The ISO has no edition '{wanted}'. Available images:\n{}", list()))
                })?
        }
        (None, None) => match images {
            [only] => only,
            _ => {
                return Err(Error::Usage(format!(
                    "The ISO holds several editions. Choose one with --edition NAME or --image-index N:\n{}",
                    list()
                )));
            }
        },
    };
    if chosen.architecture != Architecture::X64 {
        return Err(Error::Usage(format!("'{}' is not a 64-bit x86 image, which this host needs", chosen.name)));
    }
    Ok(chosen)
}

/// A minimal WIM with the given XML, for tests.
#[cfg(test)]
pub(crate) fn build_test_wim(xml: &str) -> Vec<u8> {
    let encoded: Vec<u8> = std::iter::once(0xFEFFu16).chain(xml.encode_utf16()).flat_map(u16::to_le_bytes).collect();
    let mut header = vec![0u8; 208];
    header[..8].copy_from_slice(MAGIC);
    header[8..12].copy_from_slice(&208u32.to_le_bytes());
    let size = encoded.len() as u64;
    header[72..79].copy_from_slice(&size.to_le_bytes()[..7]);
    header[80..88].copy_from_slice(&208u64.to_le_bytes());
    header[88..96].copy_from_slice(&size.to_le_bytes());
    [header, encoded].concat()
}

#[cfg(test)]
pub(crate) const TEST_XML: &str = "<WIM><TOTALBYTES>1</TOTALBYTES>\
<IMAGE INDEX=\"1\"><NAME>Windows 11 Home</NAME><DESCRIPTION>Windows 11 Home</DESCRIPTION><WINDOWS><ARCH>9</ARCH><EDITIONID>Core</EDITIONID><VERSION><MAJOR>10</MAJOR><MINOR>0</MINOR><BUILD>26100</BUILD></VERSION></WINDOWS></IMAGE>\
<IMAGE INDEX=\"2\"><NAME>Windows 11 Pro</NAME><DESCRIPTION>Windows 11 Pro &amp; more</DESCRIPTION><DISPLAYNAME>Win 11 Pro</DISPLAYNAME><WINDOWS><ARCH>9</ARCH><EDITIONID>Professional</EDITIONID><VERSION><MAJOR>10</MAJOR><MINOR>0</MINOR><BUILD>26100</BUILD></VERSION></WINDOWS></IMAGE>\
<IMAGE INDEX=\"3\"><NAME>Windows 11 Arm</NAME><WINDOWS><ARCH>12</ARCH><VERSION><MAJOR>10</MAJOR><MINOR>0</MINOR><BUILD>26100</BUILD></VERSION></WINDOWS></IMAGE></WIM>";

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn images() -> Vec<ImageInfo> {
        parse_xml(TEST_XML)
    }

    #[test]
    fn parses_images() {
        let images = images();
        assert_eq!(images.len(), 3);
        assert_eq!(images[1].index, 2);
        assert_eq!(images[1].name, "Windows 11 Pro");
        assert_eq!(images[1].description, "Windows 11 Pro & more");
        assert_eq!(images[1].edition_id, "Professional");
        assert_eq!(images[1].architecture, Architecture::X64);
        assert_eq!((images[1].major, images[1].minor, images[1].build), (10, 0, 26100));
        assert_eq!(images[2].architecture, Architecture::Arm64);
    }

    #[test]
    fn reads_a_wim_file() {
        let wim = build_test_wim(TEST_XML);
        assert_eq!(read_images(&mut Cursor::new(wim)).unwrap(), images());
    }

    #[test]
    fn rejects_files_that_are_not_wims() {
        assert!(read_images(&mut Cursor::new(b"hello".to_vec())).is_err());
        assert!(read_images(&mut Cursor::new(vec![0u8; 300])).is_err());
        let mut truncated = build_test_wim(TEST_XML);
        truncated.truncate(250);
        assert!(read_images(&mut Cursor::new(truncated)).is_err());
        assert!(read_images(&mut Cursor::new(build_test_wim("<WIM></WIM>"))).is_err(), "no images");
    }

    #[test]
    fn compressed_xml_asks_for_an_elevated_check() {
        let mut wim = build_test_wim(TEST_XML);
        wim[79] = FLAG_COMPRESSED;
        let error = read_images(&mut Cursor::new(wim)).unwrap_err().to_string();
        assert!(error.contains("elevated"), "{error}");
    }

    #[test]
    fn a_hostile_size_does_not_allocate() {
        let mut wim = build_test_wim(TEST_XML);
        wim[72..79].copy_from_slice(&[0xFF; 7]);
        assert!(read_images(&mut Cursor::new(wim)).is_err());
    }

    #[test]
    fn selection_by_index_edition_or_only_image() {
        let images = images();
        assert_eq!(select(&images, Some(2), None).unwrap().name, "Windows 11 Pro");
        assert_eq!(select(&images, None, Some("windows 11 pro")).unwrap().index, 2);
        assert_eq!(select(&images, None, Some("Professional")).unwrap().index, 2);
        assert_eq!(select(&images, None, Some("Win 11 Pro")).unwrap().index, 2);
        assert_eq!(select(&images[1..2], None, None).unwrap().index, 2);
    }

    #[test]
    fn selection_errors_list_the_choices() {
        let images = images();
        let several = select(&images, None, None).unwrap_err().to_string();
        assert!(several.contains("1: Windows 11 Home") && several.contains("2: Windows 11 Pro"), "{several}");
        assert!(select(&images, Some(9), None).is_err());
        assert!(select(&images, None, Some("Enterprise")).is_err());
        assert!(select(&images, Some(1), Some("x")).is_err());
        let arm = select(&images, Some(3), None).unwrap_err().to_string();
        assert!(arm.contains("64-bit x86"), "{arm}");
    }

    #[test]
    fn untested_versions_get_a_warning() {
        let mut image = images().remove(1);
        assert!(image.support_warning().is_none());
        image.build = 19045;
        assert!(image.support_warning().is_none());
        image.build = 17763;
        assert!(image.support_warning().unwrap().contains("17763"));
    }
}
