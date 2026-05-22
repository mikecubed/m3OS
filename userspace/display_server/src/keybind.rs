//! Phase 72 Track D — Keybind chord engine.
//!
//! Wraps `kernel_core::input::bind_table::BindTable` with a typed
//! action map so chord triggers produce structured [`KeybindAction`]
//! values the compositor's main loop can dispatch directly. Also adds
//! a [`BindStack`] (mode-stack push/pop) so resize mode can install a
//! transient binding table over the default set without rewriting the
//! base entries.

extern crate alloc;

use alloc::vec::Vec;

use kernel_core::input::bind_table::{BindError, BindId, BindKey, BindTable};
use kernel_core::input::events::{MOD_SHIFT, MOD_SUPER};
use kernel_core::input::keymap::{
    KEY_0, KEY_1, KEY_2, KEY_3, KEY_4, KEY_5, KEY_6, KEY_7, KEY_8, KEY_9, KEY_ENTER, KEY_ESC,
    KEY_H, KEY_J, KEY_K, KEY_L, KEY_Q, KEY_R, KEY_SPACE, KEY_TAB, Keycode,
};

/// Resize-step value (px) used when a resize-mode chord fires. The
/// `[keybinds] resize_step_px` config key overrides this at load /
/// reload time.
pub const DEFAULT_RESIZE_STEP_PX: i16 = 32;

/// High-level action the compositor's main loop performs in response
/// to a matched chord. Mirrors the spec's headline keybinds:
/// `SUPER+1..9`, `SUPER+SHIFT+1..9`, `SUPER+TAB`, `SUPER+RETURN`,
/// `SUPER+Q`, `SUPER+R`, and the resize-mode `H/J/K/L`/`Escape`
/// chord set.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeybindAction {
    /// Switch to workspace N (1-based).
    SwitchWorkspace(u8),
    /// Move the focused window to workspace N (1-based).
    MoveToWorkspace(u8),
    /// Cycle focus through the current workspace.
    CycleFocus,
    /// Spawn a fresh `term` instance (Phase 72 SUPER+RETURN).
    SpawnTerm,
    /// Close the focused surface gracefully (sends WMHints close to the
    /// client — placeholder until per-client close protocol lands).
    KillFocused,
    /// Enter resize mode.
    EnterResize,
    /// Exit resize mode.
    ExitResize,
    /// Resize the focused tile by `step` along `direction`.
    ResizeFocused {
        direction: layout::ResizeDirection,
        step: i16,
    },
    /// Phase 73 — launch the desktop launcher (`/bin/launcher`).
    /// Triggered by `SUPER+SPACE`. The compositor forks an unprivileged
    /// child that opens a floating Toplevel. If a launcher process is
    /// already running, the second child exits immediately when it
    /// fails to register the singleton `"launcher"` IPC service name —
    /// so the user-visible effect is "one launcher at a time" without
    /// any focus-lookup logic in the compositor.
    LaunchLauncher,
}

/// Mode tag used by [`BindStack`]. The compositor's main loop
/// switches modes via [`BindStack::push_mode`] / [`BindStack::pop_to_default`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BindMode {
    Default,
    Resize,
}

/// One mode's binding table plus its `BindId → KeybindAction` map.
pub struct BindModeTable {
    pub mode: BindMode,
    pub table: BindTable,
    pub actions: Vec<(BindId, KeybindAction)>,
}

impl BindModeTable {
    pub fn new(mode: BindMode) -> Self {
        Self {
            mode,
            table: BindTable::new(),
            actions: Vec::new(),
        }
    }

    /// Register a chord and remember its action. Returns the
    /// `BindId` so callers can unregister later. Idempotent: if the
    /// same `(mask, keycode)` is registered twice, the new action
    /// replaces the old.
    pub fn register(
        &mut self,
        mask: u16,
        keycode: Keycode,
        action: KeybindAction,
    ) -> Result<BindId, BindError> {
        let id = self.table.register(BindKey {
            modifier_mask: mask,
            keycode: keycode.0 as u32,
        })?;
        if let Some(slot) = self.actions.iter_mut().find(|(b, _)| *b == id) {
            slot.1 = action;
        } else {
            self.actions.push((id, action));
        }
        Ok(id)
    }

    /// Look up the action registered against `id`. `None` if the bind
    /// was unregistered or never existed.
    pub fn action(&self, id: BindId) -> Option<KeybindAction> {
        self.actions
            .iter()
            .find_map(|(b, a)| if *b == id { Some(*a) } else { None })
    }
}

/// Stack of [`BindModeTable`]s. The top of the stack is the active
/// table. The bottom is always the default mode; `pop_to_default`
/// drains the rest. Resize mode pushes a fresh table; `Escape` /
/// `SUPER+R` toggles pop it.
pub struct BindStack {
    stack: Vec<BindModeTable>,
    /// Pixel step used when pushing the resize mode. Defaults to
    /// [`DEFAULT_RESIZE_STEP_PX`]; the `[keybinds] resize_step_px`
    /// config key overrides this via [`BindStack::set_resize_step_px`]
    /// at startup and on `m3ctl reload`.
    resize_step_px: i16,
}

impl Default for BindStack {
    fn default() -> Self {
        Self::new()
    }
}

impl BindStack {
    /// Construct a fresh stack with the default chord set already
    /// installed at the bottom.
    pub fn new() -> Self {
        let mut stack = Vec::new();
        let mut default = BindModeTable::new(BindMode::Default);
        register_default_chords(&mut default).expect("default chord set fits in BindTable");
        stack.push(default);
        Self {
            stack,
            resize_step_px: DEFAULT_RESIZE_STEP_PX,
        }
    }

    /// Override the resize-mode step (pixels). Subsequent
    /// `push_mode(BindMode::Resize)` calls use this value when
    /// registering the H/J/K/L chords. Called once at startup from
    /// the parsed `[keybinds] resize_step_px` and again on
    /// `m3ctl reload`.
    pub fn set_resize_step_px(&mut self, step: i16) {
        self.resize_step_px = step;
    }

    /// Currently-active resize step. Useful for tests and debug
    /// surfaces.
    pub fn resize_step_px(&self) -> i16 {
        self.resize_step_px
    }

    /// Borrow the active mode's table.
    pub fn active_table(&self) -> &BindTable {
        &self.stack.last().expect("BindStack never empty").table
    }

    /// Borrow the active mode's table mutably.
    pub fn active_table_mut(&mut self) -> &mut BindTable {
        &mut self.stack.last_mut().expect("BindStack never empty").table
    }

    /// Active mode tag.
    pub fn active_mode(&self) -> BindMode {
        self.stack.last().expect("BindStack never empty").mode
    }

    /// Look up the action for a `BindId` matched by the active table.
    pub fn lookup_action(&self, id: BindId) -> Option<KeybindAction> {
        self.stack.last()?.action(id)
    }

    /// Phase 72 — look up the action by raw u32 id (the dispatcher
    /// surfaces `BindTriggered { id }` as a raw u32 since `BindId`
    /// has no public constructor). Searches the active table only.
    pub fn lookup_action_raw(&self, raw_id: u32) -> Option<KeybindAction> {
        let mode = self.stack.last()?;
        mode.actions
            .iter()
            .find_map(|(b, a)| if b.raw() == raw_id { Some(*a) } else { None })
    }

    /// Push a fresh mode on top of the stack. The compositor calls
    /// this from the `EnterResize` action handler. Resize-mode tables
    /// are registered with the currently-configured
    /// [`BindStack::resize_step_px`], so chord overlays pick up the
    /// `[keybinds] resize_step_px` config override.
    pub fn push_mode(&mut self, mode: BindMode) {
        let mut table = BindModeTable::new(mode);
        if mode == BindMode::Resize {
            register_resize_chords(&mut table, self.resize_step_px).expect("resize chord set fits");
        }
        self.stack.push(table);
    }

    /// Pop the topmost mode. No-op when only the default mode remains.
    pub fn pop_mode(&mut self) {
        if self.stack.len() > 1 {
            self.stack.pop();
        }
    }

    /// Drain the stack back to default mode (single entry).
    pub fn pop_to_default(&mut self) {
        while self.stack.len() > 1 {
            self.stack.pop();
        }
    }

    /// True iff a non-default mode is active (e.g. resize). Resize
    /// mode suppresses text input to clients per the spec — the
    /// dispatcher consults this to gate forwarding.
    pub fn non_default_active(&self) -> bool {
        self.stack.len() > 1
    }

    /// Phase 72 Track D.3 — reload the default-mode bindings from a
    /// parsed config. The new chord set replaces the entire default
    /// table; transient mode-stack entries (resize mode) survive.
    pub fn reload_default(&mut self, entries: &[(u16, Keycode, KeybindAction)]) {
        let mut new_default = BindModeTable::new(BindMode::Default);
        // Always start with the built-in defaults so a config that
        // omits a chord (e.g. `SUPER+1`) still has a working binding.
        // Then layer the user-supplied entries on top — later
        // `register` calls for the same `(mask, keycode)` pair update
        // the action via the same BindId.
        let _ = register_default_chords(&mut new_default);
        for (mask, keycode, action) in entries {
            let _ = new_default.register(*mask, *keycode, *action);
        }
        // Replace the bottom of the stack.
        if self.stack.is_empty() {
            self.stack.push(new_default);
        } else {
            self.stack[0] = new_default;
        }
    }
}

/// Install the canonical Phase 72 chord set on a fresh default-mode
/// table. Called once at startup; reloads route through
/// [`BindStack::reload_default`].
pub fn register_default_chords(table: &mut BindModeTable) -> Result<(), BindError> {
    // SUPER+1..9 → switch-workspace.
    for (idx, key) in [
        KEY_1, KEY_2, KEY_3, KEY_4, KEY_5, KEY_6, KEY_7, KEY_8, KEY_9,
    ]
    .iter()
    .enumerate()
    {
        table.register(
            MOD_SUPER,
            *key,
            KeybindAction::SwitchWorkspace((idx + 1) as u8),
        )?;
        table.register(
            MOD_SUPER | MOD_SHIFT,
            *key,
            KeybindAction::MoveToWorkspace((idx + 1) as u8),
        )?;
    }
    // SUPER+0 is a documented Hyprland habit: jump to workspace 10 or
    // toggle to the last workspace; we map it to workspace 1 as a
    // safe default since we only ship 9 slots.
    table.register(MOD_SUPER, KEY_0, KeybindAction::SwitchWorkspace(1))?;
    table.register(MOD_SUPER, KEY_TAB, KeybindAction::CycleFocus)?;
    table.register(MOD_SUPER, KEY_ENTER, KeybindAction::SpawnTerm)?;
    table.register(MOD_SUPER, KEY_Q, KeybindAction::KillFocused)?;
    table.register(MOD_SUPER, KEY_R, KeybindAction::EnterResize)?;
    // Phase 73 — SUPER+SPACE opens the launcher.
    table.register(MOD_SUPER, KEY_SPACE, KeybindAction::LaunchLauncher)?;
    Ok(())
}

/// Install the Phase 72 resize-mode chord set on a fresh table.
/// `H/J/K/L` adjust the focused tile by `step_px`; `Escape` exits.
pub fn register_resize_chords(table: &mut BindModeTable, step_px: i16) -> Result<(), BindError> {
    use layout::ResizeDirection;
    table.register(
        0,
        KEY_H,
        KeybindAction::ResizeFocused {
            direction: ResizeDirection::Left,
            step: step_px,
        },
    )?;
    table.register(
        0,
        KEY_J,
        KeybindAction::ResizeFocused {
            direction: ResizeDirection::Down,
            step: step_px,
        },
    )?;
    table.register(
        0,
        KEY_K,
        KeybindAction::ResizeFocused {
            direction: ResizeDirection::Up,
            step: step_px,
        },
    )?;
    table.register(
        0,
        KEY_L,
        KeybindAction::ResizeFocused {
            direction: ResizeDirection::Right,
            step: step_px,
        },
    )?;
    table.register(0, KEY_ESC, KeybindAction::ExitResize)?;
    // SUPER+R also exits so users can press the same combo twice to
    // toggle out.
    table.register(MOD_SUPER, KEY_R, KeybindAction::ExitResize)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_stack_includes_super_1_through_9() {
        let stack = BindStack::new();
        let table = &stack.stack[0];
        for (idx, kc) in [
            KEY_1, KEY_2, KEY_3, KEY_4, KEY_5, KEY_6, KEY_7, KEY_8, KEY_9,
        ]
        .iter()
        .enumerate()
        {
            let bid = table
                .table
                .match_bind(MOD_SUPER, kc.0 as u32)
                .expect("super+digit bound");
            assert_eq!(
                table.action(bid),
                Some(KeybindAction::SwitchWorkspace((idx + 1) as u8))
            );
        }
    }

    #[test]
    fn push_resize_mode_overlays_table() {
        let mut stack = BindStack::new();
        // No `H` binding in default mode.
        assert!(stack.active_table().match_bind(0, KEY_H.0 as u32).is_none());
        stack.push_mode(BindMode::Resize);
        let bid = stack
            .active_table()
            .match_bind(0, KEY_H.0 as u32)
            .expect("H bound in resize mode");
        assert!(matches!(
            stack.lookup_action(bid),
            Some(KeybindAction::ResizeFocused {
                direction: layout::ResizeDirection::Left,
                ..
            })
        ));
        stack.pop_mode();
        assert!(stack.active_table().match_bind(0, KEY_H.0 as u32).is_none());
    }

    #[test]
    fn pop_to_default_idempotent() {
        let mut stack = BindStack::new();
        stack.pop_to_default();
        stack.pop_to_default();
        assert_eq!(stack.active_mode(), BindMode::Default);
    }

    #[test]
    fn configured_resize_step_propagates_to_resize_mode() {
        // Phase 72 review fix — `[keybinds] resize_step_px = N` must
        // reach the H/J/K/L chord actions, not get silently ignored.
        let mut stack = BindStack::new();
        stack.set_resize_step_px(64);
        stack.push_mode(BindMode::Resize);
        let bid = stack
            .active_table()
            .match_bind(0, KEY_H.0 as u32)
            .expect("H bound in resize mode");
        match stack.lookup_action(bid) {
            Some(KeybindAction::ResizeFocused { step, .. }) => {
                assert_eq!(step, 64, "configured step reaches the chord");
            }
            other => panic!("expected ResizeFocused, got {:?}", other),
        }
    }

    #[test]
    fn non_default_active_reflects_resize_mode() {
        let mut stack = BindStack::new();
        assert!(!stack.non_default_active());
        stack.push_mode(BindMode::Resize);
        assert!(stack.non_default_active());
        stack.pop_to_default();
        assert!(!stack.non_default_active());
    }
}
