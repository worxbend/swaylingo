//! Remember the Sway XKB layout for each window and restore it on focus.
//!
//! Sway owns the keymap (`xkb_layout` in the sway config). This process only
//! listens to window and input events and runs `xkb_switch_layout`.
//!
//! swayipc 4: `Connection::subscribe` consumes the socket, so commands use a
//! second connection. https://docs.rs/swayipc/4.0.0/swayipc/struct.Connection.html

mod config;
mod memory;

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};

use swayipc::{
    Connection, Event, EventType, Input, InputChange, Node, NodeType, WindowChange, WorkspaceChange,
};

use config::{ConfigFile, LayoutToken, RuleSource};
use memory::{Action, Happening, Memory};

const HELP: &str = "\
Remember the Sway keyboard layout for each window.

Usage: skbd [options]

Options:
  -h, --help           Print this help and exit
  --version            Print the version and exit
  --verbose            Print focus, layout, and switch lines
  --config PATH        Config file to read
  --default TOKEN      Layout for a new window and an empty desktop

The config file is $XDG_CONFIG_HOME/swaylingo/config.toml, or
~/.config/swaylingo/config.toml when XDG_CONFIG_HOME is unset. A missing
file at that path is an empty config. --config requires the file to exist.
--default overrides the fallback key.

A token is a layout index, the layout's exact name, or a short code:
us, en, ua, uk, ru, de, pl, fr. App keys are a Wayland app_id or an X11
WM_CLASS.
";

#[derive(Debug)]
struct Options {
    verbose: bool,
    config: Option<PathBuf>,
    default_layout: Option<String>,
}

#[derive(Debug)]
enum Parsed {
    Run(Options),
    Help,
    Version,
}

#[derive(Debug)]
enum Stop {
    Usage(String),
    Fail(String),
    /// Exit 0. systemd `Restart=on-failure` must not loop when the socket
    /// was not imported yet.
    Idle(String),
}

fn main() {
    if let Err(stop) = start() {
        match stop {
            Stop::Usage(message) => {
                eprintln!("{message}");
                process::exit(2);
            }
            Stop::Fail(message) => {
                eprintln!("{message}");
                process::exit(1);
            }
            Stop::Idle(message) => {
                eprintln!("{message}");
            }
        }
    }
}

fn start() -> Result<(), Stop> {
    match parse_args().map_err(Stop::Usage)? {
        Parsed::Help => {
            print!("{HELP}");
            Ok(())
        }
        Parsed::Version => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Parsed::Run(options) => run(options),
    }
}

fn parse_args() -> Result<Parsed, String> {
    parse_arg_list(env::args())
}

fn parse_arg_list<I, S>(args: I) -> Result<Parsed, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args.into_iter().map(|arg| arg.as_ref().to_string());
    let _program = args.next();
    let mut verbose = false;
    let mut config = None;
    let mut default_layout = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => return Ok(Parsed::Help),
            "--version" => return Ok(Parsed::Version),
            "--verbose" => verbose = true,
            "--config" => config = Some(PathBuf::from(next_value(&mut args, "--config")?)),
            "--default" => default_layout = Some(next_value(&mut args, "--default")?),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(Parsed::Run(Options {
        verbose,
        config,
        default_layout,
    }))
}

fn next_value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    match args.next() {
        Some(value) if !value.is_empty() => Ok(value),
        _ => Err(format!("{flag} needs a value")),
    }
}

/// Set from the SIGHUP handler. The event loop applies it.
/// The handler must not reload: it may only store an atomic.
static RELOAD_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sighup(_signal: libc::c_int) {
    RELOAD_REQUESTED.store(true, Ordering::Release);
}

fn run(options: Options) -> Result<(), Stop> {
    refuse_root(current_euid())?;
    let _instance_lock = instance_lock()?;
    install_sighup()?;
    let (source_label, mut config) = read_config(&options)?;
    let fallback_label = overridden_fallback(
        &mut config,
        options.default_layout.as_deref(),
        &source_label,
    );
    let mut commands = connect()?;
    // Subscribe before seed. A layout switch during startup has to land on
    // this socket, or the echo is gone before the loop starts.
    let events = connect()?
        .subscribe([
            EventType::Window,
            EventType::Input,
            EventType::Workspace,
            EventType::Shutdown,
        ])
        .map_err(|err| Stop::Fail(err.to_string()))?;
    let loaded = LoadedConfig {
        file: &config,
        fallback_label: &fallback_label,
        source_label: &source_label,
    };
    let mut memory = seed(&mut commands, options.verbose, &loaded)?;
    // shortcut: SIGHUP reloads on the next Sway event. swayipc retries EINTR
    // and keeps the socket private, so waking that read needs a poll loop.
    maybe_reload(&mut commands, &mut memory, &options);
    for event in events {
        maybe_reload(&mut commands, &mut memory, &options);
        let event = event.map_err(|err| Stop::Fail(err.to_string()))?;
        if matches!(event, Event::Shutdown(_)) {
            return Ok(());
        }
        let action = react(&mut commands, &mut memory, &event, options.verbose)?;
        switch_if_needed(&mut commands, &mut memory, action, options.verbose)?;
    }
    Ok(())
}

fn read_config(options: &Options) -> Result<(String, ConfigFile), Stop> {
    if let Some(path) = &options.config {
        let config = config::load_required(path).map_err(Stop::Usage)?;
        return Ok((path.display().to_string(), config));
    }
    let path = config::default_path().map_err(Stop::Fail)?;
    let config = config::load_optional(&path).map_err(Stop::Usage)?;
    Ok((path.display().to_string(), config))
}

/// Open one Sway socket. Events and commands cannot share it: `subscribe`
/// consumes the connection it is called on.
fn connect() -> Result<Connection, Stop> {
    let path = classify_swaysock(read_swaysock(), under_systemd())?;
    ensure_socket(Path::new(&path))?;
    // swayipc reads I3SOCK before SWAYSOCK. Drop I3SOCK so this process
    // cannot attach to i3. Do not copy SWAYSOCK into I3SOCK.
    // SAFETY: main is the only thread. Nothing else reads the environment.
    unsafe { env::remove_var("I3SOCK") };
    Connection::new().map_err(|err| Stop::Fail(err.to_string()))
}

enum SwaySock {
    Missing,
    Empty,
    NotUtf8,
    Path(String),
}

fn read_swaysock() -> SwaySock {
    match env::var("SWAYSOCK") {
        Ok(path) if path.is_empty() => SwaySock::Empty,
        Ok(path) => SwaySock::Path(path),
        Err(env::VarError::NotPresent) => SwaySock::Missing,
        Err(env::VarError::NotUnicode(_)) => SwaySock::NotUtf8,
    }
}

/// systemd sets `INVOCATION_ID` for a service. A terminal run does not.
fn under_systemd() -> bool {
    env::var_os("INVOCATION_ID").is_some_and(|value| !value.is_empty())
}

fn classify_swaysock(sock: SwaySock, service: bool) -> Result<String, Stop> {
    let idle = |message: &str| {
        if service {
            Stop::Idle(message.to_string())
        } else {
            Stop::Fail(message.to_string())
        }
    };
    match sock {
        SwaySock::Missing => Err(idle("SWAYSOCK is not set")),
        SwaySock::Empty => Err(idle("SWAYSOCK is empty")),
        SwaySock::NotUtf8 => Err(Stop::Fail("SWAYSOCK is not valid UTF-8".to_string())),
        SwaySock::Path(path) => Ok(path),
    }
}

fn ensure_socket(path: &Path) -> Result<(), Stop> {
    match fs::metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => Ok(()),
        Ok(_) => Err(Stop::Fail(format!(
            "SWAYSOCK is not a socket: {}",
            path.display()
        ))),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Err(Stop::Fail(format!(
            "SWAYSOCK path does not exist: {}",
            path.display()
        ))),
        Err(err) => Err(Stop::Fail(format!(
            "SWAYSOCK path cannot be read: {} ({err})",
            path.display()
        ))),
    }
}

fn refuse_root(euid: u32) -> Result<(), Stop> {
    if euid == 0 {
        return Err(Stop::Fail(
            "refusing to run as root. The unit is a systemd user service".to_string(),
        ));
    }
    Ok(())
}

fn current_euid() -> u32 {
    // SAFETY: geteuid reads the process credential and takes no pointer.
    unsafe { geteuid() }
}

fn instance_lock() -> Result<File, Stop> {
    let raw = env::var_os("XDG_RUNTIME_DIR");
    let dir = require_runtime_dir(raw.as_deref())?;
    lock_runtime_dir(dir)
}

fn require_runtime_dir(raw: Option<&std::ffi::OsStr>) -> Result<&Path, Stop> {
    let Some(raw) = raw else {
        return Err(Stop::Fail("XDG_RUNTIME_DIR is unset".to_string()));
    };
    if raw.is_empty() {
        return Err(Stop::Fail("XDG_RUNTIME_DIR is empty".to_string()));
    }
    let dir = Path::new(raw);
    if !dir.is_absolute() {
        return Err(Stop::Fail("XDG_RUNTIME_DIR is not absolute".to_string()));
    }
    Ok(dir)
}

fn check_runtime_dir(dir: &Path, euid: u32) -> Result<(), Stop> {
    if !dir.is_absolute() {
        return Err(Stop::Fail("XDG_RUNTIME_DIR is not absolute".to_string()));
    }
    let meta = fs::symlink_metadata(dir).map_err(|err| {
        Stop::Fail(format!(
            "XDG_RUNTIME_DIR cannot be read: {} ({err})",
            dir.display()
        ))
    })?;
    if !meta.file_type().is_dir() {
        return Err(Stop::Fail(format!(
            "XDG_RUNTIME_DIR is not a directory: {}",
            dir.display()
        )));
    }
    let mode = meta.permissions().mode() & 0o777;
    if mode != 0o700 {
        return Err(Stop::Fail(format!(
            "XDG_RUNTIME_DIR must be mode 0700, not {mode:04o}: {}",
            dir.display()
        )));
    }
    if meta.uid() != euid {
        return Err(Stop::Fail(format!(
            "XDG_RUNTIME_DIR is not owned by this user: {}",
            dir.display()
        )));
    }
    Ok(())
}

// Linux asm-generic/fcntl.h. Rust's OpenOptions already sets O_CLOEXEC;
// the flag is repeated here because the lock must not survive exec.
const O_NOFOLLOW: i32 = 0o400000;
const O_CLOEXEC: i32 = 0o2000000;

fn open_lock(path: &Path) -> Result<File, Stop> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(O_NOFOLLOW | O_CLOEXEC)
        .open(path)
        .map_err(|err| Stop::Fail(format!("cannot open {}: {err}", path.display())))
}

fn lock_file_is_ours(file: &File, path: &Path, euid: u32) -> Result<(), Stop> {
    let meta = file
        .metadata()
        .map_err(|err| Stop::Fail(format!("cannot stat {}: {err}", path.display())))?;
    if !meta.file_type().is_file() {
        return Err(Stop::Fail(format!(
            "lock file is not a regular file: {}",
            path.display()
        )));
    }
    if meta.uid() != euid {
        return Err(Stop::Fail(format!(
            "lock file is not owned by this user: {}",
            path.display()
        )));
    }
    Ok(())
}

fn lock_runtime_dir(dir: &Path) -> Result<File, Stop> {
    let euid = current_euid();
    check_runtime_dir(dir, euid)?;
    let path = dir.join("skbd.lock");
    let file = open_lock(&path)?;
    lock_file_is_ours(&file, &path, euid)?;
    match lock_exclusive(&file) {
        Ok(()) => Ok(file),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            Err(Stop::Fail("skbd: already running".to_string()))
        }
        Err(err) => Err(Stop::Fail(format!("cannot lock {}: {err}", path.display()))),
    }
}

const LOCK_EX: i32 = 2;
const LOCK_NB: i32 = 4;

unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
    fn geteuid() -> u32;
}

fn lock_exclusive(file: &File) -> io::Result<()> {
    loop {
        // SAFETY: `file` owns this fd. LOCK_EX | LOCK_NB only sets an
        // advisory lock on that fd and does not dereference a pointer.
        let rc = unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) };
        if rc == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        if err.kind() == io::ErrorKind::WouldBlock {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "skbd: already running",
            ));
        }
        return Err(err);
    }
}

struct LoadedConfig<'a> {
    file: &'a ConfigFile,
    fallback_label: &'a str,
    source_label: &'a str,
}

/// Record every view already open, without treating it as new.
/// Only the focused view is marked focused. An empty desktop at startup
/// moves the keyboard to the fallback layout.
fn seed(
    commands: &mut Connection,
    verbose: bool,
    loaded: &LoadedConfig<'_>,
) -> Result<Memory, Stop> {
    let inputs = commands
        .get_inputs()
        .map_err(|err| Stop::Fail(err.to_string()))?;
    let keyboard = first_keyboard(&inputs).map_err(Stop::Fail)?;
    let rules = config::resolve_rules(
        loaded.file,
        &keyboard.names,
        &RuleSource {
            fallback_label: loaded.fallback_label,
            apps_label: loaded.source_label,
        },
    )
    .map_err(Stop::Usage)?;
    note(verbose, &format!("config {}", loaded.source_label));
    note(verbose, &format!("active layout {}", keyboard.active));
    note(verbose, &format!("fallback {}", rules.fallback));
    let mut memory = Memory::with_layouts(keyboard.active, rules.fallback, rules.apps);
    memory.keep_tokens(loaded.file.fallback.clone(), loaded.file.apps.clone());
    memory.watch_keyboard(keyboard.identifier);
    let tree = commands
        .get_tree()
        .map_err(|err| Stop::Fail(err.to_string()))?;
    remember_open_views(&mut memory, &tree);
    let action = if let Some(id) = memory.focused_id() {
        note(verbose, &format!("focused window {id}"));
        Action::None
    } else {
        note(verbose, "no focused window");
        memory.apply(Happening::Unfocus)
    };
    switch_if_needed(commands, &mut memory, action, verbose)?;
    Ok(memory)
}

fn remember_open_views(memory: &mut Memory, root: &Node) {
    walk_views(root, &mut |node| {
        memory.note_existing(node.id, x11_id(node));
    });
    if let Some(node) = focused_view(root) {
        memory.remember_current(node.id);
    }
}

fn walk_views(node: &Node, visit: &mut impl FnMut(&Node)) {
    if is_view(node) {
        visit(node);
    }
    for child in node.nodes.iter().chain(node.floating_nodes.iter()) {
        walk_views(child, visit);
    }
}

struct Keyboard {
    identifier: String,
    active: u32,
    names: Vec<String>,
}

/// The first keyboard that has both a layout index and a non-empty name list.
/// If none do, the first index is paired with the longest name list.
/// Layout events are kept only for the identifier chosen here. A seat can
/// list the same layout on a keyboard, its consumer-control device, and a
/// mouse that Sway also calls a keyboard.
fn first_keyboard(inputs: &[Input]) -> Result<Keyboard, String> {
    let keyboards: Vec<&Input> = inputs
        .iter()
        .filter(|input| input.input_type == "keyboard")
        .collect();
    let chosen = keyboard_with_index_and_names(&keyboards)
        .or_else(|| pair_first_index_with_longest_names(&keyboards))
        .ok_or_else(|| "no keyboard layout from sway".to_string())?;
    warn_if_indexes_disagree(&keyboards, chosen.active);
    Ok(chosen)
}

fn keyboard_with_index_and_names(keyboards: &[&Input]) -> Option<Keyboard> {
    keyboards.iter().find_map(|input| {
        let active = layout_index(input.xkb_active_layout_index)?;
        if input.xkb_layout_names.is_empty() {
            return None;
        }
        Some(Keyboard {
            identifier: input.identifier.clone(),
            active,
            names: input.xkb_layout_names.clone(),
        })
    })
}

fn pair_first_index_with_longest_names(keyboards: &[&Input]) -> Option<Keyboard> {
    let (input, active) = keyboards.iter().find_map(|input| {
        layout_index(input.xkb_active_layout_index).map(|active| (*input, active))
    })?;
    Some(Keyboard {
        identifier: input.identifier.clone(),
        active,
        names: longest_names(keyboards),
    })
}

fn longest_names(keyboards: &[&Input]) -> Vec<String> {
    let mut names: &[String] = &[];
    for input in keyboards {
        if input.xkb_layout_names.len() > names.len() {
            names = &input.xkb_layout_names;
        }
    }
    names.to_vec()
}

fn warn_if_indexes_disagree(keyboards: &[&Input], chosen: u32) {
    for input in keyboards {
        let Some(index) = layout_index(input.xkb_active_layout_index) else {
            continue;
        };
        if index != chosen {
            eprintln!("keyboards disagree on layout index ({chosen} vs {index}); using {chosen}");
        }
    }
}

/// Sway reports the index as `i32`. A negative value is not a layout.
fn layout_index(index: Option<i32>) -> Option<u32> {
    index.and_then(|index| u32::try_from(index).ok())
}

/// `Node::window` is the X11 id. Sway emits it as a JSON int, which fits i32.
fn x11_id(node: &Node) -> Option<i32> {
    i32::try_from(node.window?).ok()
}

/// `transient_for` is that same X11 id, not a Sway container id.
fn transient_parent(node: &Node) -> Option<i32> {
    node.window_properties.as_ref()?.transient_for
}

fn focus_of(node: &Node) -> Happening {
    Happening::Focus {
        id: node.id,
        app: app_name(node),
        x11: x11_id(node),
        transient_for: transient_parent(node),
    }
}

fn focused_view(node: &Node) -> Option<&Node> {
    if node.focused && is_view(node) {
        return Some(node);
    }
    node.nodes
        .iter()
        .chain(node.floating_nodes.iter())
        .find_map(focused_view)
}

/// A view has an app id (Wayland), an X11 window, or a pid.
/// Workspace and output nodes are focused when the desktop is empty, and
/// those must not be stored as windows.
fn is_view(node: &Node) -> bool {
    let is_container = matches!(node.node_type, NodeType::Con | NodeType::FloatingCon);
    is_container && (node.app_id.is_some() || node.window.is_some() || node.pid.is_some())
}

/// Wayland `app_id` when it is set. Otherwise the X11 window class.
fn app_name(node: &Node) -> Option<String> {
    let class = node
        .window_properties
        .as_ref()
        .and_then(|props| props.class.as_deref());
    app_key(node.app_id.as_deref(), class)
}

fn app_key(app_id: Option<&str>, class: Option<&str>) -> Option<String> {
    if let Some(app_id) = app_id.filter(|app_id| !app_id.is_empty()) {
        return Some(app_id.to_string());
    }
    class
        .filter(|class| !class.is_empty())
        .map(ToString::to_string)
}

/// Walk the workspace event's `current` node before `get_tree`.
/// `get_tree` runs only when that walk finds no view. A `current` node
/// with no child nodes is that case.
fn react(
    commands: &mut Connection,
    memory: &mut Memory,
    event: &Event,
    verbose: bool,
) -> Result<Action, Stop> {
    if let Event::Input(input) = event
        && let InputChange::Removed = input.change
        && input.input.identifier == memory.keyboard_id()
    {
        replace_watched_keyboard(commands, memory, verbose);
        return Ok(Action::None);
    }
    match event {
        Event::Workspace(workspace) if workspace.change == WorkspaceChange::Focus => {
            match focused_in_workspace(workspace.current.as_ref()) {
                Some(node) => {
                    let happening = focus_of(node);
                    note(verbose, &format!("{happening:?}"));
                    Ok(memory.apply(happening))
                }
                None => sync_focus(commands, memory, verbose),
            }
        }
        Event::Window(window)
            if window.change == WindowChange::Close && is_view(&window.container) =>
        {
            let closed_id = window.container.id;
            note(verbose, &format!("Close {{ id: {closed_id} }}"));
            if close_window(memory, closed_id) {
                sync_focus(commands, memory, verbose)
            } else {
                Ok(Action::None)
            }
        }
        _ => Ok(apply_event(memory, event, verbose)),
    }
}

/// `None` means the caller must read the tree. `current` is absent, has no
/// child nodes, or has children but no focused view.
fn focused_in_workspace(current: Option<&Node>) -> Option<&Node> {
    let current = current?;
    if current.nodes.is_empty() && current.floating_nodes.is_empty() {
        return None;
    }
    focused_view(current)
}

/// True when the closed id was the focused window. Read that before `Close`
/// clears it. Only that close needs a fresh tree.
fn close_window(memory: &mut Memory, id: i64) -> bool {
    let was_focused = memory.focused_id() == Some(id);
    memory.apply(Happening::Close { id });
    was_focused
}

/// Empty workspace, or the last focused window closed: fallback layout.
/// A workspace that already has a focused window keeps that window's layout.
fn sync_focus(
    commands: &mut Connection,
    memory: &mut Memory,
    verbose: bool,
) -> Result<Action, Stop> {
    let tree = commands
        .get_tree()
        .map_err(|err| Stop::Fail(err.to_string()))?;
    let happening = match focused_view(&tree) {
        Some(node) => focus_of(node),
        None => Happening::Unfocus,
    };
    note(verbose, &format!("{happening:?}"));
    Ok(memory.apply(happening))
}

fn apply_event(memory: &mut Memory, event: &Event, verbose: bool) -> Action {
    match event {
        Event::Window(window) => apply_window(memory, window.change, &window.container, verbose),
        Event::Input(input) => apply_input(memory, input.change, &input.input, verbose),
        _ => Action::None,
    }
}

fn apply_window(memory: &mut Memory, change: WindowChange, node: &Node, verbose: bool) -> Action {
    if !is_view(node) || change != WindowChange::Focus {
        return Action::None;
    }
    let happening = focus_of(node);
    note(verbose, &format!("{happening:?}"));
    memory.apply(happening)
}

fn apply_input(memory: &mut Memory, change: InputChange, input: &Input, verbose: bool) -> Action {
    if input.input_type != "keyboard" || input.identifier != memory.keyboard_id() {
        return Action::None;
    }
    let happening = match change {
        InputChange::XkbKeymap => Happening::KeymapReset {
            index: layout_index(input.xkb_active_layout_index),
            names: input.xkb_layout_names.clone(),
        },
        InputChange::XkbLayout => {
            let Some(index) = layout_index(input.xkb_active_layout_index) else {
                return Action::None;
            };
            note(verbose, &format!("layout {index}"));
            Happening::Layout { index }
        }
        _ => return Action::None,
    };
    memory.apply(happening)
}

fn switch_if_needed(
    commands: &mut Connection,
    memory: &mut Memory,
    action: Action,
    verbose: bool,
) -> Result<(), Stop> {
    let Action::SwitchTo(index) = action else {
        return Ok(());
    };
    note(verbose, &format!("switch {index}"));
    if !switch_layout(commands, index)? {
        memory.abandon_switch();
    }
    Ok(())
}

/// `type:keyboard` matches the sway input block. `input *` also hits pointers.
fn switch_layout(commands: &mut Connection, index: u32) -> Result<bool, Stop> {
    let command = format!("input type:keyboard xkb_switch_layout {index}");
    let outcomes = commands
        .run_command(command)
        .map_err(|err| Stop::Fail(err.to_string()))?;
    let mut accepted = true;
    for outcome in outcomes {
        if let Err(err) = outcome {
            eprintln!("layout switch failed: {err}");
            accepted = false;
        }
    }
    Ok(accepted)
}

fn note(verbose: bool, message: &str) {
    if verbose {
        eprintln!("{message}");
    }
}

/// SIGHUP sets [`RELOAD_REQUESTED`]. The loop applies it on the next event.
fn install_sighup() -> Result<(), Stop> {
    // SAFETY: `on_sighup` only stores an atomic. `sigemptyset` initializes
    // `mask` before `sigaction` reads it. SA_RESTART asks the kernel to
    // restart the blocked read; swayipc retries EINTR either way.
    unsafe {
        let mut mask = std::mem::zeroed::<libc::sigset_t>();
        if libc::sigemptyset(&raw mut mask) != 0 {
            return Err(Stop::Fail(format!(
                "sigemptyset failed: {}",
                io::Error::last_os_error()
            )));
        }
        let action = libc::sigaction {
            sa_sigaction: on_sighup as *const () as libc::sighandler_t,
            sa_mask: mask,
            sa_flags: libc::SA_RESTART,
            sa_restorer: None,
        };
        if libc::sigaction(libc::SIGHUP, &action, std::ptr::null_mut()) != 0 {
            return Err(Stop::Fail(format!(
                "SIGHUP handler failed: {}",
                io::Error::last_os_error()
            )));
        }
    }
    Ok(())
}

fn maybe_reload(commands: &mut Connection, memory: &mut Memory, options: &Options) {
    if RELOAD_REQUESTED.swap(false, Ordering::Acquire) {
        reload_config(commands, memory, options);
    }
}

fn reload_config(commands: &mut Connection, memory: &mut Memory, options: &Options) {
    match try_reload(commands, memory, options) {
        Ok(()) => note(options.verbose, "config reloaded"),
        Err(err) => eprintln!("config reload failed: {err}"),
    }
}

/// `--default` replaces the file fallback. The string is the error prefix.
fn overridden_fallback(
    file: &mut ConfigFile,
    default_layout: Option<&str>,
    source_label: &str,
) -> String {
    let Some(token) = default_layout else {
        return source_label.to_string();
    };
    file.fallback = Some(LayoutToken::Name(token.to_string()));
    "--default".to_string()
}

fn try_reload(
    commands: &mut Connection,
    memory: &mut Memory,
    options: &Options,
) -> Result<(), String> {
    let (source_label, mut file) =
        read_config(options).map_err(|stop| stop_text(&stop).to_string())?;
    let fallback_label =
        overridden_fallback(&mut file, options.default_layout.as_deref(), &source_label);
    let inputs = commands.get_inputs().map_err(|err| err.to_string())?;
    let names = names_for_reload(&inputs, memory.keyboard_id())?;
    memory.reload_rules(
        &file,
        &names,
        &RuleSource {
            fallback_label: &fallback_label,
            apps_label: &source_label,
        },
    )
}

fn stop_text(stop: &Stop) -> &str {
    match stop {
        Stop::Usage(message) | Stop::Fail(message) | Stop::Idle(message) => message,
    }
}

fn names_for_reload(inputs: &[Input], watched: &str) -> Result<Vec<String>, String> {
    if let Some(input) = inputs
        .iter()
        .find(|input| input.identifier == watched && !input.xkb_layout_names.is_empty())
    {
        return Ok(input.xkb_layout_names.clone());
    }
    Ok(first_keyboard(inputs)?.names)
}

fn replace_watched_keyboard(commands: &mut Connection, memory: &mut Memory, verbose: bool) {
    let inputs = match commands.get_inputs() {
        Ok(inputs) => inputs,
        Err(err) => {
            eprintln!("could not read keyboards: {err}");
            return;
        }
    };
    if let Some(identifier) = adopt_next_keyboard(memory, &inputs) {
        note(verbose, &format!("keyboard {identifier}"));
    }
}

/// First remaining keyboard that has both a layout index and a name list.
/// The removed id is skipped. Layout events stay limited to the id we watch.
fn next_keyboard(inputs: &[Input], removed: &str) -> Option<Keyboard> {
    let keyboards: Vec<&Input> = inputs
        .iter()
        .filter(|input| input.input_type == "keyboard" && input.identifier != removed)
        .collect();
    keyboard_with_index_and_names(&keyboards)
}

fn adopt_next_keyboard(memory: &mut Memory, inputs: &[Input]) -> Option<String> {
    let removed = memory.keyboard_id().to_string();
    let next = next_keyboard(inputs, &removed)?;
    let identifier = next.identifier;
    memory.watch_keyboard(identifier.clone());
    Some(identifier)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn help_lists_the_flags() {
        for flag in [
            "--help",
            "-h",
            "--version",
            "--verbose",
            "--config",
            "--default",
        ] {
            assert!(HELP.contains(flag), "{flag}");
        }
        assert!(matches!(
            parse_arg_list(["skbd", "-h"]).unwrap(),
            Parsed::Help
        ));
        assert!(matches!(
            parse_arg_list(["skbd", "--help"]).unwrap(),
            Parsed::Help
        ));
        assert!(matches!(
            parse_arg_list(["skbd", "--version"]).unwrap(),
            Parsed::Version
        ));
    }

    #[test]
    fn unknown_flag_and_missing_value_are_errors() {
        let unknown = parse_arg_list(["skbd", "--nope"]).unwrap_err();
        assert!(unknown.contains("unknown argument: --nope"), "{unknown}");
        let config = parse_arg_list(["skbd", "--config"]).unwrap_err();
        assert!(config.contains("--config needs a value"), "{config}");
        let default = parse_arg_list(["skbd", "--default"]).unwrap_err();
        assert!(default.contains("--default needs a value"), "{default}");
    }

    #[test]
    fn config_and_default_flags_are_kept() {
        let Parsed::Run(options) = parse_arg_list([
            "skbd",
            "--verbose",
            "--config",
            "dir/config.toml",
            "--default",
            "ua",
        ])
        .unwrap() else {
            panic!("expected run options");
        };
        assert!(options.verbose);
        assert_eq!(options.config.unwrap(), PathBuf::from("dir/config.toml"));
        assert_eq!(options.default_layout.as_deref(), Some("ua"));
    }

    #[test]
    fn app_id_wins_and_empty_falls_through_to_class() {
        assert_eq!(
            app_key(Some("Telegram"), Some("telegram")),
            Some("Telegram".to_string())
        );
        assert_eq!(app_key(Some(""), Some("Code")), Some("Code".to_string()));
        assert_eq!(app_key(None, Some("Code")), Some("Code".to_string()));
        assert_eq!(app_key(None, None), None);
        assert_eq!(app_key(Some(""), Some("")), None);
    }

    #[test]
    fn is_view_needs_a_container_and_an_app_window_or_pid() {
        assert!(is_view(&node("con", true, false, false)));
        assert!(is_view(&node("con", false, true, false)));
        assert!(is_view(&node("con", false, false, true)));
        assert!(is_view(&node("floating_con", false, false, true)));
        assert!(!is_view(&node("con", false, false, false)));
        assert!(!is_view(&node("workspace", true, true, true)));
        assert!(!is_view(&node("output", false, false, true)));
    }

    #[test]
    fn two_keyboards_that_disagree_use_the_first() {
        let inputs = vec![
            device("pointer", "mouse", None, &[]),
            device("keyboard", "kb-a", Some(0), &["English (US)", "Ukrainian"]),
            device("keyboard", "kb-b", Some(1), &["Ukrainian"]),
        ];

        let keyboard = first_keyboard(&inputs).unwrap();

        assert_eq!(keyboard.identifier, "kb-a");
        assert_eq!(keyboard.active, 0);
        assert_eq!(
            keyboard.names,
            vec!["English (US)".to_string(), "Ukrainian".to_string()]
        );
    }

    #[test]
    fn keyboard_with_both_an_index_and_names_wins() {
        let inputs = vec![
            device(
                "keyboard",
                "names-only",
                None,
                &["English (US)", "Ukrainian"],
            ),
            device("keyboard", "index-only", Some(1), &[]),
            device("keyboard", "both", Some(0), &["English (US)"]),
        ];

        let keyboard = first_keyboard(&inputs).unwrap();

        assert_eq!(keyboard.identifier, "both");
        assert_eq!(keyboard.active, 0);
        assert_eq!(keyboard.names, vec!["English (US)".to_string()]);
    }

    #[test]
    fn first_index_pairs_with_the_longest_name_list() {
        let inputs = vec![
            device("keyboard", "index-only", Some(1), &[]),
            device("keyboard", "short", None, &["English (US)"]),
            device("keyboard", "long", None, &["English (US)", "Ukrainian"]),
            device("keyboard", "also-long", None, &["Russian", "German"]),
        ];

        let keyboard = first_keyboard(&inputs).unwrap();

        assert_eq!(keyboard.identifier, "index-only");
        assert_eq!(keyboard.active, 1);
        assert_eq!(
            keyboard.names,
            vec!["English (US)".to_string(), "Ukrainian".to_string()]
        );
    }

    #[test]
    fn layout_event_from_another_keyboard_is_ignored() {
        let mut memory = Memory::with_layouts(0, 0, std::collections::HashMap::new());
        memory.watch_keyboard("kb-a".to_string());
        memory.remember_current(1);

        let other = device("keyboard", "kb-b", Some(1), &[]);
        assert_eq!(
            apply_input(&mut memory, InputChange::XkbLayout, &other, false),
            Action::None
        );
        assert_eq!(memory.apply(seen(2)), Action::None);

        let own = device("keyboard", "kb-a", Some(1), &[]);
        apply_input(&mut memory, InputChange::XkbLayout, &own, false);
        assert_eq!(memory.apply(seen(1)), Action::SwitchTo(0));
    }

    #[test]
    fn keymap_event_stores_the_index_it_carries() {
        let mut memory = Memory::with_layouts(1, 0, std::collections::HashMap::new());
        memory.watch_keyboard("kb-a".to_string());
        memory.remember_current(4);

        let input = device("keyboard", "kb-a", Some(0), &[]);
        apply_input(&mut memory, InputChange::XkbKeymap, &input, false);

        assert_eq!(memory.apply(seen(5)), Action::None);
    }

    #[test]
    fn empty_swaysock_is_an_error_at_the_cli_and_idle_under_systemd() {
        assert_eq!(
            fail_text(classify_swaysock(SwaySock::Empty, false)),
            "SWAYSOCK is empty"
        );
        assert_eq!(
            idle_text(classify_swaysock(SwaySock::Empty, true)),
            "SWAYSOCK is empty"
        );
        assert_eq!(
            fail_text(classify_swaysock(SwaySock::Missing, false)),
            "SWAYSOCK is not set"
        );
        assert_eq!(
            idle_text(classify_swaysock(SwaySock::Missing, true)),
            "SWAYSOCK is not set"
        );
        assert_eq!(
            fail_text(classify_swaysock(SwaySock::NotUtf8, true)),
            "SWAYSOCK is not valid UTF-8"
        );
        assert_eq!(
            classify_swaysock(SwaySock::Path("/run/user/1000/sway-ipc.sock".into()), false)
                .unwrap(),
            "/run/user/1000/sway-ipc.sock"
        );
    }

    #[test]
    fn socket_path_must_exist_and_be_a_socket() {
        let dir = scratch("socket");
        let missing = dir.join("missing");
        assert!(
            fail_text(ensure_socket(&missing)).contains("does not exist"),
            "{}",
            fail_text(ensure_socket(&missing))
        );

        let file = dir.join("file");
        fs::write(&file, b"nope").unwrap();
        assert!(
            fail_text(ensure_socket(&file)).contains("not a socket"),
            "{}",
            fail_text(ensure_socket(&file))
        );

        let sock = dir.join("sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        assert!(ensure_socket(&sock).is_ok());
        drop(listener);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn root_is_refused() {
        assert_eq!(
            fail_text(refuse_root(0)),
            "refusing to run as root. The unit is a systemd user service"
        );
        assert!(refuse_root(1000).is_ok());
    }

    #[test]
    fn runtime_dir_must_be_set_absolute_private_and_owned() {
        assert_eq!(
            fail_text(require_runtime_dir(None).map(|_| ())),
            "XDG_RUNTIME_DIR is unset"
        );
        assert_eq!(
            fail_text(require_runtime_dir(Some(std::ffi::OsStr::new(""))).map(|_| ())),
            "XDG_RUNTIME_DIR is empty"
        );
        assert_eq!(
            fail_text(require_runtime_dir(Some(std::ffi::OsStr::new("relative"))).map(|_| ())),
            "XDG_RUNTIME_DIR is not absolute"
        );

        let dir = scratch("runtime");
        assert!(check_runtime_dir(&dir, current_euid()).is_ok());
        let owned = fail_text(check_runtime_dir(&dir, current_euid().wrapping_add(1)));
        assert!(owned.contains("not owned"), "{owned}");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        let mode = fail_text(check_runtime_dir(&dir, current_euid()));
        assert!(mode.contains("0700"), "{mode}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn second_lock_reports_already_running() {
        let dir = scratch("lock");
        let first = lock_runtime_dir(&dir).unwrap();
        assert_eq!(
            fail_text(lock_runtime_dir(&dir)),
            "skbd: already running"
        );
        drop(first);
        assert!(lock_runtime_dir(&dir).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn lock_symlink_is_refused() {
        let dir = scratch("locklink");
        let path = dir.join("skbd.lock");
        std::os::unix::fs::symlink("elsewhere", &path).unwrap();
        let err = fail_text(lock_runtime_dir(&dir));
        assert!(err.contains("cannot open"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn workspace_focus_uses_the_view_in_current() {
        let mut child = node("con", true, false, false);
        child.id = 7;
        child.focused = true;
        let mut workspace = node("workspace", false, false, false);
        workspace.nodes.push(child);

        let view = focused_in_workspace(Some(&workspace)).expect("focused view");
        assert_eq!(view.id, 7);
    }

    #[test]
    fn workspace_without_a_view_asks_for_the_tree() {
        let workspace = node("workspace", false, false, false);
        assert!(focused_in_workspace(Some(&workspace)).is_none());
        assert!(focused_in_workspace(None).is_none());

        let mut child = node("con", false, false, false);
        child.focused = true;
        let mut parent = node("workspace", false, false, false);
        parent.nodes.push(child);
        assert!(focused_in_workspace(Some(&parent)).is_none());
    }

    #[test]
    fn close_syncs_only_the_focused_window() {
        let mut memory = Memory::with_layouts(0, 0, std::collections::HashMap::new());
        memory.apply(seen(4));
        memory.apply(seen(5));

        assert!(!close_window(&mut memory, 4));
        assert_eq!(memory.focused_id(), Some(5));
        assert!(close_window(&mut memory, 5));
        assert_eq!(memory.focused_id(), None);
    }

    fn seen(id: i64) -> Happening {
        Happening::Focus {
            id,
            app: None,
            x11: None,
            transient_for: None,
        }
    }

    #[test]
    fn startup_keeps_the_active_layout_for_every_open_view() {
        let mut focused = node("con", true, true, false);
        focused.id = 1;
        focused.focused = true;
        focused.app_id = Some("Telegram".to_string());
        focused.window = Some(50);
        let mut other = node("floating_con", true, false, false);
        other.id = 2;
        other.app_id = Some("Telegram".to_string());
        let mut root = node("root", false, false, false);
        root.nodes.push(focused);
        root.floating_nodes.push(other);

        let mut apps = std::collections::HashMap::new();
        apps.insert("Telegram".to_string(), 1);
        let mut memory = Memory::with_layouts(0, 0, apps);
        remember_open_views(&mut memory, &root);

        assert_eq!(memory.focused_id(), Some(1));
        assert_eq!(
            memory.apply(Happening::Focus {
                id: 2,
                app: Some("Telegram".to_string()),
                x11: None,
                transient_for: None,
            }),
            Action::None
        );
        assert_eq!(
            memory.apply(Happening::Focus {
                id: 3,
                app: Some("Telegram".to_string()),
                x11: Some(51),
                transient_for: Some(50),
            }),
            Action::None
        );
    }

    #[test]
    fn removed_watched_keyboard_is_replaced_and_the_other_is_ignored_until_then() {
        let mut memory = Memory::with_layouts(0, 0, std::collections::HashMap::new());
        memory.watch_keyboard("kb-a".to_string());
        memory.remember_current(1);

        let kb_b = device("keyboard", "kb-b", Some(1), &["English (US)", "Ukrainian"]);
        assert_eq!(
            apply_input(&mut memory, InputChange::XkbLayout, &kb_b, false),
            Action::None
        );
        assert_eq!(memory.apply(seen(2)), Action::None);

        let inputs = vec![
            device("keyboard", "kb-a", Some(0), &["English (US)", "Ukrainian"]),
            device("keyboard", "index-only", Some(1), &[]),
            device("keyboard", "kb-b", Some(1), &["English (US)", "Ukrainian"]),
            device("keyboard", "kb-c", Some(0), &["English (US)"]),
        ];
        let adopted = adopt_next_keyboard(&mut memory, &inputs).expect("next keyboard");
        assert_eq!(adopted, "kb-b");
        assert_eq!(memory.keyboard_id(), "kb-b");

        apply_input(&mut memory, InputChange::XkbLayout, &kb_b, false);
        assert_eq!(memory.apply(seen(1)), Action::SwitchTo(0));

        let kb_c = device("keyboard", "kb-c", Some(2), &["English (US)"]);
        apply_input(&mut memory, InputChange::XkbLayout, &kb_c, false);
        assert_eq!(memory.apply(seen(3)), Action::None);
    }

    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("skbd-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    fn fail_text(stopped: Result<impl Sized, Stop>) -> String {
        match stopped {
            Err(Stop::Fail(message)) => message,
            Err(other) => panic!("expected fail, got {other:?}"),
            Ok(_) => panic!("expected fail"),
        }
    }

    fn idle_text(stopped: Result<impl Sized, Stop>) -> String {
        match stopped {
            Err(Stop::Idle(message)) => message,
            Err(other) => panic!("expected idle, got {other:?}"),
            Ok(_) => panic!("expected idle"),
        }
    }

    fn node(node_type: &str, app_id: bool, window: bool, pid: bool) -> Node {
        let app_id = if app_id { "\"app\"" } else { "null" };
        let window = if window { "10" } else { "null" };
        let pid = if pid { "20" } else { "null" };
        let text = format!(
            r#"{{
                "id": 1,
                "name": "n",
                "type": "{node_type}",
                "border": "none",
                "current_border_width": 0,
                "layout": "none",
                "orientation": "none",
                "percent": null,
                "rect": {{"x": 0, "y": 0, "width": 0, "height": 0}},
                "window_rect": {{"x": 0, "y": 0, "width": 0, "height": 0}},
                "deco_rect": {{"x": 0, "y": 0, "width": 0, "height": 0}},
                "geometry": {{"x": 0, "y": 0, "width": 0, "height": 0}},
                "urgent": false,
                "focused": false,
                "focus": [],
                "floating": null,
                "nodes": [],
                "floating_nodes": [],
                "sticky": false,
                "representation": null,
                "fullscreen_mode": null,
                "scratchpad_state": null,
                "app_id": {app_id},
                "pid": {pid},
                "window": {window},
                "num": null,
                "window_properties": null,
                "marks": [],
                "inhibit_idle": null,
                "idle_inhibitors": null,
                "sandbox_engine": null,
                "sandbox_app_id": null,
                "sandbox_instance_id": null,
                "tag": null,
                "shell": null,
                "foreign_toplevel_identifier": null,
                "visible": null,
                "output": null
            }}"#
        );
        serde_json::from_str(&text).unwrap_or_else(|err| panic!("{err}: {text}"))
    }

    fn device(kind: &str, identifier: &str, index: Option<i32>, names: &[&str]) -> Input {
        let names = serde_json::to_string(names).unwrap();
        let index = index
            .map(|index| index.to_string())
            .unwrap_or_else(|| "null".to_string());
        let text = format!(
            r#"{{
                "identifier": {identifier:?},
                "name": {identifier:?},
                "type": "{kind}",
                "xkb_active_layout_name": null,
                "xkb_layout_names": {names},
                "xkb_active_layout_index": {index},
                "scroll_factor": 1.0,
                "libinput": null,
                "vendor": null,
                "product": null
            }}"#
        );
        serde_json::from_str(&text).unwrap_or_else(|err| panic!("{err}: {text}"))
    }
}
