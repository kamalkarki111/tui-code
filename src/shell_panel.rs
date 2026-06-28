//! VS Code–style integrated terminal panel: real PTY shells, multiple tabs, "+" to add.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;

extern "C" {
    fn openpty(
        amaster: *mut i32,
        aslave: *mut i32,
        name: *mut libc_char,
        termp: *const u8,
        winp: *const Winsize,
    ) -> i32;
    fn fork() -> i32;
    fn setsid() -> i32;
    fn ioctl(fd: i32, req: usize, ...) -> i32;
    fn execvp(file: *const libc_char, argv: *const *const libc_char) -> i32;
    fn close(fd: i32) -> i32;
    fn read(fd: i32, buf: *mut u8, n: usize) -> isize;
    fn write(fd: i32, buf: *const u8, n: usize) -> isize;
    fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    fn kill(pid: i32, sig: i32) -> i32;
    fn dup2(old: i32, new: i32) -> i32;
    fn chdir(path: *const libc_char) -> i32;
}

type libc_char = i8;

#[repr(C)]
struct Winsize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

#[cfg(target_os = "macos")]
const TIOCSCTTY: usize = 0x20007461;
#[cfg(not(target_os = "macos"))]
const TIOCSCTTY: usize = 0x540E;

#[cfg(target_os = "macos")]
const TIOCSWINSZ: usize = 0x80087467;
#[cfg(not(target_os = "macos"))]
const TIOCSWINSZ: usize = 0x5414;

const F_GETFL: i32 = 3;
const F_SETFL: i32 = 4;
#[cfg(target_os = "macos")]
const O_NONBLOCK: i32 = 4;
#[cfg(not(target_os = "macos"))]
const O_NONBLOCK: i32 = 0x800;

const WNOHANG: i32 = 1;
const SIGTERM: i32 = 15;
const SIGKILL: i32 = 9;

const MAX_SCROLLBACK: usize = 5000;

pub struct ShellSession {
    pub title: String,
    master: RawFd,
    /// Keep owned so FD stays open; we use master for I/O.
    _master_owned: OwnedFd,
    child_pid: i32,
    /// Raw output bytes (ANSI preserved for display — we strip for simple line buffer).
    pub lines: Vec<String>,
    /// Current incomplete line being built from PTY output.
    line_buf: String,
    pub scroll: usize,
    alive: bool,
    cols: u16,
    rows: u16,
}

impl ShellSession {
    pub fn spawn(cwd: &Path, cols: u16, rows: u16, index: usize) -> io::Result<Self> {
        let mut amaster: i32 = -1;
        let mut aslave: i32 = -1;
        let mut win = Winsize {
            ws_row: rows.max(3),
            ws_col: cols.max(20),
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let rc = unsafe { openpty(&mut amaster, &mut aslave, std::ptr::null_mut(), std::ptr::null(), &win) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }

        let pid = unsafe { fork() };
        if pid < 0 {
            unsafe {
                close(amaster);
                close(aslave);
            }
            return Err(io::Error::last_os_error());
        }

        if pid == 0 {
            // child
            unsafe {
                close(amaster);
                setsid();
                ioctl(aslave, TIOCSCTTY, 0);
                dup2(aslave, 0);
                dup2(aslave, 1);
                dup2(aslave, 2);
                if aslave > 2 {
                    close(aslave);
                }
                let cwd_c = std::ffi::CString::new(cwd.to_string_lossy().as_bytes()).ok();
                if let Some(ref c) = cwd_c {
                    chdir(c.as_ptr());
                }
                let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
                let shell_c = std::ffi::CString::new(shell).unwrap_or_else(|_| std::ffi::CString::new("/bin/sh").unwrap());
                let argv0 = shell_c.as_ptr();
                let argv = [argv0, std::ptr::null()];
                execvp(shell_c.as_ptr(), argv.as_ptr());
                // exec failed
                libc_exit(127);
            }
        }

        // parent
        unsafe {
            close(aslave);
        }
        set_nonblocking(amaster, true);
        let owned = unsafe { OwnedFd::from_raw_fd(amaster) };

        Ok(Self {
            title: format!("Terminal {}", index + 1),
            master: owned.as_raw_fd(),
            _master_owned: owned,
            child_pid: pid,
            lines: vec![String::new()],
            line_buf: String::new(),
            scroll: 0,
            alive: true,
            cols: cols.max(20),
            rows: rows.max(3),
        })
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols.max(20);
        self.rows = rows.max(3);
        let win = Winsize {
            ws_row: self.rows,
            ws_col: self.cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        unsafe {
            ioctl(self.master, TIOCSWINSZ, &win);
        }
    }

    pub fn write_bytes(&mut self, data: &[u8]) -> io::Result<()> {
        if !self.alive || data.is_empty() {
            return Ok(());
        }
        let mut off = 0;
        while off < data.len() {
            let n = unsafe { write(self.master, data[off..].as_ptr(), data.len() - off) };
            if n < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::WouldBlock {
                    break;
                }
                return Err(err);
            }
            if n == 0 {
                break;
            }
            off += n as usize;
        }
        Ok(())
    }

    pub fn write_key_bytes(&mut self, data: &[u8]) {
        let _ = self.write_bytes(data);
    }

    /// Read available PTY output into line buffer / scrollback.
    pub fn poll_output(&mut self) {
        if !self.alive {
            return;
        }
        // reap zombie
        let mut status = 0i32;
        let w = unsafe { waitpid(self.child_pid, &mut status, WNOHANG) };
        if w == self.child_pid {
            self.alive = false;
            self.lines.push("[process exited]".into());
            return;
        }

        let mut buf = [0u8; 4096];
        loop {
            let n = unsafe { read(self.master, buf.as_mut_ptr(), buf.len()) };
            if n < 0 {
                break;
            }
            if n == 0 {
                self.alive = false;
                break;
            }
            self.ingest(&buf[..n as usize]);
        }
        // auto-scroll to bottom when new output
        let visible = self.rows as usize;
        if self.lines.len() > visible {
            self.scroll = self.lines.len().saturating_sub(visible);
        }
    }

    fn ingest(&mut self, data: &[u8]) {
        // Strip CSI / OSC for line display (simple terminal emulator)
        let text = String::from_utf8_lossy(data);
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // skip escape sequences
                match chars.peek().copied() {
                    Some('[') => {
                        chars.next();
                        for ch in chars.by_ref() {
                            if ('@'..='~').contains(&ch) {
                                break;
                            }
                        }
                    }
                    Some(']') => {
                        chars.next();
                        // OSC ... BEL or ST
                        for ch in chars.by_ref() {
                            if ch == '\x07' {
                                break;
                            }
                            if ch == '\x1b' {
                                let _ = chars.next(); // skip \
                                break;
                            }
                        }
                    }
                    Some(_) => {
                        let _ = chars.next();
                    }
                    None => {}
                }
                continue;
            }
            if c == '\r' {
                continue;
            }
            if c == '\n' {
                self.lines.push(std::mem::take(&mut self.line_buf));
                if self.lines.len() > MAX_SCROLLBACK {
                    let excess = self.lines.len() - MAX_SCROLLBACK;
                    self.lines.drain(0..excess);
                    self.scroll = self.scroll.saturating_sub(excess);
                }
                continue;
            }
            if c == '\x08' || c == '\x7f' {
                self.line_buf.pop();
                continue;
            }
            if c == '\t' {
                self.line_buf.push_str("    ");
                continue;
            }
            if !c.is_control() {
                self.line_buf.push(c);
            }
        }
    }

    /// Lines to render including incomplete line_buf.
    pub fn display_lines(&self) -> Vec<String> {
        let mut v = self.lines.clone();
        if !self.line_buf.is_empty() || self.alive {
            v.push(self.line_buf.clone());
        }
        v
    }

    pub fn kill(&mut self) {
        if self.alive {
            unsafe {
                kill(self.child_pid, SIGTERM);
                waitpid(self.child_pid, std::ptr::null_mut(), 0);
            }
            self.alive = false;
        }
    }
}

impl Drop for ShellSession {
    fn drop(&mut self) {
        self.kill();
    }
}

pub struct ShellPanel {
    pub sessions: Vec<ShellSession>,
    pub active: usize,
    pub visible: bool,
    /// Height in rows including header (min 4)
    pub height: u16,
    pub focus: bool,
    cwd: std::path::PathBuf,
}

impl ShellPanel {
    pub fn new(cwd: std::path::PathBuf) -> Self {
        Self {
            sessions: Vec::new(),
            active: 0,
            visible: false,
            height: 12,
            focus: false,
            cwd,
        }
    }

    pub fn ensure_one(&mut self, cols: u16, rows: u16) {
        if self.sessions.is_empty() {
            let _ = self.add_terminal(cols, rows);
        }
    }

    pub fn add_terminal(&mut self, cols: u16, body_rows: u16) -> io::Result<()> {
        let idx = self.sessions.len();
        let session = ShellSession::spawn(&self.cwd, cols, body_rows, idx)?;
        self.sessions.push(session);
        self.active = self.sessions.len() - 1;
        self.visible = true;
        self.focus = true;
        Ok(())
    }

    pub fn close_active(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        let i = self.active.min(self.sessions.len() - 1);
        self.sessions[i].kill();
        self.sessions.remove(i);
        if self.sessions.is_empty() {
            self.active = 0;
            self.focus = false;
        } else if self.active >= self.sessions.len() {
            self.active = self.sessions.len() - 1;
        }
    }

    pub fn active_mut(&mut self) -> Option<&mut ShellSession> {
        self.sessions.get_mut(self.active)
    }

    pub fn poll_all(&mut self) {
        for s in &mut self.sessions {
            s.poll_output();
        }
    }

    pub fn resize_all(&mut self, cols: u16, body_rows: u16) {
        for s in &mut self.sessions {
            s.resize(cols, body_rows);
        }
    }

    pub fn next_tab(&mut self) {
        if !self.sessions.is_empty() {
            self.active = (self.active + 1) % self.sessions.len();
        }
    }

    pub fn prev_tab(&mut self) {
        if !self.sessions.is_empty() {
            self.active = if self.active == 0 {
                self.sessions.len() - 1
            } else {
                self.active - 1
            };
        }
    }
}

fn set_nonblocking(fd: i32, nb: bool) {
    unsafe {
        let flags = fcntl(fd, F_GETFL, 0);
        if flags < 0 {
            return;
        }
        let new_flags = if nb {
            flags | O_NONBLOCK
        } else {
            flags & !O_NONBLOCK
        };
        let _ = fcntl(fd, F_SETFL, new_flags);
    }
}

unsafe fn libc_exit(code: i32) -> ! {
    extern "C" {
        fn _exit(code: i32) -> !;
    }
    _exit(code);
}

/// Map Key-like events to bytes for PTY (used from app).
pub fn key_to_pty_bytes(key: &crate::term::Key) -> Option<Vec<u8>> {
    use crate::term::Key;
    Some(match key {
        Key::Char(c) => {
            let mut b = [0u8; 4];
            let s = c.encode_utf8(&mut b);
            s.as_bytes().to_vec()
        }
        Key::Enter => b"\r".to_vec(),
        Key::Backspace => b"\x7f".to_vec(),
        Key::Tab => b"\t".to_vec(),
        Key::Esc => b"\x1b".to_vec(),
        Key::Up => b"\x1b[A".to_vec(),
        Key::Down => b"\x1b[B".to_vec(),
        Key::Right => b"\x1b[C".to_vec(),
        Key::Left => b"\x1b[D".to_vec(),
        Key::Home => b"\x1b[H".to_vec(),
        Key::End => b"\x1b[F".to_vec(),
        Key::Delete => b"\x1b[3~".to_vec(),
        Key::PageUp => b"\x1b[5~".to_vec(),
        Key::PageDown => b"\x1b[6~".to_vec(),
        Key::Ctrl(c) => {
            let b = (*c as u8).to_ascii_lowercase().wrapping_sub(b'a').wrapping_add(1);
            if b > 0 && b < 27 {
                vec![b]
            } else {
                return None;
            }
        }
        _ => return None,
    })
}
