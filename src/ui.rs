//! The editor window (`loginux ui`).
//!
//! It never opens the keyboard. It talks to the engine over the local socket and,
//! when no engine is running, starts one inside this process; that engine hands
//! the lighting back when the window closes. Bitmap lettering is drawn here cell
//! by cell, each cell snapped to whole device pixels, so it stays sharp at any
//! display scale. Antialiasing (feathering) is off for the same reason: every
//! shape is a pixel-aligned rectangle.

use crate::engine::{self, Hosted, STOP};
use crate::ipc::{self, Control, Device, RateChange, Request, Status};
use crate::layout::{self, Kind, Model, Zone};
use crate::pixelfont;
use crate::profile::{self, Profile, Rgb, Seen};
use eframe::egui::{
    self, Color32, FontFamily, FontId, Key, Mesh, Painter, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui,
    Vec2, pos2, vec2,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

// ---- palette (direction contract: .impeccable/surfaces/src-ui-rs.md) ----------

const GROUND: Color32 = Color32::from_rgb(0x0B, 0x0B, 0x0C);
const PANEL: Color32 = Color32::from_rgb(0x15, 0x15, 0x17);
const RAISED: Color32 = Color32::from_rgb(0x1F, 0x1F, 0x23);
const RULE: Color32 = Color32::from_rgb(0x2A, 0x2A, 0x2E);
const TEXT: Color32 = Color32::from_rgb(0xED, 0xED, 0xED);
const TEXT2: Color32 = Color32::from_rgb(0xA9, 0xA9, 0xAE);
const LABEL: Color32 = Color32::from_rgb(0x8E, 0x8E, 0x94);
const RED: Color32 = Color32::from_rgb(0xFF, 0x2E, 0x3F);
const RED_HOT: Color32 = Color32::from_rgb(0xFF, 0x57, 0x65);
const GREEN: Color32 = Color32::from_rgb(0x3F, 0xD0, 0x7A);
const AMBER: Color32 = Color32::from_rgb(0xF2, 0xB8, 0x4B);
const BODY: Color32 = Color32::BLACK;
const DARK_KEY: Color32 = Color32::from_rgb(0x1A, 0x1A, 0x1D);
const DISABLED: Color32 = Color32::from_rgb(0x4A, 0x4A, 0x50);
const DOTS: Color32 = Color32::from_rgb(0x6E, 0x6E, 0x73);

const ARCHIVO: &[u8] = include_bytes!("../fonts/Archivo[wdth,wght].ttf");

// ---- bitmap lettering --------------------------------------------------------

/// A bitmap text size: `cell` is one font pixel in points before snapping.
#[derive(Clone, Copy)]
struct Px {
    bold: bool,
    cell: f32,
}

const LEGEND: Px = Px { bold: false, cell: 1.0 };
const SMALL: Px = Px { bold: false, cell: 1.5 };
const LABEL_PX: Px = Px { bold: true, cell: 1.5 };
const TAB: Px = Px { bold: false, cell: 2.0 };
const BRAND: Px = Px { bold: true, cell: 4.0 };
const HEX: Px = Px { bold: true, cell: 4.0 };
const TITLE: Px = Px { bold: true, cell: 6.0 };

type Glyph = (char, u8, [u16; pixelfont::ROWS]);

fn glyph(ch: char, bold: bool) -> &'static Glyph {
    let table = if bold { pixelfont::BOLD } else { pixelfont::REGULAR };
    let find = |c: char| table.binary_search_by_key(&c, |g| g.0).ok().map(|i| &table[i]);
    find(ch.to_ascii_uppercase()).or_else(|| find('?')).expect("'?' is in the table")
}

/// A size in whole device pixels. The small bias keeps a scale such as
/// 1.4999 (two scale factors multiplied) rounding the same way as 1.5.
fn whole(device_px: f32) -> f32 {
    (device_px + 0.01).round()
}

/// Whole device pixels per font pixel at this scale; never zero.
fn cell_px(px: Px, ppp: f32) -> f32 {
    whole(px.cell * ppp).max(1.0)
}

/// Ink size in points: Silkscreen's one-cell side bearings are trimmed.
fn px_size(text: &str, px: Px, ppp: f32) -> Vec2 {
    let cols: i32 = text.chars().map(|c| i32::from(glyph(c, px.bold).1)).sum();
    let c = cell_px(px, ppp);
    vec2((cols - 2).max(0) as f32 * c / ppp, pixelfont::CAP_ROWS as f32 * c / ppp)
}

/// Draws `text` with its ink's top-left corner at `pos`, snapped to the pixel grid.
fn px_text(painter: &Painter, pos: Pos2, text: &str, px: Px, color: Color32) -> Rect {
    let ppp = painter.pixels_per_point();
    let c = cell_px(px, ppp);
    let (x0, y0) = ((pos.x * ppp).round(), (pos.y * ppp).round());
    let mut mesh = Mesh::default();
    let mut pen = -1i32; // drop the first glyph's left bearing
    for ch in text.chars() {
        let &(_, cols, rows) = glyph(ch, px.bold);
        let cols = i32::from(cols);
        for (r, bits) in rows.iter().enumerate() {
            let mut run = None;
            for col in 0..=cols {
                let on = col < cols && bits >> col & 1 == 1;
                match (on, run) {
                    (true, None) => run = Some(col),
                    (false, Some(start)) => {
                        let (a, b) = ((pen + start) as f32 * c, (pen + col) as f32 * c);
                        let y = y0 + r as f32 * c;
                        let rect = Rect::from_min_max(pos2((x0 + a) / ppp, y / ppp), pos2((x0 + b) / ppp, (y + c) / ppp));
                        mesh.add_colored_rect(rect, color);
                        run = None;
                    }
                    _ => {}
                }
            }
        }
        pen += cols;
    }
    painter.add(Shape::mesh(mesh));
    let size = px_size(text, px, ppp);
    Rect::from_min_size(pos2(x0 / ppp, y0 / ppp), size)
}

// ---- pixel-aligned shapes -------------------------------------------------------

fn snap(r: Rect, ppp: f32) -> Rect {
    let s = |v: f32| (v * ppp).round() / ppp;
    Rect::from_min_max(pos2(s(r.min.x), s(r.min.y)), pos2(s(r.max.x), s(r.max.y)))
}

/// `points` of line width as whole device pixels, at least one.
fn line(points: f32, ppp: f32) -> f32 {
    whole(points * ppp).max(1.0) / ppp
}

fn fill(painter: &Painter, r: Rect, color: Color32) {
    painter.rect_filled(snap(r, painter.pixels_per_point()), 0.0, color);
}

fn outline(painter: &Painter, r: Rect, width: f32, color: Color32) {
    let ppp = painter.pixels_per_point();
    painter.rect_stroke(snap(r, ppp), 0.0, Stroke::new(line(width, ppp), color), StrokeKind::Inside);
}

fn rgb(c: Rgb) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

/// Black or white legend on a key of colour `c`.
fn ink_on(c: Rgb) -> Color32 {
    let l = 0.2126 * f32::from(c[0]) + 0.7152 * f32::from(c[1]) + 0.0722 * f32::from(c[2]);
    if l > 140.0 { GROUND } else { TEXT }
}

// ---- widgets -----------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Look {
    Primary,
    Outline,
    Quiet,
}

fn button(ui: &mut Ui, text: &str, look: Look, enabled: bool) -> Response {
    let ppp = ui.pixels_per_point();
    let size = px_size(text, LABEL_PX, ppp) + vec2(24.0, 18.0);
    let (rect, resp) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, text));
    let down = resp.is_pointer_button_down_on();
    let hot = resp.hovered() && enabled;
    let (bg, edge, ink) = match look {
        _ if !enabled => (Color32::TRANSPARENT, Color32::TRANSPARENT, DISABLED),
        Look::Primary => (if down { RED } else if hot { RED_HOT } else { RED }, Color32::TRANSPARENT, GROUND),
        Look::Outline => (if down { RULE } else if hot { RAISED } else { Color32::TRANSPARENT }, TEXT, TEXT),
        Look::Quiet => (if hot { RAISED } else { Color32::TRANSPARENT }, RULE, if hot { TEXT } else { TEXT2 }),
    };
    let p = ui.painter();
    fill(p, rect, bg);
    outline(p, rect, 2.0, edge);
    let t = px_size(text, LABEL_PX, ppp);
    px_text(p, rect.center() - t / 2.0, text, LABEL_PX, ink);
    if resp.has_focus() {
        outline(p, rect.expand(3.0), 2.0, RED);
    }
    resp
}

/// Bitmap text that never claims more width than the layout offers; overflow is clipped.
fn label(ui: &mut Ui, text: &str, px: Px, color: Color32) -> Rect {
    let size = px_size(text, px, ui.pixels_per_point());
    let (rect, _) = ui.allocate_exact_size(vec2(size.x.min(ui.available_width()), size.y), Sense::hover());
    px_text(&ui.painter().with_clip_rect(rect.expand(1.0)), rect.min, text, px, color)
}

/// Archivo body text.
fn body(ui: &mut Ui, text: impl Into<String>, color: Color32, size: f32) -> Response {
    ui.label(egui::RichText::new(text.into()).color(color).size(size))
}

fn rule(ui: &mut Ui) {
    let w = ui.available_width();
    let (r, _) = ui.allocate_exact_size(vec2(w, 2.0), Sense::hover());
    fill(ui.painter(), r, RULE);
}

/// The dot-matrix patch: the keyboard's own dot screen as the house texture.
fn dots(painter: &Painter, r: Rect, color: Color32) {
    let ppp = painter.pixels_per_point();
    let (pitch, dot) = (whole(6.0 * ppp), whole(2.0 * ppp).max(1.0));
    let mut mesh = Mesh::default();
    let (x0, y0) = ((r.min.x * ppp).round(), (r.min.y * ppp).round());
    let (cols, rows) = ((r.width() * ppp / pitch) as i32, (r.height() * ppp / pitch) as i32);
    for row in 0..rows {
        for col in 0..cols {
            let (x, y) = (x0 + col as f32 * pitch, y0 + row as f32 * pitch);
            mesh.add_colored_rect(Rect::from_min_size(pos2(x / ppp, y / ppp), vec2(dot / ppp, dot / ppp)), color);
        }
    }
    painter.add(Shape::mesh(mesh));
}

// ---- link to the engine ------------------------------------------------------

#[derive(Default)]
struct Outbox {
    requests: VecDeque<Request>,
    preview: Option<Option<Profile>>,
    status: Option<Result<Status, String>>,
    errors: Vec<String>,
    quit: bool,
}

/// A thread that sends requests in order (the newest preview replaces older ones)
/// and polls status once a second, which also renews preview and check leases.
#[derive(Clone)]
struct Link(Arc<(Mutex<Outbox>, Condvar)>);

impl Link {
    fn start(ctx: egui::Context) -> Link {
        let link = Link(Arc::default());
        let inner = link.0.clone();
        thread::spawn(move || {
            let (lock, wake) = &*inner;
            loop {
                let req = {
                    let mut out = lock.lock().expect("link lock");
                    if out.requests.is_empty() && out.preview.is_none() && !out.quit {
                        out = wake.wait_timeout(out, Duration::from_secs(1)).expect("link lock").0;
                    }
                    if out.quit {
                        return;
                    }
                    match out.requests.pop_front() {
                        Some(r) => r,
                        None => out.preview.take().map_or(Request::Status, |p| Request::Preview { profile: p }),
                    }
                };
                let result = ipc::request(&req);
                let mut out = lock.lock().expect("link lock");
                match result {
                    Ok(status) => out.status = Some(Ok(status)),
                    // The engine is there but said no: report it, keep the last status.
                    Err(ipc::Error::Refused(e)) => out.errors.push(e),
                    Err(ipc::Error::Unreachable(e)) => out.status = Some(Err(e)),
                }
                drop(out);
                ctx.request_repaint();
            }
        });
        link
    }

    fn send(&self, req: Request) {
        self.0.0.lock().expect("link lock").requests.push_back(req);
        self.0.1.notify_one();
    }

    fn preview(&self, profile: Option<Profile>) {
        self.0.0.lock().expect("link lock").preview = Some(profile);
        self.0.1.notify_one();
    }

    fn take(&self) -> (Option<Result<Status, String>>, Vec<String>) {
        let mut out = self.0.0.lock().expect("link lock");
        (out.status.clone(), std::mem::take(&mut out.errors))
    }

    fn quit(&self) {
        self.0.0.lock().expect("link lock").quit = true;
        self.0.1.notify_one();
    }
}

// ---- the systemd user service ------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Service {
    Unknown,
    NotInstalled,
    Enabled,
    Disabled,
}

fn service_state() -> Service {
    let out = Command::new("systemctl").args(["--user", "is-enabled", "loginux.service"]).output();
    match out.as_ref().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()) {
        Ok(s) if s.starts_with("enabled") || s == "linked" => Service::Enabled,
        Ok(s) if !s.is_empty() => Service::Disabled,
        _ => Service::NotInstalled,
    }
}

// ---- the app ---------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Edit,
    Check,
    Device,
}

#[derive(Clone, Copy, PartialEq)]
enum Set {
    Strip(usize),
    Unknown,
    Keys,
}

impl Set {
    fn all(model: &Model) -> Vec<Set> {
        let unknown = (!model.unknown().is_empty()).then_some(Set::Unknown);
        (0..model.strips.len()).map(Set::Strip).chain(unknown).chain([Set::Keys]).collect()
    }

    fn zones(self, model: &Model) -> Vec<u8> {
        match self {
            Set::Strip(i) => model.strips[i].zones.clone(),
            Set::Unknown => model.unknown(),
            Set::Keys => model.ids(Kind::Key),
        }
    }

    fn title(self, model: &Model) -> String {
        match self {
            Set::Strip(i) => model.strips[i].title.to_uppercase(),
            Set::Unknown => "UNKNOWN ZONES".into(),
            Set::Keys => if model.per_key { "ALL KEYS" } else { "ALL LIGHTS" }.into(),
        }
    }
}

enum Confirm {
    Close,
    Delete(String),
    Switch(String),
}

struct App {
    model: &'static Model,
    link: Link,
    hosted: Option<Hosted>,
    device: Option<PathBuf>,
    host_tried: Option<Instant>,
    status: Option<Result<Status, String>>,
    tab: Tab,
    names: Vec<String>,
    cache: BTreeMap<String, Profile>,
    open: Option<String>,
    saved: Profile,
    work: Profile,
    touched: bool,
    sent: Option<Option<Profile>>,
    selection: BTreeSet<u8>,
    anchor: Option<Pos2>,
    hover: Option<u8>,
    hsv: [f32; 3],
    hex_text: String,
    checks: BTreeMap<u8, Seen>,
    set: Option<Set>,
    /// The run whose answers the right column shows before a check starts.
    focus: Set,
    step: usize,
    sent_check: Option<Option<u8>>,
    service: Arc<Mutex<Service>>,
    notice: Option<(String, Color32)>,
    confirm: Option<Confirm>,
    new_name: Option<String>,
    /// A report rate the user picked, waiting for their confirmation.
    rate_confirm: Option<u32>,
    closing: bool,
    engine_up: bool,
}

impl App {
    fn new(ctx: &egui::Context, device: Option<PathBuf>, hosted: Option<Hosted>) -> App {
        let link = Link::start(ctx.clone());
        let service = Arc::new(Mutex::new(Service::Unknown));
        let model = layout::current();
        let mut app = App {
            model,
            link,
            hosted,
            device,
            host_tried: None,
            status: Some(ipc::request(&Request::Status).map_err(String::from)),
            tab: Tab::Edit,
            names: vec![],
            cache: BTreeMap::new(),
            open: None,
            saved: Profile::default(),
            work: Profile::default(),
            touched: false,
            sent: None,
            selection: BTreeSet::new(),
            anchor: None,
            hover: None,
            hsv: [0.0, 0.0, 1.0],
            hex_text: String::new(),
            checks: profile::load_checks(model),
            set: None,
            focus: Set::all(model)[0],
            step: 0,
            sent_check: None,
            service,
            notice: None,
            confirm: None,
            new_name: None,
            rate_confirm: None,
            closing: false,
            engine_up: false,
        };
        app.refresh_service();
        app.reload_list();
        let first = profile::active().filter(|a| app.names.contains(a)).or_else(|| app.names.first().cloned());
        if let Some(name) = first {
            app.open_profile(&name);
        }
        app.touched = false;
        app
    }

    fn refresh_service(&self) {
        let slot = self.service.clone();
        thread::spawn(move || *slot.lock().expect("service lock") = service_state());
    }

    fn set_service(&mut self, on: bool) {
        let slot = self.service.clone();
        let (verb, link) = (if on { "enable" } else { "disable" }, self.link.clone());
        thread::spawn(move || {
            let out = Command::new("systemctl").args(["--user", verb, "--now", "loginux.service"]).output();
            if let Ok(o) = &out
                && !o.status.success()
            {
                let mut b = link.0.0.lock().expect("link lock");
                b.errors.push(format!("systemctl --user {verb}: {}", String::from_utf8_lossy(&o.stderr).trim()));
            }
            *slot.lock().expect("service lock") = service_state();
        });
    }

    fn reload_list(&mut self) {
        self.names = profile::list();
        self.cache = self.names.iter().filter_map(|n| Some((n.clone(), profile::load(self.model, n).ok()?))).collect();
    }

    fn dirty(&self) -> bool {
        self.open.is_some() && self.work != self.saved
    }

    fn open_profile(&mut self, name: &str) {
        match profile::load(self.model, name) {
            Ok(p) => {
                self.open = Some(name.to_string());
                self.saved = p.clone();
                self.work = p;
                self.touched = true;
                self.sync_picker();
            }
            Err(e) => self.notice = Some((e, AMBER)),
        }
    }

    fn save(&mut self) -> bool {
        let Some(name) = self.open.clone() else { return false };
        normalize(self.model, &mut self.work);
        match profile::save(self.model, &name, &self.work) {
            Ok(()) => {
                self.saved = self.work.clone();
                self.reload_list();
                true
            }
            Err(e) => {
                self.notice = Some((format!("could not save: {e}"), AMBER));
                false
            }
        }
    }

    fn status(&self) -> Option<&Status> {
        self.status.as_ref().and_then(|s| s.as_ref().ok())
    }

    fn connected_zones(&self) -> Option<&[u8]> {
        match &self.status()?.device {
            Device::Connected { zones, .. } => Some(zones),
            _ => None,
        }
    }

    /// Colour shown for `id` in the editor.
    fn colour(&self, id: u8) -> Option<Rgb> {
        self.work.colour(self.model, id)
    }

    fn selection_colours(&self) -> Vec<Rgb> {
        let mut seen: Vec<Rgb> = vec![];
        for &id in &self.selection {
            let c = self.colour(id).unwrap_or([0; 3]);
            if !seen.contains(&c) {
                seen.push(c);
            }
        }
        seen
    }

    fn sync_picker(&mut self) {
        let colours = self.selection_colours();
        if let [c] = colours[..] {
            self.hsv = to_hsv(c);
            self.hex_text = profile::hex(c);
        } else {
            self.hex_text.clear();
        }
    }

    fn paint_selection(&mut self, c: Rgb) {
        for &id in &self.selection {
            self.work.zones.insert(id, c);
        }
        self.touched = true;
        self.hex_text = profile::hex(c);
    }

    /// Engine upkeep each frame: host an engine when none answers, keep the
    /// preview and the channel check current.
    fn upkeep(&mut self, ctx: &egui::Context) {
        ctx.request_repaint_after(Duration::from_secs(1));
        if STOP.load(Ordering::SeqCst) {
            self.closing = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if ctx.input(|i| i.viewport().close_requested()) && !self.closing && self.dirty() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm = Some(Confirm::Close);
        }
        let (status, errors) = self.link.take();
        if let Some(s) = status {
            self.status = Some(s);
        }
        if let Some(e) = errors.into_iter().last() {
            self.notice = Some((e, AMBER));
        }
        let engine_down = matches!(self.status, Some(Err(_)));
        // A new or restarted engine knows nothing of this window: send its wishes again.
        if !engine_down && !self.engine_up {
            self.sent = None;
            self.sent_check = None;
        }
        self.engine_up = !engine_down;
        if let Some(Device::Connected { keyboard, name, .. }) = self.status().map(|s| &s.device)
            && *keyboard != self.model.id
        {
            self.notice = Some((format!("The connected keyboard is a {name}; restart loginux to edit it."), AMBER));
        }
        if engine_down && self.hosted.is_none() && self.host_tried.is_none_or(|t| t.elapsed() > Duration::from_secs(2)) {
            self.host_tried = Some(Instant::now());
            if let Some(Err(e)) = &self.status {
                eprintln!("loginux: {e}; starting an engine in this window");
            }
            match Hosted::start(engine::Options { device: self.device.clone(), refresh: None }) {
                Ok(Some(h)) => {
                    self.hosted = Some(h);
                    self.sent = None;
                    self.sent_check = None;
                }
                Ok(None) => {} // another engine holds the lock; its socket will appear
                Err(e) => self.notice = Some((e, AMBER)),
            }
        }
        let check = if self.tab == Tab::Check { self.set.and_then(|s| s.zones(self.model).get(self.step).copied()) } else { None };
        if engine_down {
            return; // nothing to send until an engine answers
        }
        if self.sent_check != Some(check) {
            self.link.send(Request::Check { zone: check });
            self.sent_check = Some(check);
        }
        let want = (self.touched && self.open.is_some()).then(|| self.work.clone());
        if self.sent.as_ref() != Some(&want) {
            self.link.preview(want.clone());
            self.sent = Some(want);
        }

    }

    // ---- header ------------------------------------------------------------------

    fn header(&mut self, ui: &mut Ui) {
        let r = ui.max_rect();
        let p = ui.painter().clone();
        let ppp = ui.pixels_per_point();
        let brand = px_size("LOGINUX", BRAND, ppp);
        px_text(&p, pos2(r.min.x + 22.0, r.center().y - brand.y / 2.0), "LOGINUX", BRAND, TEXT);

        let mut x = r.min.x + 22.0 + brand.x + 36.0;
        for (tab, name) in [(Tab::Edit, "EDIT"), (Tab::Check, "CHANNEL CHECK"), (Tab::Device, "DEVICE")] {
            let size = px_size(name, TAB, ppp);
            let hit = Rect::from_min_max(pos2(x - 8.0, r.min.y), pos2(x + size.x + 8.0, r.max.y));
            let resp = ui.interact(hit, ui.scope_id().with(name), Sense::click());
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Tab, true, name));
            let on = self.tab == tab;
            let ink = if on || resp.hovered() { TEXT } else { LABEL };
            px_text(&p, pos2(x, r.center().y - size.y / 2.0), name, TAB, ink);
            if on {
                fill(&p, Rect::from_min_max(pos2(x - 2.0, r.max.y - 6.0), pos2(x + size.x + 2.0, r.max.y - 2.0)), RED);
            }
            if resp.has_focus() {
                outline(&p, hit.shrink(4.0), 2.0, RED);
            }
            if resp.clicked() {
                self.tab = tab;
            }
            x += size.x + 34.0;
        }

        let service = *self.service.lock().expect("service lock");
        let (text, look, enabled) = match service {
            Service::Enabled => ("RUNS AT LOGIN", Look::Quiet, true),
            Service::Disabled => ("RUN AT LOGIN", Look::Primary, true),
            Service::NotInstalled => ("SERVICE NOT INSTALLED", Look::Quiet, false),
            Service::Unknown => ("RUN AT LOGIN", Look::Quiet, false),
        };
        let bsize = px_size(text, LABEL_PX, ppp) + vec2(24.0, 18.0);
        let brect = Rect::from_min_size(pos2(r.max.x - 22.0 - bsize.x, r.center().y - bsize.y / 2.0), bsize);
        let resp = ui.put(brect, |ui: &mut Ui| button(ui, text, look, enabled));
        let resp = resp.on_hover_text(match service {
            Service::NotInstalled => {
                "Install systemd/loginux.service into ~/.config/systemd/user/ (see README) to keep lighting after this window closes."
            }
            Service::Enabled => {
                "~/.config/systemd/user/default.target.wants/loginux.service starts `loginux run` at login. Click to disable and stop it."
            }
            _ => "Lighting stays only while this window is open. Click to enable loginux.service (systemctl --user enable --now).",
        });
        if resp.clicked() {
            self.set_service(service != Service::Enabled);
        }
        let patch = Rect::from_min_max(pos2(brect.min.x - 214.0, r.center().y - 15.0), pos2(brect.min.x - 22.0, r.center().y + 15.0));
        if patch.min.x > x {
            dots(&p, patch, DOTS);
        }
        fill(&p, Rect::from_min_max(pos2(r.min.x, r.max.y - 2.0), r.max), TEXT);
    }

    // ---- left column: profiles and status ----------------------------------------

    fn left(&mut self, ui: &mut Ui) {
        ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
        label(ui, "PROFILES", LABEL_PX, LABEL);
        ui.add_space(4.0);
        let active = self.status().and_then(|s| s.active.clone()).or_else(profile::active);
        let names = self.names.clone();
        egui::ScrollArea::vertical().max_height(ui.available_height() - 210.0).show(ui, |ui| {
            for name in &names {
                let is_open = self.open.as_deref() == Some(name.as_str());
                let mark = if active.as_deref() == Some(name.as_str()) { "IN USE" } else { "" };
                if self.profile_box(ui, name, is_open, mark).clicked() && !is_open {
                    if self.dirty() {
                        self.confirm = Some(Confirm::Switch(name.clone()));
                    } else {
                        self.open_profile(name);
                        self.selection.clear();
                    }
                }
            }
            if self.names.is_empty() {
                body(ui, "No profiles yet. Create one, paint it, and save.", TEXT2, 13.0);
            }
            ui.add_space(4.0);
            match self.new_name.as_mut() {
                None => {
                    if button(ui, "+ NEW PROFILE", Look::Quiet, true)
                        .on_hover_text("Starts as a copy of the open profile")
                        .clicked()
                    {
                        self.new_name = Some(String::new());
                    }
                }
                Some(name) => {
                    let edit = ui.add(egui::TextEdit::singleline(name).hint_text("Name").desired_width(f32::INFINITY));
                    edit.request_focus();
                    let typed = name.trim().to_string();
                    let ok = profile::valid_name(&typed) && !self.names.contains(&typed);
                    let mut create = edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && ok;
                    ui.horizontal(|ui| {
                        create |= button(ui, "CREATE", Look::Outline, ok).clicked();
                        if button(ui, "CANCEL", Look::Quiet, true).clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                            self.new_name = None;
                        }
                    });
                    if !typed.is_empty() && !ok {
                        body(ui, "Letters, digits, spaces, - and _; not an existing name.", AMBER, 12.0);
                    }
                    if create {
                        let copy = self.work.clone();
                        match profile::save(self.model, &typed, &copy) {
                            Ok(()) => {
                                self.new_name = None;
                                self.reload_list();
                                self.open_profile(&typed);
                            }
                            Err(e) => self.notice = Some((e, AMBER)),
                        }
                    }
                }
            }
        });

        // Status block, pinned to the bottom.
        let bottom = ui.max_rect().max.y;
        let height = 196.0;
        let area = Rect::from_min_max(pos2(ui.max_rect().min.x, bottom - height), pos2(ui.max_rect().max.x, bottom));
        ui.scope_builder(egui::UiBuilder::new().max_rect(area), |ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 7.0);
            rule(ui);
            self.status_block(ui);
        });
    }

    fn profile_box(&self, ui: &mut Ui, name: &str, is_open: bool, mark: &str) -> Response {
        let w = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(vec2(w, 48.0), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, name));
        let p = ui.painter();
        fill(p, rect, if is_open { PANEL } else if resp.hovered() { RAISED } else { Color32::TRANSPARENT });
        outline(p, rect, 2.0, if is_open { TEXT } else { RULE });
        let inner = rect.shrink(10.0);
        px_text(&p.with_clip_rect(inner), inner.min + vec2(0.0, 1.0), &name.to_uppercase(), SMALL, TEXT);
        let m = px_size(mark, SMALL, ui.pixels_per_point());
        if !mark.is_empty() {
            px_text(p, pos2(inner.max.x - m.x, inner.max.y - m.y), mark, SMALL, TEXT2);
        }
        let shown = if is_open { Some(&self.work) } else { self.cache.get(name) };
        if let Some(prof) = shown {
            let end = if mark.is_empty() { inner.max.x } else { inner.max.x - m.x - 10.0 };
            strip(p, Rect::from_min_max(pos2(inner.min.x, inner.max.y - 5.0), pos2(end, inner.max.y)), self.model, prof);
        }
        if resp.has_focus() {
            outline(p, rect.expand(3.0), 2.0, RED);
        }
        resp
    }

    fn status_block(&mut self, ui: &mut Ui) {
        let (dev, dev_ink, detail) = match &self.status {
            None => ("STARTING".to_string(), LABEL, String::new()),
            Some(Err(e)) => ("NO ENGINE".to_string(), AMBER, e.clone()),
            Some(Ok(s)) => match &s.device {
                Device::Connected { path, .. } => ("CONNECTED".to_string(), GREEN, format!("G316 X 98 on {path}")),
                Device::Missing => ("NOT CONNECTED".to_string(), LABEL, "Plug the G316 X 98 in; loginux picks it up.".into()),
                Device::NoAccess { error } => ("NO ACCESS".to_string(), AMBER, error.clone()),
                Device::Failed { error } => ("KEYBOARD ERROR".to_string(), AMBER, error.clone()),
            },
        };
        ui.horizontal(|ui| {
            let (sq, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
            fill(ui.painter(), sq, dev_ink);
            label(ui, &dev, SMALL, if dev_ink == LABEL { TEXT2 } else { TEXT });
        });
        if !detail.is_empty() {
            body(ui, detail, TEXT2, 12.0);
        }
        let Some(Ok(s)) = &self.status else { return };
        let lighting = match &s.control {
            Control::Onboard => "Onboard lighting: loginux is not painting.".to_string(),
            Control::Waiting => "Waiting for the keyboard.".to_string(),
            Control::Profile { name } => format!("Showing {name}."),
            Control::Preview => "Showing your edits live.".to_string(),
            Control::Check { zone } => format!("Channel check: zone {zone} lit."),
            Control::Conflict { mode, flags } => {
                format!("Another program holds the lighting (software control {mode:02x} {flags:02x}).")
            }
        };
        body(ui, lighting, TEXT2, 12.0);
        if s.hosted && *self.service.lock().expect("service lock") != Service::Enabled {
            body(ui, "Lighting stays while this window is open.", LABEL, 12.0);
        }
        if let Control::Conflict { .. } = s.control {
            if button(ui, "TAKE OVER", Look::Primary, true).clicked() {
                self.link.send(Request::TakeOver);
            }
        } else if button(ui, "RELEASE LIGHTING", Look::Outline, s.control != Control::Onboard)
            .on_hover_text("Hand the lighting back to the keyboard's onboard effects and stop applying a profile")
            .clicked()
        {
            self.touched = false;
            self.link.send(Request::Use { profile: None });
            self.link.preview(None);
            self.sent = Some(None);
        }
    }

    // ---- right column: the selection specimen --------------------------------------

    fn right(&mut self, ui: &mut Ui) {
        ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
        if self.tab == Tab::Check {
            return self.check_results(ui);
        }
        if self.tab == Tab::Device {
            return self.device_notes(ui);
        }
        // Room is kept for the palette's header and one row, so it never vanishes on a short window.
        let height = ui.available_height() - 124.0 - 80.0;
        egui::ScrollArea::vertical().max_height(height.max(80.0)).auto_shrink([false, true]).show(ui, |ui| self.specimen(ui));
        ui.add_space(6.0);
        let room = ui.max_rect().max.y - 124.0 - ui.cursor().min.y;
        self.palette(ui, room);

        // Actions, pinned to the bottom.
        let max = ui.max_rect();
        let area = Rect::from_min_max(pos2(max.min.x, max.max.y - 112.0), max.max);
        ui.scope_builder(egui::UiBuilder::new().max_rect(area), |ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            rule(ui);
            self.actions(ui);
        });
    }

    fn specimen(&mut self, ui: &mut Ui) {
        let names = selection_names(self.model, &self.selection);
        label(ui, &format!("SELECTION{}", if names.is_empty() { String::new() } else { format!(": {names}") }), LABEL_PX, LABEL);
        let colours = self.selection_colours();
        let big = match colours.len() {
            0 => "NONE".to_string(),
            1 => profile::hex(colours[0]).to_uppercase(),
            _ => "MIXED".to_string(),
        };
        ui.add_space(2.0);
        label(ui, &big, HEX, if colours.is_empty() { LABEL } else { TEXT });
        ui.add_space(2.0);

        // The selection's colour as a specimen at three sizes.
        let w = ui.available_width();
        for (frac, h) in [(1.0, 72.0), (0.5, 34.0), (0.2, 14.0)].into_iter().filter(|_| !colours.is_empty()) {
            let (r, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
            let r = Rect::from_min_size(r.min, vec2(w * frac, h));
            let step = r.width() / colours.len() as f32;
            for (i, c) in colours.iter().enumerate() {
                let x = r.min.x + step * i as f32;
                fill(ui.painter(), Rect::from_min_max(pos2(x, r.min.y), pos2(x + step, r.max.y)), rgb(*c));
            }
        }
        if self.selection.is_empty() {
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 72.0), Sense::hover());
            dots(ui.painter(), r, DOTS);
            body(ui, "Click a key or strip segment. Shift-click adds to the selection; drag across the board to select several.", TEXT2, 13.0);
        } else {
            self.picker(ui);
        }
    }

    fn picker(&mut self, ui: &mut Ui) {
        let w = ui.available_width();
        let ppp = ui.pixels_per_point();
        let (sv_rect, sv) = ui.allocate_exact_size(vec2(w, 120.0), Sense::click_and_drag());
        let (hue_rect, hue) = ui.allocate_exact_size(vec2(w, 16.0), Sense::click_and_drag());
        let p = ui.painter();
        let sv_rect = snap(sv_rect, ppp);
        let hue_rect = snap(hue_rect, ppp);
        // Saturation across, value down, as a grid of flat-shaded cells.
        let mut mesh = Mesh::default();
        let n = 24;
        for i in 0..n {
            for j in 0..n {
                let (s, v) = ((i as f32 + 0.5) / n as f32, 1.0 - (j as f32 + 0.5) / n as f32);
                let c = from_hsv([self.hsv[0], s, v]);
                let cell = Rect::from_min_max(
                    sv_rect.lerp_inside(vec2(i as f32 / n as f32, j as f32 / n as f32)),
                    sv_rect.lerp_inside(vec2((i + 1) as f32 / n as f32, (j + 1) as f32 / n as f32)),
                );
                mesh.add_colored_rect(snap(cell, ppp), rgb(c));
            }
        }
        let m = 48;
        for i in 0..m {
            let c = from_hsv([i as f32 / m as f32, 1.0, 1.0]);
            let cell = Rect::from_min_max(
                hue_rect.lerp_inside(vec2(i as f32 / m as f32, 0.0)),
                hue_rect.lerp_inside(vec2((i + 1) as f32 / m as f32, 1.0)),
            );
            mesh.add_colored_rect(snap(cell, ppp), rgb(c));
        }
        p.add(Shape::mesh(mesh));
        let mut changed = None;
        let mut from_field = false;
        if let Some(pos) = sv.interact_pointer_pos().filter(|_| sv.dragged() || sv.clicked() || sv.is_pointer_button_down_on()) {
            let t = (pos - sv_rect.min) / sv_rect.size();
            self.hsv[1] = t.x.clamp(0.0, 1.0);
            self.hsv[2] = 1.0 - t.y.clamp(0.0, 1.0);
            changed = Some(from_hsv(self.hsv));
        }
        if let Some(pos) = hue.interact_pointer_pos().filter(|_| hue.dragged() || hue.clicked() || hue.is_pointer_button_down_on()) {
            self.hsv[0] = ((pos.x - hue_rect.min.x) / hue_rect.width()).clamp(0.0, 0.999);
            changed = Some(from_hsv(self.hsv));
        }
        // Square markers, black inside white, so they read on any colour.
        let mark = |at: Pos2| {
            let r = Rect::from_center_size(at, vec2(10.0, 10.0));
            outline(p, r, 2.0, TEXT);
            outline(p, r.shrink(2.0), 1.0, GROUND);
        };
        mark(sv_rect.lerp_inside(vec2(self.hsv[1], 1.0 - self.hsv[2])));
        let hx = hue_rect.lerp_inside(vec2(self.hsv[0], 0.5));
        let hr = Rect::from_center_size(hx, vec2(6.0, hue_rect.height() + 6.0));
        outline(p, hr, 2.0, TEXT);

        ui.horizontal(|ui| {
            label(ui, "HEX", LABEL_PX, LABEL);
            let edit = ui.add(egui::TextEdit::singleline(&mut self.hex_text).desired_width(96.0).hint_text("#rrggbb"));
            if edit.changed()
                && let Ok(c) = profile::parse_rgb(self.hex_text.trim())
            {
                self.hsv = to_hsv(c);
                changed = Some(c);
                from_field = true;
            }
        });
        let current = self.selection_colours();
        let mut c = if let [one] = current[..] { one } else { from_hsv(self.hsv) };
        let before = c;
        ui.horizontal(|ui| {
            for (i, name) in ["R", "G", "B"].iter().enumerate() {
                label(ui, name, LABEL_PX, LABEL);
                ui.add(egui::DragValue::new(&mut c[i]).range(0..=255).speed(0.5));
                ui.add_space(4.0);
            }
        });
        if c != before {
            self.hsv = to_hsv(c);
            changed = Some(c);
        }
        if let Some(c) = changed {
            let typed = std::mem::take(&mut self.hex_text);
            self.paint_selection(c);
            if from_field {
                self.hex_text = typed; // keep the field as typed
            }
        }
        let model = self.model;
        if self.selection.iter().any(|&id| model.zone(id).is_some_and(|z| z.kind == Kind::Unknown))
            && button(ui, "LEAVE THESE ZONES DARK", Look::Quiet, true)
                .on_hover_text("Unknown zones are lit only when a profile names them")
                .clicked()
        {
            for id in self.selection.clone() {
                if model.zone(id).is_some_and(|z| z.kind == Kind::Unknown) {
                    self.work.zones.remove(&id);
                }
            }
            self.touched = true;
        }
    }

    /// Every colour in the profile with its count; a click selects those zones.
    /// Only rows that fit `room` whole are drawn, so nothing is sliced at the fold.
    fn palette(&mut self, ui: &mut Ui, room: f32) {
        const ROW: f32 = 28.0;
        let rows = ((room - 22.0) / ROW).floor().max(1.0) as usize;
        label(ui, "COLOURS IN THIS PROFILE", LABEL_PX, LABEL);
        let counts = colour_counts(self.model, &self.work, &[Kind::Key, Kind::Strip, Kind::Unknown]);
        let slots = rows * 2;
        let shown = if counts.len() > slots { slots - 1 } else { counts.len() };
        let w = (ui.available_width() - 8.0) / 2.0;
        egui::Grid::new("palette").num_columns(2).spacing(vec2(8.0, 6.0)).show(ui, |ui| {
            for (i, (c, n)) in counts.iter().take(shown).enumerate() {
                let (r, resp) = ui.allocate_exact_size(vec2(w, 22.0), Sense::click());
                let text = format!("{}  \u{00d7}{n}", profile::hex(*c).to_uppercase());
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &text));
                let p = ui.painter();
                if resp.hovered() {
                    fill(p, r, RAISED);
                }
                let sw = Rect::from_min_size(r.min + vec2(2.0, 3.0), vec2(16.0, 16.0));
                fill(p, sw, rgb(*c));
                outline(p, sw, 1.0, RULE);
                p.text(pos2(sw.max.x + 8.0, r.center().y), egui::Align2::LEFT_CENTER, &text, FontId::proportional(12.5), TEXT2);
                if resp.clicked() {
                    self.selection = self.model.zones.iter().filter(|z| self.colour(z.id) == Some(*c)).map(|z| z.id).collect();
                    self.sync_picker();
                }
                if i % 2 == 1 {
                    ui.end_row();
                }
            }
            if shown < counts.len() {
                body(ui, format!("+{} more", counts.len() - shown), LABEL, 12.5);
            }
        });
    }

    fn actions(&mut self, ui: &mut Ui) {
        let Some(name) = self.open.clone() else {
            body(ui, "Create a profile to start painting.", TEXT2, 13.0);
            return;
        };
        let dirty = self.dirty();
        let active = self.status().and_then(|s| s.active.clone()).or_else(profile::active);
        let in_use = active.as_deref() == Some(name.as_str());
        ui.horizontal(|ui| {
            if button(ui, "SAVE", Look::Primary, dirty).clicked() {
                self.save();
            }
            if button(ui, "REVERT", Look::Quiet, dirty).clicked() {
                self.work = self.saved.clone();
                self.sync_picker();
            }
        });
        ui.horizontal(|ui| {
            if in_use && !dirty {
                let (r, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                fill(ui.painter(), r, TEXT2);
                label(ui, "IN USE", LABEL_PX, TEXT2);
                ui.add_space(10.0);
            } else if button(ui, if dirty { "SAVE AND USE" } else { "USE THIS PROFILE" }, Look::Outline, true)
                .on_hover_text("loginux keeps the profile in use applied whenever its engine runs")
                .clicked()
                && (!dirty || self.save())
            {
                self.link.send(Request::Use { profile: Some(name.clone()) });
            }
            if button(ui, "DELETE", Look::Quiet, true).clicked() {
                self.confirm = Some(Confirm::Delete(name.clone()));
            }
        });
    }

    // ---- centre: the board -----------------------------------------------------------

    fn centre(&mut self, ui: &mut Ui) {
        if let Some((text, ink)) = self.notice.clone() {
            ui.horizontal_wrapped(|ui| {
                body(ui, text, ink, 13.0);
                if button(ui, "OK", Look::Quiet, true).clicked() {
                    self.notice = None;
                }
            });
            ui.add_space(6.0);
        }
        if let Some(confirm) = &self.confirm {
            let open = self.open.clone().unwrap_or_default();
            let (question, yes, no) = match confirm {
                Confirm::Close => ("Save changes before closing?".to_string(), "SAVE AND CLOSE", "DISCARD AND CLOSE"),
                Confirm::Delete(n) => (format!("Delete the profile {n}? This removes its file."), "DELETE", "KEEP IT"),
                Confirm::Switch(n) => (format!("Save changes to {open} before opening {n}?"), "SAVE AND OPEN", "DISCARD AND OPEN"),
            };
            let mut done = false;
            ui.horizontal_wrapped(|ui| {
                body(ui, question, AMBER, 13.0);
                if button(ui, yes, Look::Primary, true).clicked() {
                    match self.confirm.take() {
                        Some(Confirm::Close) if self.save() => {
                            self.closing = true;
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        Some(Confirm::Delete(n)) => self.delete(&n),
                        Some(Confirm::Switch(n)) if self.save() => {
                            self.open_profile(&n);
                            self.selection.clear();
                        }
                        _ => {}
                    }
                    done = true;
                }
                if !done && button(ui, no, Look::Quiet, true).clicked() {
                    match self.confirm.take() {
                        Some(Confirm::Close) => {
                            self.closing = true;
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        Some(Confirm::Switch(n)) => {
                            self.open_profile(&n);
                            self.selection.clear();
                        }
                        _ => {}
                    }
                }
                if !done && button(ui, "CANCEL", Look::Quiet, true).clicked() {
                    self.confirm = None;
                }
            });
            ui.add_space(6.0);
        }
        match self.tab {
            Tab::Edit => self.edit(ui),
            Tab::Check => self.check(ui),
            Tab::Device => self.device(ui),
        }
    }

    // ---- device settings ------------------------------------------------------------

    fn device(&mut self, ui: &mut Ui) {
        label(ui, "DEVICE", TITLE, TEXT);
        ui.add_space(6.0);
        let Some(Device::Connected { name, path, protocol, .. }) = self.status().map(|s| s.device.clone()) else {
            body(ui, "Connect the keyboard to read its settings.", TEXT2, 14.0);
            return;
        };
        body(ui, format!("{name} on {path} \u{00b7} HID++ {protocol}"), TEXT2, 14.0);
        ui.add_space(14.0);
        let controls = self.status().and_then(|s| s.controls.clone()).unwrap_or_default();
        let change = self.status().and_then(|s| s.rate_change.clone());
        let row = |ui: &mut Ui, name: &str, value: &str, note: &str| {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(170.0, 20.0), Sense::hover());
                px_text(ui.painter(), r.min + vec2(0.0, 4.0), name, LABEL_PX, LABEL);
                let (v, _) = ui.allocate_exact_size(vec2(170.0, 20.0), Sense::hover());
                px_text(ui.painter(), v.min + vec2(0.0, 2.0), value, TAB, TEXT);
                body(ui, note, LABEL, 12.5);
            });
            ui.add_space(10.0);
        };
        let brightness = controls.brightness.map_or("NOT REPORTED".into(), |[now, max]| format!("{now} OF {max}"));
        row(ui, "BRIGHTNESS", &brightness, "Read only");
        let game = controls.game_mode.map_or("NOT REPORTED", |on| if on { "ON" } else { "OFF" });
        row(ui, "GAME MODE", game, "Read only; the keyboard's game-mode key sets it");
        let rate = controls.report_rate.map_or("NOT REPORTED".into(), |hz| format!("{hz} HZ"));
        row(ui, "REPORT RATE", &rate, "Changing it makes the keyboard reconnect");
        row(ui, "VOLUME KNOB", "MEDIA KEYS", "Linux handles volume and mute itself");

        if !controls.report_rates.is_empty() {
            ui.horizontal_wrapped(|ui| {
                label(ui, "SET RATE", LABEL_PX, LABEL);
                ui.add_space(4.0);
                let busy = matches!(change, Some(RateChange::Waiting { .. }));
                for hz in &controls.report_rates {
                    let current = controls.report_rate == Some(*hz);
                    let text = format!("{hz} HZ");
                    if current {
                        let (sq, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                        fill(ui.painter(), sq, TEXT2);
                        label(ui, &text, LABEL_PX, TEXT2);
                        ui.add_space(8.0);
                    } else if button(ui, &text, Look::Quiet, !busy).clicked() {
                        self.rate_confirm = Some(*hz);
                    }
                }
            });
            ui.add_space(10.0);
        }
        if let Some(hz) = self.rate_confirm {
            ui.horizontal_wrapped(|ui| {
                body(
                    ui,
                    format!("Change the report rate to {hz} Hz? The keyboard disconnects and comes back, and Loginux paints its lighting again."),
                    AMBER,
                    13.0,
                );
                if button(ui, "CHANGE RATE", Look::Primary, true).clicked() {
                    self.link.send(Request::SetReportRate { hz });
                    self.rate_confirm = None;
                }
                if button(ui, "CANCEL", Look::Quiet, true).clicked() {
                    self.rate_confirm = None;
                }
            });
            ui.add_space(10.0);
        }
        match change {
            Some(RateChange::Waiting { hz }) => body(ui, format!("Waiting for the keyboard to confirm {hz} Hz\u{2026}"), TEXT2, 13.0),
            Some(RateChange::Done { hz }) => body(ui, format!("Report rate read back as {hz} Hz."), GREEN, 13.0),
            Some(RateChange::Failed { hz, error }) => body(ui, format!("The change to {hz} Hz did not take: {error}"), AMBER, 13.0),
            None => ui.label(""),
        };
    }

    fn device_notes(&mut self, ui: &mut Ui) {
        label(ui, "ABOUT THESE SETTINGS", LABEL_PX, LABEL);
        ui.add_space(4.0);
        for text in [
            "Loginux reads these from the keyboard each time it connects and every ten seconds after.",
            "Brightness and game mode are read only until their commands are verified on this keyboard.",
            "A report-rate change is sent once, only when you confirm it. Loginux then waits for the keyboard to come back and reads the rate back; it never sends a rate on its own.",
            "G HUB was seen changing this keyboard between 1000 and 8000 Hz on Windows. On Linux the change is not verified yet, and whether the keyboard keeps the rate after a replug is unknown.",
        ] {
            body(ui, text, TEXT2, 13.0);
            ui.add_space(6.0);
        }
    }

    fn delete(&mut self, name: &str) {
        if self.status().and_then(|s| s.active.as_deref()) == Some(name) || profile::active().as_deref() == Some(name) {
            self.link.send(Request::Use { profile: None });
        }
        if let Err(e) = profile::delete(name) {
            self.notice = Some((e, AMBER));
        }
        self.reload_list();
        self.open = None;
        self.touched = false;
        self.selection.clear();
        if let Some(first) = self.names.first().cloned() {
            self.open_profile(&first);
            self.touched = false;
        }
    }

    fn edit(&mut self, ui: &mut Ui) {
        let title = self.open.clone().unwrap_or_else(|| "NO PROFILE".into()).to_uppercase();
        label(ui, &title, TITLE, TEXT);
        ui.add_space(4.0);
        let model = self.model;
        let mapped = model.zones.iter().filter(|z| z.kind != Kind::Unknown).count();
        let checked = self.checks.iter().filter(|(id, _)| model.zone(**id).is_some_and(|z| z.kind != Kind::Unknown)).count();
        let live = matches!(self.status().map(|s| &s.control), Some(Control::Preview));
        let strips: String = model
            .strips
            .iter()
            .map(|s| {
                let done = s.zones.iter().all(|id| self.checks.contains_key(id));
                format!(" \u{00b7} {} ORDER {}", s.title.to_uppercase(), if done { "CHECKED" } else { "UNCHECKED" })
            })
            .collect();
        let meta = format!(
            "{}{checked}/{mapped} CHECKED{strips}{}",
            if live { "LIVE \u{00b7} " } else { "" },
            if self.dirty() { " \u{00b7} UNSAVED" } else { "" }
        );
        label(ui, &meta, SMALL, if self.dirty() { TEXT } else { LABEL });
        ui.add_space(6.0);
        proof_key(ui);
        ui.add_space(18.0);
        if self.open.is_none() {
            body(ui, "Create a profile in the left column to start painting.", TEXT2, 14.0);
            return;
        }
        let clicked = self.board(ui, false);
        if let Some((id, add)) = clicked {
            match id {
                Some(id) if add && !self.selection.remove(&id) => {
                    self.selection.insert(id);
                }
                Some(_) if add => {}
                Some(id) => self.selection = BTreeSet::from([id]),
                None if !add => self.selection.clear(),
                None => {}
            }
            self.sync_picker();
        }
        ui.add_space(14.0);
        ui.horizontal_wrapped(|ui| {
            label(ui, "SELECT", LABEL_PX, LABEL);
            ui.add_space(4.0);
            let mut pick: Option<Vec<u8>> = None;
            if button(ui, "ALL KEYS", Look::Quiet, true).clicked() {
                pick = Some(Set::Keys.zones(model));
            }
            for (i, strip) in model.strips.iter().enumerate() {
                if button(ui, &strip.title.to_uppercase(), Look::Quiet, true).clicked() {
                    pick = Some(Set::Strip(i).zones(model));
                }
            }
            if button(ui, "NONE", Look::Quiet, !self.selection.is_empty()).clicked() {
                pick = Some(vec![]);
            }
            if let Some(ids) = pick {
                self.selection = ids.into_iter().collect();
                self.sync_picker();
            }
        });
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            label(ui, "RESEARCH ZONES", LABEL_PX, LABEL);
            ui.add_space(4.0);
            for id in model.unknown() {
                let text = id.to_string();
                let size = px_size(&text, SMALL, ui.pixels_per_point()) + vec2(18.0, 16.0);
                let (r, resp) = ui.allocate_exact_size(vec2(size.x.max(34.0), size.y), Sense::click());
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("zone {id}")));
                let p = ui.painter();
                let lit = self.work.zones.get(&id).copied();
                if let Some(c) = lit {
                    fill(p, r, rgb(c));
                }
                let sel = self.selection.contains(&id);
                outline(p, r, 2.0, if sel { RED } else if resp.hovered() { TEXT2 } else { RULE });
                let t = px_size(&text, SMALL, ui.pixels_per_point());
                px_text(p, r.center() - t / 2.0, &text, SMALL, lit.map_or(TEXT2, ink_on));
                if resp.clicked() {
                    let add = ui.input(|i| i.modifiers.shift || i.modifiers.command);
                    if add {
                        if !self.selection.remove(&id) {
                            self.selection.insert(id);
                        }
                    } else {
                        self.selection = BTreeSet::from([id]);
                    }
                    self.sync_picker();
                }
                resp.on_hover_text(format!("Zone {id}: no known key. Lit only when you set it here."));
            }
        });
        ui.add_space(10.0);
        self.hover_line(ui);
        if ui.input(|i| i.key_pressed(Key::Escape)) && self.confirm.is_none() && self.new_name.is_none() {
            self.selection.clear();
        }
        if ui.input(|i| i.modifiers.command && i.key_pressed(Key::A)) && !ui.ctx().egui_wants_keyboard_input() {
            self.selection = Set::Keys.zones(self.model).into_iter().collect();
            self.sync_picker();
        }
    }

    fn hover_line(&self, ui: &mut Ui) {
        let Some(id) = self.hover else {
            label(ui, "HOVER A KEY FOR ITS ZONE, COLOUR AND PROOF", SMALL, LABEL);
            return;
        };
        let Some(z) = self.model.zone(id) else { return };
        let colour = self.colour(id).map_or("DARK".into(), |c| profile::hex(c).to_uppercase());
        let proof = match self.checks.get(&id) {
            Some(s) if s.saw == z.name => format!("CHECKED {}", s.date),
            Some(s) => format!("CHECKED {}: LIT {}", s.date, s.saw.to_uppercase()),
            None => "LAYOUT DATA ONLY".into(),
        };
        let name = if z.name.is_empty() { format!("ZONE {id}") } else { z.name.to_uppercase() };
        label(ui, &format!("{name} \u{00b7} ZONE {id} \u{00b7} {colour} \u{00b7} {proof}"), SMALL, TEXT2);
    }

    /// Draws the board; returns a click as (zone or empty space, add-to-selection).
    fn board(&mut self, ui: &mut Ui, checking: bool) -> Option<(Option<u8>, bool)> {
        let ppp = ui.pixels_per_point();
        let pad = 10.0;
        let width = ui.available_width().min(1200.0);
        let board = self.model.board;
        let unit = ((width - 2.0 * pad) / board[0]).min(60.0);
        let size = vec2(board[0] * unit + 2.0 * pad, board[1] * unit + 2.0 * pad);
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Group, true, "keyboard map"));
        let origin = rect.min + vec2(pad, pad);
        let gap = line((unit * 0.1).max(2.0), ppp);
        let key_rect = |z: &Zone| -> Rect {
            let [x, y, w, h] = z.rect;
            let r = Rect::from_min_size(origin + vec2(x * unit, y * unit), vec2(w * unit, h * unit));
            snap(if z.kind == Kind::Strip { r.shrink2(vec2(line(1.0, ppp) / 2.0, 0.0)) } else { r.shrink(gap / 2.0) }, ppp)
        };
        let p = ui.painter();
        fill(p, rect, BODY);
        outline(p, rect.expand(8.0), 2.0, RULE);

        let pointer = resp.hover_pos();
        self.hover = None;
        let shift = ui.input(|i| i.modifiers.shift || i.modifiers.command);
        let marquee = self.anchor.zip(resp.interact_pointer_pos()).map(|(a, b)| Rect::from_two_pos(a, b));
        let mut hit = None;
        let model = self.model;
        for z in model.zones.iter().filter(|z| z.kind != Kind::Unknown) {
            let r = key_rect(z);
            let hit_area = if z.kind == Kind::Strip { r.expand2(vec2(0.0, 5.0)) } else { r };
            if pointer.is_some_and(|q| hit_area.contains(q)) {
                self.hover = Some(z.id);
                hit = Some(z.id);
            }
            let colour = if checking { None } else { self.colour(z.id) };
            let face = colour.map_or(DARK_KEY, rgb);
            fill(p, r, face);
            if z.kind == Kind::Key {
                let ink = colour.map_or(TEXT2, ink_on);
                // The label, else the definition's short legend, else nothing: never a cut word.
                let room = r.width() - 8.0;
                let legend = [Some(z.label.as_str()), z.short.as_deref()]
                    .into_iter()
                    .flatten()
                    .find(|l| px_size(l, LEGEND, ppp).x <= room);
                if let Some(legend) = legend {
                    px_text(p, r.min + vec2(4.0, 4.0), legend, LEGEND, ink);
                }
            }
            if let Some(seen) = self.checks.get(&z.id) {
                let ok = seen.saw == z.name;
                let m = whole(3.0 * ppp) / ppp;
                let at = if z.kind == Kind::Strip { pos2(r.center().x - m / 2.0, r.max.y + 3.0) } else { pos2(r.max.x - m - 3.0, r.max.y - m - 3.0) };
                fill(p, Rect::from_min_size(at, vec2(m, m)), if ok { GREEN } else { AMBER });
            }
            let selected = !checking && self.selection.contains(&z.id);
            let in_marquee = marquee.is_some_and(|m| m.intersects(r));
            if selected || in_marquee {
                outline(p, r.expand(line(2.0, ppp)), 2.0, RED);
            } else if self.hover == Some(z.id) {
                outline(p, r, 1.0, TEXT);
            }
        }
        if let Some(m) = marquee {
            outline(p, m, 1.0, RED);
        }

        if !checking && resp.drag_started() && hit.is_none() {
            self.anchor = resp.interact_pointer_pos();
        }
        if resp.drag_stopped()
            && let Some(m) = marquee
        {
            self.anchor = None;
            let inside: BTreeSet<u8> = model
                .zones
                .iter()
                .filter(|z| z.kind != Kind::Unknown && m.intersects(key_rect(z)))
                .map(|z| z.id)
                .collect();
            if shift {
                self.selection.extend(inside);
            } else {
                self.selection = inside;
            }
            self.sync_picker();
            return None;
        }
        if resp.drag_stopped() {
            self.anchor = None;
        }
        resp.clicked().then_some((hit, shift))
    }

    // ---- channel check ----------------------------------------------------------------

    fn check(&mut self, ui: &mut Ui) {
        label(ui, "CHANNEL CHECK", TITLE, TEXT);
        ui.add_space(6.0);
        let connected = self.connected_zones().is_some();
        let model = self.model;
        let Some(set) = self.set else {
            body(
                ui,
                "loginux lights one zone at a time, white, with everything else dark. Click where the light \
                 appears, or answer that nothing lit or that it lit somewhere not on the map. Answers are saved \
                 as evidence from this keyboard; layout data is not proof of which LED lights.",
                TEXT2,
                14.0,
            );
            ui.add_space(12.0);
            for set in Set::all(model) {
                let zones = set.zones(model);
                let done = zones.iter().filter(|id| self.checks.contains_key(id)).count();
                ui.horizontal(|ui| {
                    let b = button(ui, &format!("CHECK {}", set.title(model)), Look::Outline, connected);
                    if b.hovered() || b.has_focus() {
                        self.focus = set;
                    }
                    if b.clicked() {
                        self.set = Some(set);
                        self.step = zones.iter().position(|id| !self.checks.contains_key(id)).unwrap_or(0);
                    }
                    label(ui, &format!("{done} OF {} ANSWERED", zones.len()), SMALL, if done == zones.len() { GREEN } else { TEXT2 });
                });
            }
            if !connected {
                ui.add_space(4.0);
                body(ui, "Connect the keyboard to run a check.", AMBER, 13.0);
            }
            ui.add_space(14.0);
            proof_key(ui);
            ui.add_space(8.0);
            self.board(ui, true);
            ui.add_space(10.0);
            self.hover_line(ui);
            return;
        };
        let zones = set.zones(model);
        let Some(&id) = zones.get(self.step) else {
            self.set = None;
            return;
        };
        ui.horizontal(|ui| {
            label(ui, &format!("ZONE {id}"), HEX, TEXT);
            ui.add_space(12.0);
            label(ui, &format!("{} OF {} \u{00b7} {}", self.step + 1, zones.len(), set.title(model)), SMALL, TEXT2);
        });
        ui.add_space(4.0);
        body(ui, "One LED on the keyboard is lit white. Click the key or strip segment where it is.", TEXT2, 14.0);
        ui.add_space(12.0);
        let mut answer: Option<String> = None;
        if let Some((Some(hit), _)) = self.board(ui, true) {
            answer = model.zone(hit).map(|z| z.name.clone());
        }
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if button(ui, "NOTHING LIT", Look::Outline, connected).clicked() {
                answer = Some("nothing".into());
            }
            if button(ui, "NOT ON THE MAP", Look::Outline, connected).clicked() {
                answer = Some("elsewhere".into());
            }
            if button(ui, "SKIP", Look::Quiet, true).clicked() {
                self.step += 1;
            }
            if button(ui, "BACK", Look::Quiet, self.step > 0).clicked() {
                self.step -= 1;
            }
            if button(ui, "STOP", Look::Quiet, true).clicked() {
                self.set = None;
            }
        });
        ui.add_space(10.0);
        self.hover_line(ui);
        if let Some(saw) = answer.filter(|_| connected) {
            self.checks.insert(id, Seen { saw, date: profile::today() });
            if let Err(e) = profile::save_checks(model, &self.checks) {
                self.notice = Some((e, AMBER));
            }
            self.step += 1;
        }
    }

    fn check_results(&mut self, ui: &mut Ui) {
        let model = self.model;
        let set = self.set.unwrap_or(self.focus);
        label(ui, &format!("ANSWERS: {}", set.title(model)), LABEL_PX, LABEL);
        ui.add_space(4.0);
        let zones = set.zones(model);
        egui::ScrollArea::vertical().max_height(ui.available_height() - 60.0).auto_shrink([false, true]).show(ui, |ui| {
            for id in &zones {
                let Some(z) = model.zone(*id) else { continue };
                let (text, ink) = match self.checks.get(id) {
                    None => ("not answered".to_string(), LABEL),
                    Some(s) if !z.name.is_empty() && s.saw == z.name => ("as expected".to_string(), GREEN),
                    Some(s) if z.name.is_empty() => (format!("saw {}", s.saw), TEXT),
                    Some(s) => (format!("lit {}", s.saw), AMBER),
                };
                let name = if z.name.is_empty() { format!("ZONE {id}") } else { format!("{} {id}", z.name.to_uppercase()) };
                ui.horizontal(|ui| {
                    label(ui, &name, SMALL, TEXT2);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| body(ui, text, ink, 12.5));
                });
            }
        });
        let max = ui.max_rect();
        let area = Rect::from_min_max(pos2(max.min.x, max.max.y - 52.0), max.max);
        ui.scope_builder(egui::UiBuilder::new().max_rect(area), |ui| {
            rule(ui);
            if button(ui, "CLEAR THESE ANSWERS", Look::Quiet, zones.iter().any(|id| self.checks.contains_key(id))).clicked() {
                for id in &zones {
                    self.checks.remove(id);
                }
                if let Err(e) = profile::save_checks(model, &self.checks) {
                    self.notice = Some((e, AMBER));
                }
            }
        });
    }
}

/// Development aid (`--features screenshot`): LOGINUX_SHOT=out.ppm captures the window
/// after LOGINUX_SHOT_AFTER frames (default 30) and closes it the normal way.
/// LOGINUX_SHOT_SELECT="23,1,19,4" preselects zones and LOGINUX_SHOT_PAINT=#rrggbb paints
/// them; LOGINUX_SHOT_CONFIRM shows the unsaved-close question; LOGINUX_SHOT_SERVICE=off|on
/// stands in for systemd's answer; LOGINUX_SHOT_TAB=device opens the DEVICE tab
/// (LOGINUX_SHOT_CONFIRM_RATE=8000 shows the rate confirmation); LOGINUX_SHOT_TAB=check
/// opens the channel check, and LOGINUX_SHOT_SET=strip starts a run on the first strip.
#[cfg(feature = "screenshot")]
fn shot(app: &mut App, ctx: &egui::Context) {
    use std::sync::atomic::AtomicBool;
    static DONE: AtomicBool = AtomicBool::new(false);
    let Ok(path) = std::env::var("LOGINUX_SHOT") else { return };
    let frame = ctx.cumulative_frame_nr();
    if frame == 1 {
        if let Ok(ids) = std::env::var("LOGINUX_SHOT_SELECT") {
            app.selection = ids.split(',').filter_map(|i| i.trim().parse().ok()).collect();
            app.sync_picker();
        }
        if let Some(c) = std::env::var("LOGINUX_SHOT_PAINT").ok().and_then(|c| profile::parse_rgb(&c).ok()) {
            app.paint_selection(c);
        }
        if let Ok(state) = std::env::var("LOGINUX_SHOT_SERVICE") {
            *app.service.lock().expect("service lock") = if state == "on" { Service::Enabled } else { Service::Disabled };
        }
        if std::env::var("LOGINUX_SHOT_CONFIRM").is_ok() {
            app.confirm = Some(Confirm::Close);
        }
        if std::env::var("LOGINUX_SHOT_TAB").is_ok_and(|t| t == "device") {
            app.tab = Tab::Device;
            app.rate_confirm = std::env::var("LOGINUX_SHOT_CONFIRM_RATE").ok().and_then(|r| r.parse().ok());
        }
        if std::env::var("LOGINUX_SHOT_TAB").is_ok_and(|t| t == "check") {
            app.tab = Tab::Check;
            if std::env::var("LOGINUX_SHOT_SET").is_ok_and(|t| t == "strip") {
                app.set = Some(Set::Strip(0));
                app.step = 2;
            }
        }
    }
    let after: u64 = std::env::var("LOGINUX_SHOT_AFTER").ok().and_then(|n| n.parse().ok()).unwrap_or(30);
    if frame == after {
        eprintln!("shot: {} points per pixel", ctx.pixels_per_point());
        ctx.request_screenshot(move |img| {
            let mut out = format!("P6\n{} {}\n255\n", img.size[0], img.size[1]).into_bytes();
            out.extend(img.pixels.iter().flat_map(|c| [c.r(), c.g(), c.b()]));
            let _ = std::fs::write(&path, out);
            DONE.store(true, Ordering::SeqCst);
        });
    }
    if DONE.load(Ordering::SeqCst) {
        app.closing = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
    ctx.request_repaint();
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        #[cfg(feature = "screenshot")]
        shot(self, &ctx);
        self.upkeep(&ctx);
        let frame = |fill: Color32, m: i8| egui::Frame::NONE.fill(fill).inner_margin(egui::Margin::same(m));
        egui::Panel::top("header").exact_size(64.0).frame(frame(GROUND, 0)).show(ui, |ui| self.header(ui));
        egui::Panel::left("profiles")
            .exact_size(236.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(frame(GROUND, 16))
            .show(ui, |ui| self.left(ui));
        egui::Panel::right("specimen")
            .exact_size(316.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(frame(PANEL, 16))
            .show(ui, |ui| self.right(ui));
        egui::CentralPanel::default().frame(frame(GROUND, 24)).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| self.centre(ui));
        });
        // Column rules, drawn last so they sit on top of the panel edges.
        let screen = ctx.content_rect();
        let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("loginux-column-rules")));
        fill(&p, Rect::from_min_max(pos2(screen.min.x + 236.0, screen.min.y + 64.0), pos2(screen.min.x + 238.0, screen.max.y)), RULE);
        fill(&p, Rect::from_min_max(pos2(screen.max.x - 318.0, screen.min.y + 64.0), pos2(screen.max.x - 316.0, screen.max.y)), RULE);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.link.quit();
        if self.hosted.is_none() {
            // A service engine keeps running: drop what this window asked it to show.
            let _ = ipc::request(&Request::Preview { profile: None });
            let _ = ipc::request(&Request::Check { zone: None });
        }
        self.hosted = None; // stops the in-process engine, which hands the lighting back
    }
}

/// The key to the board's proof marks.
fn proof_key(ui: &mut Ui) {
    ui.horizontal_wrapped(|ui| {
        for (colour, text) in [(GREEN, "OBSERVED"), (AMBER, "DIFFERS")] {
            let (sq, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
            fill(ui.painter(), sq, colour);
            label(ui, text, SMALL, TEXT2);
            ui.add_space(6.0);
        }
        label(ui, "UNMARKED: LAYOUT DATA ONLY", SMALL, LABEL);
    });
}

/// Names for the selection in board reading order, shortened when long.
fn selection_names(model: &Model, sel: &BTreeSet<u8>) -> String {
    let mut zones: Vec<&Zone> = sel.iter().filter_map(|id| model.zone(*id)).collect();
    zones.sort_by(|a, b| {
        (a.kind == Kind::Unknown, a.rect[1], a.rect[0])
            .partial_cmp(&(b.kind == Kind::Unknown, b.rect[1], b.rect[0]))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let names: Vec<String> = zones
        .into_iter()
        .map(|z| match z.kind {
            Kind::Unknown => z.id.to_string(),
            Kind::Strip => z.name.to_uppercase(),
            Kind::Key => z.label.clone(),
        })
        .collect();
    match names.len() {
        0 => String::new(),
        1..=5 => names.join(" "),
        n => format!("{n} ZONES"),
    }
}

/// Every key and strip colour in proportion, as the profile's thumbnail.
fn strip(p: &Painter, r: Rect, model: &Model, prof: &Profile) {
    let counts = colour_counts(model, prof, &[Kind::Key, Kind::Strip, Kind::Unknown]);
    let total: usize = counts.iter().map(|c| c.1).sum::<usize>().max(1);
    let mut x = r.min.x;
    for (c, n) in counts {
        let w = r.width() * n as f32 / total as f32;
        fill(p, Rect::from_min_max(pos2(x, r.min.y), pos2(x + w, r.max.y)), rgb(c));
        x += w;
    }
}

/// Each group's most common colour becomes its base; overrides that match their
/// base are dropped, so saved files stay short.
fn normalize(model: &Model, p: &mut Profile) {
    let groups: Vec<String> = model.groups().into_iter().map(String::from).collect();
    for group in groups {
        let ids: Vec<u8> = model.zones.iter().filter(|z| z.group == group).map(|z| z.id).collect();
        let colours: Vec<(u8, Rgb)> = ids.iter().map(|&id| (id, p.colour(model, id).unwrap_or_default())).collect();
        let mut counts: Vec<(Rgb, usize)> = vec![];
        for (_, c) in &colours {
            match counts.iter_mut().find(|(k, _)| k == c) {
                Some(e) => e.1 += 1,
                None => counts.push((*c, 1)),
            }
        }
        let base = counts.iter().max_by_key(|c| c.1).map_or([0; 3], |c| c.0);
        p.bases.insert(group, base);
        for (id, c) in colours {
            if c == base {
                p.zones.remove(&id);
            } else {
                p.zones.insert(id, c);
            }
        }
    }
}

/// Distinct colours of `kinds` zones with their counts, most common first.
fn colour_counts(model: &Model, prof: &Profile, kinds: &[Kind]) -> Vec<(Rgb, usize)> {
    let mut counts: Vec<(Rgb, usize)> = vec![];
    for z in model.zones.iter().filter(|z| kinds.contains(&z.kind)) {
        if let Some(c) = prof.colour(model, z.id) {
            match counts.iter_mut().find(|(k, _)| *k == c) {
                Some(e) => e.1 += 1,
                None => counts.push((c, 1)),
            }
        }
    }
    counts.sort_by_key(|c| std::cmp::Reverse(c.1));
    counts
}

fn to_hsv([r, g, b]: Rgb) -> [f32; 3] {
    let (r, g, b) = (f32::from(r) / 255.0, f32::from(g) / 255.0, f32::from(b) / 255.0);
    let max = r.max(g).max(b);
    let d = max - r.min(g).min(b);
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        ((g - b) / d).rem_euclid(6.0) / 6.0
    } else if max == g {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    [h, if max == 0.0 { 0.0 } else { d / max }, max]
}

fn from_hsv([h, s, v]: [f32; 3]) -> Rgb {
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - f * s), v * (1.0 - (1.0 - f) * s));
    let (r, g, b) = match i as i32 % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    [r, g, b].map(|c| (c * 255.0).round() as u8)
}

fn style(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::empty();
    let face = |w: f32| {
        Arc::new(egui::FontData::from_static(ARCHIVO).tweak(egui::FontTweak {
            coords: egui::epaint::text::VariationCoords::new([(b"wght", w)]),
            ..Default::default()
        }))
    };
    fonts.font_data.insert("archivo".into(), face(400.0));
    fonts.font_data.insert("archivo-semibold".into(), face(600.0));
    fonts.families.insert(FontFamily::Proportional, vec!["archivo".into()]);
    fonts.families.insert(FontFamily::Monospace, vec!["archivo".into()]);
    fonts.families.insert(FontFamily::Name("semibold".into()), vec!["archivo-semibold".into()]);
    ctx.set_fonts(fonts);
    ctx.set_theme(egui::Theme::Dark);
    ctx.tessellation_options_mut(|o| o.feathering = false);
    ctx.style_mut_of(egui::Theme::Dark, |s| {
        s.text_styles = [
            (egui::TextStyle::Small, FontId::proportional(12.0)),
            (egui::TextStyle::Body, FontId::proportional(14.0)),
            (egui::TextStyle::Button, FontId::proportional(14.0)),
            (egui::TextStyle::Heading, FontId::new(18.0, FontFamily::Name("semibold".into()))),
            (egui::TextStyle::Monospace, FontId::proportional(14.0)),
        ]
        .into();
        s.spacing.item_spacing = vec2(8.0, 8.0);
        s.spacing.button_padding = vec2(10.0, 6.0);
        s.spacing.interact_size.y = 26.0;
        s.spacing.scroll = egui::style::ScrollStyle { bar_width: 8.0, ..egui::style::ScrollStyle::solid() };
        s.spacing.scroll.fade.strength = 0.0;
        let v = &mut s.visuals;
        v.panel_fill = GROUND;
        v.window_fill = PANEL;
        v.window_stroke = Stroke::new(2.0, RULE);
        v.window_corner_radius = 0.into();
        v.menu_corner_radius = 0.into();
        v.extreme_bg_color = RULE; // scroll-bar track
        v.text_edit_bg_color = Some(BODY);
        v.faint_bg_color = PANEL;
        v.override_text_color = None;
        v.selection.bg_fill = RED.gamma_multiply(0.45);
        v.selection.stroke = Stroke::new(1.0, TEXT);
        v.text_cursor.stroke = Stroke::new(2.0, RED);
        v.hyperlink_color = TEXT;
        v.warn_fg_color = AMBER;
        v.error_fg_color = AMBER;
        // bg_fill is the scroll-bar handle; weak_bg_fill the field behind values.
        for (w, handle, bg, edge, ink) in [
            (&mut v.widgets.noninteractive, TEXT2, PANEL, RULE, TEXT2),
            (&mut v.widgets.inactive, TEXT2, BODY, RULE, TEXT),
            (&mut v.widgets.hovered, TEXT, RAISED, TEXT2, TEXT),
            (&mut v.widgets.active, TEXT, RULE, TEXT, TEXT),
            (&mut v.widgets.open, TEXT, RAISED, TEXT2, TEXT),
        ] {
            w.bg_fill = handle;
            w.weak_bg_fill = bg;
            w.bg_stroke = Stroke::new(2.0, edge);
            w.fg_stroke = Stroke::new(1.0, ink);
            w.corner_radius = 0.into();
            w.expansion = 0.0;
        }
    });
}

/// The mark at 128 px. X11 shows it; on Wayland the desktop file's icon is used.
fn window_icon() -> egui::IconData {
    let (side, rgba) = crate::icon::rgba(8);
    egui::IconData { rgba, width: side as u32, height: side as u32 }
}

pub fn run(device: Option<PathBuf>) -> Result<(), String> {
    engine::install_signal_handlers();
    let hosted = match ipc::request(&Request::Status) {
        Ok(_) => None,
        Err(e) => {
            eprintln!("loginux: {e}; starting an engine in this window");
            Hosted::start(engine::Options { device: device.clone(), refresh: None })?
        }
    };
    // LOGINUX_UI_ZOOM scales the whole interface on top of the display's own scale.
    let zoom: Option<f32> = std::env::var("LOGINUX_UI_ZOOM").ok().and_then(|z| z.parse().ok()).filter(|z: &f32| (0.5..=4.0).contains(z));
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!("Loginux \u{00b7} {}", layout::current().name))
            .with_app_id("loginux")
            .with_icon(window_icon())
            .with_inner_size(Vec2::new(1360.0, 840.0) * zoom.unwrap_or(1.0))
            .with_min_inner_size([1120.0, 700.0]),
        ..Default::default()
    };
    eframe::run_native(
        "loginux",
        options,
        Box::new(move |cc| {
            style(&cc.egui_ctx);
            if let Some(z) = zoom {
                cc.egui_ctx.set_zoom_factor(z);
            }
            Ok(Box::new(App::new(&cc.egui_ctx, device, hosted)))
        }),
    )
    .map_err(|e| e.to_string())
}
