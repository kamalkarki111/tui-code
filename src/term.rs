//! Terminal: raw mode, ANSI draw, robust key reading (macOS/Linux).

use std::io::{self, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};

static RAW_ACTIVE: AtomicBool = AtomicBool::new(false);
static mut SAVED_TERMIOS: Option<Termios> = None;
static mut SAVED_FD: RawFd = -1;

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
struct Termios {
    c_iflag: u64,
    c_oflag: u64,
    c_cflag: u64,
    c_lflag: u64,
    c_cc: [u8; 20],
    _pad: [u8; 4],
    c_ispeed: u64,
    c_ospeed: u64,
}

#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Clone, Copy)]
struct Termios {
    c_iflag: u32,
    c_oflag: u32,
    c_cflag: u32,
    c_lflag: u32,
    c_line: u8,
    c_cc: [u8; 32],
    c_ispeed: u32,
    c_ospeed: u32,
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
#[repr(C)]
#[derive(Clone, Copy)]
struct Termios {
    c_iflag: u64,
    c_oflag: u64,
    c_cflag: u64,
    c_lflag: u64,
    c_cc: [u8; 32],
    c_ispeed: u64,
    c_ospeed: u64,
}

extern "C" {
    fn tcgetattr(fd: i32, termios_p: *mut Termios) -> i32;
    fn tcsetattr(fd: i32, optional_actions: i32, termios_p: *const Termios) -> i32;
    fn ioctl(fd: i32, request: usize, ...) -> i32;
    fn read(fd: i32, buf: *mut u8, count: usize) -> isize;
    fn open(path: *const u8, oflag: i32, ...) -> i32;
    fn isatty(fd: i32) -> i32;
    fn poll(fds: *mut PollFd, nfds: u32, timeout: i32) -> i32;
}

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

const TCSANOW: i32 = 0;
const POLLIN: i16 = 0x0001;
#[cfg(target_os = "macos")]
const O_RDWR: i32 = 2;
#[cfg(not(target_os = "macos"))]
const O_RDWR: i32 = 2;

// Full cfmakeraw-style masks so Ctrl+S / Ctrl+Q are delivered (not XON/XOFF).
#[cfg(target_os = "macos")]
mod fl {
    pub const ECHO: u64 = 0x0000_0008;
    pub const ECHONL: u64 = 0x0000_0010;
    pub const ICANON: u64 = 0x0000_0100;
    pub const ISIG: u64 = 0x0000_0080;
    pub const IEXTEN: u64 = 0x0000_0400;
    pub const IXON: u64 = 0x0000_0200;
    pub const IXOFF: u64 = 0x0000_0400;
    pub const ICRNL: u64 = 0x0000_0100;
    pub const INLCR: u64 = 0x0000_0040;
    pub const IGNCR: u64 = 0x0000_0080;
    pub const ISTRIP: u64 = 0x0000_0020;
    pub const IGNBRK: u64 = 0x0000_0001;
    pub const BRKINT: u64 = 0x0000_0002;
    pub const PARMRK: u64 = 0x0000_0008;
    pub const OPOST: u64 = 0x0000_0001;
    pub const CSIZE: u64 = 0x0000_0300;
    pub const PARENB: u64 = 0x0000_1000;
    pub const CS8: u64 = 0x0000_0300;
    pub const VMIN: usize = 16;
    pub const VTIME: usize = 17;
}

#[cfg(target_os = "linux")]
mod fl {
    pub const ECHO: u32 = 0x0000_0008;
    pub const ECHONL: u32 = 0x0000_0040;
    pub const ICANON: u32 = 0x0000_0002;
    pub const ISIG: u32 = 0x0000_0001;
    pub const IEXTEN: u32 = 0x0000_8000;
    pub const IXON: u32 = 0x0000_0400;
    pub const IXOFF: u32 = 0x0000_1000;
    pub const ICRNL: u32 = 0x0000_0100;
    pub const INLCR: u32 = 0x0000_0040;
    pub const IGNCR: u32 = 0x0000_0080;
    pub const ISTRIP: u32 = 0x0000_0020;
    pub const IGNBRK: u32 = 0x0000_0001;
    pub const BRKINT: u32 = 0x0000_0002;
    pub const PARMRK: u32 = 0x0000_0008;
    pub const OPOST: u32 = 0x0000_0001;
    pub const CSIZE: u32 = 0x0000_0030;
    pub const PARENB: u32 = 0x0000_0100;
    pub const CS8: u32 = 0x0000_0030;
    pub const VMIN: usize = 6;
    pub const VTIME: usize = 5;
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod fl {
    pub const ECHO: u64 = 0x0000_0008;
    pub const ECHONL: u64 = 0x0000_0010;
    pub const ICANON: u64 = 0x0000_0100;
    pub const ISIG: u64 = 0x0000_0080;
    pub const IEXTEN: u64 = 0x0000_0400;
    pub const IXON: u64 = 0x0000_0200;
    pub const IXOFF: u64 = 0x0000_0400;
    pub const ICRNL: u64 = 0x0000_0100;
    pub const INLCR: u64 = 0x0000_0040;
    pub const IGNCR: u64 = 0x0000_0080;
    pub const ISTRIP: u64 = 0x0000_0020;
    pub const IGNBRK: u64 = 0x0000_0001;
    pub const BRKINT: u64 = 0x0000_0002;
    pub const PARMRK: u64 = 0x0000_0008;
    pub const OPOST: u64 = 0x0000_0001;
    pub const CSIZE: u64 = 0x0000_0300;
    pub const PARENB: u64 = 0x0000_1000;
    pub const CS8: u64 = 0x0000_0300;
    pub const VMIN: usize = 16;
    pub const VTIME: usize = 17;
}

#[cfg(target_os = "macos")]
const TIOCGWINSZ: usize = 0x40087468;
#[cfg(not(target_os = "macos"))]
const TIOCGWINSZ: usize = 0x5413;

#[repr(C)]
struct Winsize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

/// Emergency restore callable from panic hook / Ctrl+C path.
pub fn force_restore_terminal() {
    if !RAW_ACTIVE.swap(false, Ordering::SeqCst) {
        return;
    }
    unsafe {
        let fd = SAVED_FD;
        if fd >= 0 {
            if let Some(ref t) = SAVED_TERMIOS {
                let _ = tcsetattr(fd, TCSANOW, t);
            }
        }
    }
    let mut out = io::stdout();
    let _ = out.write_all(b"\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[0m\x1b[?25h\x1b[?1049l\x1b[ q\r\n");
    let _ = out.flush();
}

pub struct Terminal {
    tty_fd: RawFd,
    owns_fd: bool,
    stdout: io::Stdout,
    orig: Termios,
    pub width: u16,
    pub height: u16,
}

impl Terminal {
    pub fn enter() -> io::Result<Self> {
        // Prefer /dev/tty so keys work even if stdin is redirected.
        let (tty_fd, owns_fd) = open_tty_fd()?;
        if unsafe { isatty(tty_fd) } == 0 {
            return Err(io::Error::new(io::ErrorKind::Other, "not a tty"));
        }

        let mut orig = unsafe { std::mem::zeroed::<Termios>() };
        if unsafe { tcgetattr(tty_fd, &mut orig) } != 0 {
            return Err(io::Error::last_os_error());
        }

        let mut raw = orig;
        enable_raw(&mut raw);
        if unsafe { tcsetattr(tty_fd, TCSANOW, &raw) } != 0 {
            return Err(io::Error::last_os_error());
        }

        RAW_ACTIVE.store(true, Ordering::SeqCst);
        unsafe {
            SAVED_TERMIOS = Some(orig);
            SAVED_FD = tty_fd;
        }

        let mut term = Terminal {
            tty_fd,
            owns_fd,
            stdout: io::stdout(),
            orig,
            width: 80,
            height: 24,
        };
        term.refresh_size();
        // alt screen, clear, home; keep cursor visible for editor later
        // alt screen + mouse (SGR 1006 + button tracking 1002 + wheel 1000)
        term.write_str("\x1b[?1049h\x1b[2J\x1b[H\x1b[?25h")?;
        term.write_str("\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h")?;
        term.flush()?;
        Ok(term)
    }

    pub fn refresh_size(&mut self) {
        let mut ws = Winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        if unsafe { ioctl(self.tty_fd, TIOCGWINSZ, &mut ws) } == 0 && ws.ws_col > 0 && ws.ws_row > 0
        {
            self.width = ws.ws_col;
            self.height = ws.ws_row;
        }
    }

    pub fn write_str(&mut self, s: &str) -> io::Result<()> {
        self.stdout.write_all(s.as_bytes())
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.stdout.flush()
    }

    pub fn move_to(&mut self, row: u16, col: u16) -> io::Result<()> {
        write!(self.stdout, "\x1b[{};{}H", row, col)
    }

    pub fn set_fg(&mut self, r: u8, g: u8, b: u8) -> io::Result<()> {
        write!(self.stdout, "\x1b[38;2;{};{};{}m", r, g, b)
    }

    pub fn set_bg(&mut self, r: u8, g: u8, b: u8) -> io::Result<()> {
        write!(self.stdout, "\x1b[48;2;{};{};{}m", r, g, b)
    }

    pub fn reset_style(&mut self) -> io::Result<()> {
        self.write_str("\x1b[0m")
    }

    pub fn show_cursor(&mut self) -> io::Result<()> {
        self.write_str("\x1b[?25h")
    }

    pub fn hide_cursor(&mut self) -> io::Result<()> {
        self.write_str("\x1b[?25l")
    }

    pub fn set_cursor_shape_bar(&mut self) -> io::Result<()> {
        self.write_str("\x1b[6 q")
    }

    fn read_byte_blocking(&self) -> io::Result<u8> {
        let mut b = [0u8; 1];
        loop {
            let n = unsafe { read(self.tty_fd, b.as_mut_ptr(), 1) };
            if n == 1 {
                return Ok(b[0]);
            }
            if n < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
        }
    }

    /// Read one extra byte if available within `timeout_ms` (for escape sequences).
    fn read_byte_timeout(&self, timeout_ms: i32) -> Option<u8> {
        let mut pfd = PollFd {
            fd: self.tty_fd,
            events: POLLIN,
            revents: 0,
        };
        let pr = unsafe { poll(&mut pfd, 1, timeout_ms) };
        if pr <= 0 || pfd.revents & POLLIN == 0 {
            return None;
        }
        let mut b = [0u8; 1];
        let n = unsafe { read(self.tty_fd, b.as_mut_ptr(), 1) };
        if n == 1 {
            Some(b[0])
        } else {
            None
        }
    }

    pub fn read_input(&mut self) -> io::Result<Input> {
        let first = self.read_byte_blocking()?;
        Ok(self.parse_input(first))
    }

    /// Non-blocking-ish: wait up to `timeout_ms` for a key/mouse event.
    pub fn poll_input(&mut self, timeout_ms: i32) -> io::Result<Option<Input>> {
        let mut pfd = PollFd {
            fd: self.tty_fd,
            events: POLLIN,
            revents: 0,
        };
        let pr = unsafe { poll(&mut pfd, 1, timeout_ms) };
        if pr < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                return Ok(None);
            }
            return Err(err);
        }
        if pr == 0 || pfd.revents & POLLIN == 0 {
            return Ok(None);
        }
        let first = self.read_byte_blocking()?;
        Ok(Some(self.parse_input(first)))
    }

    /// Back-compat: keys only (mouse events become Unknown key — prefer read_input).
    pub fn read_key(&mut self) -> io::Result<Key> {
        match self.read_input()? {
            Input::Key(k) => Ok(k),
            Input::Mouse(_) => Ok(Key::Unknown),
        }
    }

    fn parse_input(&self, first: u8) -> Input {
        match first {
            0x1b => {
                let Some(second) = self.read_byte_timeout(40) else {
                    return Input::Key(Key::Esc);
                };
                if second == b'[' {
                    let mut seq = Vec::new();
                    loop {
                        let Some(b) = self.read_byte_timeout(40) else {
                            break;
                        };
                        seq.push(b);
                        if (0x40..=0x7e).contains(&b) {
                            break;
                        }
                        // mouse SGR can be longer; allow more bytes
                        if seq.len() >= 64 {
                            break;
                        }
                    }
                    return parse_csi_input(&seq);
                }
                if second == b'O' {
                    let k = match self.read_byte_timeout(40) {
                        Some(b'A') => Key::Up,
                        Some(b'B') => Key::Down,
                        Some(b'C') => Key::Right,
                        Some(b'D') => Key::Left,
                        Some(b'H') => Key::Home,
                        Some(b'F') => Key::End,
                        _ => Key::Esc,
                    };
                    return Input::Key(k);
                }
                Input::Key(Key::Esc)
            }
            0x7f | 0x08 => Input::Key(Key::Backspace),
            // In raw mode, Enter is almost always CR (0x0D). LF (0x0A) is Ctrl+J —
            // do NOT treat it as Enter or the terminal panel shortcut never fires.
            b'\r' => Input::Key(Key::Enter),
            b'\t' => Input::Key(Key::Tab),
            // Ctrl+Space is NUL (0) on many terminals — treat as completion trigger
            0x00 => Input::Key(Key::Ctrl(' ')),
            // 0x0A = Ctrl+J, 0x0B = Ctrl+K, … (c < 0x20, c != 0 already handled)
            c if c > 0 && c < 0x20 => {
                let ch = (c + b'a' - 1) as char;
                Input::Key(Key::Ctrl(ch))
            }
            c if c < 0x80 => Input::Key(Key::Char(c as char)),
            c => {
                let width = utf8_width(c);
                let mut bytes = vec![c];
                for _ in 1..width {
                    if let Ok(b) = self.read_byte_blocking() {
                        bytes.push(b);
                    }
                }
                Input::Key(
                    std::str::from_utf8(&bytes)
                        .ok()
                        .and_then(|s| s.chars().next())
                        .map(Key::Char)
                        .unwrap_or(Key::Unknown),
                )
            }
        }
    }

    pub fn restore(&mut self) -> io::Result<()> {
        if !RAW_ACTIVE.swap(false, Ordering::SeqCst) {
            return Ok(());
        }
        let _ = unsafe { tcsetattr(self.tty_fd, TCSANOW, &self.orig) };
        // disable mouse modes then leave alt screen
        let _ = self.write_str(
            "\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[0m\x1b[?25h\x1b[?1049l\x1b[ q",
        );
        let _ = self.flush();
        Ok(())
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.restore();
        if self.owns_fd && self.tty_fd >= 0 {
            unsafe {
                extern "C" {
                    fn close(fd: i32) -> i32;
                }
                let _ = close(self.tty_fd);
            }
        }
    }
}

fn open_tty_fd() -> io::Result<(RawFd, bool)> {
    let stdin_fd = io::stdin().as_raw_fd();
    if unsafe { isatty(stdin_fd) } != 0 {
        return Ok((stdin_fd, false));
    }
    let path = b"/dev/tty\0";
    let fd = unsafe { open(path.as_ptr(), O_RDWR) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((fd, true))
}

fn enable_raw(t: &mut Termios) {
    use fl::*;
    // Match cfmakeraw(3): Ctrl+S (0x13) must NOT be software flow-control.
    t.c_iflag &= !(IGNBRK
        | BRKINT
        | PARMRK
        | ISTRIP
        | INLCR
        | IGNCR
        | ICRNL
        | IXON
        | IXOFF);
    t.c_oflag &= !OPOST;
    t.c_lflag &= !(ECHO | ECHONL | ICANON | ISIG | IEXTEN);
    t.c_cflag &= !(CSIZE | PARENB);
    t.c_cflag |= CS8;
    t.c_cc[VMIN] = 1;
    t.c_cc[VTIME] = 0;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Ctrl(char),
    Enter,
    Backspace,
    Tab,
    Esc,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Delete,
    Unknown,
}

/// Mouse button (SGR / X10 mapping simplified).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
    Other(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    /// Button pressed
    Down,
    /// Button released
    Up,
    /// Motion while button held (drag)
    Drag,
    /// Wheel tick (encoded as down on some terminals)
    Scroll,
}

/// 1-based cell coordinates (terminal column/row), matching ANSI cursor addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    pub button: MouseButton,
    pub action: MouseAction,
    pub col: u16,
    pub row: u16,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// Unified input from the TTY.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    Key(Key),
    Mouse(MouseEvent),
}

fn utf8_width(b: u8) -> usize {
    if b & 0x80 == 0 {
        1
    } else if b & 0xE0 == 0xC0 {
        2
    } else if b & 0xF0 == 0xE0 {
        3
    } else if b & 0xF8 == 0xF0 {
        4
    } else {
        1
    }
}

fn parse_csi_input(seq: &[u8]) -> Input {
    if seq.is_empty() {
        return Input::Key(Key::Unknown);
    }

    // SGR mouse: <Cb;Cx;Cy M  or  <Cb;Cx;Cy m  (m = release)
    if seq[0] == b'<' {
        if let Some(ev) = parse_sgr_mouse(&seq[1..]) {
            return Input::Mouse(ev);
        }
        return Input::Key(Key::Unknown);
    }

    // Legacy X10 mouse: M Cb Cx Cy (3 bytes after M is not in CSI payload — we get M as last?)
    // When sequence is [ M, cb, cx, cy ] from reading until final — actually final is M and
    // payload is empty with old style ESC [ M Cb Cx Cy as separate bytes. Handled if we
    // only got "M" as seq with prior bytes wrong. Try: seq ends with M and starts with digits — skip.

    let last = *seq.last().unwrap();
    let key = match last {
        b'A' => Key::Up,
        b'B' => Key::Down,
        b'C' => Key::Right,
        b'D' => Key::Left,
        b'H' => Key::Home,
        b'F' => Key::End,
        b'~' => {
            let num: u32 = seq
                .iter()
                .take_while(|c| c.is_ascii_digit())
                .fold(0, |a, &c| a * 10 + (c - b'0') as u32);
            match num {
                1 | 7 => Key::Home,
                4 | 8 => Key::End,
                3 => Key::Delete,
                5 => Key::PageUp,
                6 => Key::PageDown,
                _ => Key::Unknown,
            }
        }
        _ => Key::Unknown,
    };
    Input::Key(key)
}

/// Parse SGR mouse payload: `Cb;Cx;Cy` + final `M`/`m` already in `seq` last byte.
fn parse_sgr_mouse(seq: &[u8]) -> Option<MouseEvent> {
    if seq.len() < 2 {
        return None;
    }
    let final_b = *seq.last()?;
    if final_b != b'M' && final_b != b'm' {
        return None;
    }
    let body = std::str::from_utf8(&seq[..seq.len() - 1]).ok()?;
    let mut parts = body.split(';');
    let cb: u32 = parts.next()?.parse().ok()?;
    let cx: u32 = parts.next()?.parse().ok()?;
    let cy: u32 = parts.next()?.parse().ok()?;

    let btn_bits = cb & 0b11;
    let drag = (cb & 0x20) != 0;
    let wheel = (cb & 0x40) != 0;
    let ctrl = (cb & 0x10) != 0;
    let alt = (cb & 0x08) != 0;
    let shift = (cb & 0x04) != 0;

    let (button, action) = if wheel {
        let button = if btn_bits == 0 {
            MouseButton::WheelUp
        } else {
            MouseButton::WheelDown
        };
        (button, MouseAction::Scroll)
    } else {
        let button = match btn_bits {
            0 => MouseButton::Left,
            1 => MouseButton::Middle,
            2 => MouseButton::Right,
            _ => MouseButton::Other(btn_bits as u8),
        };
        let action = if final_b == b'm' {
            MouseAction::Up
        } else if drag {
            MouseAction::Drag
        } else {
            MouseAction::Down
        };
        (button, action)
    };

    Some(MouseEvent {
        button,
        action,
        col: cx.min(u16::MAX as u32) as u16,
        row: cy.min(u16::MAX as u32) as u16,
        ctrl,
        alt,
        shift,
    })
}
