//! loginux: RGB lighting for Logitech HID++ keyboards and mice on Linux.

mod claude;
mod device;
mod effects;
mod engine;
#[cfg(test)]
mod fake;
mod hidpp;
mod icon;
mod ipc;
mod layout;
mod notify;
mod pixelfont;
mod profile;
#[cfg(test)]
mod testenv;
mod tray;
mod ui;

use device::Keyboard;
use engine::{STOP, TakeError};
use ipc::{Control, Controls, Device, RateChange, Request, Status};
use layout::{Kind, Model};
use profile::{Profile, hex, parse_rgb};
use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant};

const USAGE: &str = "\
loginux: RGB lighting for Logitech HID++ keyboards and mice (built in: G316 X 98, G903 HERO)

Usage:
  loginux [ui]                       open the editor window
  loginux run [--refresh SECS]       keep the chosen profile applied (the user service runs this)
  loginux status                     keyboard, lighting control and engine state
  loginux zones                      key names, light-bar segments and unknown zones
  loginux use PROFILE | --none       choose the profile loginux keeps applied
  loginux apply PROFILE [--hold SECS | --keep]
                                  paint a profile, hold it (default 10 s), then hand the lighting back
  loginux paint ZONE=RRGGBB... [--base RRGGBB] [--hold SECS | --keep]
                                  paint keys (esc, f1, kp5, leftshift), bar1..bar30 or zone IDs
  loginux release                    hand the lighting back to the keyboard's onboard effects
  loginux rate [HZ]                  show the report rate, or change and keep it (the keyboard may reconnect)
  loginux effect CLUSTER=EFFECT[,param=value]... [--hold SECS | --keep]
                                     run a built-in effect (static, cycle, breathe, wave, ripple)
                                     on a lighting cluster, by number or light name
  loginux claude --hook              show Claude Code's status on the light bar (its hooks run this)
  loginux claude --enable | --disable
                                     connect Claude Code to the light bar, or disconnect it: its
                                     plugin and its status line (what the editor's switch does)
  loginux claude --statusline [-- COMMAND...]
                                     Claude Code's status line: the context and usage limits for
                                     the light bar, then COMMAND, the status line it had
  loginux claude EVENT [--session ID]
                                     the same by hand: start, prompt, working, tool, compact,
                                     asking, done, waiting, error, limit or end
  loginux probe --device /dev/hidrawN
                                  read-only: print a starting definition for a new device

Options:
  --device /dev/hidrawN           use this node; it must be a known device's HID++ interface

Profiles are ~/.config/loginux/profiles/NAME.toml. Lighting goes to the keyboard's RAM
only, and unplugging the keyboard always restores its onboard lighting.
";

/// How long `loginux rate` waits for the engine to confirm a change, and for a
/// keyboard it changed itself to reconnect after a pause. Tests wait less.
const CONFIRM: Duration = if cfg!(test) { Duration::from_millis(600) } else { Duration::from_secs(30) };
const RECONNECT: Duration = if cfg!(test) { Duration::from_millis(300) } else { Duration::from_secs(20) };
const PAUSE: Duration = if cfg!(test) { Duration::from_millis(10) } else { Duration::from_secs(1) };

fn main() -> ExitCode {
    match cli(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("loginux: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cli(mut args: Vec<String>) -> Result<(), String> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return Ok(());
    }
    let device = take_value(&mut args, "--device")?.map(PathBuf::from);
    let command = if args.is_empty() { "ui".to_string() } else { args.remove(0) };
    // Not in tests, where a closed pipe must not end the whole run.
    if matches!(command.as_str(), "zones" | "status" | "probe") && !cfg!(test) {
        // SAFETY: restores the default action before any thread exists; `loginux zones | head` then just ends.
        unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    }
    match command.as_str() {
        "ui" => no_more(&args).and_then(|_| ui::run(device)),
        "run" => run(device, &mut args),
        "status" => no_more(&args).and_then(|_| status(device)),
        "zones" => no_more(&args).map(|_| zones()),
        "use" => choose(&args),
        "apply" | "paint" => paint(device, &command, &mut args),
        "release" => no_more(&args).and_then(|_| release(device)),
        "probe" => no_more(&args).and_then(|_| probe(device)),
        "rate" => rate(device, &args),
        "effect" => effect(device, &mut args),
        "claude" => claude(&mut args),
        other => Err(format!("unknown command {other:?}\n\n{USAGE}")),
    }
}

fn take_value(args: &mut Vec<String>, flag: &str) -> Result<Option<String>, String> {
    let Some(i) = args.iter().position(|a| a == flag) else { return Ok(None) };
    if i + 1 >= args.len() {
        return Err(format!("{flag} needs a value"));
    }
    args.remove(i);
    Ok(Some(args.remove(i)))
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    let found = args.iter().position(|a| a == flag).map(|i| args.remove(i));
    found.is_some()
}

fn no_more(args: &[String]) -> Result<(), String> {
    match args.first() {
        Some(a) => Err(format!("unexpected argument {a:?}; see loginux --help")),
        None => Ok(()),
    }
}

fn seconds(text: Option<String>, default: f64) -> Result<f64, String> {
    match text {
        Some(t) => t.parse().ok().filter(|s: &f64| s.is_finite() && *s >= 0.0).ok_or_else(|| format!("{t:?} is not a number of seconds")),
        None => Ok(default),
    }
}

fn engine_busy() -> String {
    let pid = engine::owner().map(|p| format!(" (pid {p})")).unwrap_or_default();
    format!(
        "another loginux{pid} owns the keyboard. Use `loginux use`, the editor, or \
         `systemctl --user stop loginux` first"
    )
}

fn open(device: Option<PathBuf>) -> Result<Keyboard, String> {
    Keyboard::find(device.as_deref(), layout::current()).map_err(|e| e.to_string())
}

fn run(device: Option<PathBuf>, args: &mut Vec<String>) -> Result<(), String> {
    let refresh = take_value(args, "--refresh")?.map(|s| seconds(Some(s), 0.0)).transpose()?;
    no_more(args)?;
    engine::install_signal_handlers();
    let lock = match engine::lock(false)? {
        Some(lock) => lock,
        None => {
            eprintln!("loginux: {}; waiting for it to exit", engine_busy());
            match engine::lock(true)? {
                Some(lock) => lock,
                None => return Ok(()), // stopped while waiting
            }
        }
    };
    let opts = engine::Options { device, refresh: refresh.filter(|&s| s > 0.0).map(Duration::from_secs_f64) };
    // The tray's Quit sets `stop`; the engine then hands the lighting back and returns.
    let stop = Arc::new(AtomicBool::new(false));
    tray::spawn(stop.clone());
    engine::run(opts, false, stop, lock)
}

fn describe(status: &Status) -> String {
    let device = match &status.device {
        Device::Missing => "not connected".to_string(),
        Device::NoAccess { error } => error.clone(),
        Device::Connected { name, path, protocol, layout, zones, .. } => {
            let layout = layout.map_or("not reported".into(), |c| format!("{c:#04x}"));
            format!("{name} on {path}, HID++ {protocol}, layout code {layout}, {} zones", zones.len())
        }
        Device::Failed { error } => format!("error: {error}"),
    };
    let control = match &status.control {
        Control::Onboard => "onboard lighting (loginux is not painting)".to_string(),
        Control::Waiting => "waiting for the keyboard".to_string(),
        Control::Profile { name } => format!("showing profile {name:?}"),
        Control::Preview => "showing an unsaved edit from the editor".to_string(),
        Control::Check { zone } => format!("channel check, zone {zone} lit"),
        Control::Conflict { mode, flags } => {
            format!("another program holds the lighting (software control {mode:02x} {flags:02x}); take over in the editor")
        }
    };
    let owner = if status.hosted { "inside an editor window" } else { "as a service" };
    let mut out = format!(
        "keyboard: {device}\nlighting: {control}\nchosen profile: {}\nengine: pid {} {owner}\n",
        status.active.as_deref().unwrap_or("none"),
        status.pid
    );
    if let Some(c) = &status.controls {
        out += &controls_text(c);
    }
    match &status.rate_change {
        Some(RateChange::Waiting { hz }) => out += &format!("report-rate change: waiting for the keyboard to confirm {hz} Hz\n"),
        Some(RateChange::Failed { hz, error }) => out += &format!("report-rate change to {hz} Hz failed: {error}\n"),
        Some(RateChange::Done { .. }) | None => {}
    }
    if !status.claude.is_empty() {
        let states: Vec<String> = status.claude.iter().map(|s| format!("{s:?}").to_lowercase()).collect();
        out += &format!("claude code: {}\n", states.join(", "));
    }
    if let Some(m) = &status.message {
        out += &format!("last message: {m}\n");
    }
    out
}

fn controls_text(c: &Controls) -> String {
    let brightness = c.brightness.map_or("not reported".into(), |[now, max]| format!("{now} of {max} (read only)"));
    let list = |rates: &[u32]| rates.iter().map(|r| format!("{r}")).collect::<Vec<_>>().join(", ");
    let rate = match c.report_rate {
        Some(hz) => {
            let refused = if c.report_rate_refused.is_empty() { String::new() } else { format!(" ({} refused by the keyboard)", list(&c.report_rate_refused)) };
            let kept = c.report_rate_kept.filter(|k| !c.report_rate_refused.contains(k));
            let kept = kept.map_or_else(String::new, |k| format!("; Loginux sets it back to {k} Hz after a reconnect"));
            format!("{hz} Hz; supports {}{refused}{kept}", list(&c.report_rates))
        }
        None => "not reported".into(),
    };
    let game = c.game_mode.map_or("not reported", |on| if on { "on (read only)" } else { "off (read only)" });
    format!("brightness: {brightness}\nreport rate: {rate}\ngame mode: {game}\n")
}

fn status(device: Option<PathBuf>) -> Result<(), String> {
    if let Ok(status) = ipc::request(&Request::Status) {
        print!("{}", describe(&status));
        return Ok(());
    }
    let Some(_lock) = engine::lock(false)? else { return Err(engine_busy()) };
    println!("engine: not running");
    println!("chosen profile: {}", profile::active().unwrap_or_else(|| "none".into()));
    let mut kb = open(device)?;
    let e = |e: hidpp::Error| e.to_string();
    let (major, minor) = kb.protocol().map_err(e)?;
    let layout = kb.layout_code().map_err(e)?;
    let statics = kb.static_effects().map_err(e)?;
    let zones = engine::zones(&mut kb, &statics).map_err(e)?;
    let (mode, flags) = kb.sw_control().map_err(e)?;
    let (controls, _) = engine::read_controls(&mut kb).map_err(e)?;
    let model = kb.model;
    println!("keyboard: {} on {}, HID++ {major}.{minor}, layout code {layout:02x?}, {} zones", model.name, kb.path.display(), zones.len());
    let owner = if mode & 1 == 0 { "the keyboard's onboard effects" } else { "a host program" };
    println!("software control: {mode:02x} {flags:02x} (lighting run by {owner})");
    let missing: Vec<_> = model.zones.iter().filter(|z| !zones.contains(&z.id)).map(|z| z.id).collect();
    if !missing.is_empty() {
        println!("zones in the {} definition that this keyboard does not report: {missing:?}", model.id);
    }
    let extra: Vec<_> = zones.iter().filter(|id| model.zone(**id).is_none()).collect();
    if !extra.is_empty() {
        println!("zones this keyboard reports that the definition lacks: {extra:?}");
    }
    print!("{}", controls_text(&controls));
    Ok(())
}

fn zones() {
    let model = layout::current();
    let checks = profile::load_checks(model);
    let seen = |id: u8| checks.get(&id).map(|s| format!(" (checked {}: {})", s.date, s.saw)).unwrap_or_default();
    let list = |title: String, ids: Vec<u8>| {
        println!("{title}:");
        for z in ids.iter().filter_map(|id| model.zone(*id)) {
            let name = if z.name.is_empty() { "-" } else { z.name.as_str() };
            println!("  {name:<12} zone {:>3}{}", z.id, seen(z.id));
        }
    };
    println!("{} (definition {}, {} zones)", model.name, model.id, model.zones.len());
    list("Keys, by Linux key name".into(), model.ids(Kind::Key));
    for strip in &model.strips {
        let title = format!("{}: {name}1..{name}{} in the definition's order (unverified until checked)", strip.title, strip.zones.len(), name = strip.name);
        list(title, strip.zones.clone());
    }
    list("Unknown zones: no known key; set them by number under [zones]".into(), model.unknown());
}

fn choose(args: &[String]) -> Result<(), String> {
    let name = match args {
        [flag] if flag == "--none" => None,
        [name] => Some(name.clone()),
        _ => return Err("usage: loginux use PROFILE | --none".into()),
    };
    if let Some(name) = &name {
        profile::load(layout::current(), name)?;
    }
    match ipc::request(&Request::Use { profile: name.clone() }) {
        Ok(status) => print!("{}", describe(&status)),
        Err(ipc::Error::Refused(e)) => return Err(e),
        Err(ipc::Error::Unreachable(_)) => {
            profile::set_active(name.as_deref())?;
            match &name {
                Some(n) => println!("loginux will keep {n:?} applied whenever its engine runs (the service or the editor)"),
                None => println!("no profile chosen; loginux leaves the keyboard's onboard lighting alone"),
            }
        }
    }
    Ok(())
}

fn paint(device: Option<PathBuf>, command: &str, args: &mut Vec<String>) -> Result<(), String> {
    let keep = take_flag(args, "--keep");
    let hold = take_value(args, "--hold")?;
    if keep && hold.is_some() {
        return Err("--hold and --keep exclude each other".into());
    }
    let hold = if keep { None } else { Some(seconds(hold, 10.0)?) };
    // Everything is validated before the keyboard is touched.
    let Some(_lock) = engine::lock(false)? else { return Err(engine_busy()) };
    let mut kb = open(device)?;
    let model = kb.model;
    let profile = if command == "apply" {
        match args.as_slice() {
            [arg] => profile::load_arg(model, arg)?,
            _ => return Err("usage: loginux apply PROFILE [--hold SECS | --keep]".into()),
        }
    } else {
        assignments(model, args)?
    };
    engine::install_signal_handlers();
    let e = |e: hidpp::Error| e.to_string();
    let statics = kb.static_effects().map_err(e)?;
    let zones = engine::zones(&mut kb, &statics).map_err(e)?;
    let frame = profile.frame(model, &zones)?;
    let held = engine::take(&mut kb, false).map_err(|t| match t {
        TakeError::Conflict(m, f) => format!(
            "another program holds the lighting (software control {m:02x} {f:02x}); \
             stop it, or run `loginux release` to hand the lighting back to the keyboard"
        ),
        TakeError::Hid(err) => err.to_string(),
    })?;
    let painted = engine::paint(&mut kb, &statics, profile.base("keys"), &frame).map_err(e);
    if painted.is_ok() {
        println!("painted {} zones", frame.len());
        let Some(secs) = hold else {
            println!("holding; `loginux release` hands the lighting back");
            return Ok(());
        };
        let end = Instant::now() + Duration::from_secs_f64(secs);
        while Instant::now() < end && !STOP.load(Ordering::SeqCst) {
            sleep(Duration::from_millis(100));
        }
    }
    let after = engine::give_back(&mut kb, held.restore).map_err(e)?;
    println!("lighting handed back to the keyboard (software control now {:02x} {:02x})", after.0, after.1);
    painted
}

/// `esc=ff0000 bar3=00ff00 154=ffffff` plus `--base` for every other key and segment.
fn assignments(model: &Model, args: &mut Vec<String>) -> Result<Profile, String> {
    let base = take_value(args, "--base")?.map(|b| parse_rgb(&b)).transpose()?.unwrap_or_default();
    if args.is_empty() {
        return Err("usage: loginux paint ZONE=RRGGBB... [--base RRGGBB] [--hold SECS | --keep]".into());
    }
    let mut p = Profile::default();
    for group in model.groups() {
        p.bases.insert(group.to_string(), base);
    }
    for arg in args.iter() {
        let (key, colour) = arg.split_once('=').ok_or_else(|| format!("{arg:?} is not ZONE=RRGGBB"))?;
        let key = key.to_lowercase();
        let id = match layout::parse_id(&key) {
            Some(id) => model.zone(id).map(|z| z.id).ok_or_else(|| format!("zone {id} is not one of the {}'s zones", model.name))?,
            None => model.named(&key).map(|z| z.id).ok_or_else(|| model.why_not(&key))?,
        };
        if p.zones.insert(id, parse_rgb(colour)?).is_some() {
            return Err(format!("zone {id} is given twice"));
        }
    }
    let named: Vec<String> = p.zones.iter().map(|(id, c)| format!("{id}={}", hex(*c))).collect();
    println!("zones {}; other keys and strip segments {}", named.join(" "), hex(base));
    Ok(p)
}

fn release(device: Option<PathBuf>) -> Result<(), String> {
    if let Ok(status) = ipc::request(&Request::Use { profile: None }) {
        print!("{}", describe(&status));
        return Ok(());
    }
    let Some(_lock) = engine::lock(false)? else { return Err(engine_busy()) };
    let mut kb = open(device)?;
    let (mode, flags) = engine::release_now(&mut kb).map_err(|e| e.to_string())?;
    println!("lighting handed back to the keyboard (software control now {mode:02x} {flags:02x})");
    Ok(())
}

/// Read-only look at any hidraw node, printed as a starting keyboard definition.
fn probe(device: Option<PathBuf>) -> Result<(), String> {
    let Some(path) = device else { return Err("usage: loginux probe --device /dev/hidrawN".into()) };
    // A device Loginux already drives belongs to the engine; anything else has no owner to wait for.
    let _lock = match hidpp::identify(&path, layout::models()) {
        Ok(_) => Some(engine::lock(false)?.ok_or_else(engine_busy)?),
        Err(_) => None,
    };
    let p = device::probe(&path).map_err(|e| e.to_string())?;
    eprintln!(
        "{}: USB {:04x}:{:04x} interface {}, HID++ {}.{}, 0x8071 {}, 0x8081 {}, {} zones, layout code {}",
        path.display(),
        p.vendor,
        p.product,
        p.interface,
        p.protocol.0,
        p.protocol.1,
        if p.rgb_effects { "yes" } else { "no" },
        if p.per_key { "yes" } else { "no" },
        p.zones.len(),
        p.layout.map_or("not reported".into(), |c| format!("{c:#04x}"))
    );
    eprintln!("features (index, ID, name, type flags, version):");
    for (i, id, kind, version) in &p.features {
        eprintln!("  {i:>2}  {id:#06x}  {:<28} {kind:#04x}  v{version}", device::feature_name(*id));
    }
    for line in &p.lighting {
        eprintln!("{line}");
    }
    if p.rgb_effects && !p.per_key && p.clusters.iter().any(|c| c.1 == 0) {
        print!("{}", cluster_definition(&p));
        return Ok(());
    }
    if !(p.rgb_effects && p.per_key) {
        eprintln!("no starting definition: Loginux needs 0x8071 RGB effects, with 0x8081 per-key lighting or a lighting cluster");
        return Ok(());
    }
    // 0x8081 zones follow HID keyboard usages minus 3 (modifiers 104-111) on the
    // keyboards seen so far; these names are guesses for a channel check to confirm.
    const USAGES: &str = "a b c d e f g h i j k l m n o p q r s t u v w x y z 1 2 3 4 5 6 7 8 9 0 enter esc \
        backspace tab space minus equal leftbrace rightbrace backslash nonushash semicolon apostrophe grave \
        comma dot slash capslock f1 f2 f3 f4 f5 f6 f7 f8 f9 f10 f11 f12 sysrq scrolllock pause insert home \
        pageup delete end pagedown right left down up numlock kpslash kpasterisk kpminus kpplus kpenter kp1 \
        kp2 kp3 kp4 kp5 kp6 kp7 kp8 kp9 kp0 kpdot";
    const MODIFIERS: [&str; 8] = ["leftctrl", "leftshift", "leftalt", "leftmeta", "rightctrl", "rightshift", "rightalt", "rightmeta"];
    let usages: Vec<&str> = USAGES.split_whitespace().collect();
    let guess = |z: u8| match z {
        1..=96 => usages.get(usize::from(z) - 1).copied(),
        104..=111 => Some(MODIFIERS[usize::from(z - 104)]),
        _ => None,
    };
    let (named, unknown): (Vec<u8>, Vec<u8>) = p.zones.iter().partition(|&&z| guess(z).is_some());
    let mut keys = String::new();
    for (i, z) in named.iter().enumerate() {
        let (x, y) = (i % 16, i / 16);
        keys += &format!("  {{ name = \"{}\", zone = {z}, at = [{x}, {y}] }},\n", guess(*z).unwrap_or_default());
    }
    let rows = named.len().div_ceil(16).max(1);
    let name = if p.hid_name.is_empty() { "New keyboard".to_string() } else { p.hid_name.clone() };
    print!(
        "# Starting definition from `loginux probe` ({}). Key names are guesses from the
# 0x8081 zone convention and the positions are a placeholder grid: run a channel
# check, then fix names and positions. See CONTRIBUTING.md.
name = \"{name}\"
protocol = \"hidpp20\"
vendor = {:#06x}
product = {:#06x}
interface = {}
board = [16, {rows}]

# Zones the keyboard reports with no guessed name.
unknown = {unknown:?}

keys = [
{keys}]
",
        profile::today(),
        p.vendor,
        p.product,
        p.interface,
    );
    Ok(())
}

/// A starting definition for a device without 0x8081: one light per lighting
/// cluster, in a row. A status display is left out, as the G903's DPI LEDs are.
fn cluster_definition(p: &device::Probe) -> String {
    let name = if p.hid_name.is_empty() { "New device".to_string() } else { p.hid_name.clone() };
    let (mut lights, mut notes, mut x) = (String::new(), String::new(), 0);
    for &(cluster, modes) in &p.clusters {
        if modes != 0 {
            notes += &format!("# Cluster {cluster} is a status display (event modes {modes:02x}) whose firmware picks its LEDs; it is left out.\n");
            continue;
        }
        lights += &format!("  {{ name = \"light{cluster}\", zone = {cluster}, at = [{x}, 0] }},\n");
        x += 2;
    }
    format!(
        "# Starting definition from `loginux probe` ({}). One light per 0x8071 cluster;
# the names and positions are placeholders: light each one, then name and place
# it. See CONTRIBUTING.md, Cluster devices.
{notes}name = \"{name}\"
protocol = \"hidpp20\"
lighting = \"clusters\"
vendor = {:#06x}
product = {:#06x}
interface = {}
board = [{x}, 1]

lights = [
{lights}]
",
        profile::today(),
        p.vendor,
        p.product,
        p.interface,
    )
}

/// `loginux rate` shows the report rate; `loginux rate HZ` changes it, once, at the
/// user's request. The keyboard can re-enumerate, so the result is a readback.
fn rate(device: Option<PathBuf>, args: &[String]) -> Result<(), String> {
    let hz = match args {
        [] => None,
        [hz] => Some(hz.trim_end_matches("Hz").trim().parse::<u32>().map_err(|_| format!("{hz:?} is not a rate in Hz"))?),
        _ => return Err("usage: loginux rate [HZ]".into()),
    };
    if let Ok(status) = ipc::request(&Request::Status) {
        let Some(hz) = hz else {
            print!("{}", status.controls.as_ref().map(controls_text).unwrap_or_else(|| "keyboard not connected\n".into()));
            return Ok(());
        };
        ipc::request(&Request::SetReportRate { hz })?;
        let end = Instant::now() + CONFIRM;
        while Instant::now() < end {
            match ipc::request(&Request::Status).map(|s| s.rate_change) {
                Ok(Some(RateChange::Done { hz })) => {
                    println!("report rate is now {hz} Hz (read back from the keyboard)");
                    return Ok(());
                }
                Ok(Some(RateChange::Failed { error, .. })) => return Err(format!("report-rate change: {error}")),
                _ => sleep(Duration::from_millis(250)),
            }
        }
        return Err(format!("the engine did not confirm the change within {} s; `loginux status` shows its state", CONFIRM.as_secs()));
    }
    // No engine: this process owns the keyboard for the change.
    let Some(_lock) = engine::lock(false)? else { return Err(engine_busy()) };
    let e = |e: hidpp::Error| e.to_string();
    let mut kb = open(device.clone())?;
    let (controls, rates) = engine::read_controls(&mut kb).map_err(e)?;
    let Some(hz) = hz else {
        print!("{}", controls_text(&controls));
        return Ok(());
    };
    let (supported, current) = rates.ok_or("this keyboard does not report a report rate (0x8061)")?;
    let index = device::RATES_HZ
        .iter()
        .position(|&r| r == hz)
        .map(|i| i as u8)
        .filter(|i| supported.contains(i))
        .ok_or_else(|| format!("{hz} Hz is not a rate this keyboard reports"))?;
    if index == current {
        let _ = profile::set_kept_rate(kb.model, hz);
        println!("report rate is already {hz} Hz");
        return Ok(());
    }
    match kb.set_report_rate(index) {
        Ok(()) | Err(hidpp::Error::Gone(_)) | Err(hidpp::Error::Timeout(_)) => {}
        Err(err) => return Err(err.to_string()),
    }
    drop(kb);
    println!("sent {hz} Hz; waiting for the keyboard to reconnect");
    sleep(PAUSE);
    let end = Instant::now() + RECONNECT;
    while Instant::now() < end {
        if let Ok(mut kb) = open(device.clone()) {
            let (_, rates) = engine::read_controls(&mut kb).map_err(e)?;
            return match rates.map(|r| r.1) {
                Some(i) if i == index => {
                    let _ = profile::set_kept_rate(kb.model, hz);
                    println!("report rate is now {hz} Hz (read back from the keyboard)");
                    Ok(())
                }
                Some(i) => Err(format!("the keyboard reports rate index {i}, not {hz} Hz")),
                None => Err("the keyboard did not report its rate after the change".into()),
            };
        }
        sleep(Duration::from_millis(250));
    }
    Err(format!("the keyboard did not come back within {} s; replug it, then run `loginux rate`", RECONNECT.as_secs()))
}

/// How long a hook waits for the engine; Claude Code waits for the hook.
const HOOK_WAIT: Duration = Duration::from_secs(1);

/// `loginux claude --hook`, which Claude Code's hooks run with the event as JSON
/// on stdin, or `loginux claude EVENT [--session ID]` by hand. A hook never
/// fails and writes nothing to stdout, which Claude Code reads into the
/// conversation for some events; with no engine running there is nothing to
/// show, and the reason goes to stderr, which Claude Code only logs.
fn claude(args: &mut Vec<String>) -> Result<(), String> {
    let session = take_value(args, "--session")?;
    for (flag, on) in [("--enable", true), ("--disable", false)] {
        if take_flag(args, flag) {
            no_more(args)?;
            // On only once Claude Code is connected; off at once, whatever Claude Code says.
            if on {
                claude::set_up(true)?;
                profile::set_claude(true)?;
            } else {
                profile::set_claude(false)?;
                claude::set_up(false)?;
            }
            println!("{}", if on { "Claude Code's status shows on the light bar" } else { "Claude Code's status is off; your own status line is back" });
            return Ok(());
        }
    }
    if take_flag(args, "--statusline") {
        return match args.split_first() {
            None => status_line(&[]),
            Some((dashes, command)) if dashes == "--" => status_line(command),
            Some(_) => Err("usage: loginux claude --statusline [-- COMMAND...]".into()),
        };
    }
    if take_flag(args, "--hook") {
        no_more(args)?;
        let mut json = String::new();
        let _ = std::io::stdin().read_to_string(&mut json);
        let entrypoint = std::env::var("CLAUDE_CODE_ENTRYPOINT").ok();
        let pid = std::env::var("CLAUDE_PID").ok().and_then(|p| p.parse().ok());
        let running = claude::background_agents(&json);
        if let Some((session, signal)) = claude::from_hook(&json, entrypoint.as_deref(), pid)
            && let Err(e) = ipc::request_within(&Request::Claude { session, signal, pid, running }, HOOK_WAIT)
        {
            eprintln!("loginux: {e}");
        }
        return Ok(());
    }
    let [name] = args.as_slice() else { return Err("usage: loginux claude --hook | EVENT [--session ID]".into()) };
    let signal = claude::Signal::from_name(name)?;
    ipc::request(&Request::Claude { session: session.unwrap_or_else(|| "manual".into()), signal, pid: None, running: vec![] })?;
    Ok(())
}

/// `loginux claude --statusline [-- COMMAND...]`, which Claude Code runs as its
/// status line: tells the engine the instance's context and the usage limits
/// from the JSON on stdin, and runs COMMAND, the status line the user had, on
/// the same JSON, so what it prints is the status line. Never fails, like a hook.
fn status_line(command: &[String]) -> Result<(), String> {
    let mut json = String::new();
    let _ = std::io::stdin().read_to_string(&mut json);
    let mut child = command.split_first().and_then(|(program, rest)| {
        let spawned = std::process::Command::new(program).args(rest).stdin(std::process::Stdio::piped()).spawn();
        spawned.map_err(|e| eprintln!("loginux: {program}: {e}")).ok()
    });
    if let Some(mut input) = child.as_mut().and_then(|c| c.stdin.take()) {
        let _ = std::io::Write::write_all(&mut input, json.as_bytes()); // closed on drop: the end of its input
    }
    let entrypoint = std::env::var("CLAUDE_CODE_ENTRYPOINT").ok();
    let pid = std::env::var("CLAUDE_PID").ok().and_then(|p| p.parse().ok());
    if let Some((session, usage)) = claude::from_status_line(&json, entrypoint.as_deref(), pid)
        && let Err(e) = ipc::request_within(&Request::ClaudeUsage { session, usage, pid }, HOOK_WAIT)
    {
        eprintln!("loginux: {e}");
    }
    if let Some(mut child) = child {
        let _ = child.wait();
    }
    Ok(())
}

/// `loginux effect 0=wave logo=breathe,color=ff0000 --hold 10`: built-in effects
/// on lighting clusters, in RAM. Clusters not named are left as they are.
fn effect(device: Option<PathBuf>, args: &mut Vec<String>) -> Result<(), String> {
    let keep = take_flag(args, "--keep");
    let hold = take_value(args, "--hold")?;
    if keep && hold.is_some() {
        return Err("--hold and --keep exclude each other".into());
    }
    let hold = if keep { None } else { Some(seconds(hold, 10.0)?) };
    if args.is_empty() {
        return Err("usage: loginux effect CLUSTER=EFFECT[,param=value]... [--hold SECS | --keep]".into());
    }
    let Some(_lock) = engine::lock(false)? else { return Err(engine_busy()) };
    let mut kb = open(device)?;
    let e = |e: hidpp::Error| e.to_string();
    let lists = kb.effect_ids().map_err(e)?;
    let mut plan = vec![];
    for arg in args.iter() {
        let (target, text) = arg.split_once('=').ok_or_else(|| format!("{arg:?} is not CLUSTER=EFFECT"))?;
        // A light name on a clusters device is its cluster; otherwise the cluster number.
        let cluster = match target.parse::<u8>() {
            Ok(c) => c,
            Err(_) => kb.model.named(target).filter(|_| !kb.model.per_key).map(|z| z.id).ok_or_else(|| {
                format!("{target:?} is not a cluster number or a light of the {}", kb.model.name)
            })?,
        };
        let ids = lists.get(usize::from(cluster)).ok_or_else(|| format!("the device has {} clusters", lists.len()))?;
        let (id, params) = effects::Effect::parse(text)?.encode(ids)?;
        let index = ids.iter().position(|&i| i == id).expect("encode picks a listed ID") as u8;
        plan.push((cluster, index, id, params));
    }
    engine::install_signal_handlers();
    let held = engine::take(&mut kb, false).map_err(|t| match t {
        TakeError::Conflict(m, f) => format!("another program holds the lighting (software control {m:02x} {f:02x})"),
        TakeError::Hid(err) => err.to_string(),
    })?;
    let result = plan.iter().try_for_each(|&(cluster, index, _, params)| kb.set_effect(cluster, index, params).map_err(e));
    if result.is_ok() {
        for (cluster, _, id, params) in &plan {
            println!("cluster {cluster}: effect {id:#04x}, parameters {}", params.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" "));
        }
        let Some(secs) = hold else {
            println!("holding; `loginux release` hands the lighting back");
            return Ok(());
        };
        let end = Instant::now() + Duration::from_secs_f64(secs);
        while Instant::now() < end && !STOP.load(Ordering::SeqCst) {
            sleep(Duration::from_millis(100));
        }
    }
    let after = engine::give_back(&mut kb, held.restore).map_err(e)?;
    println!("lighting handed back to the device (software control now {:02x} {:02x})", after.0, after.1);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake;
    use crate::ipc::Reply;
    use crate::testenv::{self, stand_in};

    fn args(text: &str) -> Vec<String> {
        text.split_whitespace().map(String::from).collect()
    }

    /// One command line, as `main` runs it. Never `ui`, which opens a window.
    fn run(text: &str) -> Result<(), String> {
        cli(args(text))
    }

    /// An engine's answer, for a stand-in engine.
    fn reply(controls: Option<Controls>, rate_change: Option<RateChange>) -> Reply {
        Reply::Ok(Status {
            device: Device::Missing,
            control: Control::Onboard,
            active: None,
            message: None,
            pid: 7,
            hosted: false,
            controls,
            rate_change,
            claude: vec![],
        })
    }

    fn rates() -> Option<Controls> {
        Some(Controls { report_rate: Some(1000), report_rates: vec![1000, 2000], ..Controls::default() })
    }

    fn busy(text: &str) {
        let held = engine::lock(false).unwrap().expect("free");
        let error = run(text).unwrap_err();
        assert!(error.contains(&format!("another loginux (pid {})", std::process::id())), "{text}: {error}");
        drop(held);
    }

    fn forget_claims() {
        for id in ["g316-x-98", "g903-hero"] {
            let _ = std::fs::remove_file(ipc::runtime_dir().unwrap().join(format!("claim-{id}")));
        }
    }

    #[test]
    fn the_command_line_dispatches_and_refuses_strays() {
        let _serial = testenv::serial();
        run("--help").unwrap();
        assert!(run("bogus").unwrap_err().starts_with("unknown command \"bogus\""));
        assert!(run("status --device").unwrap_err().contains("needs a value"));
        for stray in ["ui x", "run x", "status x", "zones x", "release x", "probe x"] {
            assert!(run(stray).unwrap_err().contains("unexpected argument \"x\""), "{stray}");
        }
        assert!(run("run --refresh soon").unwrap_err().contains("not a number of seconds"));
        run("zones").unwrap();
    }

    #[test]
    fn status_comes_from_the_engine_or_the_keyboard() {
        let _serial = testenv::serial();
        let engine = stand_in(vec![reply(None, None)]);
        run("status").unwrap();
        assert!(matches!(engine.join().unwrap()[..], [Request::Status]));
        busy("status");
        // Read from the keyboard: Esc unreported, and zone 250 unknown to the definition.
        let mut spec = fake::g316();
        spec.zones.retain(|&z| z != 38);
        spec.zones.push(250);
        let _kb = fake::plug("/dev/fake-status", "g316-x-98", spec);
        run("status --device /dev/fake-status").unwrap();
        assert!(run("status --device /dev/fake-status").unwrap_err().starts_with("cannot open"), "used up");
    }

    #[test]
    fn use_chooses_through_the_engine_or_config() {
        let _serial = testenv::serial();
        assert!(run("use").unwrap_err().starts_with("usage"));
        assert!(run("use T-absent").unwrap_err().contains("T-absent"));
        let engine = stand_in(vec![reply(None, None), Reply::Err("refused for the test".into())]);
        run("use Wave").unwrap();
        assert_eq!(run("use --none").unwrap_err(), "refused for the test");
        engine.join().unwrap();
        // No engine: config.toml keeps the choice for the next one.
        run("use Wave").unwrap();
        assert_eq!(profile::active().as_deref(), Some("Wave"));
        run("use --none").unwrap();
        assert_eq!(profile::active(), None);
    }

    #[test]
    fn paint_and_apply_claim_paint_and_hand_back() {
        let _serial = testenv::serial();
        forget_claims();
        let kb = fake::plug("/dev/fake-paint", "g316-x-98", fake::g316());
        run("paint esc=ff0000 --keep --device /dev/fake-paint").unwrap();
        {
            let st = kb.state();
            assert_eq!((st.sw, st.shown[&38]), ((1, 0), [255, 0, 0]), "kept");
        }
        let mut green = Profile::default();
        green.zones.insert(38, [0, 255, 0]);
        profile::save(layout::find("g316-x-98").unwrap(), "T-cli", &green).unwrap();
        let kb = fake::plug("/dev/fake-apply", "g316-x-98", fake::g316());
        run("apply T-cli --hold 0.05 --device /dev/fake-apply").unwrap();
        {
            let st = kb.state();
            assert_eq!((st.sw, st.shown[&38]), ((0, 0), [0, 255, 0]), "shown, then handed back");
        }
        profile::delete("T-cli").unwrap();
        assert!(run("paint esc=ff0000 --keep --hold 1").unwrap_err().contains("exclude"));
        let _kb = fake::plug("/dev/fake-apply-usage", "g316-x-98", fake::g316());
        assert!(run("apply --keep --device /dev/fake-apply-usage").unwrap_err().starts_with("usage"));
        let kb = fake::plug("/dev/fake-paint-held", "g316-x-98", fake::g316());
        kb.state().sw = (1, 5);
        let error = run("paint esc=ff0000 --keep --device /dev/fake-paint-held").unwrap_err();
        assert!(error.contains("another program holds the lighting (software control 01 05)"), "{error}");
        let kb = fake::plug("/dev/fake-paint-refused", "g316-x-98", fake::g316());
        kb.state().refuse = Some(Box::new(|feature, function, _| (feature == 0x8081 && function == 1).then_some(4)));
        assert!(run("paint esc=ff0000 --hold 0 --device /dev/fake-paint-refused").unwrap_err().contains("HID++ error 0x04"));
        assert_eq!(kb.state().sw, (0, 0), "handed back after the failed frame");
        let kb = fake::plug("/dev/fake-paint-failing", "g316-x-98", fake::g316());
        kb.state().refuse = Some(Box::new(|feature, function, _| ((feature, function) == (0x8071, 5)).then_some(4)));
        assert!(run("paint esc=ff0000 --keep --device /dev/fake-paint-failing").unwrap_err().contains("HID++ error 0x04"));
        busy("paint esc=ff0000 --keep");
        forget_claims();
        testenv::default_signals();
    }

    #[test]
    fn effects_run_on_clusters_by_number_or_light_name() {
        let _serial = testenv::serial();
        forget_claims();
        let mouse = fake::plug("/dev/fake-fx-logo", "g903-hero", fake::g903());
        run("effect logo=breathe --keep --device /dev/fake-fx-logo").unwrap();
        assert_eq!(mouse.state().effects[&1].0, 3, "breathe is effect 3 on the logo");
        let mouse = fake::plug("/dev/fake-fx-hold", "g903-hero", fake::g903());
        run("effect 1=cycle --hold 0.05 --device /dev/fake-fx-hold").unwrap();
        assert_eq!(mouse.state().sw, (0, 0), "handed back");
        assert!(run("effect").unwrap_err().starts_with("usage"));
        assert!(run("effect 1=cycle --keep --hold 1").unwrap_err().contains("exclude"));
        for (i, (arg, says)) in [
            ("1", "is not CLUSTER=EFFECT"),
            ("dpi=cycle", "is not a cluster number or a light of the Logitech G903 HERO"),
            ("9=cycle", "the device has 2 clusters"),
            ("1=sparkle", "sparkle"),
            ("1=ripple", "no Ripple effect"),
        ]
        .into_iter()
        .enumerate()
        {
            let path = format!("/dev/fake-fx-wrong{i}");
            let _mouse = fake::plug(&path, "g903-hero", fake::g903());
            let error = run(&format!("effect {arg} --keep --device {path}")).unwrap_err();
            assert!(error.contains(says), "{arg}: {error}");
        }
        let _kb = fake::plug("/dev/fake-fx-keys", "g316-x-98", fake::g316());
        assert!(run("effect esc=wave --keep --device /dev/fake-fx-keys").unwrap_err().contains("not a cluster number"));
        let mouse = fake::plug("/dev/fake-fx-held", "g903-hero", fake::g903());
        mouse.state().sw = (1, 5);
        let error = run("effect 1=cycle --keep --device /dev/fake-fx-held").unwrap_err();
        assert_eq!(error, "another program holds the lighting (software control 01 05)");
        let mouse = fake::plug("/dev/fake-fx-failing", "g903-hero", fake::g903());
        mouse.state().refuse = Some(Box::new(|feature, function, _| ((feature, function) == (0x8071, 5)).then_some(4)));
        assert!(run("effect 1=cycle --keep --device /dev/fake-fx-failing").unwrap_err().contains("HID++ error 0x04"));
        let mouse = fake::plug("/dev/fake-fx-refused", "g903-hero", fake::g903());
        mouse.state().refuse = Some(Box::new(|feature, function, _| ((feature, function) == (0x8071, 1)).then_some(4)));
        assert!(run("effect 1=cycle --hold 0 --device /dev/fake-fx-refused").unwrap_err().contains("HID++ error 0x04"));
        assert_eq!(mouse.state().sw, (0, 0), "handed back after the refused effect");
        busy("effect 1=cycle --keep");
        forget_claims();
        testenv::default_signals();
    }

    #[test]
    fn release_goes_through_the_engine_or_straight_to_the_keyboard() {
        let _serial = testenv::serial();
        let engine = stand_in(vec![reply(None, None)]);
        run("release").unwrap();
        assert!(matches!(engine.join().unwrap()[..], [Request::Use { profile: None }]));
        let kb = fake::plug("/dev/fake-release", "g316-x-98", fake::g316());
        kb.state().sw = (1, 0);
        run("release --device /dev/fake-release").unwrap();
        assert_eq!(kb.state().sw, (0, 0));
        busy("release");
    }

    #[test]
    fn probe_prints_a_starting_definition() {
        let _serial = testenv::serial();
        assert!(run("probe").unwrap_err().starts_with("usage"));
        // /dev/null passes for a G316 in tests, so the lock is taken; then it reads as unplugged.
        assert!(run("probe --device /dev/null").unwrap_err().contains("disconnected"));
        busy("probe --device /dev/null");
        let _mouse = fake::plug("/dev/fake-probe-mouse", "g903-hero", fake::g903());
        run("probe --device /dev/fake-probe-mouse").unwrap(); // no per-key lighting: a cluster definition
        let _kb = fake::plug("/dev/fake-probe-keys", "g316-x-98", fake::g316());
        run("probe --device /dev/fake-probe-keys").unwrap();
        // Only a status display: nothing to paint, so no definition.
        let mut dark = fake::g903();
        dark.clusters.retain(|c| c.1 != 0);
        let _dark = fake::plug("/dev/fake-probe-dark", "g903-hero", dark);
        run("probe --device /dev/fake-probe-dark").unwrap();
    }

    #[test]
    fn a_cluster_device_gets_a_definition_that_loads() {
        let _mouse = fake::plug("/dev/fake-probe-def", "g903-hero", fake::g903());
        let text = cluster_definition(&device::probe(std::path::Path::new("/dev/fake-probe-def")).unwrap());
        let m = layout::Model::parse("probed", &text).unwrap();
        assert!(!m.per_key && (m.vendor, m.product, m.interface) == (0x046d, 0xc091, 2));
        let lights: Vec<_> = m.zones.iter().map(|z| (z.id, z.name.as_str())).collect();
        assert_eq!(lights, [(1, "light1")], "the DPI status display, cluster 0, is left out");
        assert!(text.contains("# Cluster 0 is a status display (event modes 07)"));
    }

    #[test]
    fn rate_through_the_engine() {
        let _serial = testenv::serial();
        let engine = stand_in(vec![reply(rates(), None), reply(None, None)]);
        run("rate").unwrap();
        run("rate").unwrap(); // not connected
        engine.join().unwrap();
        let waiting = Some(RateChange::Waiting { hz: 2000 });
        let engine = stand_in(vec![reply(rates(), None), reply(rates(), waiting.clone()), reply(rates(), waiting), reply(rates(), Some(RateChange::Done { hz: 2000 }))]);
        run("rate 2000Hz").unwrap();
        assert!(matches!(engine.join().unwrap()[1], Request::SetReportRate { hz: 2000 }));
        let failed = Some(RateChange::Failed { hz: 2000, error: "the keyboard reports 1000 Hz".into() });
        let engine = stand_in(vec![reply(rates(), None), reply(rates(), None), reply(rates(), failed)]);
        assert_eq!(run("rate 2000").unwrap_err(), "report-rate change: the keyboard reports 1000 Hz");
        engine.join().unwrap();
        let engine = stand_in(vec![reply(rates(), None), Reply::Err("one change at a time".into())]);
        assert_eq!(run("rate 2000").unwrap_err(), "one change at a time");
        engine.join().unwrap();
        let engine = stand_in(vec![reply(rates(), None), reply(rates(), None)]);
        assert!(run("rate 2000").unwrap_err().contains("did not confirm"), "the engine went quiet");
        engine.join().unwrap();
        assert!(run("rate fast").unwrap_err().contains("not a rate in Hz"));
        assert!(run("rate 1 2").unwrap_err().starts_with("usage"));
    }

    #[test]
    fn rate_on_the_keyboard_itself() {
        let _serial = testenv::serial();
        let at = "/dev/fake-rate";
        let rate = |hz: &str| run(&format!("rate {hz} --device {at}"));
        let plug = |id: &str| fake::plug(at, id, if id == "g903-hero" { fake::g903() } else { fake::g316() });
        let g316 = layout::find("g316-x-98").unwrap();
        let _ = std::fs::remove_file(profile::rates_path(g316));
        let _kb = plug("g316-x-98");
        rate("").unwrap();
        let _kb = plug("g316-x-98");
        rate("1000").unwrap(); // already
        assert_eq!(profile::load_kept_rate(g316), Some(1000), "the choice is kept even with no write");
        let _kb = plug("g316-x-98");
        assert!(rate("1234").unwrap_err().contains("not a rate this keyboard reports"));
        let _mouse = plug("g903-hero");
        assert!(rate("1000").unwrap_err().contains("does not report a report rate"));
        let kb = plug("g316-x-98");
        kb.state().refuse_rate = Some((4, 2));
        assert!(rate("2000").unwrap_err().contains("HID++ error 0x02"));
        // Changed: the keyboard reconnects with the new rate, another one, none, or not at all.
        let (_before, after) = (plug("g316-x-98"), plug("g316-x-98"));
        after.state().rate = 4;
        rate("2000").unwrap();
        assert_eq!(profile::load_kept_rate(g316), Some(2000), "kept for the engine to set again");
        let (_before, _after) = (plug("g316-x-98"), plug("g316-x-98"));
        assert!(rate("2000").unwrap_err().contains("reports rate index 3"));
        let (_before, _after) = (plug("g316-x-98"), plug("g903-hero"));
        assert!(rate("2000").unwrap_err().contains("did not report its rate"));
        let _before = plug("g316-x-98");
        assert!(rate("2000").unwrap_err().contains("did not come back"));
        busy("rate");
        let _ = std::fs::remove_file(profile::rates_path(g316));
    }

    #[test]
    fn flags_values_and_leftovers() {
        let mut a = args("--keep esc=ff0000 --hold 5");
        assert!(take_flag(&mut a, "--keep"));
        assert!(!take_flag(&mut a, "--keep"));
        assert_eq!(take_value(&mut a, "--hold").unwrap().as_deref(), Some("5"));
        assert_eq!(take_value(&mut a, "--hold").unwrap(), None);
        assert!(no_more(&a).unwrap_err().contains("esc=ff0000"));
        assert!(take_value(&mut args("--base"), "--base").unwrap_err().contains("needs a value"));
        assert!(no_more(&[]).is_ok());
        assert_eq!(seconds(None, 10.0).unwrap(), 10.0);
        assert_eq!(seconds(Some("2.5".into()), 10.0).unwrap(), 2.5);
        for bad in ["-1", "inf", "soon"] {
            assert!(seconds(Some(bad.into()), 1.0).is_err(), "{bad}");
        }
    }

    #[test]
    fn paint_arguments_become_a_profile() {
        crate::testenv::init();
        let m = layout::find("g316-x-98").unwrap();
        let p = assignments(m, &mut args("esc=ff0000 bar1=00ff00 0x9a=0000ff --base 101010")).unwrap();
        assert_eq!((p.zones[&38], p.zones[&160], p.zones[&154]), ([255, 0, 0], [0, 255, 0], [0, 0, 255]));
        assert_eq!((p.base("keys"), p.base("bar")), ([0x10; 3], [0x10; 3]));
        for (text, says) in [
            ("", "usage"),
            ("esc", "not ZONE=RRGGBB"),
            ("shift=ffffff", "leftshift"),
            ("250=ffffff", "zone 250 is not one"),
            ("esc=ffffff 38=000000", "given twice"),
            ("esc=nothex", "not a colour"),
        ] {
            assert!(assignments(m, &mut args(text)).unwrap_err().contains(says), "{text:?}");
        }
    }

    #[test]
    fn status_text_names_the_device_and_the_lighting() {
        let mut s = Status {
            device: Device::Connected {
                keyboard: "g316-x-98".into(),
                name: "Logitech G316 X 98".into(),
                path: "/dev/hidraw2".into(),
                protocol: "4.2".into(),
                layout: Some(1),
                zones: vec![1, 2, 3],
                effects: vec![],
            },
            control: Control::Profile { name: "Test".into() },
            active: Some("Test".into()),
            message: Some("hello".into()),
            pid: 42,
            hosted: true,
            controls: Some(Controls {
                brightness: Some([100, 100]),
                report_rate: Some(1000),
                report_rates: vec![1000, 2000, 8000],
                report_rate_refused: vec![8000],
                report_rate_kept: Some(2000),
                game_mode: Some(true),
            }),
            rate_change: Some(RateChange::Failed { hz: 8000, error: "refused".into() }),
            claude: vec![claude::State::Asking, claude::State::Working],
        };
        let text = describe(&s);
        for want in [
            "keyboard: Logitech G316 X 98 on /dev/hidraw2, HID++ 4.2, layout code 0x01, 3 zones",
            "lighting: showing profile \"Test\"",
            "engine: pid 42 inside an editor window",
            "brightness: 100 of 100 (read only)",
            "report rate: 1000 Hz; supports 1000, 2000, 8000 (8000 refused by the keyboard); Loginux sets it back to 2000 Hz after a reconnect",
            "game mode: on (read only)",
            "report-rate change to 8000 Hz failed: refused",
            "claude code: asking, working",
            "last message: hello",
        ] {
            assert!(text.contains(want), "{want} missing from\n{text}");
        }
        for (device, control, want) in [
            (Device::Missing, Control::Onboard, "not connected"),
            (Device::NoAccess { error: "no access".into() }, Control::Waiting, "waiting for the keyboard"),
            (Device::Failed { error: "boom".into() }, Control::Preview, "error: boom"),
            (Device::Missing, Control::Check { zone: 9 }, "zone 9 lit"),
            (Device::Missing, Control::Conflict { mode: 1, flags: 5 }, "software control 01 05"),
        ] {
            s.device = device;
            s.control = control;
            s.rate_change = Some(RateChange::Waiting { hz: 2000 });
            let text = describe(&s);
            assert!(text.contains(want) && text.contains("waiting for the keyboard to confirm 2000 Hz"), "{want}\n{text}");
        }
        assert!(controls_text(&Controls::default()).contains("report rate: not reported"));
        s.rate_change = None;
        s.claude.clear();
        assert!(!describe(&s).contains("report-rate change") && !describe(&s).contains("claude code"));
    }

    #[test]
    fn claude_signals_go_to_the_engine() {
        let _serial = testenv::serial();
        let engine = stand_in(vec![reply(None, None), reply(None, None)]);
        run("claude done").unwrap();
        run("claude asking --session s1").unwrap();
        match &engine.join().unwrap()[..] {
            [
                Request::Claude { session: a, signal: claude::Signal::Done, pid: None, running: ra },
                Request::Claude { session: b, signal: claude::Signal::Asking, pid: None, running: rb },
            ] => {
                assert_eq!((a.as_str(), b.as_str()), ("manual", "s1"));
                assert!(ra.is_empty() && rb.is_empty(), "by hand, nothing runs in the background");
            }
            other => panic!("{other:?}"),
        }
        assert!(run("claude thinking").unwrap_err().contains("is not one of start, prompt"));
        assert!(run("claude").unwrap_err().starts_with("usage"));
        assert!(run("claude done --hook").unwrap_err().contains("unexpected argument \"done\""));
        assert!(run("claude --statusline bash x.sh").unwrap_err().starts_with("usage: loginux claude --statusline"));
        assert!(run("claude --enable now").unwrap_err().contains("unexpected argument \"now\""));
        // --enable and --disable do what the editor's switch does.
        let settings = testenv::init().join("claude/settings.json");
        let _ = std::fs::remove_file(&settings);
        run("claude --enable").unwrap();
        assert!(profile::claude_on() && std::fs::read_to_string(&settings).unwrap().contains("loginux claude --statusline"));
        run("claude --disable").unwrap();
        assert!(!profile::claude_on() && !std::fs::read_to_string(&settings).unwrap().contains("statusLine"));
        std::fs::write(&settings, "{broken").unwrap();
        assert!(run("claude --enable").unwrap_err().contains("left as it is"));
        profile::set_claude(false).unwrap();
        std::fs::remove_file(&settings).unwrap();
        assert!(run("claude done").unwrap_err().contains("no loginux engine"), "by hand, a missing engine is said");
    }
}
