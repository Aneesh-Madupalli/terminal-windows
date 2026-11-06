//! The Windows desktop: GDI capture, `SendInput` injection, and session state.

use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};

use tw_proto::ErrorCode;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, GetDC, HDC, ReleaseDC, SRCCOPY, SelectObject,
};
use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows_sys::Win32::System::RemoteDesktop::{
    ProcessIdToSessionId, WTSFreeMemory, WTSGetActiveConsoleSessionId, WTSINFOEXW, WTSQuerySessionInformationW,
    WTSSessionInfoEx,
};
use windows_sys::Win32::System::StationsAndDesktops::{
    CloseDesktop, GetUserObjectInformationW, OpenInputDesktop, UOI_NAME,
};
use windows_sys::Win32::System::Threading::{CreateMutexW, GetCurrentProcessId};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEINPUT, MapVirtualKeyW, SendInput,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CURSOR_SHOWING, CURSORINFO, DI_NORMAL, DrawIconEx, GetCursorInfo, GetIconInfo, GetSystemMetrics, ICONINFO,
    SM_CXSCREEN, SM_CYSCREEN,
};

use crate::desktop::{Bitmap, Desktop, DesktopError, Op, Screen, Session};
use crate::input::normalize;
use crate::registry::{read_string, wide};

const DESKTOP_READOBJECTS: u32 = 0x0001;
const NO_SESSION: u32 = 0xFFFF_FFFF;
const WTS_SESSIONSTATE_LOCK: i32 = 0;

/// Per-monitor DPI awareness, so coordinates and screenshots are in real pixels.
pub fn init_process() {
    // SAFETY: a plain call with a constant argument.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

/// Lets a windows-subsystem build print to the console it was started from.
pub fn attach_parent_console() {
    // SAFETY: a plain call; failure (no parent console) is fine.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

/// Holds a named mutex for the life of the process. `None` means another instance already owns it.
pub struct SingleInstance(HANDLE);

impl Drop for SingleInstance {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateMutexW and is closed once.
        unsafe { CloseHandle(self.0) };
    }
}

pub fn single_instance(name: &str) -> Option<SingleInstance> {
    let name = wide(name);
    // SAFETY: `name` is NUL-terminated.
    let handle = unsafe { CreateMutexW(null(), 0, name.as_ptr()) };
    if handle.is_null() {
        return None;
    }
    // SAFETY: reads the error of the call just made.
    if unsafe { GetLastError() } == 183 {
        // SAFETY: the handle is valid and not kept.
        unsafe { CloseHandle(handle) };
        return None;
    }
    Some(SingleInstance(handle))
}

pub struct WinDesktop;

fn last_error() -> String {
    std::io::Error::last_os_error().to_string()
}

fn locked() -> bool {
    let mut info: *mut u16 = null_mut();
    let mut bytes = 0u32;
    // SAFETY: the system allocates the result, which is released with WTSFreeMemory.
    let ok = unsafe { WTSQuerySessionInformationW(null_mut(), NO_SESSION, WTSSessionInfoEx, &mut info, &mut bytes) };
    if ok == 0 || info.is_null() {
        return false;
    }
    // SAFETY: on success `info` points at a WTSINFOEXW that stays valid until WTSFreeMemory.
    let flags = unsafe {
        let ex = &*(info.cast::<WTSINFOEXW>());
        let flags = ex.Data.WTSInfoExLevel1.SessionFlags;
        WTSFreeMemory(info.cast());
        flags
    };
    flags == WTS_SESSIONSTATE_LOCK
}

fn input_desktop_name() -> Option<String> {
    // SAFETY: plain calls; the desktop handle is closed below.
    unsafe {
        let desktop = OpenInputDesktop(0, 0, DESKTOP_READOBJECTS);
        if desktop.is_null() {
            return None;
        }
        let mut buffer = [0u16; 256];
        let mut needed = 0u32;
        let ok = GetUserObjectInformationW(
            desktop,
            UOI_NAME,
            buffer.as_mut_ptr().cast(),
            (buffer.len() * 2) as u32,
            &mut needed,
        );
        CloseDesktop(desktop);
        if ok == 0 {
            return None;
        }
        let end = buffer.iter().position(|unit| *unit == 0).unwrap_or(buffer.len());
        Some(String::from_utf16_lossy(&buffer[..end]))
    }
}

struct Gdi {
    screen: HDC,
    memory: HDC,
    bitmap: *mut c_void,
    previous: *mut c_void,
}

impl Drop for Gdi {
    fn drop(&mut self) {
        // SAFETY: every handle was created by this value and is released once, in reverse order.
        unsafe {
            if !self.memory.is_null() {
                if !self.previous.is_null() {
                    SelectObject(self.memory, self.previous);
                }
                DeleteDC(self.memory);
            }
            if !self.bitmap.is_null() {
                DeleteObject(self.bitmap);
            }
            if !self.screen.is_null() {
                ReleaseDC(null_mut(), self.screen);
            }
        }
    }
}

fn draw_cursor(memory: HDC) {
    // SAFETY: structures are initialised with their sizes; icon bitmaps from GetIconInfo are deleted.
    unsafe {
        let mut cursor: CURSORINFO = zeroed();
        cursor.cbSize = size_of::<CURSORINFO>() as u32;
        if GetCursorInfo(&mut cursor) == 0 || cursor.flags & CURSOR_SHOWING == 0 {
            return;
        }
        let mut icon: ICONINFO = zeroed();
        if GetIconInfo(cursor.hCursor, &mut icon) == 0 {
            return;
        }
        let x = cursor.ptScreenPos.x - icon.xHotspot as i32;
        let y = cursor.ptScreenPos.y - icon.yHotspot as i32;
        DrawIconEx(memory, x, y, cursor.hCursor, 0, 0, 0, null_mut(), DI_NORMAL);
        if !icon.hbmMask.is_null() {
            DeleteObject(icon.hbmMask);
        }
        if !icon.hbmColor.is_null() {
            DeleteObject(icon.hbmColor);
        }
    }
}

fn capture_error(message: impl Into<String>) -> DesktopError {
    DesktopError::new(ErrorCode::CaptureFailed, message)
}

impl Desktop for WinDesktop {
    fn session(&self) -> Session {
        let mut mine = NO_SESSION;
        // SAFETY: plain calls with valid out-pointers.
        let active = unsafe { WTSGetActiveConsoleSessionId() };
        // SAFETY: as above.
        let known = unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut mine) } != 0;
        if active == NO_SESSION || !known || mine != active {
            return Session::NoSession;
        }
        match input_desktop_name() {
            Some(name) if name.eq_ignore_ascii_case("Default") => Session::Ready,
            _ if locked() => Session::Locked,
            _ => Session::SecureDesktop,
        }
    }

    fn screen(&self) -> Result<Screen, DesktopError> {
        // SAFETY: plain calls.
        let (width, height, dpi) =
            unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN), GetDpiForSystem()) };
        if width <= 0 || height <= 0 {
            return Err(capture_error("The display size is unavailable"));
        }
        Ok(Screen { width: width as u32, height: height as u32, dpi: dpi.max(96) })
    }

    fn capture(&self) -> Result<Bitmap, DesktopError> {
        let screen = self.screen()?;
        let (width, height) = (screen.width as i32, screen.height as i32);
        let mut gdi = Gdi { screen: null_mut(), memory: null_mut(), bitmap: null_mut(), previous: null_mut() };
        // SAFETY: each handle is checked before use and released by `Gdi::drop`; `bits` points into the DIB
        // section, which stays alive until `gdi` is dropped at the end of this function.
        unsafe {
            gdi.screen = GetDC(null_mut());
            if gdi.screen.is_null() {
                return Err(capture_error("Cannot open the screen"));
            }
            gdi.memory = CreateCompatibleDC(gdi.screen);
            if gdi.memory.is_null() {
                return Err(capture_error("Cannot create a drawing surface"));
            }
            let mut info: BITMAPINFO = zeroed();
            info.bmiHeader = BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            };
            let mut bits: *mut c_void = null_mut();
            gdi.bitmap = CreateDIBSection(gdi.screen, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if gdi.bitmap.is_null() || bits.is_null() {
                return Err(capture_error(format!("Cannot allocate the capture buffer: {}", last_error())));
            }
            gdi.previous = SelectObject(gdi.memory, gdi.bitmap);
            if BitBlt(gdi.memory, 0, 0, width, height, gdi.screen, 0, 0, SRCCOPY | CAPTUREBLT) == 0 {
                return Err(capture_error(format!("Cannot copy the screen: {}", last_error())));
            }
            draw_cursor(gdi.memory);
            let length = width as usize * height as usize * 4;
            let mut data = std::slice::from_raw_parts(bits.cast::<u8>(), length).to_vec();
            // GDI leaves the alpha byte undefined.
            for pixel in data.chunks_exact_mut(4) {
                pixel[3] = 255;
            }
            Ok(Bitmap::new(screen.width, screen.height, data))
        }
    }

    fn send(&self, ops: &[Op]) -> Result<(), DesktopError> {
        let screen = self.screen()?;
        let inputs: Vec<INPUT> = ops.iter().map(|op| to_input(*op, screen)).collect();
        if inputs.is_empty() {
            return Ok(());
        }
        // SAFETY: `inputs` holds fully initialised INPUT structures.
        let sent = unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), size_of::<INPUT>() as i32) };
        if (sent as usize) < inputs.len() {
            return Err(DesktopError::new(
                ErrorCode::InputFailed,
                format!("Windows injected {sent} of {} input events: {}", inputs.len(), last_error()),
            ));
        }
        Ok(())
    }

    fn os(&self) -> String {
        let key = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
        let build = read_string(key, "CurrentBuild").unwrap_or_default();
        let mut name = read_string(key, "ProductName").unwrap_or_else(|| "Windows".into());
        // Windows 11 still reports "Windows 10" in ProductName.
        if build.parse::<u32>().is_ok_and(|b| b >= 22000) {
            name = name.replacen("Windows 10", "Windows 11", 1);
        }
        match read_string(key, "DisplayVersion") {
            Some(version) => format!("{name} {version} (build {build})"),
            None => format!("{name} (build {build})"),
        }
    }
}

fn mouse(dx: i32, dy: i32, data: u32, flags: u32) -> INPUT {
    // SAFETY: INPUT is plain data; the union member matches `r#type`.
    unsafe {
        let mut input: INPUT = zeroed();
        input.r#type = INPUT_MOUSE;
        input.Anonymous.mi = MOUSEINPUT { dx, dy, mouseData: data, dwFlags: flags, time: 0, dwExtraInfo: 0 };
        input
    }
}

fn keyboard(vk: u16, scan: u16, flags: u32) -> INPUT {
    // SAFETY: INPUT is plain data; the union member matches `r#type`.
    unsafe {
        let mut input: INPUT = zeroed();
        input.r#type = INPUT_KEYBOARD;
        input.Anonymous.ki = KEYBDINPUT { wVk: vk, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 };
        input
    }
}

fn to_input(op: Op, screen: Screen) -> INPUT {
    match op {
        Op::MoveTo { x, y } => {
            mouse(normalize(x, screen.width), normalize(y, screen.height), 0, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE)
        }
        Op::Button { right, down } => {
            let flags = match (right, down) {
                (false, true) => MOUSEEVENTF_LEFTDOWN,
                (false, false) => MOUSEEVENTF_LEFTUP,
                (true, true) => MOUSEEVENTF_RIGHTDOWN,
                (true, false) => MOUSEEVENTF_RIGHTUP,
            };
            mouse(0, 0, 0, flags)
        }
        Op::Wheel { horizontal, delta } => {
            mouse(0, 0, delta as u32, if horizontal { MOUSEEVENTF_HWHEEL } else { MOUSEEVENTF_WHEEL })
        }
        Op::Key { vk, extended, down } => {
            // SAFETY: a plain call.
            let scan = unsafe { MapVirtualKeyW(u32::from(vk), 0) } as u16;
            let mut flags = 0;
            if extended {
                flags |= KEYEVENTF_EXTENDEDKEY;
            }
            if !down {
                flags |= KEYEVENTF_KEYUP;
            }
            keyboard(vk, scan, flags)
        }
        Op::Unicode { unit, down } => keyboard(0, unit, KEYEVENTF_UNICODE | if down { 0 } else { KEYEVENTF_KEYUP }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // These tests only read the machine's state. Nothing here injects input.

    #[test]
    fn the_screen_has_a_size_and_dpi() {
        let screen = WinDesktop.screen().unwrap();
        assert!(screen.width >= 320 && screen.height >= 200, "{screen:?}");
        assert!(screen.dpi >= 96);
    }

    #[test]
    fn capture_matches_the_screen_size_and_is_opaque() {
        let screen = WinDesktop.screen().unwrap();
        let bitmap = WinDesktop.capture().unwrap();
        assert_eq!((bitmap.width, bitmap.height), (screen.width, screen.height));
        assert_eq!(bitmap.data.len(), screen.width as usize * screen.height as usize * 4);
        assert!(bitmap.data.chunks_exact(4).step_by(997).all(|p| p[3] == 255));
        // Capturing repeatedly must not leak GDI objects or fail.
        for _ in 0..5 {
            WinDesktop.capture().unwrap();
        }
    }

    #[test]
    fn the_session_can_be_queried() {
        let session = WinDesktop.session();
        assert!(
            matches!(session, Session::Ready | Session::NoSession | Session::Locked | Session::SecureDesktop),
            "{session:?}"
        );
    }

    #[test]
    fn the_os_description_names_windows_and_the_build() {
        let os = WinDesktop.os();
        assert!(os.starts_with("Windows") && os.contains("build"), "{os}");
    }

    #[test]
    fn events_convert_to_the_expected_input_records() {
        let screen = Screen { width: 1280, height: 800, dpi: 96 };
        // SAFETY: reads the union member written by `mouse`/`keyboard`.
        unsafe {
            let input = to_input(Op::MoveTo { x: 640, y: 400 }, screen);
            assert_eq!(input.r#type, INPUT_MOUSE);
            assert_eq!(input.Anonymous.mi.dwFlags, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE);
            assert_eq!(input.Anonymous.mi.dx, normalize(640, 1280));
            let wheel = to_input(Op::Wheel { horizontal: true, delta: -120 }, screen);
            assert_eq!(wheel.Anonymous.mi.dwFlags, MOUSEEVENTF_HWHEEL);
            assert_eq!(wheel.Anonymous.mi.mouseData as i32, -120);
            let unicode = to_input(Op::Unicode { unit: 0x20AC, down: false }, screen);
            assert_eq!(unicode.Anonymous.ki.wVk, 0);
            assert_eq!(unicode.Anonymous.ki.wScan, 0x20AC);
            assert_eq!(unicode.Anonymous.ki.dwFlags, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP);
            let arrow = to_input(Op::Key { vk: 0x25, extended: true, down: true }, screen);
            assert_eq!(arrow.Anonymous.ki.dwFlags, KEYEVENTF_EXTENDEDKEY);
            assert_ne!(arrow.Anonymous.ki.wScan, 0, "the scan code comes from MapVirtualKeyW");
        }
    }

    #[test]
    fn a_second_instance_of_a_named_mutex_is_refused() {
        let name = format!("Local\\terminal-windows-test-{}", std::process::id());
        let first = single_instance(&name);
        assert!(first.is_some());
        assert!(single_instance(&name).is_none());
        drop(first);
        assert!(single_instance(&name).is_some(), "released after the first owner exits");
    }
}
