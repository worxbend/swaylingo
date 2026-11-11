//! Per-window XKB layout index.
//!
//! A user layout change is stored on the focused window. A restore this
//! process requested is `pending`, so the matching Sway echo is not stored
//! again. The window we left is not rewritten on focus.
//!
//! A window seen for the first time, and a desktop with no focused window,
//! use [`DEFAULT_LAYOUT`] (the first `xkb_layout`, `us` when the list is `us,ua`).

/// First layout in the Sway `xkb_layout` list.
pub const DEFAULT_LAYOUT: u32 = 0;

use std::collections::HashMap;

/// What Sway just reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Happening {
    Focus {
        id: i64,
    },
    /// No view is focused. The keyboard goes back to [`DEFAULT_LAYOUT`].
    Unfocus,
    Close {
        id: i64,
    },
    Layout {
        index: u32,
    },
    KeymapReset,
}

/// What the daemon should do. `None` means the keyboard is already right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    SwitchTo(u32),
}

/// Layout memory for one Sway session.
///
/// Keys in `by_window` are Sway container ids. They die with the window, so
/// the map is not written to disk.
///
/// `active` is the index Sway has confirmed. `pending` is an index this
/// process just requested. The matching `xkb_layout` event is that request
/// coming back, not a key the person pressed. Writing it again would stick
/// the wrong language to the window that just gained focus.
#[derive(Debug)]
pub struct Memory {
    by_window: HashMap<i64, u32>,
    focused: Option<i64>,
    active: u32,
    pending: Option<u32>,
}

impl Memory {
    pub fn new(active: u32) -> Self {
        Self {
            by_window: HashMap::new(),
            focused: None,
            active,
            pending: None,
        }
    }

    /// The window that was already focused when the daemon started.
    /// Keeps the layout Sway is using now. A later new window does not.
    pub fn remember_current(&mut self, id: i64) {
        self.focused = Some(id);
        self.by_window.entry(id).or_insert(self.active);
    }

    /// Fold one Sway event into the map and say whether the keyboard must move.
    pub fn apply(&mut self, happening: Happening) -> Action {
        match happening {
            Happening::Focus { id } => self.on_focus(id),
            Happening::Unfocus => self.on_unfocus(),
            Happening::Close { id } => self.on_close(id),
            Happening::Layout { index } => self.on_layout(index),
            Happening::KeymapReset => self.on_keymap_reset(),
        }
    }

    /// The command socket rejected the switch, so no echo will confirm it.
    pub fn abandon_switch(&mut self) {
        self.pending = None;
    }

    fn on_close(&mut self, id: i64) -> Action {
        self.by_window.remove(&id);
        if self.focused == Some(id) {
            self.focused = None;
        }
        Action::None
    }

    fn on_keymap_reset(&mut self) -> Action {
        self.by_window.clear();
        self.pending = None;
        if let Some(id) = self.focused {
            self.by_window.insert(id, self.active);
        }
        Action::None
    }

    fn on_layout(&mut self, index: u32) -> Action {
        if self.pending == Some(index) {
            self.active = index;
            self.pending = None;
            return Action::None;
        }
        if self.pending.is_some() {
            // An older switch can still echo after a newer one replaced it.
            // Remember the keys Sway actually moved to, but do not store that
            // echo on the focused window.
            self.active = index;
            return Action::None;
        }
        if index == self.active {
            return Action::None;
        }
        self.active = index;
        if let Some(id) = self.focused {
            self.by_window.insert(id, index);
        }
        Action::None
    }

    // shortcut: no 50ms timer; add one if focus during an in-flight switch clobbers the previous window.
    fn on_focus(&mut self, id: i64) -> Action {
        if self.focused == Some(id) {
            return Action::None;
        }
        self.focused = Some(id);
        let layout = *self.by_window.entry(id).or_insert(DEFAULT_LAYOUT);
        self.request(layout)
    }

    fn on_unfocus(&mut self) -> Action {
        self.focused = None;
        self.request(DEFAULT_LAYOUT)
    }

    fn request(&mut self, index: u32) -> Action {
        if index == self.intended() {
            return Action::None;
        }
        self.pending = Some(index);
        Action::SwitchTo(index)
    }

    /// Layout already requested, or the one Sway has confirmed.
    fn intended(&self) -> u32 {
        self.pending.unwrap_or(self.active)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remember_english_and_ukrainian() -> Memory {
        let mut memory = Memory::new(0);
        memory.apply(Happening::Focus { id: 1 });
        memory.apply(Happening::Focus { id: 2 });
        memory.apply(Happening::Layout { index: 1 });
        memory
    }

    #[test]
    fn focus_restores_the_previous_windows_layout() {
        let mut memory = remember_english_and_ukrainian();

        let action = memory.apply(Happening::Focus { id: 1 });

        assert_eq!(action, Action::SwitchTo(0));
        assert_eq!(memory.pending, Some(0));
        assert_eq!(memory.by_window[&1], 0);
        assert_eq!(memory.by_window[&2], 1);
    }

    #[test]
    fn restore_echo_does_not_overwrite_the_window() {
        let mut memory = remember_english_and_ukrainian();
        memory.apply(Happening::Focus { id: 1 });

        let action = memory.apply(Happening::Layout { index: 0 });

        assert_eq!(action, Action::None);
        assert_eq!(memory.pending, None);
        assert_eq!(memory.by_window[&1], 0);
    }

    #[test]
    fn duplicate_layout_event_keeps_the_map() {
        let mut memory = remember_english_and_ukrainian();
        memory.apply(Happening::Focus { id: 1 });
        memory.apply(Happening::Layout { index: 0 });

        memory.apply(Happening::Layout { index: 0 });

        assert_eq!(memory.by_window[&1], 0);
        assert_eq!(memory.by_window[&2], 1);
    }

    #[test]
    fn new_window_uses_the_default_layout() {
        let mut memory = Memory::new(0);
        memory.apply(Happening::Focus { id: 1 });
        memory.apply(Happening::Layout { index: 1 });

        let action = memory.apply(Happening::Focus { id: 9 });

        assert_eq!(action, Action::SwitchTo(DEFAULT_LAYOUT));
        assert_eq!(memory.by_window[&9], DEFAULT_LAYOUT);
        assert_eq!(memory.by_window[&1], 1);
    }

    #[test]
    fn empty_desktop_switches_to_the_default_layout() {
        let mut memory = Memory::new(0);
        memory.apply(Happening::Focus { id: 1 });
        memory.apply(Happening::Layout { index: 1 });

        let action = memory.apply(Happening::Unfocus);

        assert_eq!(action, Action::SwitchTo(DEFAULT_LAYOUT));
        assert_eq!(memory.focused, None);
        assert_eq!(memory.by_window[&1], 1);
        memory.apply(Happening::Layout { index: 0 });
        assert_eq!(memory.by_window[&1], 1);
        assert_eq!(memory.active, DEFAULT_LAYOUT);
    }

    #[test]
    fn window_open_at_startup_keeps_the_active_layout() {
        let mut memory = Memory::new(1);
        memory.remember_current(1);

        let action = memory.apply(Happening::Focus { id: 2 });

        assert_eq!(memory.by_window[&1], 1);
        assert_eq!(action, Action::SwitchTo(DEFAULT_LAYOUT));
        assert_eq!(memory.by_window[&2], DEFAULT_LAYOUT);
    }

    #[test]
    fn focus_during_a_pending_switch_does_not_overwrite_either_window() {
        let mut memory = remember_english_and_ukrainian();
        memory.apply(Happening::Focus { id: 1 });

        let action = memory.apply(Happening::Focus { id: 2 });

        assert_eq!(action, Action::SwitchTo(1));
        assert_eq!(memory.by_window[&1], 0);
        assert_eq!(memory.by_window[&2], 1);
        memory.apply(Happening::Layout { index: 0 });
        memory.apply(Happening::Layout { index: 1 });
        assert_eq!(memory.pending, None);
        assert_eq!(memory.by_window[&1], 0);
        assert_eq!(memory.by_window[&2], 1);
    }

    #[test]
    fn close_drops_the_window_and_ignores_a_later_layout() {
        let mut memory = Memory::new(0);
        memory.apply(Happening::Focus { id: 4 });

        memory.apply(Happening::Close { id: 4 });
        memory.apply(Happening::Layout { index: 1 });

        assert_eq!(memory.by_window.get(&4), None);
    }

    #[test]
    fn keymap_reset_forgets_other_windows_and_keeps_the_focused_one() {
        let mut memory = remember_english_and_ukrainian();

        memory.apply(Happening::KeymapReset);

        assert_eq!(memory.by_window.get(&1), None);
        assert_eq!(memory.by_window[&2], 1);
        assert_eq!(memory.pending, None);
    }

    #[test]
    fn abandoned_switch_still_records_the_next_user_layout() {
        let mut memory = Memory::new(1);
        memory.remember_current(1);
        memory.apply(Happening::Layout { index: 2 });
        memory.apply(Happening::Focus { id: 3 });
        memory.abandon_switch();

        memory.apply(Happening::Layout { index: 1 });

        assert_eq!(memory.pending, None);
        assert_eq!(memory.by_window[&3], 1);
        assert_eq!(memory.active, 1);
    }
}
