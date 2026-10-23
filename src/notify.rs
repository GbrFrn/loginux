//! Desktop notifications (org.freedesktop.Notifications) for what the engine
//! does at another program's request or on its own: a profile change, a report
//! rate set or set back, lighting taken by another program. Sent with systemd's
//! busctl, which every system that runs the service has; without a notification
//! server they are dropped. The `desktop-entry` hint files them under Loginux in
//! the desktop's notification settings, where they can be turned off. Tests use
//! a stand-in busctl that logs the call.

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};
#[cfg(not(test))]
use std::thread;

static BUSCTL: OnceLock<PathBuf> = OnceLock::new();
/// The last notification's ID: the next one replaces it, so quick changes
/// update one notification instead of stacking up.
static LAST: AtomicU32 = AtomicU32::new(0);

#[cfg(test)]
pub fn use_busctl(program: PathBuf) {
    BUSCTL.set(program).expect("the stand-in is chosen before any call");
}

/// Shows `summary` and `body` without waiting for the desktop.
pub fn send(summary: &str, body: &str) {
    #[cfg(test)]
    crate::testenv::init();
    let mut cmd = Command::new(BUSCTL.get_or_init(|| PathBuf::from("busctl")));
    cmd.args(["--user", "call", "org.freedesktop.Notifications", "/org/freedesktop/Notifications"])
        .args(["org.freedesktop.Notifications", "Notify", "susssasa{sv}i", "Loginux"])
        .arg(LAST.load(Ordering::Relaxed).to_string())
        .args(["loginux", summary, body, "0", "1", "desktop-entry", "s", "loginux", "-1"]);
    let run = move || {
        let mut cmd = cmd;
        // busctl prints the reply as `u ID`.
        if let Ok(out) = cmd.output()
            && let Some(id) = String::from_utf8_lossy(&out.stdout).trim().strip_prefix("u ").and_then(|n| n.parse().ok())
        {
            LAST.store(id, Ordering::Relaxed);
        }
    };
    // Tests wait for the stand-in, so they see the notifications in order.
    #[cfg(test)]
    run();
    #[cfg(not(test))]
    thread::spawn(run); // LCOV_EXCL_LINE: tests run the stand-in inline, above
}
