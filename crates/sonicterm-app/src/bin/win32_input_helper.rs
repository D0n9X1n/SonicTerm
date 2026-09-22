//! Bounded ConPTY child reporting console key records for native input integration tests.

#[cfg(windows)]
mod console {
    use std::{
        fs::{File, OpenOptions},
        io::{self, Write},
        os::windows::io::AsRawHandle,
        thread,
        time::{Duration, Instant},
    };
    use windows::Win32::{
        Foundation::HANDLE,
        System::Console::{
            GetConsoleMode, GetNumberOfConsoleInputEvents, ReadConsoleInputW, SetConsoleMode,
            CONSOLE_MODE, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT,
            ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, INPUT_RECORD,
            KEY_EVENT,
        },
    };

    struct SavedMode<'a> {
        file: &'a File,
        original: CONSOLE_MODE,
    }

    impl<'a> SavedMode<'a> {
        fn change(file: &'a File, clear: u32, set: u32) -> io::Result<Self> {
            let mut original = CONSOLE_MODE::default();
            // SAFETY: file owns an explicitly opened console handle for this child's lifetime.
            unsafe {
                GetConsoleMode(HANDLE(file.as_raw_handle()), &mut original)
                    .map_err(io::Error::other)?;
                SetConsoleMode(
                    HANDLE(file.as_raw_handle()),
                    CONSOLE_MODE((original.0 & !clear) | set),
                )
                .map_err(io::Error::other)?;
            }
            Ok(Self { file, original })
        }
    }

    // Lifecycle: SavedMode restores this child's mode before the borrowed console file closes.
    impl Drop for SavedMode<'_> {
        fn drop(&mut self) {
            if let Err(error) =
                // SAFETY: file remains live through this guard's borrow; restoration affects only this console.
                unsafe { SetConsoleMode(HANDLE(self.file.as_raw_handle()), self.original) }
            {
                eprintln!("console mode restore failed: {error}");
            }
        }
    }

    /// Runs the child-only console exchange without inheriting potentially invalid standard handles.
    pub(super) fn run() -> io::Result<()> {
        let input = OpenOptions::new().read(true).write(true).open("CONIN$")?;
        let mut output = OpenOptions::new().read(true).write(true).open("CONOUT$")?;
        let output_mode_file = output.try_clone()?;
        let _input_mode = SavedMode::change(
            &input,
            ENABLE_PROCESSED_INPUT.0
                | ENABLE_LINE_INPUT.0
                | ENABLE_ECHO_INPUT.0
                | ENABLE_VIRTUAL_TERMINAL_INPUT.0,
            0,
        )?;
        let _output_mode =
            SavedMode::change(&output_mode_file, 0, ENABLE_VIRTUAL_TERMINAL_PROCESSING.0)?;
        output.write_all(b"[WIN32_READY]\r\n")?;
        output.flush()?;

        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            // When: deadline expires, fail rather than leave a child blocked in console input.
            if Instant::now() >= deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "console input deadline"));
            }
            let mut available = 0;
            // SAFETY: input owns this child's live console input handle.
            unsafe {
                GetNumberOfConsoleInputEvents(HANDLE(input.as_raw_handle()), &mut available)
                    .map_err(io::Error::other)?;
            }
            // When: available is zero, poll with a deadline instead of entering blocking ReadConsoleInputW.
            if available == 0 {
                thread::sleep(Duration::from_millis(5));
                continue;
            }
            let mut records = [INPUT_RECORD::default(); 16];
            let limit = usize::try_from(available).unwrap_or(records.len()).min(records.len());
            let mut read = 0;
            // SAFETY: this is the console's only reader; the bounded slice never requests unavailable records.
            unsafe {
                ReadConsoleInputW(HANDLE(input.as_raw_handle()), &mut records[..limit], &mut read)
                    .map_err(io::Error::other)?;
            }
            for record in &records[..read as usize] {
                // When: record.EventType differs from KEY_EVENT, its union has no keyboard payload.
                if u32::from(record.EventType) != KEY_EVENT {
                    continue;
                }
                let (key, unicode) =
                // SAFETY: EventType was checked as KEY_EVENT and the input API populated the Unicode union.
                unsafe {
                    let key = record.Event.KeyEvent;
                    (key, key.uChar.UnicodeChar)
                };
                writeln!(
                    output,
                    "[WIN32_RECORD {};{};{};{};{};{}]\r",
                    key.wVirtualKeyCode,
                    key.wVirtualScanCode,
                    unicode,
                    u8::from(key.bKeyDown.as_bool()),
                    key.dwControlKeyState,
                    key.wRepeatCount,
                )?;
                output.flush()?;
                // When: key.wVirtualKeyCode reaches F12 release, all requested native records have been reported.
                if key.wVirtualKeyCode == 0x7b && !key.bKeyDown.as_bool() {
                    output.write_all(b"[NATIVE_DONE]\r\n")?;
                    output.flush()?;
                    return Ok(());
                }
            }
        }
    }
}

fn main() -> std::io::Result<()> {
    #[cfg(windows)]
    console::run()?;
    Ok(())
}
