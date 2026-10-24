//! Claude Code on the light bar: an optional status display that Claude Code's
//! hooks feed through `loginux claude --hook` (the plugin in
//! packaging/claude-code installs them). The light bar is the status line:
//! every running Claude Code instance gets its own share of it, as equal as the
//! bar allows and at least one LED, for as long as its process runs, and shows
//! its state there as pixel art; its context filling up closes in on its share,
//! and the bar's end lamps warn of the usage limits, which Claude Code's status
//! line reports (`loginux claude --statusline`). The keys keep the chosen
//! profile, an effect paused on a still frame of itself, and go dark, one key to
//! a pixel, only for a short sprite: when an instance starts, when a long turn
//! needs the user or ends, on an error, and when the context or a limit crosses
//! a mark.
//! Nothing here sends anything new to the device: the engine paints these
//! frames like a profile's colours.

use crate::layout::{Kind, Model};
use crate::profile::{self, Profile, Rgb, parse_rgb};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Seconds per frame of the pixel art: 12.5 frames a second, under the 30 Hz
/// the keyboard's lamps take (research/g316-x-98.md).
const STEP: f64 = 0.08;
/// An instance whose process is unknown drops off after this long without an event.
const LEASE: Duration = Duration::from_secs(30 * 60);
/// Thinking, a tool or compacting with no event this long shows as idle: an
/// interrupt (Esc) sends no event.
const STALE: Duration = Duration::from_secs(15 * 60);
/// A turn earns a sprite across the keys when it needs the user, or ends, at
/// least this long after the user last acted.
const AWAY: Duration = Duration::from_secs(30);
/// No two sprites across the keys start closer together than this.
const COOLDOWN: Duration = Duration::from_secs(10);
/// How long an ended instance takes to close its share of the bar: twenty frames.
const CLOSE: Duration = Duration::from_millis(1600);

/// What a hook event means for the display.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Signal {
    /// A session began: its instance shows idle.
    Start,
    /// The user sent a prompt.
    Prompt,
    /// Thinking, or a subagent starting.
    Working,
    Tool,
    /// Claude Code compacts the conversation.
    Compact,
    Asking,
    Done,
    /// Claude Code waits for input, as after an interrupt: Done, with no check mark.
    Waiting,
    Error,
    /// The turn ended on a usage limit.
    Limit,
    /// The session, or a subagent, ended.
    End,
}

impl Signal {
    pub fn from_name(name: &str) -> Result<Signal, String> {
        serde_json::from_value(serde_json::Value::String(name.into())).map_err(|_| {
            format!("{name:?} is not one of start, prompt, working, tool, compact, asking, done, waiting, error, limit or end")
        })
    }
}

/// What an instance shows on its share of the light bar.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    /// Open, before the first prompt.
    Idle,
    /// Thinking.
    Working,
    /// A tool runs.
    Tool,
    /// Compacting the conversation.
    Compacting,
    /// Waiting for the user: a permission, a question or a plan to approve.
    Asking,
    /// The turn ended; waiting for the next prompt.
    Done,
    /// The turn ended on an API error.
    Error,
}

impl State {
    /// How much a state wants the user: an instance shows its most urgent agent's.
    fn rank(self) -> u8 {
        match self {
            State::Asking => 6,
            State::Error => 5,
            State::Done => 4,
            State::Tool => 3,
            State::Compacting => 2,
            State::Working => 1,
            State::Idle => 0,
        }
    }
}

/// The fields of a hook's input that matter here; Claude Code sends more. Any
/// of them may be missing or null.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Hook {
    session_id: Option<String>,
    agent_id: Option<String>,
    hook_event_name: Option<String>,
    notification_type: Option<String>,
    tool_name: Option<String>,
    source: Option<String>,
    trigger: Option<String>,
    reason: Option<String>,
    error: Option<String>,
}

/// The instance and the signal for a hook's input, or None for an event that
/// changes nothing. The instance is its Claude Code process, `pid`
/// ($CLAUDE_PID), else its session ID, plus `/agent ID` inside a subagent.
/// `entrypoint` is $CLAUDE_CODE_ENTRYPOINT: nobody watches the keyboard for
/// `claude -p` and Agent SDK sessions, which Claude Code 2.1.309 marks sdk-cli,
/// sdk-ts and sdk-py. Both variables were read from its code; neither is
/// documented.
pub fn from_hook(json: &str, entrypoint: Option<&str>, pid: Option<u32>) -> Option<(String, Signal)> {
    if entrypoint.is_some_and(|e| e.starts_with("sdk-")) {
        return None;
    }
    let h: Hook = serde_json::from_str(json).ok()?;
    let text = |field: &Option<String>| field.as_deref().unwrap_or_default().to_string();
    let agent = text(&h.agent_id);
    let signal = match (text(&h.hook_event_name).as_str(), text(&h.notification_type).as_str()) {
        ("SessionStart", _) if text(&h.source) == "compact" => return None, // the same session goes on
        ("SessionStart", _) => Signal::Start,
        ("UserPromptSubmit", _) => Signal::Prompt,
        ("PreToolUse", _) if matches!(text(&h.tool_name).as_str(), "AskUserQuestion" | "ExitPlanMode") => Signal::Asking,
        ("PreToolUse", _) => Signal::Tool,
        ("PermissionRequest", _) => Signal::Asking,
        ("Notification", "permission_prompt" | "elicitation_dialog" | "elicitation_url_dialog") => Signal::Asking,
        ("Notification", "idle_prompt") => Signal::Waiting,
        // Claude Code resumed by itself once a usage limit reset.
        ("Notification", "quota_auto_resume_fired") => Signal::Working,
        ("PostToolUse" | "PostToolUseFailure", _) => Signal::Working,
        ("PreCompact", _) => Signal::Compact,
        ("PostCompact", _) if text(&h.trigger) == "manual" => Signal::Waiting,
        ("PostCompact", _) => Signal::Working,
        ("Stop", _) => Signal::Done,
        ("StopFailure", _) if text(&h.error) == "rate_limit" => Signal::Limit,
        ("StopFailure", _) => Signal::Error,
        // /clear and /resume end one session and start the next in the same instance.
        ("SessionEnd", _) if matches!(text(&h.reason).as_str(), "clear" | "resume") => return None,
        ("SessionEnd", _) => Signal::End,
        ("SubagentStart", _) if !agent.is_empty() => Signal::Working,
        ("SubagentStop", _) if !agent.is_empty() => Signal::End, // never the whole instance
        _ => return None,
    };
    let instance = pid.map_or_else(|| text(&h.session_id), |p| p.to_string());
    Some((if agent.is_empty() { instance } else { format!("{instance}/{agent}") }, signal))
}

/// The subagents a hook's input lists as still running in the background.
/// Claude Code 2.1.310 sends them with Stop as `background_tasks` of type
/// "subagent", whose id is the agent ID its own hooks carry (read from its
/// code, not documented).
pub fn background_agents(json: &str) -> Vec<String> {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    let tasks = v.get("background_tasks").and_then(|t| t.as_array()).into_iter().flatten();
    tasks.filter(|t| t.get("type").and_then(|k| k.as_str()) == Some("subagent")).filter_map(|t| t.get("id")?.as_str().map(String::from)).collect()
}

/// One usage limit as Claude Code's status line reports it: the share used, 0
/// to 100, and when it resets, in Unix seconds.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Window {
    pub used: f32,
    pub resets_at: i64,
}

/// What Claude Code's status line reports that the display shows: the share of
/// the instance's context window in use, and the account's usage limits. Any
/// can be missing: limits come only to subscribers or behind a gateway, after
/// the session's first reply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub context: Option<f32>,
    /// The report carried usage limits: a window it leaves out has reset.
    #[serde(default)]
    pub limits: bool,
    pub five_hour: Option<Window>,
    pub seven_day: Option<Window>,
    /// A Claude apps gateway's spend limit.
    pub spend: Option<Window>,
}

/// The instance and its usage from the JSON Claude Code gives its status line,
/// or None for a run nobody watches. Instances are named as in `from_hook`.
pub fn from_status_line(json: &str, entrypoint: Option<&str>, pid: Option<u32>) -> Option<(String, Usage)> {
    if entrypoint.is_some_and(|e| e.starts_with("sdk-")) {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let number = |path: &[&str]| path.iter().try_fold(&v, |v, k| v.get(k))?.as_f64();
    let window = |name: &str| {
        let used = number(&["rate_limits", name, "used_percentage"])? as f32;
        Some(Window { used, resets_at: number(&["rate_limits", name, "resets_at"]).unwrap_or(0.0) as i64 })
    };
    let usage = Usage {
        context: number(&["context_window", "used_percentage"]).map(|n| n as f32),
        limits: v.get("rate_limits").is_some_and(|r| r.is_object()),
        five_hour: window("five_hour"),
        seven_day: window("seven_day"),
        spend: window("spend_limit"),
    };
    let session = v.get("session_id").and_then(|s| s.as_str()).unwrap_or_default();
    Some((pid.map_or_else(|| session.to_string(), |p| p.to_string()), usage))
}

/// What a gauge sprite measures, with the key that labels it.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Dial {
    Context,
    FiveHour,
    Week,
    Spend,
}

impl Dial {
    fn key(self) -> &'static str {
        match self {
            Dial::Context => "c",
            Dial::FiveHour => "h",
            Dial::Week => "w",
            Dial::Spend => "s",
        }
    }
}

/// The context share at which an instance's share of the bar starts to close
/// in, the share at which it has closed, and the one that plays its gauge.
const CONTEXT: [f32; 3] = [70.0, 95.0, 90.0];
/// The marks at which a usage limit plays its gauge, and lights the bar's end
/// lamps: amber from the first, red and blinking from the last.
const LIMIT_MARKS: [f32; 3] = [80.0, 90.0, 95.0];

// ---- Claude Code's side ---------------------------------------------------------

/// The plugin with the display's hooks, and the repository it is published from.
const PLUGIN: &str = "loginux-status@loginux";
const MARKETPLACE: &str = "GbrFrn/loginux";
/// The status line command Loginux puts in front of the user's own.
const STATUS_LINE: &str = "loginux claude --statusline";

static CLAUDE: OnceLock<PathBuf> = OnceLock::new();

/// `claude`. Tests get a stand-in script instead (`use_claude`), so no test
/// changes the machine's Claude Code.
fn claude_cli() -> Command {
    #[cfg(test)]
    crate::testenv::init();
    Command::new(CLAUDE.get_or_init(|| PathBuf::from("claude")))
}

#[cfg(test)]
pub fn use_claude(program: PathBuf) {
    CLAUDE.set(program).expect("the stand-in is chosen before any call");
}

/// Claude Code's user settings folder: $CLAUDE_CONFIG_DIR, else $HOME/.claude.
/// Neither set to an absolute path is an error, not a guess.
fn settings_dir(config: Option<std::ffi::OsString>, home: Option<std::ffi::OsString>) -> Result<PathBuf, String> {
    let absolute = |v: Option<std::ffi::OsString>| v.map(PathBuf::from).filter(|p| p.is_absolute());
    absolute(config)
        .or_else(|| absolute(home).map(|h| h.join(".claude")))
        .ok_or_else(|| "neither CLAUDE_CONFIG_DIR nor HOME is set, so Claude Code's settings cannot be found".into())
}

fn settings_path() -> Result<PathBuf, String> {
    #[cfg(test)]
    crate::testenv::init();
    Ok(settings_dir(std::env::var_os("CLAUDE_CONFIG_DIR"), std::env::var_os("HOME"))?.join("settings.json"))
}

/// The settings' text, or `{}` when there are none yet.
fn settings_text(path: &Path) -> Result<String, String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok("{}".into()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn parse_settings(path: &Path, text: &str) -> Result<serde_json::Value, String> {
    serde_json::from_str(text).map_err(|e| format!("{}: {e}; left as it is", path.display()))
}

fn read_settings() -> Result<serde_json::Value, String> {
    let path = settings_path()?;
    parse_settings(&path, &settings_text(&path)?)
}

/// How Claude Code is connected to the display, from its user settings.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Setup {
    /// The plugin with the hooks is enabled.
    pub plugin: bool,
    /// The status line passes through Loginux, for the context and the limits.
    pub status_line: bool,
}

pub fn setup() -> Setup {
    let doc = read_settings().unwrap_or_default();
    Setup {
        plugin: doc.get("enabledPlugins").and_then(|p| p.get(PLUGIN)).and_then(|v| v.as_bool()).unwrap_or(false),
        status_line: doc.pointer("/statusLine/command").and_then(|c| c.as_str()).is_some_and(|c| c.starts_with(STATUS_LINE)),
    }
}

/// `text` as one shell word.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// The status line that passes through Loginux to `command`, the one the user
/// had. If Loginux is ever missing, the shell runs the user's own on its own.
fn wrap(command: &str) -> String {
    let q = quote(command);
    format!("{STATUS_LINE} -- sh -c {q} || sh -c {q}")
}

/// The user's own status line back out of `wrap`.
fn unwrap(line: &str) -> Option<String> {
    let rest = line.strip_prefix(&format!("{STATUS_LINE} -- sh -c "))?;
    const OR: &str = " || sh -c ";
    let half = rest.len().checked_sub(OR.len()).filter(|n| n % 2 == 0)? / 2;
    let (q, tail) = (rest.get(..half)?, rest.get(half..)?);
    (tail == format!("{OR}{q}")).then_some(())?;
    let inner = q.strip_prefix('\'')?.strip_suffix('\'')?;
    Some(inner.replace(r"'\''", "'"))
}

/// Changes Claude Code's user settings with `change`, which says whether it
/// changed anything. A file reached through a link, as from a dotfiles
/// repository, is changed where the link points, and the file keeps its mode.
/// A file that changed on disk in the meantime, as when Claude Code saved a
/// setting, is left as it is.
fn edit_settings(change: impl FnOnce(&Path, &mut serde_json::Map<String, serde_json::Value>) -> Result<bool, String>) -> Result<(), String> {
    let named = settings_path()?;
    let path = fs::canonicalize(&named).unwrap_or(named);
    let before = settings_text(&path)?;
    let mut doc = parse_settings(&path, &before)?;
    let settings = doc.as_object_mut().ok_or_else(|| format!("{}: not a JSON object; left as it is", path.display()))?;
    if !change(&path, settings)? {
        return Ok(());
    }
    let text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())? + "\n";
    let tmp = path.with_file_name(format!(".settings.json.loginux-{}", std::process::id()));
    let staged = fs::create_dir_all(path.parent().unwrap_or(Path::new("/"))).and_then(|_| fs::write(&tmp, &text)).and_then(|_| {
        // A private settings file stays private: it can hold secrets in `env`.
        fs::metadata(&path).map_or(Ok(()), |m| fs::set_permissions(&tmp, m.permissions()))
    });
    staged.map_err(|e| format!("{}: {e}", path.display()))?;
    if settings_text(&path)? != before {
        let _ = fs::remove_file(&tmp);
        return Err(format!("{}: changed while Loginux was editing it; left as it is, try again", path.display()));
    }
    fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Puts the status line through Loginux (`on`), keeping the user's own behind
/// it, or puts the user's own command back; everything else stays as it is.
fn edit_status_line(on: bool) -> Result<(), String> {
    edit_settings(|path, settings| {
        let line = settings.get("statusLine");
        let current = line.and_then(|s| s.get("command")).and_then(|c| c.as_str()).map(String::from);
        if on && line.is_some() && current.is_none() {
            return Err(format!("{}: its statusLine has no command for Loginux to pass through; left as it is", path.display()));
        }
        let ours = current.as_deref().is_some_and(|c| c.starts_with(STATUS_LINE));
        match (on, current) {
            (true, _) if ours => return Ok(false),
            (true, Some(own)) => settings["statusLine"]["command"] = wrap(&own).into(),
            (true, None) => {
                settings.insert("statusLine".into(), serde_json::json!({ "type": "command", "command": STATUS_LINE }));
            }
            (false, Some(line)) if line == STATUS_LINE => {
                settings.remove("statusLine");
            }
            (false, Some(line)) if ours => {
                let own = unwrap(&line).ok_or_else(|| format!("{}: the status line was changed by hand; left as it is", path.display()))?;
                settings["statusLine"]["command"] = own.into();
            }
            (false, _) => return Ok(false),
        }
        Ok(true)
    })
}

/// How long `claude plugin` may take: an install fetches the plugin.
const CLI_WAIT: Duration = if cfg!(test) { Duration::from_millis(400) } else { Duration::from_secs(120) };

/// Runs `claude ARGS`, with what it said when it fails or does not finish in time.
fn run_claude(args: &[&str]) -> Result<(), String> {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = claude_cli()
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("claude: {e}; is Claude Code installed?"))?;
    let mut stderr = child.stderr.take().expect("piped");
    let said = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    let end = Instant::now() + CLI_WAIT;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if Instant::now() >= end {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("claude {}: no answer within {:?}", args.join(" "), CLI_WAIT));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if status.success() {
        return Ok(());
    }
    Err(format!("claude {}: {}", args.join(" "), said.join().unwrap_or_default().trim()))
}

/// Connects Claude Code to the display (`on`) or disconnects it: the plugin
/// with the hooks, through `claude plugin`, and the status line in Claude
/// Code's user settings. Disconnecting turns the plugin off, leaving it and its
/// marketplace installed, and puts the user's own status line command back.
/// Unreadable settings stop it before Claude Code is touched.
pub fn set_up(on: bool) -> Result<(), String> {
    read_settings()?;
    if on {
        // Installing adds the marketplace and enables the plugin; enabling covers
        // a plugin that was installed already and turned off.
        let (name, _) = PLUGIN.split_once('@').unwrap_or((PLUGIN, ""));
        let installed = run_claude(&["plugin", "install", name, "--marketplace", MARKETPLACE, "--scope", "user"]);
        if let (Err(e), Err(_)) = (&installed, run_claude(&["plugin", "enable", PLUGIN, "--scope", "user"])) {
            return Err(e.clone());
        }
        return edit_status_line(true);
    }
    let line = edit_status_line(false);
    // Fails when the plugin is not installed; what counts is whether it is still on.
    let _ = run_claude(&["plugin", "disable", PLUGIN, "--scope", "user"]);
    if setup().plugin {
        return Err(format!("Claude Code kept {PLUGIN} enabled; turn it off there with /plugin"));
    }
    line
}

/// The display's colours.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Palette {
    working: Rgb,
    compacting: Rgb,
    asking: Rgb,
    done: Rgb,
    error: Rgb,
}

impl Default for Palette {
    fn default() -> Palette {
        Palette {
            working: [0xff, 0x5a, 0x14],
            compacting: [0x96, 0x3c, 0xff],
            asking: [0xff, 0xb4, 0x00],
            done: [0x28, 0xff, 0x5a],
            error: [0xff, 0x00, 0x00],
        }
    }
}

/// The palette, with whatever $XDG_CONFIG_HOME/loginux/claude.toml sets: LEDs
/// do not show a colour the way a screen does. A value that is missing or
/// cannot be read keeps its default.
fn palette() -> Palette {
    let p = Palette::default();
    let text = fs::read_to_string(profile::config_dir().join("claude.toml")).unwrap_or_default();
    let doc: toml::Table = text.parse().unwrap_or_default();
    let colour = |key: &str, default: Rgb| doc.get(key).and_then(|v| v.as_str()).and_then(|s| parse_rgb(s).ok()).unwrap_or(default);
    Palette {
        working: colour("working", p.working),
        compacting: colour("compacting", p.compacting),
        asking: colour("asking", p.asking),
        done: colour("done", p.done),
        error: colour("error", p.error),
    }
}

struct Agent {
    /// Empty for the main agent.
    id: String,
    state: State,
    since: Instant,
    seen: Instant,
    /// A subagent's key on the number row while it runs: 0 for 1 up to 9 for 0.
    slot: Option<usize>,
}

/// One running Claude Code.
struct Instance {
    key: String,
    pid: Option<u32>,
    /// The main agent first, then its subagents.
    agents: Vec<Agent>,
    /// When the user last acted: a prompt, or an answer to a question.
    input: Instant,
    seen: Instant,
    closing: Option<Instant>,
    /// The share of its context window in use, from its status line.
    context: Option<f32>,
    /// Its context gauge has played since the context last emptied.
    gauged: bool,
}

impl Instance {
    fn alive(&self, now: Instant) -> bool {
        match self.closing {
            Some(at) => now - at < CLOSE,
            None => self.pid.map_or(now - self.seen < LEASE, |pid| Path::new(&format!("/proc/{pid}")).exists()),
        }
    }

    /// The state the bar shows, and since when: the most urgent agent's.
    fn shown(&self, now: Instant) -> (State, Instant) {
        let effective = |a: &Agent| match a.state {
            State::Working | State::Tool | State::Compacting if now - a.seen >= STALE => (State::Idle, a.seen + STALE),
            state => (state, a.since),
        };
        self.agents.iter().map(effective).max_by_key(|(state, _)| state.rank()).unwrap_or((State::Idle, self.seen))
    }
}

/// A sprite across the keys.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Show {
    /// An instance starts: Claude Code's spinner turns once.
    Spark,
    /// A long turn needs the user: an exclamation mark, two keys wide.
    Ask,
    /// A long turn ended: a check mark.
    Check,
    /// An API error: a cross that lands with a glitch.
    Cross,
    /// The context or a usage limit crossed a mark: its key, then the number
    /// row filling to its tenths; red once the limit is close.
    Gauge(Dial, u8, bool),
}

/// Claude Code's spinner, a shape per frame: d dot, p small plus, P plus, s star, x cross.
const SPINNER: &[u8] = b"ddppPPssxxssPPppdd";

impl Show {
    /// The sprite on the board's key grid, which is five rows from the function
    /// row down and one column per key unit from `left`, by key centres. A
    /// character is a pixel: for the spark, the arm of the spinner it belongs
    /// to; for the others, the frame it lights at. Then, per frame after the
    /// drawing, the sprite's brightness: 9 full down to 0 off, w white.
    fn sprite(self) -> (i32, [&'static str; 5], &'static str) {
        match self {
            Show::Spark => (6, ["x.P.x", ".xpx.", "PpopP", ".xpx.", "x.P.x"], ""),
            Show::Ask => (8, ["11", "22", "33", "..", "55"], "99009963"),
            Show::Check => (4, ["......7", ".....6.", "1...5..", ".2.4...", "..3...."], "99w963"),
            Show::Cross => (6, ["1...1", ".2.2.", "..3..", ".4.4.", "5...5"], "9009963"),
            Show::Gauge(..) => (0, [""; 5], "99009963"),
        }
    }

    /// Frames the drawing takes.
    fn drawing(self) -> i64 {
        let (_, rows, _) = self.sprite();
        match self {
            Show::Spark => SPINNER.len() as i64,
            Show::Gauge(_, tenths, _) => 1 + i64::from(tenths),
            _ => rows.iter().flat_map(|r| r.chars()).filter_map(|c| c.to_digit(10)).max().map_or(0, i64::from),
        }
    }

    /// Frames in all: one to dim the keys, the drawing, the outro, one to bring them back.
    fn length(self) -> i64 {
        self.drawing() + self.sprite().2.len() as i64 + 2
    }

    fn ink(self, palette: &Palette) -> Rgb {
        match self {
            Show::Spark => palette.working,
            Show::Ask => palette.asking,
            Show::Check => palette.done,
            Show::Cross | Show::Gauge(_, _, true) => palette.error,
            Show::Gauge(Dial::Context, ..) => palette.compacting,
            Show::Gauge(..) => palette.asking,
        }
    }
}

/// How brightly frame `t` of `show` lights the key `name` in `row` and `col`,
/// and whether in white.
fn glyph(show: Show, name: &str, row: i32, col: i32, t: i64) -> (f32, bool) {
    let (left, rows, outro) = show.sprite();
    let drawing = show.drawing();
    if t < 1 || t > drawing + outro.len() as i64 {
        return (0.0, false);
    }
    if let Show::Gauge(dial, tenths, _) = show {
        // The label first, then one number key a frame: 1 for a tenth up to 0 for all.
        let digit = "1234567890".find(name).filter(|&i| name.len() == 1 && i < usize::from(tenths));
        let order = if name == dial.key() { Some(0) } else { digit.map(|i| i as i64 + 1) };
        return match order {
            Some(order) if t <= drawing => (if order < t { 1.0 } else { 0.0 }, false),
            Some(_) => outro_level(outro, t - drawing - 1),
            None => (0.0, false),
        };
    }
    // A cross lands with its rows jumping sideways.
    let jump = if show == Show::Cross && t <= drawing { [0, 1, -1, 1, 0, -1][(i64::from(row) * 7 + t * 3).rem_euclid(6) as usize] } else { 0 };
    let pixel = usize::try_from(row).ok().and_then(|r| rows.get(r)).zip(usize::try_from(col - left - jump).ok()).and_then(|(r, c)| r.chars().nth(c));
    let Some(pixel) = pixel.filter(|&p| p != '.') else { return (0.0, false) };
    if t <= drawing {
        let lit = match show {
            Show::Spark => {
                let shape = SPINNER[t as usize - 1];
                match pixel {
                    'o' => true,
                    'p' => matches!(shape, b'p' | b'P' | b's'),
                    'P' => matches!(shape, b'P' | b's'),
                    _ => matches!(shape, b's' | b'x'),
                }
            }
            _ => pixel.to_digit(10).is_some_and(|d| i64::from(d) <= t),
        };
        return (if lit { 1.0 } else { 0.0 }, false);
    }
    outro_level(outro, t - drawing - 1)
}

/// Frame `k` of an outro: 9 full down to 0 off, or w white.
fn outro_level(outro: &str, k: i64) -> (f32, bool) {
    match outro.as_bytes()[k as usize] {
        b'w' => (1.0, true),
        b => (f32::from(b - b'0') / 9.0, false),
    }
}

/// One LED of an instance's share of the bar, `i` of `w`, `f` frames on the
/// display's clock and `s` frames into the state: its brightness, and whether
/// it is white.
fn pixel(state: State, i: usize, w: usize, f: i64, s: i64) -> (f32, bool) {
    const DIM: f32 = 0.25;
    const MID: f32 = 0.55;
    let (i, w) = (i as i64, w as i64);
    let reach = (w - 1) / 2; // rings from the middle out
    let ring = (2 * i - (w - 1)).abs() / 2; // 0 for the middle LED or two
    let edge = i.min(w - 1 - i); // 0 for the outermost LEDs
    let lit = |on: bool, off: f32| if on { 1.0 } else { off };
    let level = match state {
        // Standing by: the share lights from its middle, then beats every 3.2 s.
        State::Idle if s <= reach => lit(ring <= s, 0.0) * DIM,
        State::Idle => match (ring, f.rem_euclid(40)) {
            (0..=1, 0 | 1) | (0, 4 | 5) => 1.0,
            (2, 0 | 1) | (1, 4 | 5) => MID,
            _ => DIM,
        },
        // Thinking: Claude's spark pulses from the middle, four LEDs out at most.
        State::Working if reach == 0 => lit(f.rem_euclid(4) < 2, MID),
        State::Working => {
            let span = reach.min(4);
            let r = span - (f.rem_euclid(2 * span) - span).abs();
            if ring == r {
                1.0
            } else if ring < r {
                MID
            } else {
                DIM
            }
        }
        // A tool: pairs of LEDs run out from the middle.
        State::Tool => lit((ring - f).rem_euclid(6) < 2, DIM),
        // Compacting: the share squeezes in from both ends.
        State::Compacting => {
            let q = f.rem_euclid(reach + 3);
            if edge == q {
                1.0
            } else if edge > q {
                MID
            } else {
                0.0
            }
        }
        // Waiting for the user: lights run in from both ends to a blinking middle.
        State::Asking if ring == 0 => lit(f.rem_euclid(6) < 3, DIM),
        State::Asking => lit((edge - f).rem_euclid(4) == 0, DIM),
        // Done: the share fills from both ends and stays lit, a glint crossing it every 6.4 s.
        State::Done if s <= reach => lit(edge <= s, 0.0),
        State::Done if (0..2).contains(&((s - reach - 1).rem_euclid(80) - i)) => return (1.0, true),
        State::Done => lit(w < 4 || edge > 0, MID),
        // An error: two flickers, then a broken line, gaps wide enough to survive
        // the bar's diffuser, that jumps a step every 3.2 s.
        State::Error if s < 6 => lit(s % 2 == 0, 0.0),
        State::Error => lit((i + i64::from(f.rem_euclid(40) < 2)) % 5 < 3, 0.0),
    };
    (level, false)
}

/// The instances' states and the sprite playing, if any.
pub struct Display {
    instances: Vec<Instance>,
    show: Option<(Show, Instant)>,
    palette: Palette,
    /// The account's usage limits as last reported: five hours, seven days, spend.
    limits: [Option<Window>; 3],
    /// How many of LIMIT_MARKS each limit has played its gauge for.
    marked: [usize; 3],
    /// A turn ended on a usage limit, and Claude has not worked since.
    limited: bool,
    /// The pixel art runs on this clock, so patterns keep their rhythm through
    /// changes of state.
    epoch: Instant,
}

impl Default for Display {
    fn default() -> Display {
        Display {
            instances: vec![],
            show: None,
            palette: Palette::default(),
            limits: [None; 3],
            marked: [0; 3],
            limited: false,
            epoch: Instant::now(),
        }
    }
}

impl Display {
    /// Takes in one signal from an instance, or from a subagent in one.
    /// `running` names the subagents Claude Code still runs in the background
    /// when a turn ends.
    pub fn signal(&mut self, key: &str, signal: Signal, pid: Option<u32>, running: &[String], now: Instant) {
        self.palette = palette();
        self.prune(now);
        let (name, agent) = key.split_once('/').unwrap_or((key, ""));
        let at = self.instances.iter().position(|i| i.key == name);
        if signal == Signal::End {
            if let Some(inst) = at.map(|i| &mut self.instances[i]) {
                if agent.is_empty() {
                    inst.closing.get_or_insert(now);
                } else {
                    inst.agents.retain(|a| a.id != agent);
                }
            }
            return;
        }
        if matches!(signal, Signal::Prompt | Signal::Working | Signal::Tool) {
            self.limited = false; // Claude works again
        }
        self.limited |= signal == Signal::Limit;
        let i = self.instance(at, name, pid, now);
        let taken: Vec<usize> = self.instances.iter().flat_map(|i| &i.agents).filter_map(|a| a.slot).collect();
        let free = (0..10).find(|s| !taken.contains(s));
        let inst = &mut self.instances[i];
        (inst.pid, inst.seen, inst.closing) = (pid.or(inst.pid), now, None);
        let before = inst.agents.iter().find(|a| a.id == agent).map(|a| a.state);
        let state = match signal {
            Signal::Start => State::Idle,
            Signal::Prompt | Signal::Working => State::Working,
            Signal::Tool => State::Tool,
            Signal::Compact => State::Compacting,
            Signal::Asking => State::Asking,
            Signal::Done => State::Done,
            // Claude Code waits for input: a turn an Esc ended is over, while an
            // error, or an instance with no turn yet, stays as it is.
            Signal::Waiting => before.filter(|s| matches!(s, State::Idle | State::Error)).unwrap_or(State::Done),
            _ => State::Error,
        };
        if agent.is_empty() {
            // Settles what the subagents were doing, such as a permission an Esc
            // left unanswered; one that still runs comes back with its next event.
            match signal {
                // A new session, or Claude Code idle, which it never is with work
                // in the background: no subagent runs.
                Signal::Start | Signal::Waiting => inst.agents.truncate(1),
                // The end of a turn: what Claude Code lists as running goes on.
                Signal::Done => inst.agents.retain(|a| a.id.is_empty() || running.contains(&a.id)),
                // A prompt answers, or abandons, a subagent's question.
                Signal::Prompt => inst.agents.retain(|a| a.id.is_empty() || a.state != State::Asking),
                _ => {}
            }
        }
        if signal == Signal::Prompt {
            inst.input = now;
        }
        if before == Some(State::Asking) && state != State::Asking {
            inst.input = now; // answered
        }
        match inst.agents.iter_mut().find(|a| a.id == agent) {
            Some(a) if a.state == state => a.seen = now,
            Some(a) => (a.state, a.since, a.seen) = (state, now, now),
            None => inst.agents.push(Agent { id: agent.into(), state, since: now, seen: now, slot: None }),
        }
        // A subagent takes the lowest free key on the number row, and keeps it while it runs.
        if let Some(a) = inst.agents.iter_mut().find(|a| a.id == agent && !agent.is_empty() && a.slot.is_none()) {
            a.slot = free;
        }
        let changed = before != Some(state);
        let away = now - inst.input >= AWAY;
        let show = match state {
            State::Asking if changed && away => Some(Show::Ask),
            State::Done if changed && away && agent.is_empty() && signal == Signal::Done => Some(Show::Check),
            State::Error if changed && agent.is_empty() => Some(Show::Cross),
            _ => None,
        };
        if let Some(show) = show {
            self.play(show, now);
        }
    }

    /// The instance at `at`, or a new one named `name`, which starts with a spark.
    fn instance(&mut self, at: Option<usize>, name: &str, pid: Option<u32>, now: Instant) -> usize {
        at.unwrap_or_else(|| {
            self.play(Show::Spark, now);
            let main = Agent { id: String::new(), state: State::Idle, since: now, seen: now, slot: None };
            let inst = Instance { key: name.into(), pid, agents: vec![main], input: now, seen: now, closing: None, context: None, gauged: false };
            self.instances.push(inst);
            self.instances.len() - 1
        })
    }

    /// Takes in what an instance's status line reports: its context, which closes
    /// in on its share and plays a gauge at CONTEXT[2], and the account's usage
    /// limits, which play a gauge at each of LIMIT_MARKS.
    pub fn usage(&mut self, key: &str, usage: Usage, pid: Option<u32>, now: Instant) {
        self.palette = palette();
        self.prune(now);
        let name = key.split_once('/').map_or(key, |(name, _)| name);
        let at = self.instances.iter().position(|i| i.key == name);
        // A status line that names no process cannot be matched to its hooks'
        // instance: it reports only the limits, so no instance appears twice.
        if at.is_some() || pid.is_some() {
            let i = self.instance(at, name, pid, now);
            let inst = &mut self.instances[i];
            (inst.pid, inst.context) = (pid.or(inst.pid), usage.context);
            let full = usage.context.is_some_and(|c| c >= CONTEXT[2]);
            if !usage.context.is_some_and(|c| c >= 50.0) {
                inst.gauged = false; // emptied, as by compacting: the gauge can play again
            }
            if full && !inst.gauged && self.play(Show::Gauge(Dial::Context, (usage.context.unwrap_or_default() / 10.0) as u8, false), now) {
                self.instances[i].gauged = true;
            }
        }
        if !usage.limits {
            return;
        }
        for (n, (dial, window)) in [(Dial::FiveHour, usage.five_hour), (Dial::Week, usage.seven_day), (Dial::Spend, usage.spend)].into_iter().enumerate() {
            self.limits[n] = window;
            let used = window.map_or(0.0, |w| w.used);
            let marks = LIMIT_MARKS.iter().filter(|&&m| used >= m).count();
            // Below half, or gone from the report, the window has reset: its marks can play again.
            if used < 50.0 {
                self.marked[n] = 0;
            } else if marks > self.marked[n] && self.play(Show::Gauge(dial, (used / 10.0).min(10.0) as u8, used >= LIMIT_MARKS[2]), now) {
                self.marked[n] = marks;
            }
        }
    }

    /// The colour of one of the bar's end lamps for the limits in `ns`, if one
    /// is close: amber from the first mark, red and blinking from the last, red
    /// once reached.
    fn lamp(&self, ns: &[usize], f: i64) -> Option<Rgb> {
        let unix = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        let used = ns.iter().filter_map(|&n| self.limits[n]).filter(|w| w.resets_at == 0 || w.resets_at > unix).map(|w| w.used).fold(None, |a: Option<f32>, u| Some(a.map_or(u, |a| a.max(u))));
        match used {
            _ if self.limited => Some(self.palette.error),
            Some(u) if u >= 100.0 => Some(self.palette.error),
            Some(u) if u >= LIMIT_MARKS[2] => Some(scale(self.palette.error, if f.rem_euclid(8) < 4 { 1.0 } else { 0.12 })),
            Some(u) if u >= LIMIT_MARKS[0] => Some(self.palette.asking),
            _ => None,
        }
    }

    /// Starts `show` unless another started within COOLDOWN; whether it did.
    fn play(&mut self, show: Show, now: Instant) -> bool {
        let free = self.show.is_none_or(|(_, at)| now - at >= COOLDOWN);
        if free {
            self.show = Some((show, now));
        }
        free
    }

    /// Drops the instances whose process has ended, or that went quiet or closed.
    pub fn prune(&mut self, now: Instant) {
        self.instances.retain(|i| i.alive(now));
    }

    /// Frames since the epoch.
    fn step(&self, at: Instant) -> i64 {
        ((at - self.epoch).as_secs_f64() / STEP) as i64
    }

    fn showing(&self, now: Instant) -> Option<(Show, i64)> {
        self.show.map(|(show, at)| (show, self.step(now) - self.step(at))).filter(|&(show, t)| t < show.length())
    }

    /// Each running instance's state, in their order on the bar.
    pub fn states(&self, now: Instant) -> Vec<State> {
        self.instances.iter().filter(|i| i.closing.is_none() && i.alive(now)).map(|i| i.shown(now).0).collect()
    }

    /// The wait until the next frame of the pixel art, or None while there is
    /// none: every state on the bar moves now and then.
    pub fn next_frame(&self, now: Instant) -> Option<Duration> {
        let moving = self.showing(now).is_some() || self.instances.iter().any(|i| i.alive(now));
        let into = (now - self.epoch).as_secs_f64() % STEP;
        moving.then(|| Duration::from_secs_f64(STEP - into))
    }

    fn colour(&self, state: State) -> Rgb {
        match state {
            State::Compacting => self.palette.compacting,
            State::Asking => self.palette.asking,
            State::Done => self.palette.done,
            State::Error => self.palette.error,
            _ => self.palette.working,
        }
    }

    /// A subagent's key on the number row at frame `f`: blinking in its state's
    /// colour, quickly while a tool runs or it waits for the user, dark between
    /// blinks. None once it has been quiet long enough to read as idle.
    fn agent_key(&self, a: &Agent, f: i64, now: Instant) -> Option<Rgb> {
        let stale = matches!(a.state, State::Working | State::Tool | State::Compacting) && now - a.seen >= STALE;
        let period = if matches!(a.state, State::Tool | State::Asking) { 6 } else { 12 };
        (!stale).then(|| if f.rem_euclid(period) < period / 2 { self.colour(a.state) } else { [0; 3] })
    }

    /// LED `i` of `w` in an instance's share of the bar, over the profile's
    /// colour for that segment.
    fn share(&self, inst: &Instance, i: usize, w: usize, backdrop: Rgb, now: Instant) -> Rgb {
        let (state, since) = inst.shown(now);
        let ink = self.colour(state);
        let f = self.step(now);
        if let Some(at) = inst.closing {
            return closing(ink, backdrop, i, w, f - self.step(at));
        }
        // A context filling up closes in on the share from both ends, in the
        // colour of compacting, up to a third of it, leaving the rest to the state.
        let third = w.div_ceil(6);
        let walls = inst.context.map_or(0, |c| (((c - CONTEXT[0]) / (CONTEXT[1] - CONTEXT[0])).clamp(0.0, 1.0) * third as f32).ceil() as usize);
        if i.min(w - 1 - i) < walls.min((w - 1) / 2) {
            return scale(self.palette.compacting, 0.55);
        }
        match pixel(state, i, w, f, f - self.step(since)) {
            (_, true) => mix(ink, [0xff; 3], 0.7),
            (level, false) => scale(ink, level),
        }
    }

    /// The colour of each key, light and strip segment in `zones` while the
    /// display shows anything over the chosen profile; None lets the profile
    /// show on its own.
    pub fn frame(&self, model: &Model, zones: &[u8], profile: &Profile, now: Instant) -> Option<Vec<(u8, Rgb)>> {
        let shares: Vec<&Instance> = self.instances.iter().filter(|i| i.alive(now)).collect();
        let show = self.showing(now);
        if shares.is_empty() && show.is_none() {
            return None;
        }
        let bar = status_zones(model);
        let (n, k) = (bar.len(), shares.len().min(bar.len()));
        // A closing share gives its segments back to the profile only when no
        // instance stays to take them.
        let last = shares.iter().all(|i| i.closing.is_some());
        // The bar's end lamps, on a bar long enough to spare them: five hours at
        // the left, the week or the spend limit at the right.
        let f = self.step(now);
        let lamps = if n >= 4 && k > 0 { [self.lamp(&[0], f), self.lamp(&[1, 2], f)] } else { [None; 2] };
        // Each running subagent blinks its key on the number row, 1 to 0.
        let mut digits = [None; 10];
        for a in shares.iter().filter(|i| i.closing.is_none()).flat_map(|i| &i.agents) {
            if let Some(slot) = a.slot {
                digits[slot] = self.agent_key(a, f, now);
            }
        }
        let mut out = vec![];
        for z in zones.iter().filter_map(|&id| model.zone(id)).filter(|z| z.kind != Kind::Unknown) {
            let [x, y, w, h] = z.rect;
            let backdrop = match &profile.effect {
                Some(e) => e.still((x + w / 2.0) / model.board[0]),
                None => profile.colour(model, z.id).unwrap_or_default(),
            };
            let digit = "1234567890".find(z.name.as_str()).filter(|_| z.name.len() == 1).and_then(|d| digits[d]);
            let colour = match (bar.iter().position(|&b| b == z.id), show) {
                (Some(0), _) if lamps[0].is_some() => lamps[0].unwrap_or_default(),
                (Some(j), _) if j == n - 1 && lamps[1].is_some() => lamps[1].unwrap_or_default(),
                (Some(j), _) if k > 0 => {
                    let (m, i, w) = share_of(j, n, k);
                    self.share(shares[m], i, w, if last { backdrop } else { [0; 3] }, now)
                }
                (None, Some((show, t))) => {
                    // The keys go dark around the sprite, with a frame at each end to soften the cut.
                    let dimmed = scale(backdrop, if t == 0 || t == show.length() - 1 { 0.3 } else { 0.0 });
                    match glyph(show, &z.name, (y + h / 2.0).floor() as i32, (x + w / 2.0).floor() as i32, t) {
                        (level, true) => mix(dimmed, [0xff; 3], level),
                        (level, false) => mix(dimmed, show.ink(&self.palette), level),
                    }
                }
                (None, None) if digit.is_some() => digit.unwrap_or_default(),
                _ => backdrop,
            };
            out.push((z.id, colour));
        }
        Some(out)
    }
}

/// LED `i` of `w` in the share of an instance that ended `s` frames ago: a
/// white flash, then the share shrinks to its middle in its colour, giving the
/// segments back to the profile from both ends, and its last dot fades.
fn closing(ink: Rgb, backdrop: Rgb, i: usize, w: usize, s: i64) -> Rgb {
    let (i, w) = (i as i64, w as i64);
    let reach = (w - 1) / 2;
    let ring = (2 * i - (w - 1)).abs() / 2;
    match s {
        0 => [0xff; 3],
        1 => mix(ink, [0xff; 3], 0.5),
        2..14 if ring <= reach - (s - 2) * (reach + 1) / 12 => ink,
        14..17 if ring == 0 => ink,
        17..20 if ring == 0 => mix(backdrop, ink, [0.6, 0.3, 0.1][(s - 17) as usize]),
        _ => backdrop,
    }
}

/// When `k` instances split a bar of `n` LEDs in their order, each as equal a
/// share as the bar allows: the instance LED `j` belongs to, the LED's place in
/// that share, and the share's width. Instances beyond the bar's length wait
/// for a share.
fn share_of(j: usize, n: usize, k: usize) -> (usize, usize, usize) {
    let m = ((j + 1) * k - 1) / n;
    let (start, end) = (m * n / k, (m + 1) * n / k);
    (m, j - start, end - start)
}

/// The zones that show the status: the light strips, or on a device without
/// one, its lights.
fn status_zones(model: &Model) -> Vec<u8> {
    let strips: Vec<u8> = model.strips.iter().flat_map(|s| s.zones.iter().copied()).collect();
    if !strips.is_empty() {
        return strips;
    }
    model.zones.iter().filter(|z| z.group == "lights").map(|z| z.id).collect()
}

fn scale(c: Rgb, f: f32) -> Rgb {
    c.map(|v| (f32::from(v) * f.clamp(0.0, 1.0)).round() as u8)
}

fn mix(from: Rgb, to: Rgb, f: f32) -> Rgb {
    let f = f.clamp(0.0, 1.0);
    std::array::from_fn(|i| (f32::from(from[i]) + (f32::from(to[i]) - f32::from(from[i])) * f).round() as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout;

    fn g316() -> &'static Model {
        crate::testenv::init();
        layout::find("g316-x-98").unwrap()
    }

    fn zones(m: &Model) -> Vec<u8> {
        m.zones.iter().map(|z| z.id).collect()
    }

    fn hook(json: &str) -> Option<(String, Signal)> {
        from_hook(json, Some("cli"), None)
    }

    /// The middle of frame `f` of the display's clock.
    fn at(d: &Display, f: i64) -> Instant {
        d.epoch + Duration::from_secs_f64((f as f64 + 0.5) * STEP)
    }

    /// A pid no process has: above Linux's largest.
    const GONE: Option<u32> = Some(u32::MAX);

    #[test]
    fn hook_events_become_signals() {
        use Signal::*;
        for (event, extra, want) in [
            ("SessionStart", r#""source":"startup""#, Some(Start)),
            ("SessionStart", r#""source":"clear""#, Some(Start)),
            ("SessionStart", r#""source":"compact""#, None),
            ("UserPromptSubmit", r#""prompt":"hi""#, Some(Prompt)),
            ("PreToolUse", r#""tool_name":"Bash","tool_input":{"command":"ls"}"#, Some(Tool)),
            ("PreToolUse", r#""tool_name":"AskUserQuestion""#, Some(Asking)),
            ("PreToolUse", r#""tool_name":"ExitPlanMode""#, Some(Asking)),
            ("PermissionRequest", r#""tool_name":"Bash""#, Some(Asking)),
            ("PostToolUse", r#""tool_response":{"big":[1,2,3]}"#, Some(Working)),
            ("PostToolUseFailure", r#""error":"Exit code 1""#, Some(Working)),
            ("Notification", r#""notification_type":"permission_prompt""#, Some(Asking)),
            ("Notification", r#""notification_type":"elicitation_dialog""#, Some(Asking)),
            ("Notification", r#""notification_type":"idle_prompt""#, Some(Waiting)),
            ("Notification", r#""notification_type":"auth_success""#, None),
            ("PreCompact", r#""trigger":"auto""#, Some(Compact)),
            ("PostCompact", r#""trigger":"manual""#, Some(Waiting)),
            ("PostCompact", r#""trigger":"auto""#, Some(Working)),
            ("Stop", r#""stop_hook_active":false"#, Some(Done)),
            ("StopFailure", r#""error":"rate_limit""#, Some(Limit)),
            ("StopFailure", r#""error":"overloaded""#, Some(Error)),
            ("Notification", r#""notification_type":"quota_auto_resume_fired""#, Some(Working)),
            ("SessionEnd", r#""reason":"prompt_input_exit""#, Some(End)),
            ("SessionEnd", r#""reason":"clear""#, None),
            ("SessionEnd", r#""reason":"resume""#, None),
            ("CwdChanged", r#""cwd":"/""#, None),
        ] {
            let json = format!(r#"{{"session_id":"s1","hook_event_name":"{event}",{extra}}}"#);
            assert_eq!(hook(&json), want.map(|w| ("s1".to_string(), w)), "{event} {extra}");
        }
    }

    #[test]
    fn an_instance_is_its_claude_code_process_and_headless_runs_are_left_out() {
        let tool = r#"{"session_id":"s1","agent_id":"a7","hook_event_name":"PreToolUse","tool_name":"Read"}"#;
        assert_eq!(from_hook(tool, Some("cli"), Some(4242)), Some(("4242/a7".into(), Signal::Tool)));
        assert_eq!(hook(tool), Some(("s1/a7".into(), Signal::Tool)), "no process: the session");
        let stop = r#"{"session_id":"s1","agent_id":"a7","hook_event_name":"SubagentStop"}"#;
        assert_eq!(hook(stop), Some(("s1/a7".into(), Signal::End)));
        assert_eq!(hook(r#"{"session_id":"s1","hook_event_name":"SubagentStop"}"#), None, "never ends the instance");
        let start = r#"{"session_id":"s1","agent_id":"a7","agent_type":"Explore","hook_event_name":"SubagentStart"}"#;
        assert_eq!(hook(start), Some(("s1/a7".into(), Signal::Working)), "a subagent starts");
        assert_eq!(hook(r#"{"session_id":"s1","hook_event_name":"SubagentStart"}"#), None);
        let tasks = r#"{"hook_event_name":"Stop","background_tasks":[{"id":"a7","type":"subagent","status":"running"},{"id":"b2","type":"shell"},{"type":"subagent"},{"id":3,"type":"subagent"}]}"#;
        assert_eq!(background_agents(tasks), ["a7"], "subagents only, with an ID");
        assert!(background_agents(r#"{"hook_event_name":"Stop"}"#).is_empty() && background_agents("not json").is_empty());
        let nulls = r#"{"session_id":"s1","agent_id":null,"hook_event_name":"Stop","notification_type":null}"#;
        assert_eq!(hook(nulls), Some(("s1".into(), Signal::Done)), "a null field reads as missing");
        for headless in ["sdk-cli", "sdk-ts", "sdk-py"] {
            assert_eq!(from_hook(tool, Some(headless), None), None, "{headless}");
        }
        assert!(from_hook(tool, None, None).is_some(), "no entrypoint: shown");
        assert_eq!(hook("not json"), None);
        assert_eq!(Signal::from_name("compact").unwrap(), Signal::Compact);
        assert!(Signal::from_name("thinking").unwrap_err().contains("is not one of start, prompt, working"));
    }

    #[test]
    fn instances_split_the_bar_as_equally_as_it_allows() {
        let widths = |n: usize, k: usize| (0..n).filter(|&j| share_of(j, n, k).1 == 0).map(|j| share_of(j, n, k).2).collect::<Vec<_>>();
        assert_eq!(widths(30, 1), [30]);
        assert_eq!(widths(30, 3), [10, 10, 10]);
        assert_eq!(widths(30, 4), [7, 8, 7, 8], "as equal as 30 allows");
        assert_eq!(widths(30, 30), [1; 30]);
        assert_eq!((share_of(7, 30, 4), share_of(29, 30, 4)), ((1, 0, 8), (3, 7, 8)));
        // In the display: in their order, one LED each at most thirty.
        let _serial = crate::testenv::serial();
        let mut d = Display::default();
        let t = at(&d, 0);
        for i in 0..31 {
            d.signal(&i.to_string(), Signal::Start, None, &[], t);
        }
        d.signal("0", Signal::Error, None, &[], t);
        let frame = d.frame(g316(), &zones(g316()), &Profile::default(), at(&d, 100)).unwrap();
        let bar = |j: usize| frame.iter().find(|z| z.0 == g316().strips[0].zones[j]).unwrap().1;
        assert_eq!((bar(0), bar(1)), (Palette::default().error, scale(Palette::default().working, 0.25)), "the first instance, alone on LED 1");
        assert_eq!(d.states(t).len(), 31, "the 31st runs, waiting for an LED");
    }

    #[test]
    fn an_instance_stays_while_its_process_runs() {
        let _serial = crate::testenv::serial();
        let mut d = Display::default();
        let t = at(&d, 0);
        d.signal("mine", Signal::Start, Some(std::process::id()), &[], t);
        d.signal("gone", Signal::Start, GONE, &[], t);
        d.signal("manual", Signal::Start, None, &[], t);
        d.prune(t);
        assert_eq!(d.states(t), [State::Idle, State::Idle], "a process that ended goes at once");
        let later = t + LEASE;
        assert_eq!(d.states(later), [State::Idle], "one without a process lapses after half an hour");
        // An ended instance closes its share, then leaves; a new session reopens it.
        d.signal("mine", Signal::End, None, &[], t);
        assert!(d.states(t).len() == 1 && d.instances[0].alive(t + CLOSE / 2));
        d.signal("mine", Signal::Start, None, &[], t);
        assert_eq!(d.states(t).len(), 2);
        d.signal("mine", Signal::End, None, &[], t);
        d.prune(t + CLOSE);
        assert_eq!(d.instances.len(), 1);
        d.signal("unknown", Signal::End, None, &[], t); // nothing to end
    }

    #[test]
    fn the_most_urgent_agent_shows_and_a_turn_settles_the_subagents() {
        let _serial = crate::testenv::serial();
        let mut d = Display::default();
        let t = at(&d, 0);
        d.signal("1", Signal::Prompt, None, &[], t);
        d.signal("1/a", Signal::Asking, None, &[], t);
        assert_eq!(d.states(t), [State::Asking], "a subagent waiting for the user");
        d.signal("1", Signal::Done, None, &[], t);
        assert_eq!((d.states(t), d.instances[0].agents.len()), (vec![State::Done], 1), "the turn's end settles it");
        d.signal("1/b", Signal::Tool, None, &[], t);
        assert_eq!(d.states(t), [State::Done], "still running, below finished");
        d.signal("1/b", Signal::End, None, &[], t);
        assert_eq!(d.instances[0].agents.len(), 1);
        // Thinking with no event for a quarter of an hour reads as idle: an Esc sends none.
        d.signal("1", Signal::Compact, None, &[], t);
        assert_eq!((d.states(t + STALE - Duration::from_secs(1)), d.states(t + STALE)), (vec![State::Compacting], vec![State::Idle]));
    }

    #[test]
    fn subagents_blink_their_keys_on_the_number_row() {
        let _serial = crate::testenv::serial();
        let _ = fs::remove_file(profile::config_dir().join("claude.toml"));
        let (m, p) = (g316(), Palette::default());
        let mut grey = Profile::default();
        grey.bases.insert("keys".into(), [9; 3]);
        // The number row at frame f: w working or a tool, a asking, . dark between blinks, - the profile.
        let row = |d: &Display, f: i64| -> String {
            let frame = d.frame(m, &zones(m), &grey, at(d, f)).unwrap();
            let key = |c: char| frame.iter().find(|z| z.0 == m.named(&c.to_string()).unwrap().id).unwrap().1;
            "1234567890".chars().map(|c| [('-', [9; 3]), ('.', [0; 3]), ('w', p.working), ('a', p.asking)].iter().find(|k| k.1 == key(c)).map_or('?', |k| k.0)).collect()
        };
        let mut d = Display::default();
        let t = at(&d, 0);
        d.signal("1", Signal::Prompt, None, &[], t);
        for agent in ["1/a", "1/b", "1/c"] {
            d.signal(agent, Signal::Working, None, &[], t); // SubagentStart
        }
        d.signal("1/b", Signal::Tool, None, &[], t);
        d.show = None;
        // Thinking blinks once a second, a tool twice.
        assert_eq!([row(&d, 0), row(&d, 3), row(&d, 6), row(&d, 9)], ["www-------", "w.w-------", ".w.-------", "...-------"]);
        // A key stays with its agent; the next agent takes the lowest free one.
        d.signal("1/b", Signal::End, None, &[], t);
        assert_eq!(row(&d, 0), "w-w-------");
        d.signal("1/d", Signal::Working, None, &[], t);
        d.signal("2/x", Signal::Working, None, &[], t); // another instance's subagent
        d.show = None;
        assert_eq!(row(&d, 0), "wwww------");
        d.signal("1/c", Signal::Asking, None, &[], t);
        assert_eq!([row(&d, 0), row(&d, 6)], ["wwaw------", "..a.------"], "waiting for the user: the quick blink");
        // The turn ends with a and c running in the background: d is done.
        d.signal("1", Signal::Done, None, &["a".into(), "c".into()], t);
        assert_eq!(row(&d, 0), "w-aw------");
        // A prompt answers c's question; idle, Claude Code runs nothing more.
        d.signal("1", Signal::Prompt, None, &[], t);
        assert_eq!(row(&d, 0), "w--w------");
        d.signal("2", Signal::Waiting, None, &[], t);
        assert_eq!(row(&d, 0), "w---------");
        // Quiet for a quarter of an hour: no longer shown.
        assert_eq!(row(&d, 12 + (STALE.as_secs_f64() / STEP) as i64), "----------");
        d.signal("1", Signal::Start, None, &[], t);
        assert_eq!(row(&d, 0), "----------", "a new session");
        // Ten keys: an eleventh agent waits for one, and takes it with its next event.
        for n in 0..11 {
            d.signal(&format!("1/n{n}"), Signal::Working, None, &[], t);
        }
        assert_eq!((row(&d, 0), d.instances[0].agents[11].slot), ("wwwwwwwwww".into(), None));
        d.signal("1/n4", Signal::End, None, &[], t);
        d.signal("1/n10", Signal::Tool, None, &[], t);
        assert_eq!((row(&d, 6), d.instances[0].agents[10].slot), ("....w.....".into(), Some(4)));
        // A closing instance's subagents go with it.
        d.signal("1", Signal::End, None, &[], t);
        assert_eq!(row(&d, 0), "----------");
    }

    /// Which sprite plays.
    fn shown(d: &Display) -> Option<Show> {
        d.show.map(|s| s.0)
    }

    #[test]
    fn sprites_play_when_an_instance_starts_and_when_a_long_turn_wants_the_user() {
        let _serial = crate::testenv::serial();
        let mut d = Display::default();
        let t = at(&d, 0);
        d.signal("1", Signal::Start, None, &[], t);
        assert_eq!(shown(&d), Some(Show::Spark));
        d.show = None;
        d.signal("1", Signal::Start, None, &[], t);
        assert_eq!(shown(&d), None, "a new session in a running instance: no spark");
        // Within half a minute of the prompt the user is still watching: the bar is enough.
        d.signal("1", Signal::Prompt, None, &[], t);
        d.signal("1", Signal::Asking, None, &[], t + AWAY / 2);
        d.signal("1", Signal::Working, None, &[], t + AWAY);
        d.signal("1", Signal::Done, None, &[], t + AWAY);
        assert_eq!(shown(&d), None);
        // After a long turn they may have looked away.
        d.signal("1", Signal::Prompt, None, &[], t);
        d.signal("1/x", Signal::Asking, None, &[], t + AWAY);
        assert_eq!(shown(&d), Some(Show::Ask), "a subagent asks too");
        d.show = None;
        d.signal("1/x", Signal::Working, None, &[], t + AWAY * 2); // answered
        d.signal("1/x", Signal::Done, None, &[], t + AWAY * 3);
        assert_eq!(shown(&d), None, "only the main agent's turn ends");
        d.signal("1", Signal::Done, None, &[], t + AWAY * 3);
        assert_eq!(shown(&d), Some(Show::Check));
        // An error always shows, unless another sprite started within ten seconds.
        d.signal("1", Signal::Error, None, &[], t + AWAY * 3 + COOLDOWN / 2);
        assert_eq!(shown(&d), Some(Show::Check));
        d.signal("1", Signal::Prompt, None, &[], t + AWAY * 4);
        d.signal("1", Signal::Error, None, &[], t + AWAY * 4);
        assert_eq!(shown(&d), Some(Show::Cross));
    }

    #[test]
    fn bar_patterns_hold_at_any_width() {
        let row = |state: State, w: usize, f: i64, s: i64| (0..w).map(|i| pixel(state, i, w, f, s)).map(|(l, white)| if white { 'w' } else { ".-=#".as_bytes()[((l * 3.0).ceil() as usize).min(3)] as char }).collect::<String>();
        // Idle: lit from the middle, then a beat every 40 frames.
        assert_eq!(row(State::Idle, 10, 0, 1), "...----...");
        assert_eq!((row(State::Idle, 10, 40, 9), row(State::Idle, 10, 44, 9), row(State::Idle, 10, 2, 9)), ("--=####=--".into(), "---=##=---".into(), "----------".into()));
        // Thinking: a pulse four LEDs out at most.
        assert_eq!((row(State::Working, 10, 0, 9), row(State::Working, 10, 4, 9)), ("----##----".into(), "#========#".into()), "the wavefront bright, inside it half");
        assert_eq!((row(State::Working, 30, 4, 9).matches('#').count(), row(State::Working, 1, 0, 9), row(State::Working, 1, 2, 9)), (2, "#".into(), "=".into()));
        // A tool: pairs run out from the middle.
        assert_eq!((row(State::Tool, 12, 0, 9), row(State::Tool, 12, 1, 9)), ("----####----".into(), "---##--##---".into()));
        // Compacting squeezes in; asking runs in to a blinking middle.
        assert_eq!((row(State::Compacting, 8, 0, 9), row(State::Compacting, 8, 2, 9)), ("#======#".into(), "..#==#..".into()));
        assert_eq!((row(State::Asking, 9, 0, 9), row(State::Asking, 9, 3, 9)), ("#---#---#".into(), "---#-#---".into()));
        // Done fills from both ends, a glint crosses, and it stays lit between dim ends.
        assert_eq!((row(State::Done, 8, 0, 1), row(State::Done, 8, 0, 6), row(State::Done, 8, 0, 20)), ("##....##".into(), "=ww####=".into(), "=######=".into()));
        assert_eq!(row(State::Done, 2, 0, 20), "##", "too short for dim ends");
        assert_eq!(row(State::Done, 8, 0, 86), row(State::Done, 8, 0, 6), "the glint comes back every 80 frames");
        // An error flickers twice, then breaks, and the break jumps every 40 frames.
        assert_eq!((row(State::Error, 6, 0, 1), row(State::Error, 6, 0, 2), row(State::Error, 6, 5, 9)), ("......".into(), "######".into(), "###..#".into()));
        assert_eq!(row(State::Error, 6, 40, 9), "##..##");
    }

    /// The keys a sprite lights at frame `t`, by name.
    fn sprite_keys(show: Show, t: i64) -> Vec<String> {
        let m = g316();
        let mut keys: Vec<String> = m
            .zones
            .iter()
            .filter(|z| z.kind == Kind::Key && {
                let [x, y, w, h] = z.rect;
                glyph(show, &z.name, (y + h / 2.0).floor() as i32, (x + w / 2.0).floor() as i32, t).0 > 0.5
            })
            .map(|z| z.name.clone())
            .collect();
        keys.sort();
        keys
    }

    #[test]
    fn sprites_are_drawn_on_the_key_grid() {
        let _serial = crate::testenv::serial();
        let cross = ["7", "9", "b", "dot", "f5", "f9", "h", "k", "u"];
        assert_eq!(sprite_keys(Show::Ask, 5), ["8", "9", "comma", "f7", "f8", "i", "m", "u"], "an exclamation mark");
        assert_eq!(sprite_keys(Show::Ask, 3), ["8", "9", "f7", "f8", "i", "u"], "drawn from the top");
        assert_eq!(sprite_keys(Show::Check, 7), ["9", "b", "e", "f", "f9", "h", "u"]);
        assert_eq!(sprite_keys(Show::Cross, 6), cross, "landed");
        assert_eq!(sprite_keys(Show::Cross, 1), ["f10", "f6"], "its top row jumps while it lands");
        assert_eq!(sprite_keys(Show::Spark, 1), ["u"], "the spinner's dot");
        assert_eq!(sprite_keys(Show::Spark, 9), cross, "its cross");
        assert_eq!(sprite_keys(Show::Spark, 7).len(), 17, "its star");
        assert_eq!(glyph(Show::Check, "f9", 0, 10, 10), (1.0, true), "the check mark flashes white");
        assert_eq!(glyph(Show::Ask, "m", 4, 8, 8), (0.0, false), "and the exclamation mark blinks");
        assert_eq!(sprite_keys(Show::Ask, Show::Ask.length() - 1), Vec::<String>::new());
        // Over the keys, the profile dims while a sprite plays.
        let mut d = Display::default();
        let mut p = Profile::default();
        p.bases.insert("keys".into(), [100, 100, 100]);
        d.signal("1", Signal::Start, None, &[], at(&d, 0));
        let esc = |f: i64| d.frame(g316(), &[38], &p, at(&d, f)).unwrap()[0].1;
        assert_eq!((esc(0), esc(1), esc(Show::Spark.length() - 1), esc(Show::Spark.length())), ([30; 3], [0; 3], [30; 3], [100; 3]));
    }

    #[test]
    fn colours_follow_the_state_and_the_sprite() {
        let _serial = crate::testenv::serial();
        let _ = fs::remove_file(profile::config_dir().join("claude.toml"));
        let (m, p) = (g316(), Palette::default());
        let colour = |d: &Display, name: &str, f: i64| {
            let frame = d.frame(m, &zones(m), &Profile::default(), at(d, f)).unwrap();
            frame.iter().find(|z| z.0 == m.named(name).unwrap().id).unwrap().1
        };
        let mut d = Display::default();
        d.signal("1", Signal::Asking, None, &[], at(&d, 0));
        assert_eq!(colour(&d, "bar15", 0), p.asking, "the middle of the share");
        d.signal("1", Signal::Compact, None, &[], at(&d, 0));
        assert_eq!(colour(&d, "bar1", 0), p.compacting);
        d.signal("1", Signal::Done, None, &[], at(&d, 0));
        assert_eq!(colour(&d, "bar1", 15), mix(p.done, [0xff; 3], 0.7), "the glint");
        d.show = Some((Show::Ask, at(&d, 100)));
        assert_eq!(colour(&d, "m", 105), p.asking, "the exclamation mark's dot");
        d.show = Some((Show::Check, at(&d, 200)));
        assert_eq!(colour(&d, "e", 210), [0xff; 3], "the check mark's flash");
        d.show = Some((Show::Cross, at(&d, 300)));
        assert_eq!(colour(&d, "u", 306), p.error);
    }

    #[test]
    fn the_engine_waits_for_the_next_frame_while_an_instance_shows() {
        let _serial = crate::testenv::serial();
        let mut d = Display::default();
        assert_eq!(d.next_frame(at(&d, 0)), None, "nothing to show");
        d.signal("1", Signal::Done, None, &[], at(&d, 0));
        let wait = d.next_frame(at(&d, 0)).unwrap();
        assert!((wait.as_secs_f64() - STEP / 2.0).abs() < 1e-3, "to the next frame of the clock: {wait:?}");
        d.signal("1", Signal::End, None, &[], at(&d, 60));
        assert!(d.next_frame(at(&d, 61)).is_some(), "closing");
        assert_eq!(d.next_frame(at(&d, 60) + CLOSE), None, "closed");
    }

    #[test]
    fn the_keys_keep_the_profile_and_an_effect_holds_still() {
        let _serial = crate::testenv::serial();
        let _ = fs::remove_file(profile::config_dir().join("claude.toml"));
        let m = g316();
        let mut d = Display::default();
        d.signal("1", Signal::Done, None, &[], at(&d, 0));
        let wave = profile::load(m, "Wave").unwrap();
        let frame = d.frame(m, &zones(m), &wave, at(&d, 60)).unwrap();
        let colour = |name: &str| frame.iter().find(|z| z.0 == m.named(name).unwrap().id).unwrap().1;
        assert_eq!((colour("esc"), colour("bar15")), (wave.effect.as_ref().unwrap().still(0.5 / 19.5), Palette::default().done));
        assert!(frame.iter().all(|z| m.zone(z.0).unwrap().kind != Kind::Unknown), "unknown zones stay out");
        // Closing: a white flash, then the share shrinks to its middle and the
        // profile takes the segments back from the ends.
        d.signal("1", Signal::End, None, &[], at(&d, 60));
        let bar = |f: i64, name: &str| d.frame(m, &zones(m), &wave, at(&d, f)).map(|fr| fr.iter().find(|z| z.0 == m.named(name).unwrap().id).unwrap().1);
        let done = Palette::default().done;
        let still = |name: &str| {
            let [x, _, w, _] = m.named(name).unwrap().rect;
            wave.effect.as_ref().unwrap().still((x + w / 2.0) / m.board[0])
        };
        assert_eq!((bar(60, "bar1"), bar(61, "bar1")), (Some([0xff; 3]), Some(mix(done, [0xff; 3], 0.5))), "a white flash");
        assert_eq!((bar(62, "bar1"), bar(63, "bar1")), (Some(done), Some(still("bar1"))), "then the ends go back to the profile");
        assert_eq!((bar(75, "bar15"), bar(75, "bar14")), (Some(done), Some(still("bar14"))), "down to the middle dot");
        assert_eq!(bar(78, "bar15"), Some(mix(still("bar15"), done, 0.3)), "which fades");
        assert_eq!(bar(80, "bar1"), None, "gone: the profile shows on its own");
    }

    #[test]
    fn a_device_without_a_strip_shows_its_first_instance_on_its_lights() {
        let _serial = crate::testenv::serial();
        let _ = fs::remove_file(profile::config_dir().join("claude.toml"));
        let m = layout::find("g903-hero").unwrap();
        assert_eq!(status_zones(m), [1]);
        let mut d = Display::default();
        d.signal("1", Signal::Error, None, &[], at(&d, 0));
        d.signal("2", Signal::Done, None, &[], at(&d, 0));
        assert_eq!(d.frame(m, &[0, 1], &Profile::default(), at(&d, 30)).unwrap(), [(1, Palette::default().error)]);
    }

    #[test]
    fn the_status_line_reports_context_and_limits() {
        let json = r#"{"session_id":"s1","context_window":{"used_percentage":81.5},"rate_limits":{"five_hour":{"used_percentage":23.5,"resets_at":1738425600},"seven_day":{"used_percentage":41.2},"spend_limit":{"used_percentage":62.8,"resets_at":1740787200}}}"#;
        let (key, usage) = from_status_line(json, Some("cli"), Some(4242)).unwrap();
        assert_eq!(key, "4242");
        assert_eq!(usage.context, Some(81.5));
        assert_eq!(usage.five_hour, Some(Window { used: 23.5, resets_at: 1738425600 }));
        assert_eq!((usage.seven_day.map(|w| w.resets_at), usage.spend.map(|w| w.used)), (Some(0), Some(62.8)));
        let (key, usage) = from_status_line(r#"{"session_id":"s1","context_window":{"used_percentage":null}}"#, None, None).unwrap();
        assert_eq!((key.as_str(), usage), ("s1", Usage::default()), "early in a session everything can be missing");
        assert_eq!(from_status_line(json, Some("sdk-cli"), None), None);
        assert_eq!(from_status_line("not json", None, None), None);
    }

    #[test]
    fn the_context_closes_in_and_the_limits_light_the_end_lamps() {
        let _serial = crate::testenv::serial();
        let _ = fs::remove_file(profile::config_dir().join("claude.toml"));
        let (m, p) = (g316(), Palette::default());
        let mut d = Display::default();
        let bar = |d: &Display, j: usize, f: i64| {
            let frame = d.frame(m, &zones(m), &Profile::default(), at(d, f)).unwrap();
            frame.iter().find(|z| z.0 == m.strips[0].zones[j]).unwrap().1
        };
        let usage = |context: f32, five: f32, week: f32| Usage { context: Some(context), limits: true, five_hour: Some(Window { used: five, resets_at: 0 }), seven_day: Some(Window { used: week, resets_at: 0 }), spend: None };
        d.signal("1", Signal::Done, None, &[], at(&d, 0));
        d.show = None;
        d.usage("1", usage(91.0, 10.0, 10.0), None, at(&d, 0));
        assert_eq!(shown(&d), Some(Show::Gauge(Dial::Context, 9, false)), "the context passed 90 %");
        // Walls a third of the share deep at most: five segments of thirty from each end.
        assert_eq!((bar(&d, 4, 60), bar(&d, 25, 60), bar(&d, 5, 60)), (scale(p.compacting, 0.55), scale(p.compacting, 0.55), p.done));
        d.show = None;
        d.usage("1", usage(92.0, 10.0, 10.0), None, at(&d, 0));
        d.usage("1", usage(30.0, 10.0, 10.0), None, at(&d, 0));
        assert_eq!((shown(&d), bar(&d, 0, 60)), (None, scale(p.done, 0.55)), "once a turn; compacted, no walls");
        d.usage("1", usage(91.0, 10.0, 10.0), None, at(&d, 0));
        assert!(shown(&d).is_some(), "full again: the gauge again");
        // Limits: amber from 80 %, then red and blinking from 95 %, at the two ends.
        d.show = None;
        d.usage("1", usage(10.0, 85.0, 97.0), None, at(&d, 0));
        assert_eq!(shown(&d), Some(Show::Gauge(Dial::FiveHour, 8, false)), "the first limit to cross a mark plays");
        assert_eq!((bar(&d, 0, 60), bar(&d, 29, 56), bar(&d, 29, 59), bar(&d, 29, 60)), (p.asking, p.error, p.error, scale(p.error, 0.12)));
        d.show = None;
        d.usage("1", usage(10.0, 86.0, 98.0), None, at(&d, 0));
        assert_eq!(shown(&d), Some(Show::Gauge(Dial::Week, 9, true)), "the week's gauge, held back by the cooldown, plays now");
        d.show = None;
        d.usage("1", usage(10.0, 87.0, 98.0), None, at(&d, 0));
        assert_eq!(shown(&d), None, "no mark crossed since");
        d.usage("1", usage(10.0, 91.0, 98.0), None, at(&d, 0));
        assert_eq!(shown(&d), Some(Show::Gauge(Dial::FiveHour, 9, false)), "the next mark plays");
        d.usage("1", usage(10.0, 100.0, 20.0), None, at(&d, 0));
        assert_eq!((bar(&d, 0, 60), bar(&d, 29, 60)), (p.error, scale(p.done, 0.55)), "reached: red; the week reset, its end dim as done ends are");
        // A turn that ended on a limit: both lamps red until Claude works again.
        d.usage("1", usage(10.0, 20.0, 20.0), None, at(&d, 0));
        d.signal("1", Signal::Limit, None, &[], at(&d, 0));
        assert_eq!((bar(&d, 0, 60), bar(&d, 29, 60), d.states(at(&d, 60))), (p.error, p.error, vec![State::Error]));
        d.signal("1", Signal::Working, None, &[], at(&d, 0));
        assert_ne!(bar(&d, 0, 60), p.error);
        // A window past its reset time no longer counts; the spend limit shares the right lamp.
        let gone = Usage { limits: true, spend: Some(Window { used: 90.0, resets_at: 1 }), ..Usage::default() };
        d.usage("1", gone, None, at(&d, 0));
        assert_ne!(bar(&d, 29, 60), p.asking);
        let g903 = layout::find("g903-hero").unwrap();
        d.usage("1", usage(10.0, 100.0, 100.0), None, at(&d, 0));
        assert_eq!(d.frame(g903, &[1], &Profile::default(), at(&d, 60)).unwrap()[0].1, p.working, "no lamps on one light: it keeps the state");
    }

    #[test]
    fn claude_code_is_connected_and_put_back_as_it_was() {
        let _serial = crate::testenv::serial();
        let (settings, log) = (settings_path().unwrap(), crate::testenv::claude_cli().join("log"));
        let _ = fs::remove_file(&log);
        let own = r#"printf '%s' "it's" | sh -c 'cat' || sh -c x"#;
        // Laid out the way Claude Code writes it: two spaces, one value a line.
        let before = format!("{{\n  \"zeta\": 1,\n  \"statusLine\": {{\n    \"type\": \"command\",\n    \"command\": {}\n  }},\n  \"alpha\": [\n    true\n  ]\n}}\n", serde_json::to_string(own).unwrap());
        fs::create_dir_all(settings.parent().unwrap()).unwrap();
        fs::write(&settings, &before).unwrap();
        set_up(true).unwrap();
        let text = fs::read_to_string(&settings).unwrap();
        assert!(text.find("zeta").unwrap() < text.find("alpha").unwrap(), "the user's key order stays");
        assert_eq!(read_settings().unwrap()["statusLine"]["command"], wrap(own));
        assert_eq!(setup(), Setup { plugin: false, status_line: true });
        set_up(true).unwrap(); // already through Loginux: left alone
        let calls = fs::read_to_string(&log).unwrap();
        assert!(calls.starts_with("plugin install loginux-status --marketplace GbrFrn/loginux --scope user\n"), "{calls}");
        set_up(false).unwrap();
        assert_eq!(fs::read_to_string(&settings).unwrap(), before, "put back exactly");
        assert!(fs::read_to_string(&log).unwrap().ends_with("plugin disable loginux-status@loginux --scope user\n"));
        set_up(false).unwrap(); // nothing to put back
        // No status line before: one that only feeds Loginux, removed again after.
        fs::write(&settings, "{\"enabledPlugins\": {\"loginux-status@loginux\": true}}").unwrap();
        set_up(true).unwrap();
        assert_eq!((read_settings().unwrap()["statusLine"]["command"].as_str(), setup()), (Some(STATUS_LINE), Setup { plugin: true, status_line: true }));
        set_up(false).unwrap();
        assert_eq!((read_settings().unwrap().get("statusLine"), setup().plugin), (None, false), "the plugin off, the status line gone");
        // A plugin Claude Code keeps on is said.
        fs::write(&settings, "{\"enabledPlugins\":{\"loginux-status@loginux\":true}}").unwrap();
        assert!(set_up(false).unwrap_err().contains("kept loginux-status@loginux enabled"));
        // A missing settings file is made; a broken or hand-changed one is left alone.
        fs::remove_file(&settings).unwrap();
        set_up(true).unwrap();
        fs::write(&settings, "{\"statusLine\": {\"command\": \"loginux claude --statusline -- edited\"}}").unwrap();
        assert!(set_up(false).unwrap_err().contains("changed by hand"));
        for broken in ["{oops", "[1]"] {
            fs::write(&settings, broken).unwrap();
            assert!(set_up(true).unwrap_err().contains("left as it is"), "{broken}");
            assert_eq!(fs::read_to_string(&settings).unwrap(), broken);
        }
        assert_eq!(setup(), Setup::default(), "unreadable: nothing connected");
        // Claude Code that refuses: said, and nothing else is changed.
        fs::write(crate::testenv::claude_cli().join("fails"), "").unwrap();
        fs::write(&settings, "{}").unwrap();
        assert_eq!(set_up(true).unwrap_err(), "claude plugin install loginux-status --marketplace GbrFrn/loginux --scope user: refused for the test");
        assert_eq!(fs::read_to_string(&settings).unwrap(), "{}");
        set_up(false).unwrap();
        fs::remove_file(crate::testenv::claude_cli().join("fails")).unwrap();
        // Claude Code that never answers: given up on, and said.
        fs::write(crate::testenv::claude_cli().join("hold"), "").unwrap();
        assert!(set_up(true).unwrap_err().contains("no answer within"));
        fs::remove_file(crate::testenv::claude_cli().join("hold")).unwrap();
        // Unreadable settings stop it before Claude Code is touched.
        fs::write(&settings, "{oops").unwrap();
        let calls = fs::read_to_string(&log).unwrap().lines().count();
        assert!(set_up(true).is_err() && set_up(false).is_err());
        assert_eq!(fs::read_to_string(&log).unwrap().lines().count(), calls);
        // A status line with no command is not Loginux's to pass through.
        fs::write(&settings, "{\"statusLine\": {\"type\": \"command\", \"padding\": 1}}").unwrap();
        assert!(set_up(true).unwrap_err().contains("no command"));
        // A private file stays private, and a link stays a link.
        use std::os::unix::fs::PermissionsExt;
        let target = settings.with_file_name("dotfiles-settings.json");
        fs::write(&target, "{}").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        fs::remove_file(&settings).unwrap();
        std::os::unix::fs::symlink(&target, &settings).unwrap();
        set_up(true).unwrap();
        assert!(fs::symlink_metadata(&settings).unwrap().file_type().is_symlink());
        assert_eq!(fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(fs::read_to_string(&target).unwrap().contains(STATUS_LINE));
        fs::remove_file(&settings).unwrap();
        fs::remove_file(&target).unwrap();
        // A file Claude Code changed while Loginux edited it is left as it is.
        fs::write(&settings, "{}").unwrap();
        let edited = edit_settings(|path, _| {
            fs::write(path, "{\"theme\": \"dark\"}").unwrap();
            Ok(true)
        });
        assert!(edited.unwrap_err().contains("changed while Loginux was editing it"));
        assert_eq!(fs::read_to_string(&settings).unwrap(), "{\"theme\": \"dark\"}");
        fs::remove_file(&settings).unwrap();
        // With neither CLAUDE_CONFIG_DIR nor HOME, nothing is guessed.
        assert!(settings_dir(None, Some("".into())).unwrap_err().contains("cannot be found"));
        assert_eq!(settings_dir(Some("relative".into()), Some("/home/u".into())).unwrap(), PathBuf::from("/home/u/.claude"));
        // Quoting round-trips, and only Loginux's own wrapping is taken apart.
        for own in ["bash ~/.claude/statusline.sh", "echo 'a' || sh -c 'b'", "x"] {
            assert_eq!(unwrap(&wrap(own)).as_deref(), Some(own));
        }
        assert_eq!(unwrap("loginux claude --statusline -- sh -c 'a' || sh -c 'b'"), None);
        assert_eq!(unwrap("loginux claude --statusline -- sh -c x"), None);
    }

    #[test]
    fn waiting_after_an_interrupt_plays_nothing_and_a_limit_holds() {
        let _serial = crate::testenv::serial();
        let mut d = Display::default();
        let t = at(&d, 0);
        d.signal("1", Signal::Prompt, None, &[], t);
        d.show = None;
        // Esc, then Claude Code's notice a minute later: finished, but no check mark.
        d.signal("1", Signal::Waiting, None, &[], t + AWAY * 2);
        assert_eq!((d.states(t), shown(&d)), (vec![State::Done], None));
        // A turn that ended on a limit stays red through that notice, until Claude works.
        d.signal("1", Signal::Prompt, None, &[], t + AWAY * 2);
        d.signal("1", Signal::Limit, None, &[], t + AWAY * 2);
        d.signal("1", Signal::Waiting, None, &[], t + AWAY * 3);
        assert_eq!((d.states(t), d.limited), (vec![State::Error], true));
        d.signal("1", Signal::Tool, None, &[], t + AWAY * 3);
        assert!(!d.limited);
        // An error stays through it, and so does an instance with no turn yet.
        d.signal("1", Signal::Error, None, &[], t + AWAY * 3);
        d.signal("1", Signal::Waiting, None, &[], t + AWAY * 4);
        d.signal("2", Signal::Start, None, &[], t + AWAY * 4);
        d.signal("2", Signal::Waiting, None, &[], t + AWAY * 4);
        assert_eq!(d.states(t + AWAY * 4), vec![State::Error, State::Idle]);
    }

    #[test]
    fn gauges_wait_out_the_cooldown_and_limits_follow_each_report() {
        let _serial = crate::testenv::serial();
        let mut d = Display::default();
        let t = at(&d, 0);
        d.signal("1", Signal::Start, None, &[], t);
        assert_eq!(shown(&d), Some(Show::Spark));
        let full = Usage { context: Some(92.0), ..Usage::default() };
        d.usage("1", full, None, t);
        assert_eq!(shown(&d), Some(Show::Spark), "within the cooldown: not played");
        d.usage("1", full, None, t + COOLDOWN);
        assert_eq!(shown(&d), Some(Show::Gauge(Dial::Context, 9, false)), "played once it could");
        // A status line that names no process, for an instance its hooks named by
        // process: it does not make a second instance, but its limits count.
        let limits = Usage { limits: true, five_hour: Some(Window { used: 60.0, resets_at: 0 }), ..Usage::default() };
        d.usage("session-id", limits, None, t);
        assert_eq!((d.instances.len(), d.limits[0].map(|w| w.used)), (1, Some(60.0)));
        // A report without limits keeps them; one with limits but without a window clears it.
        d.usage("1", Usage::default(), None, t);
        assert!(d.limits[0].is_some());
        d.usage("1", Usage { limits: true, ..Usage::default() }, None, t);
        assert_eq!(d.limits, [None; 3]);
    }

    #[test]
    fn gauges_label_their_key_and_fill_the_number_row() {
        let _serial = crate::testenv::serial();
        let _ = fs::remove_file(profile::config_dir().join("claude.toml"));
        let (m, p) = (g316(), Palette::default());
        let mut d = Display::default();
        d.signal("1", Signal::Done, None, &[], at(&d, 0));
        let key = |d: &Display, name: &str, f: i64| {
            let frame = d.frame(m, &zones(m), &Profile::default(), at(d, f)).unwrap();
            frame.iter().find(|z| z.0 == m.named(name).unwrap().id).unwrap().1
        };
        d.show = Some((Show::Gauge(Dial::Week, 8, false), at(&d, 100)));
        assert_eq!((key(&d, "w", 101), key(&d, "1", 101)), (p.asking, [0; 3]), "the label first");
        assert_eq!((key(&d, "2", 103), key(&d, "3", 103), key(&d, "9", 109)), (p.asking, [0; 3], [0; 3]), "then a number a frame, to eight");
        assert_eq!((key(&d, "8", 110), key(&d, "8", 112), key(&d, "q", 110)), (p.asking, [0; 3], [0; 3]), "then a blink");
        d.show = Some((Show::Gauge(Dial::Context, 9, false), at(&d, 200)));
        assert_eq!(key(&d, "c", 201), p.compacting);
        d.show = Some((Show::Gauge(Dial::Spend, 9, true), at(&d, 300)));
        assert_eq!((key(&d, "s", 301), Dial::FiveHour.key()), (p.error, "h"), "red once the limit is close");
        // Settings that cannot be read: nothing connected, and nothing written.
        let settings = settings_path().unwrap();
        let _ = fs::remove_file(&settings);
        fs::create_dir_all(&settings).unwrap();
        assert_eq!(setup(), Setup::default());
        assert!(set_up(true).unwrap_err().contains("settings.json"));
        fs::remove_dir(&settings).unwrap();
    }

    #[test]
    fn colours_come_from_claude_toml_when_it_sets_them() {
        let _serial = crate::testenv::serial();
        let file = profile::config_dir().join("claude.toml");
        let _ = fs::remove_file(&file);
        assert_eq!(palette(), Palette::default(), "no file");
        fs::write(&file, "working = \"#102030\"\nasking = \"nonsense\"\ncompacting = \"#000003\"\n").unwrap();
        let p = palette();
        assert_eq!((p.working, p.asking, p.compacting), ([0x10, 0x20, 0x30], Palette::default().asking, [0, 0, 3]));
        fs::write(&file, "done = \"#000001\"\nerror = \"#000002\"\n").unwrap();
        assert_eq!((palette().done, palette().error), ([0, 0, 1], [0, 0, 2]));
        fs::write(&file, "not toml [").unwrap();
        assert_eq!(palette(), Palette::default(), "a broken file reads as none");
        fs::remove_file(&file).unwrap();
    }
}
