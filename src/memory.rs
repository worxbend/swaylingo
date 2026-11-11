//! Per-window XKB layout index.
//!
//! A user layout change is stored on the focused window. `pending` remembers
//! every index this process requested, including one a later request replaced.
//! When the newest request is echoed, `pending` is cleared. A replaced index
//! is still ignored once, so that late echo is not stored. Any other index is
//! stored on the focused window, and the outstanding requests are cleared.
//! The window we left is not rewritten on focus.
//!
//! A window seen for the first time uses the layout for its app, or the
//! fallback. The fallback is [`DEFAULT_LAYOUT`] when the config does not set
//! one (the first `xkb_layout`, `us` when the list is `us,ua`). A desktop
//! with no focused window uses the fallback. A window already in the map
//! keeps the layout stored on it.
//!
//! Windows already open at startup keep the layout that was active then,
//! focused or not. An app rule does not apply to them. An X11 dialog whose
//! `transient_for` is the X11 id of a window already in the map copies that
//! window's layout and does not use an app rule. A window with no
//! `transient_for` still uses the app rule.

/// First layout in the Sway `xkb_layout` list.
pub const DEFAULT_LAYOUT: u32 = 0;

use std::collections::{HashMap, HashSet};

use crate::config::{self, ConfigFile, LayoutToken, RuleSource};

/// What Sway just reported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Happening {
    Focus {
        id: i64,
        /// Wayland `app_id`, or X11 `WM_CLASS` when there is no app id.
        app: Option<String>,
        /// X11 window id of this view (`Node::window`). Absent on Wayland.
        x11: Option<i32>,
        /// X11 id of the parent (`window_properties.transient_for`).
        /// Not a Sway container id. Absent when the window is not a dialog.
        transient_for: Option<i32>,
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
    /// `names` is that event's `xkb_layout_names`.
    KeymapReset {
        index: Option<u32>,
        names: Vec<String>,
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
/// replaced. `requested` is the newest of those. When that echo arrives,
/// `pending` is cleared and the replaced indexes move to `replaced`, so one
/// late echo is still ignored. An echo is that request coming back, not a
/// key the person pressed. Writing it would stick the wrong language to the
/// window that just gained focus.
///
/// `fallback_token` and `app_tokens` are the config tokens. A keymap reset
/// resolves them again. The indexes are not kept across that reset.
#[derive(Debug)]
pub struct Memory {
    by_window: HashMap<i64, u32>,
    /// X11 window id to the Sway container that owns it.
    /// `transient_for` looks up a parent here, then in `by_window`.
    container_by_x11: HashMap<i32, i64>,
    apps: HashMap<String, u32>,
    fallback: u32,
    fallback_token: Option<LayoutToken>,
    app_tokens: Vec<(String, LayoutToken)>,
    focused: Option<i64>,
    active: u32,
    pending: HashSet<u32>,
    replaced: HashSet<u32>,
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
            container_by_x11: HashMap::new(),
            apps,
            fallback,
            fallback_token: None,
            app_tokens: Vec::new(),
            focused: None,
            active,
            pending: HashSet::new(),
            replaced: HashSet::new(),
            requested: None,
            keyboard_id: String::new(),
        }
    }

    /// Keep the config tokens so a keymap reset can resolve them again.
    pub fn keep_tokens(&mut self, fallback: Option<LayoutToken>, apps: Vec<(String, LayoutToken)>) {
        self.fallback_token = fallback;
        self.app_tokens = apps;
    }

    /// SIGHUP. Resolve first so a bad token leaves the old rules in place.
    /// `by_window` and the focused id are not touched.
    pub fn reload_rules(
        &mut self,
        file: &ConfigFile,
        names: &[String],
        from: &RuleSource<'_>,
    ) -> Result<(), String> {
        let rules = config::resolve_rules(file, names, from)?;
        self.fallback = rules.fallback;
        self.apps = rules.apps;
        self.keep_tokens(file.fallback.clone(), file.apps.clone());
        Ok(())
    }

    pub fn focused_id(&self) -> Option<i64> {
        self.focused
    }

    /// Layout events from any other Sway input are ignored.
    pub fn watch_keyboard(&mut self, identifier: String) {
        self.keyboard_id = identifier;
    }

    pub fn keyboard_id(&self) -> &str {
        &self.keyboard_id
    }

    /// A view that already existed at startup. Stores the active layout.
    /// Does not apply an app rule.
    pub fn note_existing(&mut self, id: i64, x11: Option<i32>) {
        self.by_window.entry(id).or_insert(self.active);
        if let Some(x11) = x11 {
            self.remember_x11(id, x11);
        }
    }

    /// The view that had focus at startup. Does not store a layout.
    fn mark_focused(&mut self, id: i64) {
        self.focused = Some(id);
    }

    /// The window that was already focused when the daemon started.
    /// Keeps the layout Sway is using now. A later new window does not.
    pub fn remember_current(&mut self, id: i64) {
        self.note_existing(id, None);
        self.mark_focused(id);
    }

    /// Fold one Sway event into the map and say whether the keyboard must move.
    pub fn apply(&mut self, happening: Happening) -> Action {
        match happening {
            Happening::Focus {
                id,
                app,
                x11,
                transient_for,
            } => self.on_focus(id, app.as_deref(), x11, transient_for),
            Happening::Unfocus => self.on_unfocus(),
            Happening::Close { id } => self.on_close(id),
            Happening::Layout { index } => self.on_layout(index),
            Happening::KeymapReset { index, names } => self.on_keymap_reset(index, &names),
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
        self.container_by_x11
            .retain(|_, container| *container != id);
        if self.focused == Some(id) {
            self.focused = None;
        }
        Action::None
    }

    fn on_keymap_reset(&mut self, index: Option<u32>, names: &[String]) -> Action {
        self.retarget(names);
        self.by_window.clear();
        self.pending.clear();
        self.replaced.clear();
        self.requested = None;
        if let Some(index) = index {
            self.active = index;
        }
        if let Some(id) = self.focused {
            self.by_window.insert(id, self.active);
        }
        Action::None
    }

    /// Resolve stored tokens against the keymap event. A miss skips that app.
    /// Leaving the process is wrong here: the old index is not kept.
    fn retarget(&mut self, names: &[String]) {
        if self.fallback_token.is_none() && self.app_tokens.is_empty() {
            return;
        }
        if let Some(token) = &self.fallback_token {
            self.fallback = match config::resolve_token(token, names) {
                Ok(index) => index,
                Err(err) => {
                    eprintln!("fallback layout no longer matches: {err}");
                    DEFAULT_LAYOUT
                }
            };
        }
        self.apps.clear();
        for (app, token) in &self.app_tokens {
            match config::resolve_token(token, names) {
                Ok(index) => {
                    self.apps.insert(app.clone(), index);
                }
                Err(err) => eprintln!("skipping app {app}: {err}"),
            }
        }
    }

    fn on_layout(&mut self, index: u32) -> Action {
        if self.requested == Some(index) {
            self.active = index;
            self.pending.remove(&index);
            self.replaced.extend(self.pending.drain());
            self.requested = None;
            return Action::None;
        }
        if self.pending.remove(&index) || self.replaced.remove(&index) {
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

    fn on_focus(
        &mut self,
        id: i64,
        app: Option<&str>,
        x11: Option<i32>,
        transient_for: Option<i32>,
    ) -> Action {
        if let Some(x11) = x11 {
            self.remember_x11(id, x11);
        }
        if self.focused == Some(id) {
            return Action::None;
        }
        self.focused = Some(id);
        let layout = match self.by_window.get(&id) {
            Some(layout) => *layout,
            None => {
                let layout = self.layout_for_new_window(app, transient_for);
                self.by_window.insert(id, layout);
                layout
            }
        };
        self.request(layout)
    }

    /// X11 id of this container. A later id for the same container replaces it.
    fn remember_x11(&mut self, container: i64, x11: i32) {
        self.container_by_x11.retain(|_, id| *id != container);
        self.container_by_x11.insert(x11, container);
    }

    /// App rule only on the first sighting. A stored window is left alone.
    /// An X11 dialog copies its parent when that parent is already remembered.
    fn layout_for_new_window(&self, app: Option<&str>, transient_for: Option<i32>) -> u32 {
        if let Some(layout) = self.layout_of_transient(transient_for) {
            return layout;
        }
        app.and_then(|app| self.apps.get(app).copied())
            .unwrap_or(self.fallback)
    }

    fn layout_of_transient(&self, transient_for: Option<i32>) -> Option<u32> {
        let x11 = transient_for?;
        let container = self.container_by_x11.get(&x11).copied()?;
        self.by_window.get(&container).copied()
    }

    fn on_unfocus(&mut self) -> Action {
        self.focused = None;
        self.request(self.fallback)
    }

    fn request(&mut self, index: u32) -> Action {
        if index == self.intended() {
            return Action::None;
        }
        self.replaced.remove(&index);
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

    use crate::config::LayoutToken;

    use super::*;

    fn focus(id: i64) -> Happening {
        Happening::Focus {
            id,
            app: None,
            x11: None,
            transient_for: None,
        }
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

        memory.apply(Happening::KeymapReset {
            index: Some(0),
            names: Vec::new(),
        });

        assert_eq!(memory.by_window.get(&1), None);
        assert_eq!(memory.by_window[&2], 0);
        assert_eq!(memory.active, 0);
        assert!(memory.pending.is_empty());
        assert_eq!(memory.requested, None);
    }

    #[test]
    fn replaced_switch_echo_after_the_newest_echo_does_not_change_the_focused_window() {
        let mut memory = remember_english_and_ukrainian();
        memory.apply(focus(1));
        memory.apply(focus(2));
        assert_eq!(memory.requested, Some(1));
        assert!(memory.pending.contains(&0));

        memory.apply(Happening::Layout { index: 1 });

        assert_eq!(memory.active, 1);
        assert!(memory.pending.is_empty());
        assert_eq!(memory.requested, None);
        assert_eq!(memory.by_window[&2], 1);

        memory.apply(Happening::Layout { index: 0 });

        assert_eq!(memory.focused, Some(2));
        assert_eq!(memory.active, 1);
        assert_eq!(memory.by_window[&2], 1);
        assert_eq!(memory.by_window[&1], 0);
    }

    #[test]
    fn keymap_reset_keeps_tokens_and_skips_an_unknown_app() {
        let mut memory = Memory::with_layouts(0, 0, HashMap::new());
        memory.keep_tokens(
            Some(LayoutToken::Name("us".into())),
            vec![
                ("Telegram".into(), LayoutToken::Name("ua".into())),
                ("Ghost".into(), LayoutToken::Name("zz".into())),
            ],
        );
        memory.remember_current(4);
        memory.apply(focus(8));

        memory.apply(Happening::KeymapReset {
            index: Some(0),
            names: vec!["Ukrainian".into(), "English (US)".into()],
        });

        assert_eq!(memory.fallback, 1);
        assert_eq!(memory.apps.get("Telegram").copied(), Some(0));
        assert!(!memory.apps.contains_key("Ghost"));
        assert_eq!(memory.by_window.get(&4), None);
        assert_eq!(memory.by_window[&8], 0);
        assert_eq!(memory.fallback_token, Some(LayoutToken::Name("us".into())));

        memory.apply(Happening::KeymapReset {
            index: Some(0),
            names: vec!["English (US)".into(), "Ukrainian".into()],
        });

        assert_eq!(memory.fallback, 0);
        assert_eq!(memory.apps.get("Telegram").copied(), Some(1));
        assert!(!memory.apps.contains_key("Ghost"));
    }

    #[test]
    fn keymap_reset_uses_the_default_when_the_fallback_token_is_gone() {
        let mut memory = Memory::with_layouts(1, 1, HashMap::new());
        memory.keep_tokens(Some(LayoutToken::Name("zz".into())), Vec::new());

        memory.apply(Happening::KeymapReset {
            index: Some(1),
            names: vec!["English (US)".into()],
        });

        assert_eq!(memory.fallback, DEFAULT_LAYOUT);
        assert_eq!(memory.active, 1);
    }

    #[test]
    fn keymap_reset_without_an_index_keeps_the_active_layout() {
        let mut memory = remember_english_and_ukrainian();

        memory.apply(Happening::KeymapReset {
            index: None,
            names: Vec::new(),
        });

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
            x11: None,
            transient_for: None,
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
            x11: None,
            transient_for: None,
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
            x11: None,
            transient_for: None,
        });
        memory.apply(Happening::Layout { index: 1 });
        memory.apply(Happening::Layout { index: 0 });
        memory.apply(focus(8));
        memory.apply(Happening::Layout { index: 1 });

        let action = memory.apply(Happening::Focus {
            id: 7,
            app: Some("Telegram".to_string()),
            x11: None,
            transient_for: None,
        });

        assert_eq!(action, Action::SwitchTo(0));
        assert_eq!(memory.by_window[&7], 0);
    }

    fn seen(id: i64, app: &str, x11: Option<i32>, transient_for: Option<i32>) -> Happening {
        Happening::Focus {
            id,
            app: Some(app.to_string()),
            x11,
            transient_for,
        }
    }

    #[test]
    fn x11_dialog_copies_the_parent_layout_and_skips_the_app_rule() {
        let mut memory = telegram_rules(0);
        memory.apply(seen(10, "Telegram", Some(100), None));
        memory.apply(Happening::Layout { index: 1 });
        memory.apply(Happening::Layout { index: 0 });

        let dialog = memory.apply(seen(11, "Telegram", Some(101), Some(100)));

        assert_eq!(dialog, Action::None);
        assert_eq!(memory.by_window[&11], 0);
        assert_eq!(memory.by_window[&10], 0);

        let nested = memory.apply(seen(14, "Telegram", Some(102), Some(101)));
        assert_eq!(nested, Action::None);
        assert_eq!(memory.by_window[&14], 0);

        let wayland = memory.apply(seen(12, "Telegram", None, None));
        assert_eq!(wayland, Action::SwitchTo(1));
        assert_eq!(memory.by_window[&12], 1);
    }

    #[test]
    fn reload_replaces_apps_and_does_not_clear_window_indexes() {
        let mut memory = telegram_rules(0);
        memory.watch_keyboard("kb-a".to_string());
        memory.apply(seen(4, "Telegram", Some(40), None));
        memory.apply(Happening::Layout { index: 1 });
        let windows = memory.by_window.clone();
        let focused = memory.focused;
        let names = vec!["English (US)".to_string(), "Ukrainian".to_string()];
        let from = RuleSource {
            fallback_label: "c.toml",
            apps_label: "c.toml",
        };

        memory
            .reload_rules(
                &ConfigFile {
                    fallback: Some(LayoutToken::Index(0)),
                    apps: vec![("Firefox".to_string(), LayoutToken::Index(1))],
                },
                &names,
                &from,
            )
            .unwrap();

        assert_eq!(memory.by_window, windows);
        assert_eq!(memory.focused, focused);
        assert_eq!(memory.keyboard_id(), "kb-a");
        assert_eq!(memory.fallback, 0);
        assert_eq!(memory.apps.get("Firefox").copied(), Some(1));
        assert!(!memory.apps.contains_key("Telegram"));

        let err = memory
            .reload_rules(
                &ConfigFile {
                    fallback: Some(LayoutToken::Name("zz".to_string())),
                    apps: vec![("Nope".to_string(), LayoutToken::Index(0))],
                },
                &names,
                &from,
            )
            .unwrap_err();
        assert!(err.contains("zz"), "{err}");
        assert_eq!(memory.by_window, windows);
        assert_eq!(memory.focused, focused);
        assert_eq!(memory.apps.get("Firefox").copied(), Some(1));
        assert!(!memory.apps.contains_key("Nope"));

        let opened = memory.apply(seen(9, "Firefox", None, None));
        assert_eq!(opened, Action::None);
        assert_eq!(memory.by_window[&9], 1);
        assert_eq!(memory.by_window[&4], windows[&4]);
    }
}
