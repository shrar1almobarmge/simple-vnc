mod rfb;

use eframe::egui::{self, Color32, FontId, Key, RichText, TextStyle};
use rfb::{Connection, Out, State};

struct App {
    address: String,
    error: String,
    conn: Option<Connection>,
    texture: Option<egui::TextureHandle>,
    last_pointer: (u16, u16, u8),
    mods: egui::Modifiers,
    shot: Option<String>,
    frames: u32,
    auto_start: bool,
    saved: bool,
    scan: Option<std::sync::Arc<std::sync::Mutex<rfb::Scan>>>,
    found: Vec<String>,
    show_manual: bool,
    fallback_scan: bool,
    failed_now: bool,
    last_failed: String,
}

fn settings_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(std::path::PathBuf::from(base).join("SimpleVNC").join("last.txt"))
}

fn load_settings() -> String {
    let text = settings_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    let mut lines = text.lines();
    let address = lines.next().unwrap_or("").trim().to_string();
    address
}

fn save_settings(address: &str) {
    if let Some(p) = settings_path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, format!("{}\n", address.trim()));
    }
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, shot: Option<String>, cli_address: Option<String>) -> Self {
        cc.egui_ctx.style_mut(|s| {
            s.text_styles
                .insert(TextStyle::Body, FontId::proportional(18.0));
            s.text_styles
                .insert(TextStyle::Button, FontId::proportional(18.0));
            s.text_styles
                .insert(TextStyle::Heading, FontId::proportional(34.0));
            s.spacing.item_spacing = egui::vec2(10.0, 12.0);
        });
        let address = cli_address.unwrap_or_else(load_settings);
        let auto_start = true;
        App {
            address,
            error: String::new(),
            conn: None,
            texture: None,
            last_pointer: (0, 0, 0),
            mods: egui::Modifiers::default(),
            shot,
            frames: 0,
            auto_start,
            saved: false,
            scan: None,
            found: Vec::new(),
            show_manual: false,
            fallback_scan: false,
            failed_now: false,
            last_failed: String::new(),
        }
    }

    fn start(&mut self, ctx: &egui::Context) {
        if self.address.trim().is_empty() {
            self.error = "Please type the address of the other computer first.".to_string();
            return;
        }
        self.error.clear();
        self.saved = false;
        self.texture = None;
        self.last_pointer = (0, 0, 0);
        self.conn = Some(rfb::connect(
            self.address.clone(),
            ctx.clone(),
        ));
    }

    fn stop(&mut self) {
        if let Some(c) = self.conn.take() {
            let _ = c.tx.send(Out::Close);
        }
        self.texture = None;
    }

    fn state(&mut self) -> State {
        let Some(c) = &self.conn else {
            return State::Idle;
        };
        let (state, msg) = {
            let s = c.shared.lock().unwrap();
            (s.state, s.message.clone())
        };
        if state == State::Failed || state == State::Idle {
            self.error = msg;
            self.last_failed = self.address.clone();
            self.failed_now = true;
            self.conn = None;
            self.texture = None;
            return State::Idle;
        }
        state
    }

    fn begin_scan(&mut self, ctx: &egui::Context) {
        self.found.clear();
        self.scan = Some(rfb::scan(ctx.clone()));
    }

    fn poll_scan(&mut self, ctx: &egui::Context) {
        let Some(sc) = &self.scan else { return };
        let (found, done) = {
            let g = sc.lock().unwrap();
            (g.found.clone(), g.done)
        };
        self.found = found;
        if !done {
            return;
        }
        self.scan = None;
        if self.conn.is_none() {
            if self.found.len() == 1 && self.found[0] != self.last_failed {
                self.address = self.found[0].clone();
                self.start(ctx);
            } else if self.found.is_empty() {
                self.show_manual = true;
            }
        }
    }

    fn connect_screen(&mut self, ctx: &egui::Context, connecting: bool) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(30.0);
                ui.heading("Simple VNC Viewer");
                ui.label(
                    RichText::new("It finds the other computer by itself.")
                        .color(Color32::from_gray(170)),
                );
                ui.add_space(20.0);

                let mut chosen: Option<String> = None;
                if connecting {
                    ui.horizontal(|ui| {
                        ui.add_space(ui.available_width() / 2.0 - 110.0);
                        ui.spinner();
                        ui.label(format!("Connecting to {}...", self.address));
                    });
                    if ui.button("Cancel").clicked() {
                        self.stop();
                    }
                } else if self.scan.is_some() && self.found.is_empty() {
                    ui.horizontal(|ui| {
                        ui.add_space(ui.available_width() / 2.0 - 190.0);
                        ui.spinner();
                        ui.label("Looking for computers on your network...");
                    });
                } else if self.found.is_empty() {
                    ui.label("No computer found on your network.");
                    if ui.button("Search again").clicked() {
                        self.begin_scan(ctx);
                    }
                } else {
                    ui.label("Computers found. Press one to connect:");
                    for ip in self.found.clone() {
                        let label = if ip == "127.0.0.1" {
                            "This computer".to_string()
                        } else {
                            ip.clone()
                        };
                        let b = ui.add_sized(
                            [340.0, 52.0],
                            egui::Button::new(
                                RichText::new(label).size(22.0).color(Color32::WHITE),
                            )
                            .fill(Color32::from_rgb(40, 110, 220)),
                        );
                        if b.clicked() {
                            chosen = Some(ip);
                        }
                    }
                    if self.scan.is_none() && ui.button("Search again").clicked() {
                        self.begin_scan(ctx);
                    }
                }

                if !connecting {
                    if self.show_manual {
                        ui.label("Or type its address yourself");
                        let addr = ui.add(
                            egui::TextEdit::singleline(&mut self.address)
                                .hint_text("for example 192.168.1.20")
                                .desired_width(340.0)
                                .margin(egui::vec2(10.0, 8.0)),
                        );
                        let go = ui.add_sized([340.0, 44.0], egui::Button::new("Connect"));
                        let enter = addr.lost_focus()
                            && ctx.input(|i| i.key_pressed(Key::Enter));
                        if go.clicked() || enter {
                            chosen = Some(self.address.clone());
                        }
                    } else if ui.small_button("Type the address myself").clicked() {
                        self.show_manual = true;
                    }
                }
                if let Some(ip) = chosen {
                    self.address = ip;
                    self.start(ctx);
                }

                if !self.error.is_empty() {
                    ui.add_space(6.0);
                    ui.label(RichText::new(&self.error).color(Color32::from_rgb(255, 120, 110)));
                }

                ui.add_space(14.0);
                egui::Frame::none()
                    .fill(Color32::from_gray(36))
                    .rounding(8.0)
                    .inner_margin(14.0)
                    .show(ui, |ui| {
                        ui.set_width(340.0);
                        ui.label(RichText::new("How it works").strong());
                        let dim = Color32::from_gray(185);
                        ui.label(RichText::new("1. On the other computer, open Simple VNC Server (the second app) and keep it open. It must be on the same network.").color(dim));
                        ui.hyperlink_to("Or use UltraVNC Server (uvnc.com)", "https://uvnc.com");
                        ui.label(RichText::new("2. This app looks for it and connects by itself. No password needed.").color(dim));
                        ui.label(RichText::new("3. You then see its screen and use your mouse and keyboard on it. Next time it reconnects by itself.").color(dim));
                    });
            });
        });
    }

    fn remote_screen(&mut self, ctx: &egui::Context) {
        let Some(conn) = &self.conn else { return };
        let (name, new_image) = {
            let mut s = conn.shared.lock().unwrap();
            let img = if s.dirty {
                s.dirty = false;
                Some(egui::ColorImage::from_rgba_unmultiplied(
                    [s.width, s.height],
                    &s.rgba,
                ))
            } else {
                None
            };
            (s.name.clone(), img)
        };
        if let Some(img) = new_image {
            match &mut self.texture {
                Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                None => {
                    self.texture = Some(ctx.load_texture("remote", img, egui::TextureOptions::LINEAR))
                }
            }
        }

        let mut disconnect = false;
        let mut cad = false;
        egui::TopBottomPanel::top("bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let r = ui.button("Disconnect");
                if r.has_focus() {
                    r.surrender_focus();
                }
                disconnect = r.clicked();
                let r = ui.button("Send Ctrl+Alt+Del");
                if r.has_focus() {
                    r.surrender_focus();
                }
                cad = r.clicked();
                ui.label(RichText::new(name).color(Color32::from_gray(160)));
            });
        });
        if disconnect {
            self.stop();
            return;
        }
        if cad {
            if let Some(c) = &self.conn {
                for (down, k) in [
                    (true, 0xffe3u32),
                    (true, 0xffe9),
                    (true, 0xffff),
                    (false, 0xffff),
                    (false, 0xffe9),
                    (false, 0xffe3),
                ] {
                    let _ = c.tx.send(Out::Key { down, keysym: k });
                }
            }
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(Color32::BLACK))
            .show(ctx, |ui| {
                let Some(tex) = &self.texture else {
                    ui.centered_and_justified(|ui| ui.spinner());
                    return;
                };
                let avail = ui.available_size();
                let (area, _) = ui.allocate_exact_size(avail, egui::Sense::hover());
                let size = tex.size_vec2();
                let scale = (avail.x / size.x).min(avail.y / size.y);
                let img_rect = egui::Rect::from_center_size(area.center(), size * scale);
                ui.painter().image(
                    tex.id(),
                    img_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
                self.send_input(ctx, img_rect, scale, size);
            });
    }

    fn send_input(&mut self, ctx: &egui::Context, img_rect: egui::Rect, scale: f32, size: egui::Vec2) {
        let Some(conn) = &self.conn else { return };
        let tx = &conn.tx;
        let (pos, down, scroll, mods, events) = ctx.input(|i| {
            let mut b = 0u8;
            if i.pointer.button_down(egui::PointerButton::Primary) {
                b |= 1;
            }
            if i.pointer.button_down(egui::PointerButton::Middle) {
                b |= 2;
            }
            if i.pointer.button_down(egui::PointerButton::Secondary) {
                b |= 4;
            }
            (
                i.pointer.latest_pos(),
                b,
                i.raw_scroll_delta.y,
                i.modifiers,
                i.events.clone(),
            )
        });

        if let Some(p) = pos {
            let inside = img_rect.contains(p);
            if inside || down != 0 || self.last_pointer.2 != 0 {
                let x = (((p.x - img_rect.min.x) / scale).max(0.0) as u16).min(size.x as u16 - 1);
                let y = (((p.y - img_rect.min.y) / scale).max(0.0) as u16).min(size.y as u16 - 1);
                if (x, y, down) != self.last_pointer {
                    self.last_pointer = (x, y, down);
                    let _ = tx.send(Out::Pointer { x, y, buttons: down });
                }
                if inside && scroll != 0.0 {
                    let wheel = if scroll > 0.0 { 8 } else { 16 };
                    let _ = tx.send(Out::Pointer { x, y, buttons: down | wheel });
                    let _ = tx.send(Out::Pointer { x, y, buttons: down });
                }
            }
        }

        for (was, now, keysym) in [
            (self.mods.shift, mods.shift, 0xffe1u32),
            (self.mods.ctrl, mods.ctrl, 0xffe3),
            (self.mods.alt, mods.alt, 0xffe9),
        ] {
            if was != now {
                let _ = tx.send(Out::Key { down: now, keysym });
            }
        }
        self.mods = mods;

        for e in events {
            match e {
                egui::Event::Key {
                    key, pressed, modifiers, ..
                } => {
                    let sym = special_keysym(key).or_else(|| {
                        if modifiers.ctrl || modifiers.alt {
                            letter_keysym(key)
                        } else {
                            None
                        }
                    });
                    if let Some(keysym) = sym {
                        let _ = tx.send(Out::Key { down: pressed, keysym });
                    }
                }
                egui::Event::Text(t) => {
                    for ch in t.chars().filter(|c| !c.is_control()) {
                        let cp = ch as u32;
                        let keysym = if cp < 0x100 { cp } else { 0x0100_0000 + cp };
                        let _ = tx.send(Out::Key { down: true, keysym });
                        let _ = tx.send(Out::Key { down: false, keysym });
                    }
                }
                _ => {}
            }
        }
    }
}

fn special_keysym(key: Key) -> Option<u32> {
    Some(match key {
        Key::Enter => 0xff0d,
        Key::Backspace => 0xff08,
        Key::Tab => 0xff09,
        Key::Escape => 0xff1b,
        Key::Delete => 0xffff,
        Key::Insert => 0xff63,
        Key::Home => 0xff50,
        Key::End => 0xff57,
        Key::PageUp => 0xff55,
        Key::PageDown => 0xff56,
        Key::ArrowLeft => 0xff51,
        Key::ArrowUp => 0xff52,
        Key::ArrowRight => 0xff53,
        Key::ArrowDown => 0xff54,
        Key::F1 => 0xffbe,
        Key::F2 => 0xffbf,
        Key::F3 => 0xffc0,
        Key::F4 => 0xffc1,
        Key::F5 => 0xffc2,
        Key::F6 => 0xffc3,
        Key::F7 => 0xffc4,
        Key::F8 => 0xffc5,
        Key::F9 => 0xffc6,
        Key::F10 => 0xffc7,
        Key::F11 => 0xffc8,
        Key::F12 => 0xffc9,
        _ => return None,
    })
}

fn letter_keysym(key: Key) -> Option<u32> {
    if key == Key::Space {
        return Some(0x20);
    }
    let name = key.name();
    let mut chars = name.chars();
    let c = chars.next()?;
    if chars.next().is_some() || !c.is_ascii_alphanumeric() {
        return None;
    }
    Some(c.to_ascii_lowercase() as u32)
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frames += 1;

        if self.auto_start {
            self.auto_start = false;
            if self.address.is_empty() {
                self.begin_scan(ctx);
            } else {
                self.fallback_scan = true;
                self.start(ctx);
            }
        }

        let state = self.state();
        if self.failed_now {
            self.failed_now = false;
            if self.fallback_scan {
                self.fallback_scan = false;
                self.begin_scan(ctx);
            }
        }
        self.poll_scan(ctx);

        match state {
            State::Connected => {
                self.fallback_scan = false;
                if !self.saved {
                    self.saved = true;
                    save_settings(&self.address);
                }
                self.remote_screen(ctx)
            }
            State::Connecting => self.connect_screen(ctx, true),
            _ => self.connect_screen(ctx, false),
        }

        if let Some(path) = self.shot.clone() {
            ctx.request_repaint();
            if self.frames == 120 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
            }
            let img = ctx.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
            });
            if let Some(img) = img {
                let [w, h] = img.size;
                let mut buf = Vec::with_capacity(w * h * 4);
                for p in &img.pixels {
                    buf.extend_from_slice(&p.to_array());
                }
                if let Some(parent) = std::path::Path::new(&path).parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let ok = image::save_buffer(&path, &buf, w as u32, h as u32, image::ColorType::Rgba8)
                    .is_ok();
                std::process::exit(if ok { 0 } else { 1 });
            }
            if self.frames > 900 {
                std::process::exit(1);
            }
        }
    }
}

const HELP: &str = "Simple VNC Viewer

Usage:
  simple-vnc-viewer                  open the window and look for a computer by itself
  simple-vnc-viewer 192.168.1.20     connect straight to that address
  simple-vnc-viewer 192.168.1.20:5901   same, with a port
  simple-vnc-viewer --help           show this help
  simple-vnc-viewer --screenshot f.png   save a picture of the window and exit
";

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h" || a == "/?") {
        print!("{}", HELP);
        return Ok(());
    }
    let mut shot = None;
    let mut cli_address = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--screenshot" {
            shot = it.next().cloned();
        } else if a.starts_with('-') {
            eprintln!("Unknown option {}. Use --help to see what is possible.", a);
            std::process::exit(2);
        } else if cli_address.is_none() {
            cli_address = Some(a.clone());
        }
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([720.0, 700.0])
            .with_min_inner_size([480.0, 400.0])
            .with_title("Simple VNC Viewer"),
        ..Default::default()
    };
    eframe::run_native(
        "Simple VNC Viewer",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, shot, cli_address)))),
    )
}
