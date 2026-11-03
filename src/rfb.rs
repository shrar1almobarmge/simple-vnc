use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;


pub enum Out {
    Pointer { x: u16, y: u16, buttons: u8 },
    Key { down: bool, keysym: u32 },
    Request { w: u16, h: u16, inc: bool },
    Close,
}

#[derive(Default, PartialEq, Clone, Copy)]
pub enum State {
    #[default]
    Idle,
    Connecting,
    Connected,
    Failed,
}

#[derive(Default)]
pub struct Screen {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
    pub dirty: bool,
    pub name: String,
    pub state: State,
    pub message: String,
}

pub struct Connection {
    pub shared: Arc<Mutex<Screen>>,
    pub tx: mpsc::Sender<Out>,
}

fn fail<T>(m: &str) -> io::Result<T> {
    Err(io::Error::new(io::ErrorKind::Other, m.to_string()))
}

fn friendly(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => {
            "The other computer did not answer. Check the address.".to_string()
        }
        io::ErrorKind::ConnectionRefused => {
            "Nothing is waiting for you at that address. Is the VNC server running?".to_string()
        }
        io::ErrorKind::Other => e.to_string(),
        _ => format!("Connection problem: {}", e),
    }
}

fn read_u8(s: &mut impl Read) -> io::Result<u8> {
    let mut b = [0u8; 1];
    s.read_exact(&mut b)?;
    Ok(b[0])
}

fn read_u16(s: &mut impl Read) -> io::Result<u16> {
    let mut b = [0u8; 2];
    s.read_exact(&mut b)?;
    Ok(u16::from_be_bytes(b))
}

fn read_u32(s: &mut impl Read) -> io::Result<u32> {
    let mut b = [0u8; 4];
    s.read_exact(&mut b)?;
    Ok(u32::from_be_bytes(b))
}

fn skip(s: &mut impl Read, n: u64) -> io::Result<()> {
    io::copy(&mut (&mut *s).take(n), &mut io::sink())?;
    Ok(())
}

// "host", "host:5900", "host:1" (a small number means display number, like UltraVNC).
fn split_address(address: &str) -> (String, u16) {
    let a = address.trim();
    if let Some((host, port)) = a.rsplit_once(':') {
        if let Ok(p) = port.trim_start_matches(':').parse::<u32>() {
            let real = if p < 100 { 5900 + p } else { p };
            return (host.trim_end_matches(':').to_string(), real.min(65535) as u16);
        }
    }
    (a.to_string(), 5900)
}

pub struct Scan {
    pub found: Vec<String>,
    pub done: bool,
}

fn local_ip() -> Option<std::net::Ipv4Addr> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    match sock.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}

fn is_vnc(ip: std::net::Ipv4Addr) -> bool {
    let addr = std::net::SocketAddr::new(std::net::IpAddr::V4(ip), 5900);
    let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(600)) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_millis(2000)));
    let mut hello = [0u8; 4];
    s.read_exact(&mut hello).is_ok() && &hello == b"RFB "
}

// Looks for VNC servers on this computer and on the home/office network (the same x.y.z.* range).
pub fn scan(ctx: eframe::egui::Context) -> Arc<Mutex<Scan>> {
    let state = Arc::new(Mutex::new(Scan {
        found: Vec::new(),
        done: false,
    }));
    let state2 = state.clone();
    std::thread::spawn(move || {
        let mut targets = vec![std::net::Ipv4Addr::LOCALHOST];
        if let Some(me) = local_ip() {
            let o = me.octets();
            for i in 1..=254u8 {
                let t = std::net::Ipv4Addr::new(o[0], o[1], o[2], i);
                if t != me {
                    targets.push(t);
                }
            }
        }
        let queue = Mutex::new(targets);
        std::thread::scope(|sc| {
            for _ in 0..64 {
                sc.spawn(|| loop {
                    let next = queue.lock().unwrap().pop();
                    let Some(ip) = next else { break };
                    if is_vnc(ip) {
                        let mut g = state2.lock().unwrap();
                        g.found.push(ip.to_string());
                        g.found.sort_by_key(|a| a == "127.0.0.1");
                        drop(g);
                        ctx.request_repaint();
                    }
                });
            }
        });
        state2.lock().unwrap().done = true;
        ctx.request_repaint();
    });
    state
}

pub fn connect(address: String, ctx: eframe::egui::Context) -> Connection {
    let shared = Arc::new(Mutex::new(Screen {
        state: State::Connecting,
        ..Default::default()
    }));
    let (tx, rx) = mpsc::channel();
    let shared2 = shared.clone();
    let tx2 = tx.clone();
    std::thread::spawn(move || {
        let result = run(&address, &shared2, tx2, rx, &ctx);
        {
            let mut s = shared2.lock().unwrap();
            match result {
                Ok(()) => {
                    s.state = State::Idle;
                    s.message = "Disconnected.".to_string();
                }
                Err(e) => {
                    s.state = State::Failed;
                    s.message = friendly(&e);
                }
            }
        }
        ctx.request_repaint();
    });
    Connection { shared, tx }
}

fn run(
    address: &str,
    shared: &Arc<Mutex<Screen>>,
    tx: mpsc::Sender<Out>,
    rx: mpsc::Receiver<Out>,
    ctx: &eframe::egui::Context,
) -> io::Result<()> {
    let (host, port) = split_address(address);
    if host.is_empty() {
        return fail("Please type the address of the other computer.");
    }
    let addr = match (host.as_str(), port).to_socket_addrs()?.next() {
        Some(a) => a,
        None => return fail("Could not find that address."),
    };
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(8))?;
    s.set_nodelay(true)?;
    s.set_read_timeout(Some(Duration::from_secs(15)))?;

    // Version
    let mut ver = [0u8; 12];
    s.read_exact(&mut ver)?;
    if &ver[0..4] != b"RFB " {
        return fail("That address is not a VNC server.");
    }
    let minor: u32 = std::str::from_utf8(&ver[8..11])
        .ok()
        .and_then(|t| t.parse().ok())
        .unwrap_or(3);
    let minor = if minor >= 8 {
        8
    } else if minor == 7 {
        7
    } else {
        3
    };
    s.write_all(format!("RFB 003.{:03}\n", minor).as_bytes())?;

    // Security: only "no password" logins are supported
    let no_password_ok = if minor >= 7 {
        let n = read_u8(&mut s)?;
        if n == 0 {
            return fail("The other computer refused the connection.");
        }
        let mut types = vec![0u8; n as usize];
        s.read_exact(&mut types)?;
        if types.contains(&1) {
            s.write_all(&[1])?;
            true
        } else {
            false
        }
    } else {
        read_u32(&mut s)? == 1
    };
    if !no_password_ok {
        return fail("That computer asks for a password. Use Simple VNC Server there, which needs none.");
    }
    if minor >= 8 && read_u32(&mut s)? != 0 {
        return fail("The other computer said no.");
    }

    // Client init (share the desktop) and server init
    s.write_all(&[1])?;
    let width = read_u16(&mut s)? as usize;
    let height = read_u16(&mut s)? as usize;
    skip(&mut s, 16)?;
    let name_len = read_u32(&mut s)?;
    let mut name = vec![0u8; name_len.min(4096) as usize];
    s.read_exact(&mut name)?;
    if name_len > 4096 {
        skip(&mut s, (name_len - 4096) as u64)?;
    }

    // Ask for 32-bit colour, then the picture types we understand
    let mut setup: Vec<u8> = vec![0, 0, 0, 0, 32, 24, 0, 1];
    setup.extend_from_slice(&255u16.to_be_bytes());
    setup.extend_from_slice(&255u16.to_be_bytes());
    setup.extend_from_slice(&255u16.to_be_bytes());
    setup.extend_from_slice(&[16, 8, 0, 0, 0, 0]);
    setup.extend_from_slice(&[2, 0, 0, 3]);
    for enc in [1i32, 0, -223] {
        setup.extend_from_slice(&enc.to_be_bytes());
    }
    setup.extend_from_slice(&[3, 0, 0, 0, 0, 0]);
    setup.extend_from_slice(&(width as u16).to_be_bytes());
    setup.extend_from_slice(&(height as u16).to_be_bytes());
    s.write_all(&setup)?;
    s.set_read_timeout(None)?;

    {
        let mut sc = shared.lock().unwrap();
        sc.width = width;
        sc.height = height;
        sc.rgba = black(width, height);
        sc.name = String::from_utf8_lossy(&name).to_string();
        sc.state = State::Connected;
        sc.dirty = true;
    }
    ctx.request_repaint();

    let mut wr = s.try_clone()?;
    std::thread::spawn(move || {
        for m in rx {
            let ok = match m {
                Out::Pointer { x, y, buttons } => {
                    let mut b = [5u8, buttons, 0, 0, 0, 0];
                    b[2..4].copy_from_slice(&x.to_be_bytes());
                    b[4..6].copy_from_slice(&y.to_be_bytes());
                    wr.write_all(&b).is_ok()
                }
                Out::Key { down, keysym } => {
                    let mut b = [4u8, down as u8, 0, 0, 0, 0, 0, 0];
                    b[4..8].copy_from_slice(&keysym.to_be_bytes());
                    wr.write_all(&b).is_ok()
                }
                Out::Request { w, h, inc } => {
                    let mut b = [3u8, inc as u8, 0, 0, 0, 0, 0, 0, 0, 0];
                    b[6..8].copy_from_slice(&w.to_be_bytes());
                    b[8..10].copy_from_slice(&h.to_be_bytes());
                    wr.write_all(&b).is_ok()
                }
                Out::Close => false,
            };
            if !ok {
                break;
            }
        }
        let _ = wr.shutdown(Shutdown::Both);
    });

    let r = read_loop(&mut s, shared, &tx, ctx);
    let _ = s.shutdown(Shutdown::Both);
    r
}

fn black(w: usize, h: usize) -> Vec<u8> {
    let mut v = vec![0u8; w * h * 4];
    for p in v.chunks_exact_mut(4) {
        p[3] = 255;
    }
    v
}

fn read_loop(
    s: &mut TcpStream,
    shared: &Arc<Mutex<Screen>>,
    tx: &mpsc::Sender<Out>,
    ctx: &eframe::egui::Context,
) -> io::Result<()> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        match read_u8(s)? {
            0 => {
                skip(s, 1)?;
                let count = read_u16(s)?;
                let mut resized = false;
                for _ in 0..count {
                    let x = read_u16(s)? as usize;
                    let y = read_u16(s)? as usize;
                    let w = read_u16(s)? as usize;
                    let h = read_u16(s)? as usize;
                    let enc = read_u32(s)? as i32;
                    match enc {
                        0 => {
                            buf.resize(w * h * 4, 0);
                            s.read_exact(&mut buf)?;
                            let mut guard = shared.lock().unwrap();
                            let sc = &mut *guard;
                            if x + w > sc.width || y + h > sc.height {
                                return fail("The other computer sent a bad picture.");
                            }
                            for row in 0..h {
                                let src = &buf[row * w * 4..(row + 1) * w * 4];
                                let start = ((y + row) * sc.width + x) * 4;
                                let dst = &mut sc.rgba[start..start + w * 4];
                                for (d, p) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                                    d[0] = p[2];
                                    d[1] = p[1];
                                    d[2] = p[0];
                                    d[3] = 255;
                                }
                            }
                            sc.dirty = true;
                        }
                        1 => {
                            let sx = read_u16(s)? as usize;
                            let sy = read_u16(s)? as usize;
                            let mut guard = shared.lock().unwrap();
                            let sc = &mut *guard;
                            if x + w > sc.width
                                || y + h > sc.height
                                || sx + w > sc.width
                                || sy + h > sc.height
                            {
                                return fail("The other computer sent a bad picture.");
                            }
                            let mut tmp = Vec::with_capacity(w * h * 4);
                            for row in 0..h {
                                let st = ((sy + row) * sc.width + sx) * 4;
                                tmp.extend_from_slice(&sc.rgba[st..st + w * 4]);
                            }
                            for row in 0..h {
                                let dt = ((y + row) * sc.width + x) * 4;
                                sc.rgba[dt..dt + w * 4]
                                    .copy_from_slice(&tmp[row * w * 4..(row + 1) * w * 4]);
                            }
                            sc.dirty = true;
                        }
                        -223 => {
                            let mut sc = shared.lock().unwrap();
                            sc.width = w;
                            sc.height = h;
                            sc.rgba = black(w, h);
                            sc.dirty = true;
                            resized = true;
                        }
                        _ => return fail("The other computer used a picture type this viewer does not know."),
                    }
                }
                let (fw, fh) = {
                    let sc = shared.lock().unwrap();
                    (sc.width as u16, sc.height as u16)
                };
                let _ = tx.send(Out::Request {
                    w: fw,
                    h: fh,
                    inc: !resized,
                });
                ctx.request_repaint();
            }
            1 => return fail("This server uses a colour mode this viewer does not support."),
            2 => {}
            3 => {
                skip(s, 3)?;
                let len = read_u32(s)? as u64;
                skip(s, len)?;
            }
            _ => return fail("The other computer sent something unexpected."),
        }
    }
}
