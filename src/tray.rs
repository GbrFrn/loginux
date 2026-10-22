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
}

/// Shows the tray icon until `stop` is set. Without a tray host (no session
/// bus, or a desktop with no StatusNotifierItem support) it logs and returns.
pub fn spawn(stop: Arc<AtomicBool>) {
    let tray = Tray { status: None, profiles: profile::list(), editor: None, stop: stop.clone() };
    // The service can start before the panel; the icon appears once the panel's watcher does.
    let handle = match tray.assume_sni_available(true).spawn() {
        Ok(handle) => handle,
        Err(e) => return eprintln!("loginux: no tray icon: {e}"),
    };
    thread::spawn(move || {
        let mut last = None;
        while !stop.load(Ordering::SeqCst) {
            let now = (ipc::request(&Request::Status).ok(), profile::list());
            if last.as_ref() != Some(&now) {
                let (status, profiles) = now.clone();
                if handle.update(|t| (t.status, t.profiles) = (status, profiles)).is_none() {
                    return; // the tray service ended
                }
                last = Some(now);
            }
            thread::sleep(Duration::from_secs(2));
        }
    });
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
        let Ok(exe) = std::env::current_exe() else { return };
        // Under systemd the service can predate the desktop's WAYLAND_DISPLAY, so
        // the editor starts as its own user unit: it gets the session's current
        // environment, outlives a service stop, and a second start is refused.
        let mut cmd = if std::env::var_os("INVOCATION_ID").is_some() {
            let mut c = Command::new("systemd-run");
            c.args(["--user", "--unit=loginux-editor", "--collect", "--quiet", "--"]).arg(exe);
            c
        } else {
            Command::new(exe)
        };
        self.editor = cmd.arg("ui").spawn().ok();
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
                let data = rgba.chunks_exact(4).flat_map(|p| [p[3], p[0], p[1], p[2]]).collect();
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
