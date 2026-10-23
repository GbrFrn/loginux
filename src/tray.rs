//! The tray icon of `loginux run`: a StatusNotifierItem, the tray protocol of
//! KDE Plasma (and of GNOME with the AppIndicator extension). It is a client of
//! the engine like the command line: it reads the status over the socket every
//! two seconds and sends the same requests.

use crate::icon;
use crate::ipc::{self, Control, Device, Request, Status};
use crate::profile;
use ksni::blocking::TrayMethods;
use ksni::menu::{RadioGroup, RadioItem, StandardItem, SubMenu};
use ksni::{Category, Icon, MenuItem, ToolTip};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

struct Tray {
    status: Option<Status>,
    profiles: Vec<String>,
    editor: Option<Child>,
    stop: Arc<AtomicBool>,
    /// The program the editor runs: this executable.
    exe: PathBuf,
    under_systemd: bool,
}

/// Shows the tray icon until `stop` is set. Without a tray host (no session
/// bus, or a desktop with no StatusNotifierItem support) it logs and returns.
pub fn spawn(stop: Arc<AtomicBool>) {
    let tray = Tray {
        status: None,
        profiles: profile::list(),
        editor: None,
        stop: stop.clone(),
        exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("loginux")),
        under_systemd: std::env::var_os("INVOCATION_ID").is_some(),
    };
    // The service can start before the panel; the icon appears once the panel's watcher does.
    let handle = match tray.assume_sni_available(true).spawn() {
        Ok(handle) => handle,
        Err(e) => return eprintln!("loginux: no tray icon: {e}"),
    };
    thread::spawn(move || {
        poll(&stop, Duration::from_secs(2), |status, profiles| {
            handle.update(|t| (t.status, t.profiles) = (status, profiles)).is_some()
        })
    });
}

/// Reads the engine's status and the profile list every `every` and hands them
/// to `show` when they change, until `stop` is set or `show` returns false.
fn poll(stop: &AtomicBool, every: Duration, mut show: impl FnMut(Option<Status>, Vec<String>) -> bool) {
    let mut last = None;
    while !stop.load(Ordering::SeqCst) {
        let now = (ipc::request(&Request::Status).ok(), profile::list());
        if last.as_ref() != Some(&now) {
            let (status, profiles) = now.clone();
            if !show(status, profiles) {
                return; // the tray service ended
            }
            last = Some(now);
        }
        thread::sleep(every);
    }
}

/// `exe ui`. Under systemd the service can predate the desktop's
/// WAYLAND_DISPLAY, so the editor starts as its own user unit: it gets the
/// session's current environment, outlives a service stop, and a second start
/// is refused.
fn editor_command(exe: &Path, under_systemd: bool) -> Command {
    let mut cmd = if under_systemd {
        let mut c = Command::new("systemd-run");
        c.args(["--user", "--unit=loginux-editor", "--collect", "--quiet", "--"]).arg(exe);
        c
    } else {
        Command::new(exe)
    };
    cmd.arg("ui");
    cmd
}

/// Sends one request off the tray's thread; the next status poll shows the result.
fn send(req: Request) {
    thread::spawn(move || {
        if let Err(e) = ipc::request(&req) {
            eprintln!("loginux: tray request refused: {e}");
        }
    });
}

impl Tray {
    /// One line for the menu's top and the tooltip.
    fn headline(&self) -> String {
        let Some(s) = &self.status else { return "Starting".into() };
        let name = match &s.device {
            Device::Connected { name, .. } => name,
            Device::Missing => return "No device connected".into(),
            Device::NoAccess { .. } => return "No access to the device".into(),
            Device::Failed { .. } => return "Device error".into(),
        };
        match &s.control {
            Control::Profile { name: p } => format!("{name} · {p}"),
            Control::Onboard => format!("{name} · onboard lighting"),
            Control::Waiting => format!("Waiting for the {name}"),
            Control::Preview => format!("{name} · editor preview"),
            Control::Check { .. } => format!("{name} · channel check"),
            Control::Conflict { .. } => "Another program holds the lighting".into(),
        }
    }

    fn conflict(&self) -> bool {
        matches!(self.status, Some(Status { control: Control::Conflict { .. }, .. }))
    }

    fn open_editor(&mut self) {
        if self.editor.as_mut().is_some_and(|c| matches!(c.try_wait(), Ok(None))) {
            return; // still starting, or still open
        }
        self.editor = editor_command(&self.exe, self.under_systemd).spawn().ok();
    }
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "loginux".into()
    }

    fn title(&self) -> String {
        "Loginux".into()
    }

    fn category(&self) -> Category {
        Category::Hardware
    }

    fn status(&self) -> ksni::Status {
        if self.conflict() { ksni::Status::NeedsAttention } else { ksni::Status::Active }
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        (1..=4)
            .map(|scale| {
                let (side, rgba) = icon::rgba(scale);
                // ARGB32 in network byte order.
                let data = rgba.as_chunks::<4>().0.iter().flat_map(|&[r, g, b, a]| [a, r, g, b]).collect();
                Icon { width: side as i32, height: side as i32, data }
            })
            .collect()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip { title: "Loginux".into(), description: self.headline(), ..Default::default() }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.open_editor();
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let active = self.status.as_ref().and_then(|s| s.active.clone());
        let names = self.profiles.clone();
        let selected = active.and_then(|a| names.iter().position(|n| *n == a)).map_or(0, |i| i + 1);
        let options = std::iter::once("Onboard Lighting".to_string())
            .chain(names.iter().cloned())
            .map(|label| RadioItem { label, ..Default::default() })
            .collect();
        vec![
            StandardItem { label: self.headline(), enabled: false, ..Default::default() }.into(),
            StandardItem {
                label: "Take Over Lighting".into(),
                visible: self.conflict(),
                activate: Box::new(|_| send(Request::TakeOver)),
                ..Default::default()
            }
            .into(),
            SubMenu {
                label: "Profile".into(),
                submenu: vec![
                    RadioGroup {
                        selected,
                        select: Box::new(move |_, i| {
                            send(Request::Use { profile: i.checked_sub(1).and_then(|i| names.get(i).cloned()) })
                        }),
                        options,
                    }
                    .into(),
                ],
                ..Default::default()
            }
            .into(),
            StandardItem { label: "Open Editor".into(), activate: Box::new(Self::open_editor), ..Default::default() }.into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|t: &mut Self| t.stop.store(true, Ordering::SeqCst)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::{Controls, RateChange};
    use ksni::Tray as _;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    fn tray(device: Device, control: Control, active: Option<&str>) -> Tray {
        let status = Status {
            device,
            control,
            active: active.map(String::from),
            message: None,
            pid: 1,
            hosted: false,
            controls: Some(Controls::default()),
            rate_change: None::<RateChange>,
        };
        let mut t = idle();
        (t.status, t.profiles) = (Some(status), vec!["Evening".into(), "Test".into()]);
        t
    }

    /// A tray before its first status, whose editor is a stand-in that waits.
    fn idle() -> Tray {
        let exe = crate::testenv::stand_in_program("editor");
        Tray { status: None, profiles: vec![], editor: None, stop: Arc::default(), exe, under_systemd: false }
    }

    fn connected() -> Device {
        Device::Connected {
            keyboard: "g316-x-98".into(),
            name: "G316".into(),
            path: "/dev/hidraw2".into(),
            protocol: "4.2".into(),
            layout: Some(1),
            zones: vec![],
            effects: vec![],
        }
    }

    #[test]
    fn the_headline_follows_the_engine() {
        assert_eq!(idle().headline(), "Starting");
        for (device, control, want) in [
            (connected(), Control::Profile { name: "Test".into() }, "G316 · Test"),
            (connected(), Control::Onboard, "G316 · onboard lighting"),
            (connected(), Control::Waiting, "Waiting for the G316"),
            (connected(), Control::Preview, "G316 · editor preview"),
            (connected(), Control::Check { zone: 4 }, "G316 · channel check"),
            (connected(), Control::Conflict { mode: 1, flags: 5 }, "Another program holds the lighting"),
            (Device::Missing, Control::Onboard, "No device connected"),
            (Device::NoAccess { error: "x".into() }, Control::Onboard, "No access to the device"),
            (Device::Failed { error: "x".into() }, Control::Onboard, "Device error"),
        ] {
            let t = tray(device, control, None);
            assert_eq!(t.headline(), want);
            assert_eq!(t.tool_tip().description, want);
        }
    }

    #[test]
    fn a_conflict_asks_for_attention_and_offers_a_take_over() {
        let t = tray(connected(), Control::Conflict { mode: 1, flags: 5 }, None);
        assert!(t.conflict());
        assert!(matches!(t.status(), ksni::Status::NeedsAttention));
        let MenuItem::Standard(take) = &t.menu()[1] else { panic!("take over item") };
        assert!(take.visible && take.label == "Take Over Lighting");
        let calm = tray(connected(), Control::Onboard, None);
        assert!(matches!(calm.status(), ksni::Status::Active));
        let MenuItem::Standard(take) = &calm.menu()[1] else { panic!() };
        assert!(!take.visible);
    }

    #[test]
    fn the_menu_lists_profiles_with_the_active_one_chosen() {
        let t = tray(connected(), Control::Profile { name: "Test".into() }, Some("Test"));
        let menu = t.menu();
        let MenuItem::Standard(head) = &menu[0] else { panic!() };
        assert_eq!((head.label.as_str(), head.enabled), ("G316 · Test", false));
        let MenuItem::SubMenu(profiles) = &menu[2] else { panic!("profile submenu") };
        let MenuItem::RadioGroup(group) = &profiles.submenu[0] else { panic!("radio group") };
        let labels: Vec<&str> = group.options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!((labels, group.selected), (vec!["Onboard Lighting", "Evening", "Test"], 2));
        assert!(matches!(&menu[3], MenuItem::Standard(i) if i.label == "Open Editor"));
        assert!(matches!(menu[4], MenuItem::Separator));
        assert!(matches!(&menu[5], MenuItem::Standard(i) if i.label == "Quit"));
        let idle = tray(connected(), Control::Onboard, None);
        let MenuItem::SubMenu(p) = &idle.menu()[2] else { panic!() };
        assert!(matches!(&p.submenu[0], MenuItem::RadioGroup(g) if g.selected == 0), "no profile: onboard lighting");
    }

    #[test]
    fn choosing_a_profile_sends_it_to_the_engine_and_quit_stops_it() {
        let _serial = crate::testenv::serial();
        let path = ipc::socket_path().unwrap();
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let mut t = tray(connected(), Control::Onboard, None);
        let menu = t.menu();
        let MenuItem::SubMenu(p) = &menu[2] else { panic!() };
        let MenuItem::RadioGroup(group) = &p.submenu[0] else { panic!() };
        (group.select)(&mut t, 1);
        let (stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        assert_eq!(line, "{\"use\":{\"profile\":\"Evening\"}}\n");
        (&stream).write_all(b"{\"err\":\"test\"}\n").unwrap();
        let MenuItem::Standard(quit) = &menu[5] else { panic!() };
        (quit.activate)(&mut t);
        assert!(t.stop.load(Ordering::SeqCst));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn the_editor_runs_this_program_or_a_user_unit() {
        let exe = Path::new("/usr/bin/loginux");
        let direct = editor_command(exe, false);
        assert_eq!(Path::new(direct.get_program()), exe);
        assert_eq!(direct.get_args().collect::<Vec<_>>(), ["ui"]);
        let unit = editor_command(exe, true);
        assert_eq!(unit.get_program(), "systemd-run");
        let args = ["--user", "--unit=loginux-editor", "--collect", "--quiet", "--", "/usr/bin/loginux", "ui"];
        assert_eq!(unit.get_args().collect::<Vec<_>>(), args);
    }

    #[test]
    fn activating_opens_one_editor_at_a_time() {
        // It forks the stand-in editor. A child inherits every open descriptor until
        // its exec, the engine-lock file's included, so a fork while another test
        // releases that lock keeps it held a moment longer: run alone.
        let _serial = crate::testenv::serial();
        let mut t = idle();
        t.activate(0, 0);
        let first = t.editor.as_ref().expect("the editor started").id();
        let MenuItem::Standard(open) = &t.menu()[3] else { panic!() };
        (open.activate)(&mut t);
        assert_eq!(t.editor.as_ref().unwrap().id(), first, "still open: not started twice");
        let editor = t.editor.as_mut().unwrap();
        editor.kill().unwrap();
        editor.wait().unwrap();
        t.activate(0, 0);
        let editor = t.editor.as_mut().expect("started again");
        assert_ne!(editor.id(), first);
        editor.kill().unwrap();
        editor.wait().unwrap();
    }

    #[test]
    fn polling_shows_changes_until_stopped() {
        let _serial = crate::testenv::serial();
        let stop = AtomicBool::new(false);
        let mut seen = vec![];
        poll(&stop, Duration::from_millis(10), |status, profiles| {
            seen.push((status.is_some(), profiles));
            false // the tray service ended
        });
        assert_eq!(seen.len(), 1);
        let (_, profiles) = &seen[0];
        assert!(!seen[0].0 && profiles == &profile::list(), "no engine: no status");
        // Nothing changes between reads: one show, until the stop.
        let stop = Arc::new(AtomicBool::new(false));
        let stopper = {
            let stop = stop.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(100));
                stop.store(true, Ordering::SeqCst);
            })
        };
        let mut shows = 0;
        poll(&stop, Duration::from_millis(10), |_, _| {
            shows += 1;
            true
        });
        stopper.join().unwrap();
        assert_eq!(shows, 1);
    }

    #[test]
    fn the_icon_registers_on_the_session_bus() {
        let _serial = crate::testenv::serial();
        let stop = Arc::new(AtomicBool::new(false));
        spawn(stop.clone()); // no bus listens yet: logged, no icon
        let bus = crate::testenv::session_bus();
        spawn(stop.clone());
        let end = std::time::Instant::now() + Duration::from_secs(5);
        while !bus.names().contains("org.kde.StatusNotifierItem-") {
            assert!(std::time::Instant::now() < end, "no tray item on the bus: {}", bus.names());
            thread::sleep(Duration::from_millis(20));
        }
        thread::sleep(Duration::from_millis(200)); // the first status poll
        stop.store(true, Ordering::SeqCst);
    }

    #[test]
    fn the_icon_comes_in_four_sizes_as_argb() {
        let t = tray(connected(), Control::Onboard, None);
        let icons = t.icon_pixmap();
        assert_eq!(icons.iter().map(|i| (i.width, i.data.len())).collect::<Vec<_>>(), [(16, 1024), (32, 4096), (48, 9216), (64, 16384)]);
        assert_eq!(&icons[0].data[..4], &[0, 0, 0, 0], "transparent corner");
        let keycap = (16 + 1) * 4; // row 1, column 1: Ground
        assert_eq!(&icons[0].data[keycap..keycap + 4], &[0xFF, 0x0B, 0x0B, 0x0C]);
        assert_eq!((t.id(), t.title()), ("loginux".to_string(), "Loginux".to_string()));
        assert!(matches!(t.category(), Category::Hardware));
    }
}
