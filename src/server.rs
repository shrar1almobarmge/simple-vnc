use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;


// ---------------------------------------------------------------- shared state

static PORT: AtomicU16 = AtomicU16::new(5900);

struct Shared {
    running: AtomicBool,
    viewers: AtomicUsize,
    error: Mutex<String>,
}

struct Request {
    inc: bool,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
}

#[derive(Clone, Copy)]
struct PixFmt {
    bpp: u8,
    big: bool,
    max: [u32; 3],
    shift: [u32; 3],
}

const DEFAULT_FMT: PixFmt = PixFmt {
    bpp: 32,
    big: false,
    max: [255, 255, 255],
    shift: [16, 8, 0],
};

struct ClientState {
    fmt: PixFmt,
    req: Option<Request>,
}

fn other(m: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Other, m.to_string())
}

fn rd8(s: &mut impl Read) -> io::Result<u8> {
    let mut b = [0u8; 1];
    s.read_exact(&mut b)?;
    Ok(b[0])
}

fn rd16(s: &mut impl Read) -> io::Result<u16> {
    let mut b = [0u8; 2];
    s.read_exact(&mut b)?;
    Ok(u16::from_be_bytes(b))
}

fn rd32(s: &mut impl Read) -> io::Result<u32> {
    let mut b = [0u8; 4];
    s.read_exact(&mut b)?;
    Ok(u32::from_be_bytes(b))
}

fn skip(s: &mut impl Read, n: u64) -> io::Result<()> {
    io::copy(&mut (&mut *s).take(n), &mut io::sink())?;
    Ok(())
}

// ---------------------------------------------------------------- screen + input (Windows)

mod win {
    use std::ffi::c_void;
    use std::mem::{size_of, zeroed};
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_ALL, HWND};
    use windows_sys::Win32::Graphics::Gdi::*;
    use windows_sys::Win32::System::StationsAndDesktops::*;
    use windows_sys::Win32::System::Threading::{OpenEventW, SetEvent, EVENT_MODIFY_STATE};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    pub fn make_dpi_aware() {
        unsafe {
            SetProcessDPIAware();
        }
    }

    thread_local! {
        static DESK: std::cell::Cell<(HDESK, [u16; 64])> = const { std::cell::Cell::new((null_mut(), [0; 64])) };
    }

    // Moves this thread onto the desktop that is on screen now (normal desktop, lock screen,
    // or the "Do you want to allow" prompt). Only works when running as SYSTEM (the service);
    // otherwise nothing changes. Returns true when the thread moved.
    pub fn follow_input_desktop() -> bool {
        unsafe {
            let h = OpenInputDesktop(0, 0, GENERIC_ALL);
            if h.is_null() {
                return false;
            }
            let mut name = [0u16; 64];
            let mut need = 0u32;
            GetUserObjectInformationW(h as _, UOI_NAME, name.as_mut_ptr() as _, 128, &mut need);
            let (old, old_name) = DESK.with(|d| d.get());
            if name == old_name || SetThreadDesktop(h) == 0 {
                CloseDesktop(h);
                return false;
            }
            if !old.is_null() {
                CloseDesktop(old);
            }
            DESK.with(|d| d.set((h, name)));
            true
        }
    }

    // Asks the service to press the real Ctrl+Alt+Del (programs are not allowed to fake it).
    pub fn request_ctrl_alt_del() {
        unsafe {
            let name: Vec<u16> = "Global\\SimpleVncSAS\0".encode_utf16().collect();
            let ev = OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr());
            if !ev.is_null() {
                SetEvent(ev);
                CloseHandle(ev);
            }
        }
    }

    pub fn screen_size() -> (usize, usize) {
        unsafe {
            (
                GetSystemMetrics(SM_CXSCREEN).max(1) as usize,
                GetSystemMetrics(SM_CYSCREEN).max(1) as usize,
            )
        }
    }

    pub struct Grabber {
        pub w: usize,
        pub h: usize,
        screen: HDC,
        mem: HDC,
        bmp: HBITMAP,
        old: HGDIOBJ,
        bits: *mut c_void,
    }

    impl Grabber {
        pub fn new() -> Option<Grabber> {
            unsafe {
                let (w, h) = screen_size();
                let screen = GetDC(null_mut() as HWND);
                if screen.is_null() {
                    return None;
                }
                let mem = CreateCompatibleDC(screen);
                let mut bi: BITMAPINFO = zeroed();
                bi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
                bi.bmiHeader.biWidth = w as i32;
                bi.bmiHeader.biHeight = -(h as i32);
                bi.bmiHeader.biPlanes = 1;
                bi.bmiHeader.biBitCount = 32;
                bi.bmiHeader.biCompression = BI_RGB;
                let mut bits: *mut c_void = null_mut();
                let bmp = CreateDIBSection(screen, &bi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
                if bmp.is_null() || bits.is_null() {
                    return None;
                }
                let old = SelectObject(mem, bmp as HGDIOBJ);
                Some(Grabber { w, h, screen, mem, bmp, old, bits })
            }
        }

        // Fills `out` with the screen as B,G,R,X bytes (top row first), cursor included.
        pub fn grab(&mut self, out: &mut Vec<u8>) {
            unsafe {
                BitBlt(
                    self.mem,
                    0,
                    0,
                    self.w as i32,
                    self.h as i32,
                    self.screen,
                    0,
                    0,
                    SRCCOPY | CAPTUREBLT,
                );
                let mut ci: CURSORINFO = zeroed();
                ci.cbSize = size_of::<CURSORINFO>() as u32;
                if GetCursorInfo(&mut ci) != 0 && (ci.flags & CURSOR_SHOWING) != 0 {
                    let mut ii: ICONINFO = zeroed();
                    let (mut hx, mut hy) = (0, 0);
                    if GetIconInfo(ci.hCursor, &mut ii) != 0 {
                        hx = ii.xHotspot as i32;
                        hy = ii.yHotspot as i32;
                        if !ii.hbmMask.is_null() {
                            DeleteObject(ii.hbmMask as HGDIOBJ);
                        }
                        if !ii.hbmColor.is_null() {
                            DeleteObject(ii.hbmColor as HGDIOBJ);
                        }
                    }
                    DrawIconEx(
                        self.mem,
                        ci.ptScreenPos.x - hx,
                        ci.ptScreenPos.y - hy,
                        ci.hCursor,
                        0,
                        0,
                        0,
                        null_mut(),
                        DI_NORMAL,
                    );
                }
                GdiFlush();
                let n = self.w * self.h * 4;
                out.clear();
                out.extend_from_slice(std::slice::from_raw_parts(self.bits as *const u8, n));
            }
        }
    }

    impl Drop for Grabber {
        fn drop(&mut self) {
            unsafe {
                SelectObject(self.mem, self.old);
                DeleteObject(self.bmp as HGDIOBJ);
                DeleteDC(self.mem);
                ReleaseDC(null_mut() as HWND, self.screen);
            }
        }
    }

    fn send(inputs: &[INPUT]) {
        unsafe {
            SendInput(inputs.len() as u32, inputs.as_ptr(), size_of::<INPUT>() as i32);
        }
    }

    fn mouse(dx: i32, dy: i32, data: u32, flags: u32) -> INPUT {
        let mut i: INPUT = unsafe { zeroed() };
        i.r#type = INPUT_MOUSE;
        i.Anonymous.mi = MOUSEINPUT {
            dx,
            dy,
            mouseData: data,
            dwFlags: flags,
            time: 0,
            dwExtraInfo: 0,
        };
        i
    }

    fn key(vk: u16, scan: u16, flags: u32) -> INPUT {
        let mut i: INPUT = unsafe { zeroed() };
        i.r#type = INPUT_KEYBOARD;
        i.Anonymous.ki = KEYBDINPUT {
            wVk: vk,
            wScan: scan,
            dwFlags: flags,
            time: 0,
            dwExtraInfo: 0,
        };
        i
    }

    #[derive(Default)]
    pub struct InputState {
        buttons: u8,
        shift: bool,
        ctrl: bool,
        alt: bool,
    }

    impl InputState {
        pub fn pointer(&mut self, x: u16, y: u16, buttons: u8) {
            let (w, h) = screen_size();
            let nx = (x as i64 * 65535 / (w as i64 - 1).max(1)).min(65535) as i32;
            let ny = (y as i64 * 65535 / (h as i64 - 1).max(1)).min(65535) as i32;
            let mut v = vec![mouse(nx, ny, 0, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE)];
            let changed = buttons ^ self.buttons;
            for (bit, down, up) in [
                (1u8, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
                (2, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
                (4, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
            ] {
                if changed & bit != 0 {
                    v.push(mouse(0, 0, 0, if buttons & bit != 0 { down } else { up }));
                }
            }
            if changed & buttons & 8 != 0 {
                v.push(mouse(0, 0, 120, MOUSEEVENTF_WHEEL));
            }
            if changed & buttons & 16 != 0 {
                v.push(mouse(0, 0, (-120i32) as u32, MOUSEEVENTF_WHEEL));
            }
            self.buttons = buttons;
            send(&v);
        }

        pub fn key(&mut self, down: bool, keysym: u32) {
            match keysym {
                0xffe3 | 0xffe4 => self.ctrl = down,
                0xffe9 | 0xffea => self.alt = down,
                0xffff if down && self.ctrl && self.alt => request_ctrl_alt_del(),
                _ => {}
            }
            let up = if down { 0 } else { KEYEVENTF_KEYUP };
            let named: Option<(u16, bool)> = match keysym {
                0xff08 => Some((VK_BACK, false)),
                0xff09 => Some((VK_TAB, false)),
                0xff0d => Some((VK_RETURN, false)),
                0xff1b => Some((VK_ESCAPE, false)),
                0xffff => Some((VK_DELETE, true)),
                0xff63 => Some((VK_INSERT, true)),
                0xff50 => Some((VK_HOME, true)),
                0xff57 => Some((VK_END, true)),
                0xff55 => Some((VK_PRIOR, true)),
                0xff56 => Some((VK_NEXT, true)),
                0xff51 => Some((VK_LEFT, true)),
                0xff52 => Some((VK_UP, true)),
                0xff53 => Some((VK_RIGHT, true)),
                0xff54 => Some((VK_DOWN, true)),
                0xffbe..=0xffc9 => Some((VK_F1 + (keysym - 0xffbe) as u16, false)),
                0xffe1 | 0xffe2 => {
                    self.shift = down;
                    Some((VK_LSHIFT, false))
                }
                0xffe3 => Some((VK_LCONTROL, false)),
                0xffe4 => Some((VK_RCONTROL, true)),
                0xffe9 => Some((VK_LMENU, false)),
                0xffea => Some((VK_RMENU, true)),
                0xffeb | 0xffec => Some((VK_LWIN, true)),
                0xffe5 => Some((VK_CAPITAL, false)),
                _ => None,
            };
            if let Some((vk, ext)) = named {
                let f = up | if ext { KEYEVENTF_EXTENDEDKEY } else { 0 };
                send(&[key(vk, 0, f)]);
                return;
            }
            let cp = if keysym >= 0x0100_0000 {
                keysym - 0x0100_0000
            } else {
                keysym
            };
            let Some(ch) = char::from_u32(cp) else { return };
            let mut utf16 = [0u16; 2];
            let units = ch.encode_utf16(&mut utf16);
            if units.len() == 1 {
                let scan = unsafe { VkKeyScanW(units[0]) };
                if scan != -1 {
                    let vk = (scan & 0xff) as u16;
                    let needs_shift = (scan >> 8) & 1 != 0;
                    if needs_shift && !self.shift {
                        send(&[
                            key(VK_LSHIFT, 0, 0),
                            key(vk, 0, up),
                            key(VK_LSHIFT, 0, KEYEVENTF_KEYUP),
                        ]);
                    } else {
                        send(&[key(vk, 0, up)]);
                    }
                    return;
                }
            }
            for u in units.iter() {
                send(&[key(0, *u, KEYEVENTF_UNICODE | up)]);
            }
        }
    }
}

// ---------------------------------------------------------------- the VNC server part

fn pixels(frame: &[u8], w: usize, x0: usize, x1: usize, y0: usize, y1: usize, f: &PixFmt) -> Vec<u8> {
    let bytes = (f.bpp / 8) as usize;
    let mut out = Vec::with_capacity((x1 - x0) * (y1 - y0) * bytes);
    let fast = f.bpp == 32 && !f.big && f.max == [255, 255, 255] && f.shift == [16, 8, 0];
    for y in y0..y1 {
        let row = &frame[(y * w + x0) * 4..(y * w + x1) * 4];
        if fast {
            for p in row.chunks_exact(4) {
                out.extend_from_slice(&[p[0], p[1], p[2], 0]);
            }
        } else {
            for p in row.chunks_exact(4) {
                let (r, g, b) = (p[2] as u32, p[1] as u32, p[0] as u32);
                let v = ((r * f.max[0] / 255) << f.shift[0])
                    | ((g * f.max[1] / 255) << f.shift[1])
                    | ((b * f.max[2] / 255) << f.shift[2]);
                let be = v.to_be_bytes();
                let le = v.to_le_bytes();
                if f.big {
                    out.extend_from_slice(&be[4 - bytes..]);
                } else {
                    out.extend_from_slice(&le[..bytes]);
                }
            }
        }
    }
    out
}

fn sender(mut s: TcpStream, cs: Arc<Mutex<ClientState>>, alive: Arc<AtomicBool>, sh: Arc<Shared>) {
    win::follow_input_desktop();
    let Some(mut grab) = win::Grabber::new() else {
        alive.store(false, Ordering::SeqCst);
        return;
    };
    let (w, h) = (grab.w, grab.h);
    let mut last = vec![0u8; w * h * 4];
    let mut frame: Vec<u8> = Vec::new();
    while alive.load(Ordering::SeqCst) && sh.running.load(Ordering::SeqCst) {
        if win::follow_input_desktop() {
            // The screen switched (lock screen <-> desktop): grab from the new one.
            match win::Grabber::new() {
                Some(g) if g.w == w && g.h == h => grab = g,
                _ => break,
            }
            last.fill(1);
        }
        let (req, fmt) = {
            let g = cs.lock().unwrap();
            match &g.req {
                Some(r) => (
                    Request { inc: r.inc, x: r.x, y: r.y, w: r.w, h: r.h },
                    g.fmt,
                ),
                None => {
                    drop(g);
                    std::thread::sleep(Duration::from_millis(15));
                    continue;
                }
            }
        };
        grab.grab(&mut frame);
        let x0 = (req.x as usize).min(w);
        let y0 = (req.y as usize).min(h);
        let x1 = (x0 + req.w as usize).min(w);
        let y1 = (y0 + req.h as usize).min(h);
        let (mut top, mut bottom) = (y0, y1);
        if req.inc {
            let mut first = None;
            let mut end = y0;
            for y in y0..y1 {
                let a = (y * w + x0) * 4;
                let b = (y * w + x1) * 4;
                if frame[a..b] != last[a..b] {
                    if first.is_none() {
                        first = Some(y);
                    }
                    end = y + 1;
                }
            }
            match first {
                Some(f) => {
                    top = f;
                    bottom = end;
                }
                None => {
                    std::thread::sleep(Duration::from_millis(25));
                    continue;
                }
            }
        }
        cs.lock().unwrap().req = None;

        let mut msg: Vec<u8> = vec![0, 0];
        if x1 > x0 && bottom > top {
            msg.extend_from_slice(&1u16.to_be_bytes());
            msg.extend_from_slice(&(x0 as u16).to_be_bytes());
            msg.extend_from_slice(&(top as u16).to_be_bytes());
            msg.extend_from_slice(&((x1 - x0) as u16).to_be_bytes());
            msg.extend_from_slice(&((bottom - top) as u16).to_be_bytes());
            msg.extend_from_slice(&0i32.to_be_bytes());
            msg.extend_from_slice(&pixels(&frame, w, x0, x1, top, bottom, &fmt));
            for y in top..bottom {
                let a = (y * w + x0) * 4;
                let b = (y * w + x1) * 4;
                last[a..b].copy_from_slice(&frame[a..b]);
            }
        } else {
            msg.extend_from_slice(&0u16.to_be_bytes());
        }
        if s.write_all(&msg).is_err() {
            break;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    alive.store(false, Ordering::SeqCst);
    let _ = s.shutdown(Shutdown::Both);
}

fn client(mut s: TcpStream, sh: &Arc<Shared>) -> io::Result<()> {
    s.set_nodelay(true)?;
    s.set_read_timeout(Some(Duration::from_secs(20)))?;
    s.write_all(b"RFB 003.008\n")?;
    let mut ver = [0u8; 12];
    s.read_exact(&mut ver)?;
    if &ver[0..4] != b"RFB " {
        return Err(other("not a VNC viewer"));
    }
    let minor: u32 = std::str::from_utf8(&ver[8..11])
        .ok()
        .and_then(|t| t.parse().ok())
        .unwrap_or(3);
    if minor >= 7 {
        s.write_all(&[1, 1])?;
        if rd8(&mut s)? != 1 {
            return Err(other("viewer does not accept open login"));
        }
    } else {
        s.write_all(&1u32.to_be_bytes())?;
    }
    if minor >= 8 {
        s.write_all(&0u32.to_be_bytes())?;
    }
    rd8(&mut s)?; // shared flag, ignored

    let (w, h) = win::screen_size();
    let mut init = Vec::new();
    init.extend_from_slice(&(w as u16).to_be_bytes());
    init.extend_from_slice(&(h as u16).to_be_bytes());
    init.extend_from_slice(&[32, 24, 0, 1]);
    for _ in 0..3 {
        init.extend_from_slice(&255u16.to_be_bytes());
    }
    init.extend_from_slice(&[16, 8, 0, 0, 0, 0]);
    let name = b"Simple VNC Server";
    init.extend_from_slice(&(name.len() as u32).to_be_bytes());
    init.extend_from_slice(name);
    s.write_all(&init)?;
    s.set_read_timeout(None)?;
    let who = s.peer_addr().map(|a| a.ip().to_string()).unwrap_or_default();
    println!("Connected: {}", who);

    let cs = Arc::new(Mutex::new(ClientState {
        fmt: DEFAULT_FMT,
        req: None,
    }));
    let alive = Arc::new(AtomicBool::new(true));
    {
        let (w2, cs2, a2, sh2) = (s.try_clone()?, cs.clone(), alive.clone(), sh.clone());
        std::thread::spawn(move || sender(w2, cs2, a2, sh2));
    }

    let mut input = win::InputState::default();
    let result = (|| -> io::Result<()> {
        loop {
            if !alive.load(Ordering::SeqCst) || !sh.running.load(Ordering::SeqCst) {
                return Ok(());
            }
            let kind = rd8(&mut s)?;
            if kind == 4 || kind == 5 {
                win::follow_input_desktop();
            }
            match kind {
                0 => {
                    skip(&mut s, 3)?;
                    let bpp = rd8(&mut s)?;
                    skip(&mut s, 1)?;
                    let big = rd8(&mut s)? != 0;
                    let true_colour = rd8(&mut s)? != 0;
                    let max = [rd16(&mut s)? as u32, rd16(&mut s)? as u32, rd16(&mut s)? as u32];
                    let shift = [rd8(&mut s)? as u32, rd8(&mut s)? as u32, rd8(&mut s)? as u32];
                    skip(&mut s, 3)?;
                    if true_colour && matches!(bpp, 8 | 16 | 32) && shift.iter().all(|v| *v < 32) {
                        cs.lock().unwrap().fmt = PixFmt { bpp, big, max, shift };
                    }
                }
                2 => {
                    skip(&mut s, 1)?;
                    let n = rd16(&mut s)? as u64;
                    skip(&mut s, n * 4)?;
                }
                3 => {
                    let inc = rd8(&mut s)? != 0;
                    let r = Request {
                        inc,
                        x: rd16(&mut s)?,
                        y: rd16(&mut s)?,
                        w: rd16(&mut s)?,
                        h: rd16(&mut s)?,
                    };
                    let mut g = cs.lock().unwrap();
                    let keep_old = matches!(&g.req, Some(o) if !o.inc);
                    if !keep_old {
                        g.req = Some(r);
                    }
                }
                4 => {
                    let down = rd8(&mut s)? != 0;
                    skip(&mut s, 2)?;
                    let keysym = rd32(&mut s)?;
                    input.key(down, keysym);
                }
                5 => {
                    let buttons = rd8(&mut s)?;
                    let x = rd16(&mut s)?;
                    let y = rd16(&mut s)?;
                    input.pointer(x, y, buttons);
                }
                6 => {
                    skip(&mut s, 3)?;
                    let n = rd32(&mut s)? as u64;
                    skip(&mut s, n)?;
                }
                _ => return Err(other("unexpected message")),
            }
        }
    })();
    alive.store(false, Ordering::SeqCst);
    let _ = s.shutdown(Shutdown::Both);
    println!("Disconnected: {}", who);
    result
}

fn serve(sh: Arc<Shared>) {
    let mut listener = None;
    for _ in 0..15 {
        match TcpListener::bind(("0.0.0.0", PORT.load(Ordering::SeqCst))) {
            Ok(l) => {
                listener = Some(l);
                break;
            }
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
    let Some(listener) = listener else {
        *sh.error.lock().unwrap() =
            "Could not start sharing: another VNC server is probably already running.".to_string();
        sh.running.store(false, Ordering::SeqCst);
        return;
    };
    let _ = listener.set_nonblocking(true);
    while sh.running.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((s, _)) => {
                let _ = s.set_nonblocking(false);
                let sh2 = sh.clone();
                std::thread::spawn(move || {
                    sh2.viewers.fetch_add(1, Ordering::SeqCst);
                    let _ = client(s, &sh2);
                    sh2.viewers.fetch_sub(1, Ordering::SeqCst);
                });
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

// ---------------------------------------------------------------- the Windows service
//
// The service runs as SYSTEM in the background, so it starts with Windows before anyone logs in.
// A service cannot see the screen itself, so it starts a copy of this program ("--helper") in the
// session that is on the monitor. That copy follows the screen to the lock screen and back.

mod svc {
    use std::mem::{size_of, zeroed};
    use std::ptr::null_mut;
    use std::sync::atomic::{AtomicBool, Ordering};
    use windows_sys::Win32::Foundation::*;
    use windows_sys::Win32::Security::*;
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows_sys::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId;
    use windows_sys::Win32::System::Services::*;
    use windows_sys::Win32::System::Threading::*;

    pub const NAME: &str = "SimpleVNC";
    static STOP: AtomicBool = AtomicBool::new(false);
    static mut STATUS: SERVICE_STATUS_HANDLE = null_mut();

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    fn set_state(state: u32) {
        let st = SERVICE_STATUS {
            dwServiceType: SERVICE_WIN32_OWN_PROCESS,
            dwCurrentState: state,
            dwControlsAccepted: if state == SERVICE_RUNNING {
                SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
            } else {
                0
            },
            dwWin32ExitCode: 0,
            dwServiceSpecificExitCode: 0,
            dwCheckPoint: 0,
            dwWaitHint: 3000,
        };
        unsafe {
            SetServiceStatus(STATUS, &st);
        }
    }

    unsafe extern "system" fn handler(ctrl: u32) {
        if ctrl == SERVICE_CONTROL_STOP || ctrl == SERVICE_CONTROL_SHUTDOWN {
            STOP.store(true, Ordering::SeqCst);
            set_state(SERVICE_STOP_PENDING);
        }
    }

    unsafe extern "system" fn service_main(_argc: u32, _argv: *mut windows_sys::core::PWSTR) {
        let name = wide(NAME);
        STATUS = RegisterServiceCtrlHandlerW(name.as_ptr(), Some(handler));
        set_state(SERVICE_RUNNING);
        supervise();
        set_state(SERVICE_STOPPED);
    }

    pub fn run() {
        let mut name = wide(NAME);
        let table = [
            SERVICE_TABLE_ENTRYW { lpServiceName: name.as_mut_ptr(), lpServiceProc: Some(service_main) },
            SERVICE_TABLE_ENTRYW { lpServiceName: null_mut(), lpServiceProc: None },
        ];
        unsafe {
            if StartServiceCtrlDispatcherW(table.as_ptr()) == 0 {
                eprintln!("--service is only for Windows to use. Use --install instead.");
                std::process::exit(2);
            }
        }
    }

    // Starts "--helper" as SYSTEM inside the session shown on the monitor.
    fn launch(session: u32) -> HANDLE {
        unsafe {
            let (mut tok, mut dup) = (null_mut(), null_mut());
            if OpenProcessToken(GetCurrentProcess(), TOKEN_ALL_ACCESS, &mut tok) == 0 {
                return null_mut();
            }
            let ok = DuplicateTokenEx(tok, TOKEN_ALL_ACCESS, null_mut(), SecurityImpersonation, TokenPrimary, &mut dup);
            CloseHandle(tok);
            if ok == 0 {
                return null_mut();
            }
            SetTokenInformation(dup, TokenSessionId, &session as *const u32 as _, 4);
            let exe = std::env::current_exe().unwrap_or_default();
            let port = super::PORT.load(Ordering::SeqCst);
            let mut cmd = wide(&format!("\"{}\" --helper --port {}", exe.display(), port));
            let mut desktop = wide("winsta0\\default");
            let mut si: STARTUPINFOW = zeroed();
            si.cb = size_of::<STARTUPINFOW>() as u32;
            si.lpDesktop = desktop.as_mut_ptr();
            let mut pi: PROCESS_INFORMATION = zeroed();
            let ok = CreateProcessAsUserW(
                dup,
                null_mut(),
                cmd.as_mut_ptr(),
                null_mut(),
                null_mut(),
                0,
                CREATE_NO_WINDOW,
                null_mut(),
                null_mut(),
                &si,
                &mut pi,
            );
            CloseHandle(dup);
            if ok == 0 {
                return null_mut();
            }
            CloseHandle(pi.hThread);
            pi.hProcess
        }
    }

    fn send_ctrl_alt_del() {
        unsafe {
            let lib = LoadLibraryW(wide("sas.dll").as_ptr());
            if lib.is_null() {
                return;
            }
            if let Some(f) = GetProcAddress(lib, b"SendSAS\0".as_ptr()) {
                let send_sas: unsafe extern "system" fn(i32) = std::mem::transmute(f);
                send_sas(0);
            }
        }
    }

    fn supervise() {
        unsafe {
            let sas = CreateEventW(null_mut(), 0, 0, wide("Global\\SimpleVncSAS").as_ptr());
            let (mut child, mut child_session): (HANDLE, u32) = (null_mut(), u32::MAX);
            while !STOP.load(Ordering::SeqCst) {
                let session = WTSGetActiveConsoleSessionId();
                if !child.is_null() {
                    let mut code = 0u32;
                    GetExitCodeProcess(child, &mut code);
                    if code != STILL_ACTIVE as u32 || session != child_session {
                        TerminateProcess(child, 0);
                        CloseHandle(child);
                        child = null_mut();
                        std::thread::sleep(std::time::Duration::from_millis(500));
                    }
                }
                if child.is_null() && session != u32::MAX {
                    child = launch(session);
                    child_session = session;
                }
                if !sas.is_null() && WaitForSingleObject(sas, 1000) == WAIT_OBJECT_0 {
                    send_ctrl_alt_del();
                } else if sas.is_null() {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            }
            if !child.is_null() {
                TerminateProcess(child, 0);
                CloseHandle(child);
            }
        }
    }

    pub fn is_admin() -> bool {
        unsafe {
            let mut tok = null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) == 0 {
                return false;
            }
            let mut elevated = 0u32;
            let mut len = 0u32;
            GetTokenInformation(tok, TokenElevation, &mut elevated as *mut u32 as _, 4, &mut len);
            CloseHandle(tok);
            elevated != 0
        }
    }
}

// Runs a Windows command line as written (keeps the quotes we put in).
fn run_cmd(line: &str) -> bool {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("cmd")
        .raw_arg(format!("/c {} >nul 2>&1", line))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// Re-opens this program as administrator (Windows asks "Yes / No") and waits for it.
fn run_as_admin(args: &str) {
    let exe = std::env::current_exe().unwrap_or_default();
    let ps = format!(
        "Start-Process -FilePath '{}' -ArgumentList '{}' -Verb RunAs -Wait",
        exe.display().to_string().replace('\'', "''"),
        args
    );
    let ok = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &ps])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("Windows did not allow it. Press Yes when Windows asks.");
        std::process::exit(1);
    }
}

fn install(port: u16) {
    if !svc::is_admin() {
        println!("Asking Windows for permission (press Yes)...");
        run_as_admin(&format!("--install --port {} --pause", port));
        return;
    }
    let dir = std::path::PathBuf::from(std::env::var("ProgramFiles").unwrap_or("C:\\Program Files".into()))
        .join("Simple VNC");
    let target = dir.join("simple-vnc-server.exe");
    run_cmd(&format!("sc stop {}", svc::NAME));
    std::thread::sleep(Duration::from_secs(2));
    let me = std::env::current_exe().unwrap_or_default();
    let copied = std::fs::create_dir_all(&dir).is_ok()
        && (me == target || std::fs::copy(&me, &target).is_ok());
    if !copied {
        eprintln!("Could not copy the program to {}.", dir.display());
        return;
    }
    let bin = format!("\"\\\"{}\\\" --service --port {}\"", target.display(), port);
    let made = run_cmd(&format!(
        "sc create {} binPath= {} start= auto DisplayName= \"Simple VNC Server\"",
        svc::NAME, bin
    )) || run_cmd(&format!("sc config {} binPath= {} start= auto", svc::NAME, bin));
    if !made {
        eprintln!("Could not register the background service.");
        return;
    }
    run_cmd(&format!(
        "sc description {} \"Shares this screen over VNC, also before login.\"",
        svc::NAME
    ));
    run_cmd(&format!("sc failure {} reset= 60 actions= restart/5000", svc::NAME));
    run_cmd(&format!(
        "netsh advfirewall firewall delete rule name=\"Simple VNC Server {}\"",
        port
    ));
    run_cmd(&format!(
        "netsh advfirewall firewall add rule name=\"Simple VNC Server {0}\" dir=in action=allow protocol=TCP localport={0} profile=any",
        port
    ));
    // Lets the service press the real Ctrl+Alt+Del for the viewer's button.
    run_cmd("reg add HKLM\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Policies\\System /v SoftwareSASGeneration /t REG_DWORD /d 1 /f");
    if run_cmd(&format!("sc start {}", svc::NAME)) {
        println!("Done. Simple VNC Server now starts with Windows, also before login.");
        println!("This computer's address: {}", local_ip());
        println!("To remove it: simple-vnc-server --uninstall");
    } else {
        eprintln!("Installed, but the service did not start. Restart the computer and try again.");
    }
}

fn uninstall() {
    if !svc::is_admin() {
        println!("Asking Windows for permission (press Yes)...");
        run_as_admin("--uninstall --pause");
        return;
    }
    run_cmd(&format!("sc stop {}", svc::NAME));
    std::thread::sleep(Duration::from_secs(2));
    let removed = run_cmd(&format!("sc delete {}", svc::NAME));
    for port in [5900u16, PORT.load(Ordering::SeqCst)] {
        run_cmd(&format!(
            "netsh advfirewall firewall delete rule name=\"Simple VNC Server {}\"",
            port
        ));
    }
    let dir = std::path::PathBuf::from(std::env::var("ProgramFiles").unwrap_or("C:\\Program Files".into()))
        .join("Simple VNC");
    let _ = std::fs::remove_dir_all(dir);
    if removed {
        println!("Removed. Simple VNC Server no longer starts with Windows.");
    } else {
        println!("Simple VNC Server was not installed as a service.");
    }
}

// ---------------------------------------------------------------- the terminal

fn local_ip() -> String {
    let find = || -> Option<String> {
        let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
        sock.connect("8.8.8.8:80").ok()?;
        Some(sock.local_addr().ok()?.ip().to_string())
    };
    find().unwrap_or_else(|| "not connected to a network".to_string())
}

const HELP: &str = "Simple VNC Server

Usage:
  simple-vnc-server                 start sharing this screen
  simple-vnc-server --port 5901     share on another port (default 5900)
  simple-vnc-server --install       start with Windows, also before login (asks for admin)
  simple-vnc-server --uninstall     stop starting with Windows
  simple-vnc-server --help          show this help

Press Ctrl+C to stop sharing.
";

enum Mode {
    Terminal,
    Install,
    Uninstall,
    Service,
    Helper,
}

fn main() {
    win::make_dpi_aware();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h" || a == "/?") {
        print!("{}", HELP);
        return;
    }
    let mut mode = Mode::Terminal;
    let mut pause = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--install" => mode = Mode::Install,
            "--uninstall" => mode = Mode::Uninstall,
            "--service" => mode = Mode::Service,
            "--helper" => mode = Mode::Helper,
            "--pause" => pause = true,
            "--port" => match it.next().and_then(|v| v.parse::<u16>().ok()).filter(|p| *p > 0) {
                Some(p) => PORT.store(p, Ordering::SeqCst),
                None => {
                    eprintln!("--port needs a number from 1 to 65535.");
                    std::process::exit(2);
                }
            },
            _ => {
                eprintln!("Unknown option {}. Use --help to see what is possible.", a);
                std::process::exit(2);
            }
        }
    }

    let port = PORT.load(Ordering::SeqCst);
    let shared = Arc::new(Shared {
        running: AtomicBool::new(true),
        viewers: AtomicUsize::new(0),
        error: Mutex::new(String::new()),
    });
    match mode {
        Mode::Terminal => {}
        Mode::Install | Mode::Uninstall => {
            if matches!(mode, Mode::Install) {
                install(port);
            } else {
                uninstall();
            }
            if pause {
                println!("\nPress Enter to close this window.");
                let _ = io::stdin().read_line(&mut String::new());
            }
            return;
        }
        Mode::Service => return svc::run(),
        Mode::Helper => return serve(shared),
    }

    let ip = local_ip();
    let shown = if port == 5900 { ip } else { format!("{}:{}", ip, port) };
    println!("Simple VNC Server");
    println!("Sharing is ON. This computer's address: {}", shown);
    println!();
    println!("Other computer can't find this one? Run this once in PowerShell and press Yes:");
    println!(
        "  Start-Process powershell -Verb RunAs -ArgumentList '-Command New-NetFirewallRule -DisplayName ''Simple VNC Server {0}'' -Direction Inbound -Protocol TCP -LocalPort {0} -Action Allow -Profile Any'",
        port
    );
    println!();
    println!("Waiting for connection... (press Ctrl+C to stop)");
    serve(shared.clone());
    let error = shared.error.lock().unwrap().clone();
    if !error.is_empty() {
        eprintln!("{}", error);
        std::process::exit(1);
    }
}
