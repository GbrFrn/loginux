//! The editor window (`loginux ui`).
//!
//! It never opens the keyboard. It talks to the engine over the local socket and,
//! when no engine is running, starts one inside this process; that engine hands
//! the lighting back when the window closes. Bitmap lettering is drawn here cell
//! by cell, each cell snapped to whole device pixels, so it stays sharp at any
//! display scale. Antialiasing (feathering) is off for the same reason: every
//! shape is a pixel-aligned rectangle.

use crate::effects::{self as fx, Effect};
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
use std::sync::{Arc, Condvar, Mutex, OnceLock};
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

/// The login switch, right-aligned on `at.x` and centred on `at.y`: a fixed RUN AT
/// LOGIN label and a square switch whose 8pt knob shows whether loginux.service
/// starts at login. While systemctl works, the knob shows the side asked for and
/// the switch takes no clicks. Returns the response and the drawn group.
fn login_switch(ui: &mut Ui, at: Pos2, service: Service) -> (Response, Rect) {
    let ppp = ui.pixels_per_point();
    let installed = service != Service::NotInstalled;
    let text = if installed { "RUN AT LOGIN" } else { "SERVICE NOT INSTALLED" };
    let (on, enabled) = match service {
        Service::Enabled => (Some(true), true),
        Service::Disabled => (Some(false), true),
        Service::Changing(to) => (Some(to), false),
        Service::Unknown | Service::NotInstalled => (None, false),
    };
    let t = px_size(text, LABEL_PX, ppp);
    let track = vec2(32.0, 16.0);
    let width = t.x + if installed { 10.0 + track.x } else { 0.0 };
    let group = Rect::from_min_max(pos2(at.x - width, at.y - track.y / 2.0), pos2(at.x, at.y + track.y / 2.0));
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let resp = ui.interact(group.expand(8.0), ui.scope_id().with("login switch"), sense);
    resp.widget_info(|| match on {
        Some(on) => egui::WidgetInfo::selected(egui::Role::Switch, enabled, on, text),
        None => egui::WidgetInfo::labeled(egui::Role::Switch, enabled, text),
    });
    let hot = enabled && resp.hovered();
    // On: a red track, the knob in Ground at the right. Off: a grey edged track, the
    // knob at the left. Waiting or unknown: Disabled grey.
    let (ink, bg, edge, knob) = match service {
        Service::Enabled => (TEXT, if hot { RED_HOT } else { RED }, Color32::TRANSPARENT, GROUND),
        Service::Disabled if hot => (TEXT, RAISED, TEXT, TEXT),
        Service::Disabled => (TEXT2, Color32::TRANSPARENT, TEXT2, TEXT2),
        _ => (DISABLED, Color32::TRANSPARENT, RULE, DISABLED),
    };
    let p = ui.painter();
    px_text(p, pos2(group.min.x, at.y - t.y / 2.0), text, LABEL_PX, ink);
    if installed {
        let r = Rect::from_min_size(pos2(at.x - track.x, group.min.y), track);
        fill(p, r, bg);
        outline(p, r, 2.0, edge);
        let x = match on {
            Some(true) => r.max.x - 12.0,
            Some(false) => r.min.x + 4.0,
            None => r.center().x - 4.0,
        };
        fill(p, Rect::from_min_size(pos2(x, r.min.y + 4.0), vec2(8.0, 8.0)), knob);
    }
    if resp.has_focus() {
        outline(p, group.expand(3.0), 2.0, RED);
    }
    (resp, group)
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

const POLL: Duration = if cfg!(test) { Duration::from_millis(100) } else { Duration::from_secs(1) };

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
                        out = wake.wait_timeout(out, POLL).expect("link lock").0;
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
    /// systemctl is enabling (true) or disabling the service; read again when it returns.
    Changing(bool),
}

static SYSTEMCTL: OnceLock<PathBuf> = OnceLock::new();

/// `systemctl --user`. Tests get a stand-in script instead (`use_systemctl`), so
/// no test asks, starts or stops the real service.
fn systemctl() -> Command {
    #[cfg(test)]
    crate::testenv::init();
    let mut cmd = Command::new(SYSTEMCTL.get_or_init(|| PathBuf::from("systemctl")));
    cmd.arg("--user");
    cmd
}

#[cfg(test)]
pub fn use_systemctl(program: PathBuf) {
    SYSTEMCTL.set(program).expect("the stand-in is chosen before any call");
}

fn service_state() -> Service {
    let out = systemctl().args(["is-enabled", "loginux.service"]).output();
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
    /// When this window asked systemd to start the service; no engine is hosted
    /// here while the service may still be starting.
    service_start: Option<Instant>,
    status: Result<Status, String>,
    tab: Tab,
    names: Vec<String>,
    cache: BTreeMap<String, Profile>,
    /// Names in `names` that are built-in presets with no file of their own.
    built_in: BTreeSet<String>,
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
    fn new(ctx: &egui::Context, model: &'static Model, device: Option<PathBuf>, hosted: Option<Hosted>) -> App {
        let link = Link::start(ctx.clone());
        let service = Arc::new(Mutex::new(Service::Unknown));
        let mut app = App {
            model,
            link,
            hosted,
            device,
            host_tried: None,
            service_start: None,
            status: ipc::request(&Request::Status).map_err(String::from),
            tab: Tab::Edit,
            names: vec![],
            cache: BTreeMap::new(),
            built_in: BTreeSet::new(),
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
        if on {
            // An engine in this window holds the lock the service needs: hand over.
            self.hosted = None;
            self.service_start = Some(Instant::now());
        }
        // The switch moves at once and takes no clicks until systemctl returns.
        *self.service.lock().expect("service lock") = Service::Changing(on);
        self.systemctl(&[if on { "enable" } else { "disable" }, "--now"]);
    }

    /// Runs `systemctl --user ARGS loginux.service` off the UI thread; a failure
    /// shows as a notice, and the service state is read again afterwards.
    fn systemctl(&self, args: &[&'static str]) {
        let (slot, link, args) = (self.service.clone(), self.link.clone(), args.to_vec());
        thread::spawn(move || {
            let out = systemctl().args(&args).arg("loginux.service").output();
            if let Ok(o) = &out
                && !o.status.success()
            {
                let mut b = link.0.0.lock().expect("link lock");
                b.errors.push(format!("systemctl --user {}: {}", args.join(" "), String::from_utf8_lossy(&o.stderr).trim()));
            }
            *slot.lock().expect("service lock") = service_state();
        });
    }

    fn reload_list(&mut self) {
        self.names = profile::list();
        self.cache = self.names.iter().filter_map(|n| Some((n.clone(), profile::load(self.model, n).ok()?))).collect();
        self.built_in = self.names.iter().filter(|n| profile::built_in(n)).cloned().collect();
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
        self.status.as_ref().ok()
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

    /// The colours the picker edits: the selection's, or the running effect's.
    fn selection_colours(&self) -> Vec<Rgb> {
        if let Some(e) = &self.work.effect {
            return vec![e.color];
        }
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
        if let Some(e) = &mut self.work.effect {
            e.color = c;
        } else {
            for &id in &self.selection {
                self.work.zones.insert(id, c);
            }
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
            self.status = s;
        }
        if let Some(e) = errors.into_iter().last() {
            self.notice = Some((e, AMBER));
        }
        let engine_down = self.status.is_err();
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
        let service_starting = self.service_start.is_some_and(|t| t.elapsed() < Duration::from_secs(6));
        if engine_down
            && self.hosted.is_none()
            && !service_starting
            && self.host_tried.is_none_or(|t| t.elapsed() > Duration::from_secs(2))
        {
            let service = *self.service.lock().expect("service lock");
            // An enabled service that is not running (quit from the tray, say) is
            // started once, which brings the tray back; only a disabled or missing
            // service, or one that failed to start, leaves the engine to this window.
            if matches!(service, Service::Unknown | Service::Changing(_)) {
                return; // systemd has not answered yet
            }
            self.host_tried = Some(Instant::now());
            if service == Service::Enabled && self.service_start.is_none() {
                self.service_start = Some(Instant::now());
                self.systemctl(&["start"]);
                return;
            }
            if let Err(e) = &self.status {
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
        let (resp, group) = login_switch(ui, pos2(r.max.x - 22.0, r.center().y), service);
        let resp = resp.on_hover_text(match service {
            Service::NotInstalled => {
                "Install systemd/loginux.service into ~/.config/systemd/user/ (see README) to keep lighting after this window closes."
            }
            Service::Enabled => {
                "On: loginux.service starts at every login and keeps the lighting applied. Click to turn it off; the lighting then stays only while this window is open."
            }
            Service::Disabled => {
                "Off: the lighting stays only while this window is open. Click to start loginux.service now and at every login."
            }
            Service::Changing(_) => "Waiting for systemd\u{2026}",
            Service::Unknown => "Asking systemd whether loginux.service starts at login\u{2026}",
        });
        if resp.clicked() {
            self.set_service(service != Service::Enabled);
        }
        let patch = Rect::from_min_max(pos2(group.min.x - 214.0, r.center().y - 15.0), pos2(group.min.x - 22.0, r.center().y + 15.0));
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
                let mark = if active.as_deref() == Some(name.as_str()) {
                    "IN USE"
                } else if self.built_in.contains(name) {
                    "BUILT IN"
                } else {
                    ""
                };
                if self.profile_box(ui, name, is_open, mark).clicked() && !is_open {
                    if self.dirty() {
                        self.confirm = Some(Confirm::Switch(name.clone()));
                    } else {
                        self.open_profile(name);
                        self.selection.clear();
                    }
                }
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
            Err(e) => ("NO ENGINE".to_string(), AMBER, e.clone()),
            Ok(s) => match &s.device {
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
        let Ok(s) = &self.status else { return };
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
        if self.open.is_some() && self.work.effect.is_some() {
            let height = ui.available_height() - 124.0;
            egui::ScrollArea::vertical().max_height(height.max(80.0)).auto_shrink([false, true]).show(ui, |ui| self.effect_panel(ui));
        } else {
            // Room is kept for the palette's header and one row, so it never vanishes on a short window.
            let height = ui.available_height() - 124.0 - 80.0;
            egui::ScrollArea::vertical().max_height(height.max(80.0)).auto_shrink([false, true]).show(ui, |ui| self.specimen(ui));
            ui.add_space(6.0);
            let room = ui.max_rect().max.y - 124.0 - ui.cursor().min.y;
            self.palette(ui, room);
        }

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

    /// The right column while the device runs an effect: what it does and its settings.
    fn effect_panel(&mut self, ui: &mut Ui) {
        let Some(e) = self.work.effect.clone() else { return };
        label(ui, "EFFECT", LABEL_PX, LABEL);
        ui.add_space(2.0);
        label(ui, &e.kind.name().to_uppercase(), HEX, TEXT);
        ui.add_space(4.0);
        body(ui, effect_note(e.kind), TEXT2, 13.0);
        ui.add_space(12.0);
        if e.kind.coloured() {
            label(ui, "COLOUR", LABEL_PX, LABEL);
            ui.add_space(2.0);
            self.picker(ui);
            ui.add_space(12.0);
        }
        if matches!(e.kind, fx::Kind::Breathe | fx::Kind::Cycle | fx::Kind::Wave) {
            let mut secs = f32::from(e.period) / 1000.0;
            ui.horizontal(|ui| {
                label(ui, "SECONDS PER CYCLE", LABEL_PX, LABEL);
                ui.add(egui::DragValue::new(&mut secs).range(1.0..=20.0).speed(0.05).max_decimals(1));
            });
            let period = (secs * 1000.0).round() as u16;
            if period != e.period
                && let Some(w) = &mut self.work.effect
            {
                w.period = period;
                self.touched = true;
            }
            ui.add_space(12.0);
        }
        if e.kind == fx::Kind::Wave {
            label(ui, "DIRECTION", LABEL_PX, LABEL);
            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| {
                for (name, n) in fx::DIRECTIONS {
                    let text = name.replace('-', " ").to_uppercase();
                    if e.direction == n {
                        current_mark(ui, &text);
                    } else if button(ui, &text, Look::Quiet, true).clicked()
                        && let Some(w) = &mut self.work.effect
                    {
                        w.direction = n;
                        self.touched = true;
                    }
                }
            });
        }
    }

    /// Per-key (or per-light) colours, or one of the device's own effects.
    fn lighting_row(&mut self, ui: &mut Ui) {
        let current = self.work.effect.as_ref().map(|e| e.kind);
        let supported: Option<Vec<String>> = match self.status().map(|s| &s.device) {
            Some(Device::Connected { effects, .. }) => Some(effects.clone()),
            _ => None,
        };
        let name = self.model.name.clone();
        ui.horizontal_wrapped(|ui| {
            label(ui, "LIGHTING", LABEL_PX, LABEL);
            ui.add_space(4.0);
            let colours = colours_label(self.model);
            if current.is_none() {
                current_mark(ui, colours);
            } else if button(ui, colours, Look::Quiet, true).clicked() {
                self.work.effect = None;
                self.touched = true;
                self.sync_picker();
            }
            for kind in [fx::Kind::Breathe, fx::Kind::Cycle, fx::Kind::Wave, fx::Kind::Ripple] {
                let text = kind.name().to_uppercase();
                if current == Some(kind) {
                    current_mark(ui, &text);
                    continue;
                }
                let ok = supported.as_ref().is_none_or(|s| s.iter().any(|n| n == kind.name()));
                let b = button(ui, &text, Look::Quiet, ok);
                if !ok {
                    b.on_hover_text(format!("The {name} has no {} effect", kind.name()));
                } else if b.clicked() {
                    let mut e = Effect::new(kind);
                    if let Some(old) = &self.work.effect {
                        e.color = old.color;
                    }
                    self.work.effect = Some(e);
                    self.touched = true;
                    self.sync_picker();
                }
            }
        });
    }

    fn picker(&mut self, ui: &mut Ui) {
        let w = ui.available_width();
        let ppp = ui.pixels_per_point();
        let (sv_rect, sv) = ui.allocate_exact_size(vec2(w, 120.0), Sense::click_and_drag());
        sv.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Slider, true, "saturation and brightness"));
        let (hue_rect, hue) = ui.allocate_exact_size(vec2(w, 16.0), Sense::click_and_drag());
        hue.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Slider, true, "hue"));
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
        let built_in = self.built_in.contains(&name);
        ui.horizontal(|ui| {
            if built_in {
                if button(ui, "SAVE AS NEW", Look::Primary, dirty)
                    .on_hover_text("Built-in presets stay as they are; name your version in the left column")
                    .clicked()
                {
                    self.new_name = Some(String::new());
                }
            } else if button(ui, "SAVE", Look::Primary, dirty).clicked() {
                self.save();
            }
            if button(ui, "REVERT", Look::Quiet, dirty).clicked() {
                self.work = self.saved.clone();
                self.sync_picker();
            }
        });
        ui.horizontal(|ui| {
            if in_use && !dirty {
                current_mark(ui, "IN USE");
            } else if built_in && dirty {
                // a changed preset is used once it is saved under its own name
            } else if button(ui, if dirty { "SAVE AND USE" } else { "USE THIS PROFILE" }, Look::Outline, true)
                .on_hover_text("loginux keeps the profile in use applied whenever its engine runs")
                .clicked()
                && (!dirty || self.save())
            {
                self.link.send(Request::Use { profile: Some(name.clone()) });
            }
            if !built_in && button(ui, "DELETE", Look::Quiet, true).clicked() {
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
        let kept = controls.report_rate_kept.filter(|k| !controls.report_rate_refused.contains(k));
        let note = kept.map_or_else(|| "Changes only when you choose a rate".into(), |k| format!("Set back to {k} Hz after every reconnect"));
        row(ui, "REPORT RATE", &rate, &note);
        row(ui, "VOLUME KNOB", "MEDIA KEYS", "Linux handles volume and mute itself");

        if !controls.report_rates.is_empty() {
            ui.horizontal_wrapped(|ui| {
                label(ui, "SET RATE", LABEL_PX, LABEL);
                ui.add_space(4.0);
                let busy = matches!(change, Some(RateChange::Waiting { .. }));
                for hz in &controls.report_rates {
                    let current = controls.report_rate == Some(*hz);
                    let refused = controls.report_rate_refused.contains(hz);
                    let text = format!("{hz} HZ");
                    if current {
                        current_mark(ui, &text);
                    } else if refused {
                        button(ui, &format!("{text} \u{00b7} REFUSED"), Look::Quiet, false).on_hover_text(
                            "The keyboard lists this rate but refused it (HID++ error 0x02). Delete this keyboard's file in ~/.config/loginux/rates/ to offer it again.",
                        );
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
                    format!("Change the report rate to {hz} Hz? The keyboard may disconnect and come back. Loginux keeps this rate and sets it again whenever the keyboard reconnects, as G HUB does."),
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
            "Loginux reads these from the keyboard each time it connects, every two seconds after, and at once when the keyboard reports a brightness change.",
            "Brightness and game mode are read only until their commands are verified on this keyboard.",
            "A report-rate change is sent when you confirm it, and Loginux reads the rate back, waiting for the keyboard if it reconnects. The keyboard forgets the rate when it reconnects, so Loginux sends the rate you chose again then, as G HUB does, and stops if the keyboard drops off right after three of those in ten minutes.",
            "For each change, Loginux switches the keyboard's profile management to host mode, as G HUB does, and back to onboard mode right after.",
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
        self.lighting_row(ui);
        ui.add_space(12.0);
        if self.work.effect.is_some() {
            self.board(ui, true);
            ui.add_space(12.0);
            let back = colours_label(self.model);
            body(ui, format!("The device runs this effect on its own, so the board's colours are not shown. {back} brings them back."), TEXT2, 13.0);
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
/// them; LOGINUX_SHOT_CONFIRM shows the unsaved-close question; LOGINUX_SHOT_SERVICE=off|on|changing
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
    // Every frame up to the capture: the window's own systemd probe would replace it.
    if frame <= after
        && let Ok(state) = std::env::var("LOGINUX_SHOT_SERVICE")
    {
        *app.service.lock().expect("service lock") = match state.as_str() {
            "on" => Service::Enabled,
            "changing" => Service::Changing(true),
            _ => Service::Disabled,
        };
    }
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

/// The current choice in a row of options: a small square and its label.
fn current_mark(ui: &mut Ui, text: &str) {
    let (sq, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
    fill(ui.painter(), sq, TEXT2);
    label(ui, text, LABEL_PX, TEXT2);
    ui.add_space(8.0);
}

/// What the colour mode is called on this device.
fn colours_label(model: &Model) -> &'static str {
    if model.per_key { "PER-KEY" } else { "COLOURS" }
}

fn effect_note(kind: fx::Kind) -> &'static str {
    match kind {
        fx::Kind::Static => "Every light shows one colour.",
        fx::Kind::Breathe => "Every light fades in and out in one colour.",
        fx::Kind::Cycle => "Every light moves through the colour wheel together.",
        fx::Kind::Wave => "A rainbow travels across the keys.",
        fx::Kind::Ripple => "Each key press sends a ring of colour outwards.",
    }
}

/// Every key and strip colour in proportion, as the profile's thumbnail; an
/// effect shows its colour, or the colour wheel when it has none.
fn strip(p: &Painter, r: Rect, model: &Model, prof: &Profile) {
    if let Some(e) = &prof.effect {
        if e.kind.coloured() {
            fill(p, r, rgb(e.color));
        } else {
            let n = 24;
            for i in 0..n {
                let x = |i: usize| r.min.x + r.width() * i as f32 / n as f32;
                fill(p, Rect::from_min_max(pos2(x(i), r.min.y), pos2(x(i + 1), r.max.y)), rgb(from_hsv([i as f32 / n as f32, 1.0, 1.0])));
            }
        }
        return;
    }
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

// LCOV_EXCL_START: opens the native window, which tests cannot; App holds everything it shows.
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
            Ok(Box::new(App::new(&cc.egui_ctx, layout::current(), device, hosted)))
        }),
    )
    .map_err(|e| e.to_string())
}
// LCOV_EXCL_STOP

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::App as _;
    use egui::accesskit::Role;

    fn g316() -> &'static Model {
        crate::testenv::init();
        layout::find("g316-x-98").unwrap()
    }

    #[test]
    fn hsv_round_trips_the_primaries_and_greys() {
        for c in [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 0], [0, 255, 255], [255, 0, 255], [128, 128, 128], [0, 0, 0], [255, 46, 63]] {
            assert_eq!(from_hsv(to_hsv(c)), c, "{c:?}");
        }
        assert_eq!(to_hsv([255, 0, 0]), [0.0, 1.0, 1.0]);
        assert_eq!(to_hsv([0, 0, 0])[1], 0.0, "black has no saturation");
    }

    #[test]
    fn legends_pick_ink_by_luminance() {
        assert_eq!(ink_on([255, 255, 255]), GROUND);
        assert_eq!(ink_on([0, 0, 64]), TEXT);
        assert_eq!(rgb([1, 2, 3]), Color32::from_rgb(1, 2, 3));
    }

    #[test]
    fn normalize_makes_the_commonest_colour_the_base() {
        let m = g316();
        let mut p = Profile::default();
        for z in m.zones.iter().filter(|z| z.group == "keys") {
            p.zones.insert(z.id, [5, 5, 5]);
        }
        p.zones.insert(38, [255, 0, 0]);
        normalize(m, &mut p);
        assert_eq!(p.base("keys"), [5, 5, 5]);
        assert_eq!(p.zones.get(&38), Some(&[255, 0, 0]));
        assert_eq!(p.zones.get(&39), None, "colours equal to the base are dropped");
        assert_eq!(p.base("bar"), [0, 0, 0]);
        let counts = colour_counts(m, &p, &[Kind::Key]);
        assert_eq!(counts[0], ([5, 5, 5], 94));
        assert_eq!(counts[1], ([255, 0, 0], 1));
    }

    #[test]
    fn selections_read_as_names() {
        let m = g316();
        let pick = |ids: &[u8]| selection_names(m, &ids.iter().copied().collect());
        assert_eq!(pick(&[]), "");
        assert_eq!(pick(&[38]), "ESC");
        assert_eq!(pick(&[160, 38, 154]), "ESC BAR1 154", "keys and strips by position, unknown zones last");
        assert_eq!(pick(&[1, 2, 3, 4, 5, 6]), "6 ZONES");
    }

    #[test]
    fn check_sets_and_labels_fit_the_device() {
        let m = g316();
        let sets = Set::all(m);
        assert_eq!(sets.iter().map(|s| s.title(m)).collect::<Vec<_>>(), ["LIGHT BAR", "UNKNOWN ZONES", "ALL KEYS"]);
        assert_eq!((Set::Unknown.zones(m).len(), Set::Keys.zones(m).len(), Set::Strip(0).zones(m).len()), (8, 95, 30));
        let mouse = layout::find("g903-hero").unwrap();
        let sets = Set::all(mouse);
        assert_eq!(sets.iter().map(|s| s.title(mouse)).collect::<Vec<_>>(), ["ALL LIGHTS"], "no empty unknown set");
        assert_eq!((colours_label(m), colours_label(mouse)), ("PER-KEY", "COLOURS"));
        for k in fx::Kind::ALL {
            assert!(effect_note(k).ends_with('.'));
        }
    }

    /// One accessibility node: what a screen reader announces (a label, or a
    /// text's value), and where.
    struct Node {
        label: String,
        role: Role,
        rect: Rect,
        enabled: bool,
        /// On or off, for a switch or a check box.
        toggled: Option<bool>,
        focused: bool,
    }

    /// The editor without a window: frames run headless, and controls are found
    /// and clicked by their accessibility labels, as a user would.
    struct Window {
        ctx: egui::Context,
        app: App,
        frame: eframe::Frame,
        nodes: Vec<Node>,
        events: Vec<egui::Event>,
        modifiers: egui::Modifiers,
        /// The window manager asks to close before the next frame.
        close: bool,
        /// Viewport commands the editor sent, such as Close.
        commands: Vec<egui::ViewportCommand>,
    }

    impl Window {
        /// An editor for the G316 on `device`, which hosts an engine of its own
        /// once it finds none answering.
        fn on(device: Option<&str>) -> Window {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            style(&ctx);
            let app = App::new(&ctx, g316(), device.map(PathBuf::from), None);
            let frame = eframe::Frame::_new_kittest();
            Window { ctx, app, frame, nodes: vec![], events: vec![], modifiers: Default::default(), close: false, commands: vec![] }
        }

        /// An editor whose device does not exist, once it hosts its own engine.
        fn open() -> Window {
            Window::hosting(Some("/nonexistent/loginux-ui-test"))
        }

        fn hosting(device: Option<&str>) -> Window {
            let mut w = Window::on(device);
            w.until("the editor hosts an engine", |w| w.app.hosted.is_some() && w.app.status.is_ok());
            w
        }

        fn run(&mut self, frames: usize) {
            for _ in 0..frames {
                let mut events = vec![egui::Event::ModifiersChanged(self.modifiers)];
                events.append(&mut self.events);
                let mut input = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1360.0, 840.0))),
                    events,
                    ..Default::default()
                };
                if std::mem::take(&mut self.close) {
                    input.viewports.entry(egui::ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
                }
                let (app, frame) = (&mut self.app, &mut self.frame);
                let mut out = self.ctx.run_ui(input, |ui| app.ui(ui, frame));
                out.textures_delta.clear(); // nothing paints them
                self.commands.extend(out.viewport_output.into_values().flat_map(|v| v.commands));
                if let Some(update) = out.platform_output.accesskit_update {
                    let focus = update.focus;
                    self.nodes = update
                        .nodes
                        .iter()
                        .filter_map(|(id, n)| {
                            let b = n.bounds()?;
                            let rect = Rect::from_min_max(pos2(b.x0 as f32, b.y0 as f32), pos2(b.x1 as f32, b.y1 as f32));
                            let label = n.label().or(n.value()).unwrap_or_default().to_string();
                            let toggled = n.toggled().map(|t| t == egui::accesskit::Toggled::True);
                            Some(Node { label, role: n.role(), rect, enabled: !n.is_disabled(), toggled, focused: *id == focus })
                        })
                        .collect();
                }
            }
        }

        /// Runs frames until `cond` holds, for up to 15 seconds.
        fn until(&mut self, what: &str, cond: impl Fn(&Window) -> bool) {
            let end = Instant::now() + Duration::from_secs(15);
            while !cond(self) {
                assert!(Instant::now() < end, "timed out waiting until {what}");
                self.run(1);
                thread::sleep(Duration::from_millis(20));
            }
        }

        fn has(&self, label: &str) -> bool {
            self.nodes.iter().any(|n| n.label == label)
        }

        fn enabled(&self, label: &str) -> bool {
            self.nodes.iter().any(|n| n.label == label && n.enabled)
        }

        fn toggled(&self, label: &str) -> Option<bool> {
            self.nodes.iter().find(|n| n.label == label).and_then(|n| n.toggled)
        }

        fn focused(&self, label: &str) -> bool {
            self.nodes.iter().any(|n| n.label == label && n.focused)
        }

        /// The leftmost enabled control with this label.
        fn find(&self, label: &str) -> Rect {
            self.nodes
                .iter()
                .filter(|n| n.label == label && n.enabled)
                .map(|n| n.rect)
                .min_by(|a, b| a.min.x.total_cmp(&b.min.x))
                .unwrap_or_else(|| panic!("no enabled {label:?}; have {:?}", self.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()))
        }

        /// The topmost control of `role` in the right column.
        fn right(&self, role: Role) -> Rect {
            self.nodes
                .iter()
                .filter(|n| n.role == role && n.rect.min.x > 1044.0)
                .map(|n| n.rect)
                .min_by(|a, b| a.min.y.total_cmp(&b.min.y))
                .unwrap_or_else(|| panic!("no {role:?} in the right column"))
        }

        /// The centre of zone `id` on the board, from the board's place and the definition.
        fn zone(&self, id: u8) -> Pos2 {
            let board = self.find("keyboard map");
            let unit = (board.width() - 20.0) / self.app.model.board[0];
            let [x, y, w, h] = self.app.model.zone(id).expect("a zone of the definition").rect;
            board.min + vec2(10.0 + (x + w / 2.0) * unit, 10.0 + (y + h / 2.0) * unit)
        }

        /// Clicks the leftmost enabled control with this label, as the window shows it now.
        fn click(&mut self, label: &str) {
            self.run(1);
            let at = self.find(label).center();
            self.click_at(at);
        }

        fn click_at(&mut self, pos: Pos2) {
            let modifiers = self.modifiers;
            let button = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers };
            self.events = vec![egui::Event::PointerMoved(pos), button(true)];
            self.run(1);
            self.events = vec![button(false)];
            self.run(2);
        }

        fn hover(&mut self, pos: Pos2) {
            self.events = vec![egui::Event::PointerMoved(pos)];
            self.run(2);
        }

        fn drag(&mut self, from: Pos2, to: Pos2) {
            let modifiers = self.modifiers;
            let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers };
            self.events = vec![egui::Event::PointerMoved(from), button(from, true)];
            self.run(1);
            // A few points first, so the drag starts where it was pressed, as a hand's would.
            for step in [(to - from).normalized() * 8.0, (to - from) * 0.5, to - from] {
                self.events = vec![egui::Event::PointerMoved(from + step)];
                self.run(1);
            }
            self.events = vec![button(to, false)];
            self.run(2);
        }

        fn key(&mut self, key: Key) {
            let modifiers = self.modifiers;
            let event = |pressed| egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers };
            self.events = vec![event(true), event(false)];
            self.run(2);
        }

        fn type_text(&mut self, text: &str) {
            self.events = vec![egui::Event::Text(text.into())];
            self.run(2);
        }
    }

    impl Drop for Window {
        fn drop(&mut self) {
            self.app.on_exit(None); // as eframe does when the window closes
        }
    }

    fn scratch_profile(name: &str, esc: Rgb) {
        let mut p = Profile::default();
        p.bases.insert("keys".into(), [0x20, 0x20, 0x20]);
        p.zones.insert(38, esc);
        profile::save(g316(), name, &p).unwrap();
    }

    #[test]
    fn create_paint_save_copy_and_switch_profiles_in_the_editor() {
        let _serial = crate::testenv::serial();
        let mut w = Window::open();
        assert!(w.has("+ NEW PROFILE") && w.has("SERVICE NOT INSTALLED"));

        // Create: the new profile starts as a copy of the open one and opens.
        w.click("+ NEW PROFILE");
        w.type_text("T ui one");
        w.click("CREATE");
        assert_eq!(w.app.open.as_deref(), Some("T ui one"));
        assert!(profile::path("T ui one").unwrap().exists());
        // With no profile chosen the editor opened the first preset, so the copy
        // runs an effect; per-key colours come back with PER-KEY.
        if w.app.work.effect.is_some() {
            w.click("PER-KEY");
        }
        assert!(w.app.work.effect.is_none());

        // Paint Esc red, save: the file holds it, and the window is clean again.
        w.app.selection = [38].into_iter().collect();
        w.app.paint_selection([255, 0, 0]);
        w.run(2);
        assert!(w.app.dirty() && w.enabled("SAVE"));
        w.click("SAVE");
        assert!(!w.app.dirty());
        assert_eq!(profile::load(g316(), "T ui one").unwrap().zones.get(&38), Some(&[255, 0, 0]));

        // A second profile copies the first; switching back opens it.
        w.click("+ NEW PROFILE");
        w.type_text("T ui two");
        w.click("CREATE");
        assert_eq!(profile::load(g316(), "T ui two").unwrap().zones.get(&38), Some(&[255, 0, 0]));
        w.click("T ui one");
        assert_eq!(w.app.open.as_deref(), Some("T ui one"));

        // Use it: the hosted engine takes it as the profile to keep applied.
        w.click("USE THIS PROFILE");
        w.until("the engine reports the profile in use", |w| w.app.status().and_then(|s| s.active.as_deref()) == Some("T ui one"));
        assert_eq!(profile::active().as_deref(), Some("T ui one"));
        w.run(1);
        assert!(!w.has("USE THIS PROFILE"), "an IN USE mark takes the button's place");

        // Unsaved changes before switching ask first; discarding opens the other profile.
        w.app.selection = [38].into_iter().collect(); // opening a profile clears the selection
        w.app.paint_selection([0, 0, 255]);
        w.run(1);
        w.click("T ui two");
        assert!(matches!(w.app.confirm, Some(Confirm::Switch(ref n)) if n == "T ui two"));
        w.click("DISCARD AND OPEN");
        assert_eq!((w.app.open.as_deref(), w.app.dirty()), (Some("T ui two"), false));

        // Delete asks, KEEP IT keeps, DELETE removes the file.
        w.click("DELETE");
        assert!(matches!(w.app.confirm, Some(Confirm::Delete(_))));
        w.click("KEEP IT");
        assert!(w.app.confirm.is_none() && profile::path("T ui two").unwrap().exists());
        w.click("DELETE");
        w.click("DELETE"); // the confirm bar's button sits left of the column's
        assert!(!profile::path("T ui two").unwrap().exists());
        assert!(!w.has("T ui two"));

        w.click("T ui one");
        ok_use_none(&mut w);
        profile::delete("T ui one").unwrap();
    }

    /// Hands the lighting back so later tests start with no profile chosen.
    fn ok_use_none(w: &mut Window) {
        w.app.link.send(Request::Use { profile: None });
        w.until("the engine drops the profile", |w| w.app.status().is_some_and(|s| s.active.is_none()));
    }

    #[test]
    fn presets_open_read_only_and_save_as_new() {
        let _serial = crate::testenv::serial();
        let mut w = Window::open();
        w.click("Wave");
        assert_eq!(w.app.work.effect.as_ref().map(|e| e.kind), Some(fx::Kind::Wave));
        assert!(w.app.built_in.contains("Wave") && w.has("SAVE AS NEW") && !w.enabled("SAVE AS NEW") && !w.has("DELETE"));
        assert!(!w.has("RIGHT") && w.has("LEFT") && w.has("CENTRE OUT"), "the current direction is a mark, the rest buttons");
        w.click("LEFT");
        assert_eq!(w.app.work.effect.as_ref().map(|e| e.direction), Some(6));
        assert!(w.app.dirty() && !w.has("USE THIS PROFILE"), "a changed preset is saved under its own name first");
        w.click("CYCLE");
        assert_eq!(w.app.work.effect.as_ref().map(|e| e.kind), Some(fx::Kind::Cycle));
        w.click("SAVE AS NEW");
        w.type_text("T my cycle");
        w.click("CREATE");
        assert_eq!(w.app.open.as_deref(), Some("T my cycle"));
        assert_eq!(profile::load(g316(), "T my cycle").unwrap().effect.map(|e| e.kind), Some(fx::Kind::Cycle));
        assert!(profile::built_in("Wave"), "the preset itself is untouched");

        // Back to per-key colours, then a coloured effect with its picker.
        w.click("PER-KEY");
        assert!(w.app.work.effect.is_none());
        w.click("BREATHE");
        assert!(w.app.work.effect.as_ref().is_some_and(|e| e.kind == fx::Kind::Breathe));
        w.app.paint_selection([1, 2, 3]);
        assert_eq!(w.app.work.effect.as_ref().map(|e| e.color), Some([1, 2, 3]), "the picker edits the effect's colour");
        w.click("SAVE");
        assert_eq!(profile::load(g316(), "T my cycle").unwrap().effect.map(|e| e.color), Some([1, 2, 3]));
        profile::delete("T my cycle").unwrap();
    }

    #[test]
    fn every_tab_and_notice_renders() {
        let _serial = crate::testenv::serial();
        scratch_profile("T tabs", [9, 9, 9]);
        let mut w = Window::open();
        w.click("T tabs");
        w.app.selection = Set::Keys.zones(w.app.model).into_iter().collect();
        w.app.sync_picker();
        w.run(2);
        assert!(w.has("ALL KEYS") && w.has("LIGHT BAR") && w.has("keyboard map"));
        w.click("CHANNEL CHECK");
        assert!(w.app.tab == Tab::Check && w.has("CHECK ALL KEYS") && !w.enabled("CHECK ALL KEYS"), "a check needs a connected device");
        w.app.set = Some(Set::Strip(0));
        w.run(2);
        assert!(w.has("NOTHING LIT") && w.has("STOP"));
        w.click("STOP");
        assert!(w.app.set.is_none());
        w.click("DEVICE");
        assert!(w.app.tab == Tab::Device);
        w.click("EDIT");
        w.app.notice = Some(("Something to read".into(), AMBER));
        w.run(1);
        w.click("OK");
        assert!(w.app.notice.is_none());
        w.app.confirm = Some(Confirm::Close);
        w.run(1);
        assert!(w.has("SAVE AND CLOSE") && w.has("DISCARD AND CLOSE"));
        w.app.confirm = None;
        w.app.model = layout::find("g903-hero").unwrap();
        w.app.work = Profile { effect: Some(Effect::new(fx::Kind::Breathe)), ..Profile::default() };
        w.run(2);
        assert!(w.has("COLOURS") && w.has("keyboard map"), "a mouse offers its colours mode");
        profile::delete("T tabs").unwrap();
    }

    const NO_DEVICE: &str = "/nonexistent/loginux-ui-test";

    fn service(w: &Window) -> Service {
        *w.app.service.lock().unwrap()
    }

    fn control(w: &Window) -> Option<Control> {
        w.app.status().map(|s| s.control.clone())
    }

    fn noticed(w: &Window, text: &str) -> bool {
        w.app.notice.as_ref().is_some_and(|n| n.0.contains(text))
    }

    /// The stand-in systemctl answering `state`, until dropped.
    struct ServiceState;

    impl ServiceState {
        fn set(state: &str) -> ServiceState {
            std::fs::write(crate::testenv::systemd().join("state"), format!("{state}\n")).unwrap();
            ServiceState
        }
    }

    impl Drop for ServiceState {
        fn drop(&mut self) {
            for file in ["state", "fails", "hold"] {
                let _ = std::fs::remove_file(crate::testenv::systemd().join(file));
            }
        }
    }

    fn forget_claims() {
        for id in ["g316-x-98", "g903-hero"] {
            let _ = std::fs::remove_file(ipc::runtime_dir().unwrap().join(format!("claim-{id}")));
        }
    }

    #[test]
    fn the_board_selects_by_click_shift_drag_and_keys() {
        let _serial = crate::testenv::serial();
        scratch_profile("T board", [255, 0, 0]);
        let mut w = Window::open();
        w.click("T board");
        let m = w.app.model;
        let (esc, f1, f2) = (38, m.named("f1").unwrap().id, m.named("f2").unwrap().id);
        let at = w.zone(esc);
        w.click_at(at);
        assert_eq!(w.app.selection, BTreeSet::from([esc]));
        w.modifiers = egui::Modifiers::SHIFT;
        let at = w.zone(f1);
        w.click_at(at);
        assert_eq!(w.app.selection, BTreeSet::from([esc, f1]), "shift adds");
        let at = w.zone(esc);
        w.click_at(at);
        assert_eq!(w.app.selection, BTreeSet::from([f1]), "and removes");
        let corner = w.find("keyboard map").min + vec2(3.0, 3.0);
        w.click_at(corner);
        assert_eq!(w.app.selection, BTreeSet::from([f1]), "shift-clicking empty board keeps the selection");
        w.modifiers = egui::Modifiers::NONE;
        w.click_at(corner);
        assert!(w.app.selection.is_empty(), "a plain click there clears it");

        // Hovering names a key with its proof: checked as expected, checked as different, or layout data only.
        w.app.checks.insert(esc, Seen { saw: "esc".into(), date: "2026-10-22".into() });
        w.app.checks.insert(f1, Seen { saw: "nothing".into(), date: "2026-10-22".into() });
        for id in [esc, f1, f2] {
            let at = w.zone(id);
            w.hover(at);
            assert_eq!(w.app.hover, Some(id));
        }

        // Dragging from empty board selects what the box touches; with shift it adds.
        let to = w.zone(f1);
        w.drag(corner, to);
        assert!(w.app.selection.contains(&esc) && w.app.selection.contains(&f1), "{:?}", w.app.selection);
        let far = w.find("keyboard map").max - vec2(3.0, 3.0);
        w.modifiers = egui::Modifiers::SHIFT;
        w.drag(far, far - vec2(40.0, 40.0));
        assert!(w.app.selection.contains(&esc) && w.app.selection.len() > 2, "added to");
        w.modifiers = egui::Modifiers::NONE;
        let before = w.app.selection.clone();
        let (from, to) = (w.zone(f2), w.zone(f1));
        w.drag(from, to);
        assert_eq!(w.app.selection, before, "a drag that starts on a key draws no box");

        w.click("ALL KEYS");
        assert_eq!(w.app.selection.len(), 95);
        w.click("LIGHT BAR");
        assert_eq!(w.app.selection.len(), 30);
        w.click("NONE");
        assert!(w.app.selection.is_empty());

        // Research zones: selected by their buttons, painted, then left dark again.
        let unknown = m.unknown();
        let (a, b) = (format!("zone {}", unknown[0]), format!("zone {}", unknown[1]));
        w.click(&a);
        w.modifiers = egui::Modifiers::SHIFT;
        w.click(&b);
        assert_eq!(w.app.selection, BTreeSet::from([unknown[0], unknown[1]]));
        w.click(&b);
        assert_eq!(w.app.selection, BTreeSet::from([unknown[0]]));
        w.modifiers = egui::Modifiers::NONE;
        w.app.paint_selection([0, 0, 255]);
        w.run(1);
        w.click("LEAVE THESE ZONES DARK");
        assert!(!w.app.work.zones.contains_key(&unknown[0]));

        w.key(Key::Escape);
        assert!(w.app.selection.is_empty());
        w.modifiers = egui::Modifiers::COMMAND;
        w.key(Key::A);
        w.modifiers = egui::Modifiers::NONE;
        assert_eq!(w.app.selection.len(), 95, "Ctrl+A selects every key");
        profile::delete("T board").unwrap();
    }

    #[test]
    fn the_picker_and_palette_paint_and_select() {
        let _serial = crate::testenv::serial();
        scratch_profile("T picker", [255, 0, 0]);
        let mut w = Window::open();
        w.click("T picker");
        let at = w.zone(38);
        w.click_at(at);
        let sv = w.find("saturation and brightness");
        w.click_at(sv.min + vec2(sv.width() * 0.5, sv.height() * 0.25));
        let after_sv = w.app.work.zones[&38];
        assert_eq!(after_sv, from_hsv([0.0, 0.5, 0.75]));
        let hue = w.find("hue");
        w.click_at(hue.center());
        let after_hue = w.app.work.zones[&38];
        assert_ne!(after_hue, after_sv);
        let r = w.right(Role::SpinButton);
        w.drag(r.center(), r.center() + vec2(40.0, 0.0));
        assert_eq!(w.app.work.zones[&38][0], after_hue[0] + 20, "R up by half a point per point dragged");

        // Two colours selected: the hex field is empty, and a typed colour paints both.
        w.modifiers = egui::Modifiers::SHIFT;
        let at = w.zone(39);
        w.click_at(at);
        w.modifiers = egui::Modifiers::NONE;
        assert!(w.app.hex_text.is_empty());
        let hex = w.right(Role::TextInput);
        w.click_at(hex.center());
        w.type_text("#00ff00");
        assert_eq!((w.app.work.zones[&38], w.app.work.zones[&39]), ([0, 255, 0], [0, 255, 0]));
        assert_eq!(w.app.hex_text, "#00ff00", "the field keeps what was typed");

        // A palette row selects every zone of its colour.
        w.key(Key::Escape);
        w.run(1);
        let row = w.nodes.iter().find(|n| n.label.starts_with("#00FF00")).expect("a palette row").rect.center();
        w.hover(row);
        w.click_at(row);
        assert_eq!(w.app.selection, BTreeSet::from([38, 39]));
        for (i, id) in Set::Keys.zones(w.app.model).into_iter().enumerate() {
            w.app.work.zones.insert(id, [i as u8, 1, 1]);
        }
        w.run(2);
        assert!(w.nodes.iter().any(|n| n.label.starts_with('+') && n.label.ends_with(" more")), "the last slot counts the rest");
        w.click("REVERT");
        assert!(!w.app.dirty());
        profile::delete("T picker").unwrap();
    }

    #[test]
    fn effect_settings_follow_the_kind() {
        let _serial = crate::testenv::serial();
        let mut w = Window::open();
        w.click("Colour Cycle");
        let before = w.app.work.effect.as_ref().unwrap().period;
        let secs = w.right(Role::SpinButton);
        w.drag(secs.center(), secs.center() + vec2(40.0, 0.0));
        assert_eq!(w.app.work.effect.as_ref().unwrap().period, before + 2000, "two seconds more");
        w.click("Ripple");
        w.click("DISCARD AND OPEN");
        assert_eq!(w.app.work.effect.as_ref().map(|e| e.kind), Some(fx::Kind::Ripple));
        w.run(1);
        assert_eq!(w.nodes.iter().filter(|n| n.role == Role::SpinButton).count(), 3, "R, G and B; no period");
    }

    #[test]
    fn names_and_failed_saves_are_explained() {
        let _serial = crate::testenv::serial();
        profile::set_active(None).unwrap();
        scratch_profile("T names", [1, 2, 3]);
        let mut w = Window::open();
        w.click("T names");
        w.click("+ NEW PROFILE");
        w.type_text("T names");
        assert!(!w.enabled("CREATE") && w.has("Letters, digits, spaces, - and _; not an existing name."));
        w.click("CANCEL");
        assert!(w.app.new_name.is_none());
        w.click("+ NEW PROFILE");
        w.key(Key::Escape);
        assert!(w.app.new_name.is_none(), "Escape leaves too");

        let dir = profile::profiles_dir();
        std::fs::create_dir_all(dir.join("T blocked.toml.tmp")).unwrap(); // in the way of the save
        w.click("+ NEW PROFILE");
        w.type_text("T blocked");
        w.click("CREATE");
        assert!(noticed(&w, "T blocked.toml") && w.app.new_name.is_some());
        w.click("CANCEL");
        w.click("OK");
        std::fs::create_dir_all(dir.join("T names.toml.tmp")).unwrap();
        w.app.selection = BTreeSet::from([38]);
        w.app.paint_selection([9, 9, 9]);
        w.run(1);
        w.click("SAVE");
        assert!(noticed(&w, "could not save") && w.app.dirty());
        w.click("REVERT");
        std::fs::remove_dir(dir.join("T blocked.toml.tmp")).unwrap();
        std::fs::remove_dir(dir.join("T names.toml.tmp")).unwrap();

        // In use, then deleted: the engine is told to stop using it.
        w.click("USE THIS PROFILE");
        w.until("the profile is in use", |w| w.app.status().and_then(|s| s.active.as_deref()) == Some("T names"));
        w.click("DELETE");
        w.click("DELETE");
        w.until("the engine drops it", |w| w.app.status().is_some_and(|s| s.active.is_none()));

        // A file that vanished behind the editor's back cannot be deleted again.
        scratch_profile("T gone", [4, 4, 4]);
        w.app.reload_list();
        w.run(1);
        w.click("T gone");
        profile::delete("T gone").unwrap();
        w.click("DELETE");
        w.click("DELETE");
        assert!(w.app.notice.is_some());
        drop(w);

        // A broken file is listed, but it does not open.
        std::fs::write(dir.join("A broken.toml"), "zones = ").unwrap();
        let mut w = Window::open();
        assert!(w.app.open.is_none() && noticed(&w, "A broken.toml"));
        w.run(2);
        assert!(w.has("A broken") && w.has("Create a profile to start painting."));
        std::fs::remove_file(dir.join("A broken.toml")).unwrap();
    }

    #[test]
    fn closing_or_switching_with_unsaved_changes_asks_first() {
        let _serial = crate::testenv::serial();
        scratch_profile("T close", [1, 1, 1]);
        scratch_profile("T other", [2, 2, 2]);
        let mut w = Window::open();
        w.click("T close");
        let edit = |w: &mut Window, c: Rgb| {
            w.app.selection = BTreeSet::from([38]);
            w.app.paint_selection(c);
            w.run(1);
        };
        edit(&mut w, [3, 3, 3]);
        w.close = true;
        w.run(2);
        assert!(matches!(w.app.confirm, Some(Confirm::Close)));
        assert!(w.commands.iter().any(|c| matches!(c, egui::ViewportCommand::CancelClose)));
        w.click("CANCEL");
        assert!(w.app.confirm.is_none() && !w.app.closing);
        w.close = true;
        w.run(2);
        w.click("DISCARD AND CLOSE");
        assert!(w.app.closing && w.commands.iter().any(|c| matches!(c, egui::ViewportCommand::Close)));
        w.app.closing = false;
        // A save that fails keeps the window open, with the changes.
        let blocked = profile::profiles_dir().join("T close.toml.tmp");
        std::fs::create_dir_all(&blocked).unwrap();
        w.close = true;
        w.run(2);
        w.click("SAVE AND CLOSE");
        assert!(!w.app.closing && w.app.dirty() && noticed(&w, "could not save"));
        std::fs::remove_dir(&blocked).unwrap();
        w.close = true;
        w.run(2);
        w.click("SAVE AND CLOSE");
        assert!(w.app.closing && !w.app.dirty());
        assert_eq!(profile::load(g316(), "T close").unwrap().zones.get(&38), Some(&[3, 3, 3]));
        w.app.closing = false;

        edit(&mut w, [4, 4, 4]);
        w.click("T other");
        w.click("SAVE AND OPEN");
        assert_eq!(w.app.open.as_deref(), Some("T other"));
        assert_eq!(profile::load(g316(), "T close").unwrap().zones.get(&38), Some(&[4, 4, 4]));

        // A signal closes the window the normal way.
        STOP.store(true, Ordering::SeqCst);
        w.run(1);
        STOP.store(false, Ordering::SeqCst);
        assert!(w.app.closing);
        profile::delete("T close").unwrap();
        profile::delete("T other").unwrap();
    }

    #[test]
    fn keyboard_focus_moves_with_tab_and_shows() {
        let _serial = crate::testenv::serial();
        let mut w = Window::open();
        for _ in 0..12 {
            w.key(Key::Tab);
        }
        assert!(w.ctx.memory(|m| m.focused()).is_some());
    }

    #[test]
    fn the_service_switch_runs_systemctl_and_shows_failures() {
        let _serial = crate::testenv::serial();
        let systemd = crate::testenv::systemd();
        let _ = std::fs::remove_file(systemd.join("log"));
        let _state = ServiceState::set("disabled");
        let mut w = Window::open();
        w.until("systemd answers", |w| service(w) == Service::Disabled);
        assert_eq!(w.toggled("RUN AT LOGIN"), Some(false), "a switch, off");
        // While systemctl works, the switch shows the side asked for and takes no clicks.
        std::fs::write(systemd.join("hold"), "").unwrap();
        w.click("RUN AT LOGIN");
        assert!(service(&w) == Service::Changing(true));
        assert_eq!((w.toggled("RUN AT LOGIN"), w.enabled("RUN AT LOGIN")), (Some(true), false));
        std::fs::remove_file(systemd.join("hold")).unwrap();
        w.until("the service is enabled", |w| service(w) == Service::Enabled);
        assert!(w.app.hosted.is_none(), "the window handed its engine over");
        w.run(1);
        assert_eq!(w.toggled("RUN AT LOGIN"), Some(true), "on, under the same label");
        // The keyboard works it too: Tab to the switch, Space turns it off.
        for _ in 0..40 {
            if w.focused("RUN AT LOGIN") {
                break;
            }
            w.key(Key::Tab);
        }
        w.key(Key::Space);
        w.until("the service is disabled", |w| service(w) == Service::Disabled);
        std::fs::write(systemd.join("fails"), "").unwrap();
        w.click("RUN AT LOGIN");
        w.until("the failure shows", |w| noticed(w, "systemctl --user enable --now: Failed to enable loginux.service"));
        w.until("the switch is off again", |w| service(w) == Service::Disabled);
        std::fs::remove_file(systemd.join("fails")).unwrap();
        let log = std::fs::read_to_string(systemd.join("log")).unwrap();
        let changes: Vec<&str> = log.lines().filter(|l| l.contains("--now")).collect();
        let want = ["--user enable --now loginux.service", "--user disable --now loginux.service", "--user enable --now loginux.service"];
        assert_eq!(changes, want, "one systemctl call per click, none while one runs");
        *w.app.service.lock().unwrap() = Service::Unknown;
        w.run(1);
        assert!(w.has("RUN AT LOGIN") && !w.enabled("RUN AT LOGIN"), "nothing to click before systemd answers");
    }

    #[test]
    fn hosting_waits_for_systemd_and_other_engines() {
        let _serial = crate::testenv::serial();
        let systemd = crate::testenv::systemd();
        // An enabled service that is not running is started, once.
        let _ = std::fs::remove_file(systemd.join("log"));
        let state = ServiceState::set("enabled");
        let mut w = Window::on(Some(NO_DEVICE));
        let started = |_: &Window| std::fs::read_to_string(crate::testenv::systemd().join("log")).is_ok_and(|l| l.contains("start loginux.service"));
        w.until("the service is started", started);
        assert!(w.app.hosted.is_none());
        drop(w);
        drop(state);

        // Before systemd answers, nothing is hosted.
        let mut w = Window::on(Some(NO_DEVICE));
        let end = Instant::now() + Duration::from_secs(5);
        while service(&w) == Service::Unknown && Instant::now() < end {
            thread::sleep(Duration::from_millis(10));
        }
        *w.app.service.lock().unwrap() = Service::Unknown;
        w.until("the link finds no engine", |w| matches!(w.app.link.0.0.lock().unwrap().status, Some(Err(_))));
        assert!(w.app.hosted.is_none() && w.app.host_tried.is_none(), "waiting for systemd is not an attempt");

        // Another engine holds the lock: its socket will appear, so nothing is hosted either.
        *w.app.service.lock().unwrap() = Service::NotInstalled;
        let held = engine::lock(false).unwrap().expect("free");
        w.app.host_tried = None;
        w.run(1);
        assert!(w.app.hosted.is_none() && w.app.host_tried.is_some());
        drop(held);

        // The lock cannot be taken at all: explained.
        let lock = ipc::runtime_dir().unwrap().join("lock");
        let _ = std::fs::remove_file(&lock);
        std::fs::create_dir(&lock).unwrap();
        w.app.host_tried = None;
        w.run(1);
        std::fs::remove_dir(&lock).unwrap();
        assert!(noticed(&w, "lock"));
    }

    #[test]
    fn a_connected_keyboard_shows_its_state_checks_and_settings() {
        let _serial = crate::testenv::serial();
        forget_claims();
        profile::set_active(None).unwrap();
        let _ = std::fs::remove_file(profile::checks_path(g316()));
        let _ = std::fs::remove_file(profile::rates_path(g316()));
        scratch_profile("T live", [255, 0, 0]);
        let kb = crate::fake::plug("/dev/fake-ui", "g316-x-98", crate::fake::g316());
        let mut w = Window::hosting(Some("/dev/fake-ui"));
        w.until("the keyboard connects", |w| w.app.connected_zones().is_some());
        assert_eq!(w.app.open.as_deref(), Some("T live"), "saved profiles come before the presets");
        let at = w.zone(38);
        w.click_at(at);
        w.app.paint_selection([255, 0, 0]);
        w.until("the edits show live", |w| control(w) == Some(Control::Preview));
        assert!(w.has("G316 X 98 on /dev/fake-ui") && w.has("Showing your edits live."));
        w.until("Esc is red on the keyboard", |_| kb.state().shown.get(&38) == Some(&[255, 0, 0]));
        w.click("RELEASE LIGHTING");
        w.until("onboard lighting again", |w| control(w) == Some(Control::Onboard));
        w.click("USE THIS PROFILE");
        w.until("the profile is shown", |w| matches!(control(w), Some(Control::Profile { .. })));
        w.run(1);
        assert!(w.has("Showing T live."));
        kb.state().sw = (1, 5); // another program takes the lighting
        w.until("the conflict shows", |w| matches!(control(w), Some(Control::Conflict { .. })));
        w.run(1);
        w.click("TAKE OVER");
        w.until("taken back", |w| matches!(control(w), Some(Control::Profile { .. })));

        // Channel check: answers by clicking where the light is, or by the buttons.
        w.click("CHANNEL CHECK");
        let strip = w.app.model.strips[0].zones.clone();
        let at = w.find("CHECK LIGHT BAR").center();
        w.hover(at);
        assert!(w.app.focus == Set::Strip(0));
        w.click("CHECK LIGHT BAR");
        assert!(w.app.set == Some(Set::Strip(0)) && w.app.step == 0);
        w.until("zone lit", |w| control(w) == Some(Control::Check { zone: strip[0] }));
        let at = w.zone(strip[0]);
        w.click_at(at);
        w.click("NOTHING LIT");
        w.click("NOT ON THE MAP");
        w.click("SKIP");
        w.click("BACK");
        assert_eq!(w.app.step, 3);
        let saw = |w: &Window, i: usize| w.app.checks.get(&strip[i]).map(|s| s.saw.clone());
        assert_eq!((saw(&w, 0), saw(&w, 1), saw(&w, 2)), (Some("bar1".into()), Some("nothing".into()), Some("elsewhere".into())));
        w.app.step = strip.len() - 1;
        w.click("SKIP");
        w.run(1);
        assert!(w.app.set.is_none(), "past the last zone the run ends");
        w.click("CHECK UNKNOWN ZONES");
        w.click("NOTHING LIT");
        w.click("STOP");
        w.run(1);
        assert!(w.has("saw nothing"));
        let at = w.find("CHECK LIGHT BAR").center();
        w.hover(at);
        assert!(w.has("as expected") && w.has("lit nothing") && w.has("not answered"));
        w.click("CLEAR THESE ANSWERS");
        assert!(strip.iter().all(|id| !w.app.checks.contains_key(id)));
        let tmp = profile::checks_path(g316()).with_extension("toml.tmp");
        std::fs::create_dir_all(&tmp).unwrap(); // saving answers fails
        w.click("CHECK LIGHT BAR");
        w.click("NOTHING LIT");
        assert!(noticed(&w, "g316-x-98.toml"));
        w.click("STOP");
        w.click("OK");
        w.click("CLEAR THESE ANSWERS");
        assert!(noticed(&w, "g316-x-98.toml"));
        std::fs::remove_dir(&tmp).unwrap();
        w.click("OK");

        // Device settings, and a report-rate change: cancelled, confirmed, then refused.
        w.click("DEVICE");
        w.run(1);
        assert!(w.has("Changes only when you choose a rate"), "nothing kept yet");
        w.click("2000 HZ");
        w.click("CANCEL");
        assert!(w.app.rate_confirm.is_none());
        w.click("2000 HZ");
        w.click("CHANGE RATE");
        w.until("the change waits for the keyboard", |w| w.has("Waiting for the keyboard to confirm 2000 Hz\u{2026}"));
        w.until("the rate reads back", |w| w.has("Report rate read back as 2000 Hz."));
        w.until("the rate is kept", |w| w.has("Set back to 2000 Hz after every reconnect"));
        kb.state().refuse_rate = Some((5, 2));
        w.click("4000 HZ");
        w.click("CHANGE RATE");
        w.until("the refusal shows", |w| w.nodes.iter().any(|n| n.label.starts_with("The change to 4000 Hz did not take")));
        assert!(noticed(&w, "refused 4000 Hz"));
        let refused = "4000 HZ \u{00b7} REFUSED";
        w.until("the refused rate is marked", |w| w.has(refused));
        assert!(!w.enabled(refused), "a refused rate is not offered again");

        // A second window uses this engine, and leaves it running when it closes.
        let second = Window::on(Some("/dev/fake-ui"));
        assert!(second.app.status.is_ok() && second.app.hosted.is_none());
        drop(second);
        assert!(w.app.status().is_some());
        ok_use_none(&mut w);
        profile::delete("T live").unwrap();
        let _ = std::fs::remove_file(profile::checks_path(g316()));
        let _ = std::fs::remove_file(profile::rates_path(g316()));
        forget_claims();
    }

    #[test]
    fn a_missing_wrong_or_other_device_is_explained() {
        let _serial = crate::testenv::serial();
        let device = |w: &Window| w.app.status().map(|s| s.device.clone());
        let mut w = Window::hosting(None);
        w.until("nothing plugged in", |w| matches!(device(w), Some(Device::Missing)));
        w.run(1);
        assert!(w.has("Plug the G316 X 98 in; loginux picks it up."));
        drop(w);
        let file = crate::testenv::init().join("not-a-keyboard");
        std::fs::write(&file, "").unwrap();
        let mut w = Window::hosting(file.to_str());
        w.until("not a keyboard", |w| matches!(device(w), Some(Device::Failed { .. })));
        w.run(1);
        drop(w);

        // A mouse, while the window edits the keyboard: said so, and its effects decide the row.
        forget_claims();
        let _mouse = crate::fake::plug("/dev/fake-ui-mouse", "g903-hero", crate::fake::g903());
        scratch_profile("T mouse", [1, 1, 1]);
        let mut w = Window::hosting(Some("/dev/fake-ui-mouse"));
        w.click("T mouse");
        w.until("the mouse connects", |w| w.app.connected_zones().is_some());
        w.run(1);
        assert!(noticed(&w, "The connected keyboard is a Logitech G903 HERO"));
        assert!(w.has("WAVE") && !w.enabled("WAVE") && w.enabled("BREATHE"));
        w.click("DEVICE");
        w.run(1);
        assert!(!w.nodes.iter().any(|n| n.label.ends_with(" HZ")), "no report rates to set on this mouse");
        profile::delete("T mouse").unwrap();
        forget_claims();
    }

    #[test]
    fn the_window_icon_is_the_mark_at_128_pixels() {
        let icon = window_icon();
        assert_eq!((icon.width, icon.height, icon.rgba.len()), (128, 128, 128 * 128 * 4));
    }

}
