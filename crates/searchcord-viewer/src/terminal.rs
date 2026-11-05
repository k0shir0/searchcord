//! Shared native startup header. Drop restores the terminal scroll region.
use std::io::{self, IsTerminal, Write};
pub struct Terminal {
    restore: bool,
}
fn size() -> (usize, usize) {
    #[cfg(unix)]
    {
        let mut size = std::mem::MaybeUninit::<libc::winsize>::zeroed();
        // The output buffer is initialized by ioctl on success.
        if unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, size.as_mut_ptr()) } == 0 {
            let size = unsafe { size.assume_init() };
            if size.ws_col > 0 && size.ws_row > 0 {
                return (size.ws_col as usize, size.ws_row as usize);
            }
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::*;
        let mut info = std::mem::MaybeUninit::<CONSOLE_SCREEN_BUFFER_INFO>::zeroed();
        // The console API writes only within the supplied output buffer.
        if unsafe { GetConsoleScreenBufferInfo(GetStdHandle(STD_OUTPUT_HANDLE), info.as_mut_ptr()) }
            != 0
        {
            let info = unsafe { info.assume_init() };
            return (
                (info.srWindow.Right - info.srWindow.Left + 1).max(1) as usize,
                (info.srWindow.Bottom - info.srWindow.Top + 1).max(1) as usize,
            );
        }
    }
    (
        std::env::var("COLUMNS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(80),
        std::env::var("LINES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(24),
    )
}
fn ansi() -> bool {
    if !io::stdout().is_terminal()
        || std::env::var_os("NO_COLOR").is_some()
        || std::env::var("TERM").ok().as_deref() == Some("dumb")
    {
        return false;
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::*;
        let mut mode = 0;
        // This updates the current console output mode without retaining a handle.
        unsafe {
            let output = GetStdHandle(STD_OUTPUT_HANDLE);
            GetConsoleMode(output, &mut mode) != 0 && SetConsoleMode(output, mode | 4) != 0
        }
    }
    #[cfg(not(windows))]
    true
}
pub fn banner(width: usize, height: Option<usize>, color: bool) -> Vec<String> {
    let width = width.saturating_sub(1).max(1);
    let available = height.map(|h| h.saturating_sub(if color { 2 } else { 0 }));
    let fits = |lines: &[&str]| {
        lines.iter().all(|s| s.len() <= width) && available.is_none_or(|h| h >= lines.len())
    };
    let mut lines: Vec<String> = if color && fits(LARGE) {
        LARGE.iter().map(|s| (*s).into()).collect()
    } else if fits(WIDE) {
        WIDE.iter().map(|s| (*s).into()).collect()
    } else if fits(STACKED) {
        STACKED.iter().map(|s| (*s).into()).collect()
    } else if width >= 14 {
        vec!["[ searchcord ]".into()]
    } else {
        "searchcord"
            .as_bytes()
            .chunks(width)
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect()
    };
    if available.is_some_and(|h| lines.len() > h.max(1)) {
        lines = vec!["searchcord"[..width.min(10)].into()];
    }
    if color {
        if height.is_none_or(|h| h >= lines.len() + 2) {
            lines.insert(0, String::new());
            lines.push(String::new());
        }
        lines = lines
            .into_iter()
            .map(|line| {
                let padding = width.saturating_sub(line.len());
                let left = padding / 2;
                let right = padding - left;
                format!(
                    "\x1b[48;2;255;255;255m\x1b[38;2;141;216;176m{}{line}{}\x1b[K\x1b[0m",
                    " ".repeat(left),
                    " ".repeat(right)
                )
                .replace('#', "█")
            })
            .collect();
    }
    lines
}
impl Terminal {
    pub fn start() -> Self {
        let color = ansi();
        let (width, height) = size();
        let lines = banner(
            width,
            color.then_some(height.saturating_sub(5).max(1)),
            color,
        );
        let mut stdout = io::stdout().lock();
        if color {
            let _ = write!(stdout, "\x1b[0m\x1b[r\x1b[H\x1b[2J");
        }
        let _ = writeln!(stdout, "{}\n", lines.join("\n"));
        let start = lines.len() + 3;
        let restore = color && start < height;
        if restore {
            let _ = write!(stdout, "\x1b[{start};r\x1b[{start};1H");
        }
        let _ = stdout.flush();
        Self { restore }
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        if self.restore {
            let mut stdout = io::stdout().lock();
            let _ = write!(stdout, "\x1b7\x1b[r\x1b8\x1b[0m");
            let _ = stdout.flush();
        }
    }
}
pub fn local_url(host: &str, port: u16) -> String {
    let host = match host {
        "0.0.0.0" => "127.0.0.1",
        "::" => "::1",
        host => host,
    };
    if host.contains(':') {
        format!("http://[{host}]:{port}")
    } else {
        format!("http://{host}:{port}")
    }
}

const LARGE: &[&str] = &[
    " #####   #######   #####   ######    #####   ##   ##   #####    #####   ######   ###### ",
    "##   ##  ##       ##   ##  ##   ##  ##   ##  ##   ##  ##   ##  ##   ##  ##   ##  ##   ##",
    "##       ##       ##   ##  ##   ##  ##       ##   ##  ##       ##   ##  ##   ##  ##   ##",
    " #####   ######   #######  ######   ##       #######  ##       ##   ##  ######   ##   ##",
    "     ##  ##       ##   ##  ## ##    ##       ##   ##  ##       ##   ##  ## ##    ##   ##",
    "##   ##  ##       ##   ##  ##  ##   ##   ##  ##   ##  ##   ##  ##   ##  ##  ##   ##   ##",
    " #####   #######  ##   ##  ##   ##   #####   ##   ##   #####    #####   ##   ##  ###### ",
];

const WIDE: &[&str] = &[
    " #### #####  ###  ####   #### #   #  ####  ###  ####  #### ",
    "#     #     #   # #   # #     #   # #     #   # #   # #   #",
    "#     #     #   # #   # #     #   # #     #   # #   # #   #",
    " ###  ####  ##### ####  #     ##### #     #   # ####  #   #",
    "    # #     #   # # #   #     #   # #     #   # # #   #   #",
    "    # #     #   # #  #  #     #   # #     #   # #  #  #   #",
    "####  ##### #   # #   #  #### #   #  ####  ###  #   # #### ",
];

const STACKED: &[&str] = &[
    " #### #####  ###  ####   #### ## ##",
    "##    ##    ## ## ## ## ##    ## ##",
    " ###  ####  ##### ####  ##    #####",
    "   ## ##    ## ## ## #  ##    ## ##",
    "####  ##### ## ## ##  #  #### ## ##",
    "",
    " ####  ###  ####  ####",
    "##    ## ## ## ## ## ##",
    "##    ## ## ####  ## ##",
    "##    ## ## ## #  ## ##",
    " ####  ###  ##  # ####",
];
