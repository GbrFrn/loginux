//! loginux: RGB lighting for Logitech HID++ keyboards and mice on Linux.

mod device;
mod effects;
mod engine;
mod hidpp;
mod icon;
mod ipc;
mod layout;
mod pixelfont;
mod profile;
mod tray;
mod ui;

use device::Keyboard;
use engine::{STOP, TakeError};
use ipc::{Control, Controls, Device, RateChange, Request, Status};
use layout::{Kind, Model};
use profile::{Profile, hex, parse_rgb};
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
  loginux rate [HZ]                  show the report rate, or change it (the keyboard reconnects)
  loginux effect CLUSTER=EFFECT[,param=value]... [--hold SECS | --keep]
                                     run a built-in effect (static, cycle, breathe, wave, ripple)
                                     on a lighting cluster, by number or light name
  loginux probe --device /dev/hidrawN
                                  read-only: print a starting definition for a new keyboard

Options:
  --device /dev/hidrawN           use this node; it must be a known keyboard's HID++ interface

Profiles are ~/.config/loginux/profiles/NAME.toml. Nothing is written to the keyboard's
flash, and unplugging the keyboard always restores its onboard lighting.
";

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let result = take_value(&mut args, "--device").and_then(|device| {
        let device = device.map(PathBuf::from);
        let command = if args.is_empty() { "ui".to_string() } else { args.remove(0) };
        if matches!(command.as_str(), "zones" | "status" | "probe") {
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
            other => Err(format!("unknown command {other:?}\n\n{USAGE}")),
        }
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("loginux: {e}");
            ExitCode::FAILURE
        }
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
    let found = match device {
        Some(path) => hidpp::identify(&path, layout::models()).map(|m| (path, m)),
        None => hidpp::find(layout::models()),
    };
    let (path, model) = found.map_err(|e| e.to_string())?;
    Keyboard::open(&path, model).map_err(|e| e.to_string())
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
    if let Some(m) = &status.message {
        out += &format!("last message: {m}\n");
    }
    out
}

fn controls_text(c: &Controls) -> String {
    let brightness = c.brightness.map_or("not reported".into(), |[now, max]| format!("{now} of {max} (read only)"));
    let rate = match c.report_rate {
        Some(hz) => format!("{hz} Hz; supports {}", c.report_rates.iter().map(|r| format!("{r}")).collect::<Vec<_>>().join(", ")),
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
    if !(p.rgb_effects && p.per_key) {
        eprintln!("no starting definition: Loginux drives devices with 0x8071 RGB effects and 0x8081 per-key lighting");
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
        let end = Instant::now() + Duration::from_secs(30);
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
        return Err("the engine did not confirm the change within 30 s; `loginux status` shows its state".into());
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
        println!("report rate is already {hz} Hz");
        return Ok(());
    }
    match kb.set_report_rate(index) {
        Ok(()) | Err(hidpp::Error::Gone(_)) | Err(hidpp::Error::Timeout(_)) => {}
        Err(err) => return Err(err.to_string()),
    }
    drop(kb);
    println!("sent {hz} Hz; waiting for the keyboard to reconnect");
    sleep(Duration::from_secs(1));
    let end = Instant::now() + Duration::from_secs(20);
    while Instant::now() < end {
        if let Ok(mut kb) = open(device.clone()) {
            let (_, rates) = engine::read_controls(&mut kb).map_err(e)?;
            return match rates.map(|r| r.1) {
                Some(i) if i == index => {
                    println!("report rate is now {hz} Hz (read back from the keyboard)");
                    Ok(())
                }
                Some(i) => Err(format!("the keyboard reports rate index {i}, not {hz} Hz")),
                None => Err("the keyboard did not report its rate after the change".into()),
            };
        }
        sleep(Duration::from_millis(250));
    }
    Err("the keyboard did not come back within 20 s; replug it, then run `loginux rate`".into())
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
