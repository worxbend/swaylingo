//! Per-window XKB layout index.
//!
//! A user layout change is stored on the focused window. `pending` remembers
//! every index this process requested, including one a later request replaced.
//! Only those echoes are ignored. Any other index is stored on the focused
//! window, and the outstanding requests are cleared. The window we left is
//! not rewritten on focus.
//!
//! A window seen for the first time uses the layout for its app, or the
//! fallback. The fallback is [`DEFAULT_LAYOUT`] when the config does not set
//! one (the first `xkb_layout`, `us` when the list is `us,ua`). A desktop
//! with no focused window uses the fallback. A window already in the map
//! keeps the layout stored on it.

/// First layout in the Sway `xkb_layout` list.
pub const DEFAULT_LAYOUT: u32 = 0;

use std::collections::{HashMap, HashSet};

/// What Sway just reported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Happening {
    Focus {
        id: i64,
        /// Wayland `app_id`, or X11 `WM_CLASS` when there is no app id.
        app: Option<String>,
    },
    /// No view is focused. The keyboard goes back to the fallback layout.
    Unfocus,
    Close {
        id: i64,
    },
    Layout {
        index: u32,
    },
    /// `index` is the layout the keymap event reported, when it had one.
    KeymapReset {
        index: Option<u32>,
    },
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
/// `active` is the index Sway has confirmed. `pending` is every index this
/// process requested and has not seen echoed, including one a later request
/// replaced. `requested` is the newest of those. An echo is that request
/// coming back, not a key the person pressed. Writing it would stick the
/// wrong language to the window that just gained focus.
#[derive(Debug)]
pub struct Memory {
    by_window: HashMap<i64, u32>,
    apps: HashMap<String, u32>,
    fallback: u32,
    focused: Option<i64>,
    active: u32,
    pending: HashSet<u32>,
    requested: Option<u32>,
    keyboard_id: String,
}

impl Memory {
    /// `fallback` is the index for a new window with no app rule, and for an
    /// empty desktop. `apps` maps an app id to a layout index. Pass
    /// [`DEFAULT_LAYOUT`] and an empty map when the config sets neither.
    pub fn with_layouts(active: u32, fallback: u32, apps: HashMap<String, u32>) -> Self {
        Self {
            by_window: HashMap::new(),
            apps,
            fallback,
            focused: None,
            active,
            pending: HashSet::new(),
            requested: None,
            keyboard_id: String::new(),
        }
    }

    /// Layout events from any other Sway input are ignored.
    pub fn watch_keyboard(&mut self, identifier: String) {
        self.keyboard_id = identifier;
    }

    pub fn keyboard_id(&self) -> &str {
        &self.keyboard_id
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
            Happening::Focus { id, app } => self.on_focus(id, app.as_deref()),
            Happening::Unfocus => self.on_unfocus(),
            Happening::Close { id } => self.on_close(id),
            Happening::Layout { index } => self.on_layout(index),
            Happening::KeymapReset { index } => self.on_keymap_reset(index),
        }
    }

    /// The command socket rejected the newest switch, so that one will not echo.
    /// An older request can still echo and must stay ignored.
    pub fn abandon_switch(&mut self) {
        if let Some(index) = self.requested.take() {
            self.pending.remove(&index);
        }
    }

    fn on_close(&mut self, id: i64) -> Action {
        self.by_window.remove(&id);
        if self.focused == Some(id) {
            self.focused = None;
        }
        Action::None
    }

    fn on_keymap_reset(&mut self, index: Option<u32>) -> Action {
        self.by_window.clear();
        self.pending.clear();
        self.requested = None;
        if let Some(index) = index {
            self.active = index;
        }
        if let Some(id) = self.focused {
            self.by_window.insert(id, self.active);
        }
        Action::None
    }

    fn on_layout(&mut self, index: u32) -> Action {
        if self.pending.remove(&index) {
            if self.requested == Some(index) {
                self.active = index;
                self.requested = None;
            }
            return Action::None;
        }
        if index == self.active {
            return Action::None;
        }
        self.active = index;
        self.pending.clear();
        self.requested = None;
        if let Some(id) = self.focused {
            self.by_window.insert(id, index);
        }
        Action::None
    }

    fn on_focus(&mut self, id: i64, app: Option<&str>) -> Action {
        if self.focused == Some(id) {
            return Action::None;
        }
        self.focused = Some(id);
        let layout = match self.by_window.get(&id) {
            Some(layout) => *layout,
            None => {
                let layout = self.layout_for_new_window(app);
                self.by_window.insert(id, layout);
                layout
            }
        };
        self.request(layout)
    }

    /// App rule only on the first sighting. A stored window is left alone.
    fn layout_for_new_window(&self, app: Option<&str>) -> u32 {
        app.and_then(|app| self.apps.get(app).copied())
            .unwrap_or(self.fallback)
    }

    fn on_unfocus(&mut self) -> Action {
        self.focused = None;
        self.request(self.fallback)
    }

    fn request(&mut self, index: u32) -> Action {
        if index == self.intended() {
            return Action::None;
        }
        self.pending.insert(index);
        self.requested = Some(index);
        Action::SwitchTo(index)
    }

    /// Newest layout this process asked for, or the one Sway has confirmed.
    fn intended(&self) -> u32 {
        self.requested.unwrap_or(self.active)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn focus(id: i64) -> Happening {
        Happening::Focus { id, app: None }
    }

    fn memory_at(active: u32) -> Memory {
        Memory::with_layouts(active, DEFAULT_LAYOUT, HashMap::new())
    }

    fn remember_english_and_ukrainian() -> Memory {
        let mut memory = memory_at(0);
        memory.apply(focus(1));
        memory.apply(focus(2));
        memory.apply(Happening::Layout { index: 1 });
        memory
    }

    #[test]
    fn focus_restores_the_previous_windows_layout() {
        let mut memory = remember_english_and_ukrainian();

        let action = memory.apply(focus(1));

        assert_eq!(action, Action::SwitchTo(0));
        assert_eq!(memory.requested, Some(0));
        assert!(memory.pending.contains(&0));
        assert_eq!(memory.by_window[&1], 0);
        assert_eq!(memory.by_window[&2], 1);
    }

    #[test]
    fn restore_echo_does_not_overwrite_the_window() {
        let mut memory = remember_english_and_ukrainian();
        memory.apply(focus(1));

        let action = memory.apply(Happening::Layout { index: 0 });

        assert_eq!(action, Action::None);
        assert!(memory.pending.is_empty());
        assert_eq!(memory.requested, None);
        assert_eq!(memory.by_window[&1], 0);
    }

    #[test]
    fn duplicate_layout_event_keeps_the_map() {
        let mut memory = remember_english_and_ukrainian();
        memory.apply(focus(1));
        memory.apply(Happening::Layout { index: 0 });

        memory.apply(Happening::Layout { index: 0 });

        assert_eq!(memory.by_window[&1], 0);
        assert_eq!(memory.by_window[&2], 1);
    }

    #[test]
    fn new_window_uses_the_default_layout() {
        let mut memory = memory_at(0);
        memory.apply(focus(1));
        memory.apply(Happening::Layout { index: 1 });

        let action = memory.apply(focus(9));

        assert_eq!(action, Action::SwitchTo(DEFAULT_LAYOUT));
        assert_eq!(memory.by_window[&9], DEFAULT_LAYOUT);
        assert_eq!(memory.by_window[&1], 1);
    }

    #[test]
    fn empty_desktop_switches_to_the_default_layout() {
        let mut memory = memory_at(0);
        memory.apply(focus(1));
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
        let mut memory = memory_at(1);
        memory.remember_current(1);

        let action = memory.apply(focus(2));

        assert_eq!(memory.by_window[&1], 1);
        assert_eq!(action, Action::SwitchTo(DEFAULT_LAYOUT));
        assert_eq!(memory.by_window[&2], DEFAULT_LAYOUT);
    }

    #[test]
    fn focus_during_a_pending_switch_does_not_overwrite_either_window() {
        let mut memory = remember_english_and_ukrainian();
        memory.apply(focus(1));

        let action = memory.apply(focus(2));

        assert_eq!(action, Action::SwitchTo(1));
        assert!(memory.pending.contains(&0));
        assert!(memory.pending.contains(&1));
        assert_eq!(memory.by_window[&1], 0);
        assert_eq!(memory.by_window[&2], 1);
        memory.apply(Happening::Layout { index: 0 });
        memory.apply(Happening::Layout { index: 1 });
        assert!(memory.pending.is_empty());
        assert_eq!(memory.by_window[&1], 0);
        assert_eq!(memory.by_window[&2], 1);
    }

    #[test]
    fn user_layout_clears_outstanding_requests_and_stores_on_the_focused_window() {
        let mut memory = remember_english_and_ukrainian();
        memory.apply(focus(1));
        memory.apply(focus(2));

        memory.apply(Happening::Layout { index: 2 });

        assert!(memory.pending.is_empty());
        assert_eq!(memory.requested, None);
        assert_eq!(memory.active, 2);
        assert_eq!(memory.by_window[&2], 2);
        assert_eq!(memory.by_window[&1], 0);
    }

    #[test]
    fn close_drops_the_window_and_ignores_a_later_layout() {
        let mut memory = memory_at(0);
        memory.apply(focus(4));

        memory.apply(Happening::Close { id: 4 });
        memory.apply(Happening::Layout { index: 1 });

        assert_eq!(memory.by_window.get(&4), None);
    }

    #[test]
    fn close_of_the_focused_window_clears_focus() {
        let mut memory = memory_at(0);
        memory.apply(focus(4));
        assert_eq!(memory.focused, Some(4));

        let action = memory.apply(Happening::Close { id: 4 });

        assert_eq!(action, Action::None);
        assert_eq!(memory.focused, None);
        assert_eq!(memory.by_window.get(&4), None);
        memory.apply(Happening::Layout { index: 1 });
        assert_eq!(memory.focused, None);
        assert_eq!(memory.by_window.get(&4), None);
    }

    #[test]
    fn keymap_reset_forgets_other_windows_and_keeps_the_focused_one() {
        let mut memory = remember_english_and_ukrainian();

        memory.apply(Happening::KeymapReset { index: Some(0) });

        assert_eq!(memory.by_window.get(&1), None);
        assert_eq!(memory.by_window[&2], 0);
        assert_eq!(memory.active, 0);
        assert!(memory.pending.is_empty());
        assert_eq!(memory.requested, None);
    }

    #[test]
    fn keymap_reset_without_an_index_keeps_the_active_layout() {
        let mut memory = remember_english_and_ukrainian();

        memory.apply(Happening::KeymapReset { index: None });

        assert_eq!(memory.by_window[&2], 1);
        assert_eq!(memory.active, 1);
    }

    #[test]
    fn abandoned_switch_still_records_the_next_user_layout() {
        let mut memory = memory_at(1);
        memory.remember_current(1);
        memory.apply(Happening::Layout { index: 2 });
        memory.apply(focus(3));
        memory.abandon_switch();

        memory.apply(Happening::Layout { index: 1 });

        assert!(memory.pending.is_empty());
        assert_eq!(memory.requested, None);
        assert_eq!(memory.by_window[&3], 1);
        assert_eq!(memory.active, 1);
    }

    fn telegram_rules(fallback: u32) -> Memory {
        let mut apps = HashMap::new();
        apps.insert("Telegram".to_string(), 1);
        Memory::with_layouts(0, fallback, apps)
    }

    #[test]
    fn new_telegram_uses_the_ua_index() {
        let mut memory = telegram_rules(0);

        let action = memory.apply(Happening::Focus {
            id: 3,
            app: Some("Telegram".to_string()),
        });

        assert_eq!(action, Action::SwitchTo(1));
        assert_eq!(memory.by_window[&3], 1);
    }

    #[test]
    fn new_unknown_app_uses_the_fallback() {
        let mut memory = telegram_rules(2);

        let action = memory.apply(Happening::Focus {
            id: 4,
            app: Some("Firefox".to_string()),
        });

        assert_eq!(action, Action::SwitchTo(2));
        assert_eq!(memory.by_window[&4], 2);
    }

    #[test]
    fn empty_desktop_requests_the_fallback_index() {
        let mut memory = Memory::with_layouts(0, 2, HashMap::new());
        memory.remember_current(1);

        let action = memory.apply(Happening::Unfocus);

        assert_eq!(action, Action::SwitchTo(2));
        assert_eq!(memory.focused, None);
        assert_eq!(memory.by_window[&1], 0);
    }

    #[test]
    fn remembered_window_ignores_the_app_rule() {
        let mut memory = telegram_rules(0);
        memory.apply(Happening::Focus {
            id: 7,
            app: Some("Telegram".to_string()),
        });
        memory.apply(Happening::Layout { index: 1 });
        memory.apply(Happening::Layout { index: 0 });
        memory.apply(focus(8));
        memory.apply(Happening::Layout { index: 1 });

        let action = memory.apply(Happening::Focus {
            id: 7,
            app: Some("Telegram".to_string()),
        });

        assert_eq!(action, Action::SwitchTo(0));
        assert_eq!(memory.by_window[&7], 0);
    }
}
