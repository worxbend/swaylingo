//! Remember the Sway XKB layout for each window and restore it on focus.
//!
//! Sway owns the keymap (`xkb_layout` in the sway config). This process only
//! listens to window and input events and runs `xkb_switch_layout`.
//!
//! swayipc 4: `Connection::subscribe` consumes the socket, so commands use a
//! second connection. https://docs.rs/swayipc/4.0.0/swayipc/struct.Connection.html

mod memory;

use std::env;
use std::process;

use swayipc::{
    Connection, Event, EventType, Input, InputChange, Node, NodeType, WindowChange, WorkspaceChange,
};

use memory::{Action, Happening, Memory};

fn main() {
    let verbose = match parse_args() {
        Ok(verbose) => verbose,
        Err(message) => {
            eprintln!("{message}");
            process::exit(2);
        }
    };
    if let Err(message) = run(verbose) {
        eprintln!("{message}");
        process::exit(1);
    }
}

fn parse_args() -> Result<bool, String> {
    let mut verbose = false;
    for arg in env::args().skip(1) {
        match arg.as_str() {
            "--verbose" => verbose = true,
            "--version" => {
                println!("{}", env!("CARGO_PKG_VERSION"));
                process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(verbose)
}

fn run(verbose: bool) -> Result<(), String> {
    let mut commands = connect()?;
    let mut memory = seed(&mut commands, verbose)?;
    let events = connect()?
        .subscribe([EventType::Window, EventType::Input, EventType::Workspace])
        .map_err(|err| err.to_string())?;
    for event in events {
        let event = event.map_err(|err| err.to_string())?;
        if matches!(event, Event::Shutdown(_)) {
            return Ok(());
        }
        let action = react(&mut commands, &mut memory, &event, verbose)?;
        switch_if_needed(&mut commands, &mut memory, action, verbose)?;
    }
    Ok(())
}

/// Open one Sway socket. Events and commands cannot share it: `subscribe`
/// consumes the connection it is called on.
fn connect() -> Result<Connection, String> {
    if env::var_os("SWAYSOCK").is_none() {
        return Err("SWAYSOCK is not set".to_string());
    }
    Connection::new().map_err(|err| err.to_string())
}

/// Record the window that is already focused, without treating it as new.
/// An empty desktop at startup moves the keyboard to layout 0.
fn seed(commands: &mut Connection, verbose: bool) -> Result<Memory, String> {
    let inputs = commands.get_inputs().map_err(|err| err.to_string())?;
    let active = first_keyboard_layout(&inputs)?;
    note(verbose, &format!("active layout {active}"));
    let mut memory = Memory::new(active);
    let tree = commands.get_tree().map_err(|err| err.to_string())?;
    let action = if let Some(id) = focused_view_id(&tree) {
        note(verbose, &format!("focused window {id}"));
        memory.remember_current(id);
        Action::None
    } else {
        note(verbose, "no focused window");
        memory.apply(Happening::Unfocus)
    };
    switch_if_needed(commands, &mut memory, action, verbose)?;
    Ok(memory)
}

/// Index of the first keyboard that reports one.
///
/// A seat can list the same layout on a keyboard, its consumer-control
/// device, and a mouse that Sway also calls a keyboard. They should agree.
/// If they do not, the first index wins and the mismatch is printed.
fn first_keyboard_layout(inputs: &[Input]) -> Result<u32, String> {
    let mut selected = None;
    for input in inputs {
        if input.input_type != "keyboard" {
            continue;
        }
        let Some(index) = layout_index(input.xkb_active_layout_index) else {
            continue;
        };
        match selected {
            None => selected = Some(index),
            Some(current) if current != index => {
                eprintln!(
                    "keyboards disagree on layout index ({current} vs {index}); using {current}"
                );
            }
            Some(_) => {}
        }
    }
    selected.ok_or_else(|| "no keyboard layout from sway".to_string())
}

/// Sway reports the index as `i32`. A negative value is not a layout.
fn layout_index(index: Option<i32>) -> Option<u32> {
    index.and_then(|index| u32::try_from(index).ok())
}

fn focused_view_id(node: &Node) -> Option<i64> {
    if node.focused && is_view(node) {
        return Some(node.id);
    }
    node.nodes
        .iter()
        .chain(node.floating_nodes.iter())
        .find_map(focused_view_id)
}

/// A view has an app id (Wayland), an X11 window, or a pid.
/// Workspace and output nodes are focused when the desktop is empty, and
/// those must not be stored as windows.
fn is_view(node: &Node) -> bool {
    let is_container = matches!(node.node_type, NodeType::Con | NodeType::FloatingCon);
    is_container && (node.app_id.is_some() || node.window.is_some() || node.pid.is_some())
}

/// Workspace focus and the last window closing both need a fresh tree.
/// The workspace event does not include the views on that workspace.
fn react(
    commands: &mut Connection,
    memory: &mut Memory,
    event: &Event,
    verbose: bool,
) -> Result<Action, String> {
    match event {
        Event::Workspace(workspace) if workspace.change == WorkspaceChange::Focus => {
            sync_focus(commands, memory, verbose)
        }
        Event::Window(window)
            if window.change == WindowChange::Close && is_view(&window.container) =>
        {
            note(verbose, &format!("Close {{ id: {} }}", window.container.id));
            memory.apply(Happening::Close {
                id: window.container.id,
            });
            sync_focus(commands, memory, verbose)
        }
        _ => Ok(apply_event(memory, event, verbose)),
    }
}

/// Empty workspace, or the last window closed: default layout.
/// A workspace that already has a focused window keeps that window's layout.
fn sync_focus(
    commands: &mut Connection,
    memory: &mut Memory,
    verbose: bool,
) -> Result<Action, String> {
    let tree = commands.get_tree().map_err(|err| err.to_string())?;
    let happening = match focused_view_id(&tree) {
        Some(id) => Happening::Focus { id },
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
    let happening = Happening::Focus { id: node.id };
    note(verbose, &format!("{happening:?}"));
    memory.apply(happening)
}

fn apply_input(memory: &mut Memory, change: InputChange, input: &Input, verbose: bool) -> Action {
    if input.input_type != "keyboard" {
        return Action::None;
    }
    let happening = match change {
        InputChange::XkbKeymap => Happening::KeymapReset,
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
) -> Result<(), String> {
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
fn switch_layout(commands: &mut Connection, index: u32) -> Result<bool, String> {
    let command = format!("input type:keyboard xkb_switch_layout {index}");
    let outcomes = commands
        .run_command(command)
        .map_err(|err| err.to_string())?;
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
