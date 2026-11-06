//! The terminal the viewer draws on: size, output, and raw input bytes.

pub trait Terminal: Send + Sync {
    /// Columns and rows of the visible window.
    fn size(&self) -> (usize, usize);
    fn write(&self, text: &str);
    /// Returns once the terminal has accepted the bytes, so a slow terminal slows rendering down.
    fn write_and_wait(&self, text: &str);
}

#[cfg(windows)]
pub use windows::WindowsTerminal;

#[cfg(windows)]
mod windows {
    use std::io::Read;

    use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
    use tw_core::{Error, Result};
    use tw_term::TerminalWriter;
    use windows_sys::Win32::System::Console::{
        CONSOLE_MODE, CONSOLE_SCREEN_BUFFER_INFO, GetConsoleMode, GetConsoleScreenBufferInfo, GetStdHandle,
        STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleCP, SetConsoleMode, SetConsoleOutputCP,
    };

    use super::Terminal;

    const ENABLE_PROCESSED_INPUT: u32 = 0x0001;
    const ENABLE_LINE_INPUT: u32 = 0x0002;
    const ENABLE_ECHO_INPUT: u32 = 0x0004;
    const ENABLE_QUICK_EDIT_MODE: u32 = 0x0040;
    const ENABLE_EXTENDED_FLAGS: u32 = 0x0080;
    const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;
    const ENABLE_PROCESSED_OUTPUT: u32 = 0x0001;
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
    const DISABLE_NEWLINE_AUTO_RETURN: u32 = 0x0008;
    const UTF8: u32 = 65001;

    /// The Windows console in raw VT mode. The original modes come back when it is dropped.
    pub struct WindowsTerminal {
        writer: TerminalWriter,
        input_mode: CONSOLE_MODE,
        output_mode: CONSOLE_MODE,
    }

    fn handle(which: u32) -> windows_sys::Win32::Foundation::HANDLE {
        // SAFETY: a plain call; the returned handle is owned by the process.
        unsafe { GetStdHandle(which) }
    }

    impl WindowsTerminal {
        /// Puts the console in raw VT mode and starts reading input. Fails when stdin or stdout is not a console.
        pub fn open() -> Result<(Self, UnboundedReceiver<Vec<u8>>)> {
            let (input, output) = (handle(STD_INPUT_HANDLE), handle(STD_OUTPUT_HANDLE));
            let (mut input_mode, mut output_mode) = (0, 0);
            // SAFETY: valid out-pointers; a failure means the stream is not a console.
            let is_console =
                unsafe { GetConsoleMode(input, &mut input_mode) != 0 && GetConsoleMode(output, &mut output_mode) != 0 };
            if !is_console {
                return Err(Error::Usage(
                    "view needs an interactive terminal; run it in Windows Terminal or a console".into(),
                ));
            }
            let raw_input = (input_mode
                & !(ENABLE_PROCESSED_INPUT | ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_QUICK_EDIT_MODE))
                | ENABLE_EXTENDED_FLAGS
                | ENABLE_VIRTUAL_TERMINAL_INPUT;
            let vt_output = output_mode
                | ENABLE_PROCESSED_OUTPUT
                | ENABLE_VIRTUAL_TERMINAL_PROCESSING
                | DISABLE_NEWLINE_AUTO_RETURN;
            // SAFETY: plain calls with valid handles.
            let configured = unsafe {
                SetConsoleCP(UTF8);
                SetConsoleOutputCP(UTF8);
                SetConsoleMode(input, raw_input) != 0 && SetConsoleMode(output, vt_output) != 0
            };
            if !configured {
                return Err(Error::Prerequisite(
                    "This console does not support virtual terminal mode. Use Windows Terminal".into(),
                ));
            }
            let (sender, receiver) = unbounded_channel();
            std::thread::Builder::new()
                .name("terminal-input".into())
                .spawn(move || {
                    let mut stdin = std::io::stdin().lock();
                    let mut buffer = [0u8; 4096];
                    while let Ok(count) = stdin.read(&mut buffer) {
                        if count == 0 || sender.send(buffer[..count].to_vec()).is_err() {
                            break;
                        }
                    }
                })
                .map_err(Error::Io)?;
            Ok((Self { writer: TerminalWriter::stdout(), input_mode, output_mode }, receiver))
        }
    }

    impl Drop for WindowsTerminal {
        fn drop(&mut self) {
            self.writer.flush();
            // SAFETY: plain calls restoring the modes read in `open`.
            unsafe {
                SetConsoleMode(handle(STD_INPUT_HANDLE), self.input_mode);
                SetConsoleMode(handle(STD_OUTPUT_HANDLE), self.output_mode);
            }
        }
    }

    impl Terminal for WindowsTerminal {
        fn size(&self) -> (usize, usize) {
            // SAFETY: a zeroed structure is a valid out-parameter.
            let info = unsafe {
                let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
                if GetConsoleScreenBufferInfo(handle(STD_OUTPUT_HANDLE), &mut info) == 0 {
                    return (80, 24);
                }
                info
            };
            let columns = (info.srWindow.Right - info.srWindow.Left + 1).max(1) as usize;
            let rows = (info.srWindow.Bottom - info.srWindow.Top + 1).max(1) as usize;
            (columns, rows)
        }

        fn write(&self, text: &str) {
            self.writer.write(text);
        }

        fn write_and_wait(&self, text: &str) {
            self.writer.write_and_wait(text);
        }
    }
}
