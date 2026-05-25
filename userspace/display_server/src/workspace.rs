//! Phase 72 Track C — Workspace state machine.
//!
//! Per-output set of N numbered workspaces (default 9). Each workspace
//! holds its own window list (a `Vec<SurfaceId>` keeping insertion
//! order) and an active [`PolicyKind`] selection. Operations:
//!
//! * `switch_workspace(n)` — activate workspace N, triggering a full
//!   damage redraw on the next compose pass.
//! * `move_to_workspace(surface, n)` — detach from the current
//!   workspace and append to workspace N.
//! * `set_layout(kind)` — replace the active workspace's layout.
//!
//! The state machine is pure logic — no IPC, no framebuffer access —
//! so the host-side unit tests in this module can exercise every
//! transition without QEMU.

extern crate alloc;

use alloc::vec::Vec;

use kernel_core::display::layout::{LayoutPolicy, LayoutSurface, OutputGeometry, usable_rect};
use kernel_core::display::protocol::{Rect, SurfaceId};
use layout::{
    DwindleLayout, FullscreenLayout, GapConfig, GridLayout, LayoutError, MasterStackLayout,
    PolicyKind, ResizeDirection, TabbedLayout, TiledLayoutPolicy, TiledWindow, apply_outer_gaps,
};

/// Number of workspaces per output. Matches the spec ("nine numbered
/// workspaces") and the `SUPER+1..9` keybind allocation.
pub const NUM_WORKSPACES: usize = 9;

/// Per-workspace policy storage. Each workspace remembers its own
/// active policy so workspace 1 can be master-stack for coding while
/// workspace 9 is fullscreen for DOOM. The full per-kind state for
/// each policy (e.g. dwindle's split ratios, tabbed's focused id) is
/// owned by [`PolicySet`] so layout-specific bookkeeping survives a
/// workspace round trip.
#[derive(Clone, Debug)]
pub struct Workspace {
    /// Window ids in insertion order. The active tiling policy reads
    /// this slice on every compose pass.
    windows: Vec<SurfaceId>,
    /// Active policy tag. The matching state inside [`policies`] is
    /// the one consulted at compose time.
    policy: PolicyKind,
    /// Per-policy state. Each variant accumulates its own bookkeeping
    /// across calls (split ratios for dwindle, master ratio for
    /// master-stack, focused id for tabbed/fullscreen) so toggling
    /// between layouts keeps the user-adjusted ratios.
    policies: PolicySet,
}

impl Workspace {
    /// Construct an empty workspace at the given default policy.
    pub fn new(policy: PolicyKind) -> Self {
        Self {
            windows: Vec::new(),
            policy,
            policies: PolicySet::new(),
        }
    }

    /// Currently-tiled window ids in insertion order.
    pub fn windows(&self) -> &[SurfaceId] {
        &self.windows
    }

    /// Active layout policy tag.
    pub fn policy(&self) -> PolicyKind {
        self.policy
    }

    /// Replace the active layout policy.
    pub fn set_policy(&mut self, policy: PolicyKind) {
        self.policy = policy;
    }

    /// Insert a window if it is not already present. Returns `true`
    /// if the window was newly inserted. Notifies the active layout
    /// policy via `TiledLayoutPolicy::on_window_added` so policies with
    /// persistent structure (e.g. `DwindleLayout`'s split tree) stay
    /// consistent with the window set.
    pub fn insert(&mut self, id: SurfaceId) -> bool {
        if self.windows.contains(&id) {
            return false;
        }
        self.windows.push(id);
        self.policies.on_window_added(
            self.policy,
            TiledWindow {
                id,
                preferred_size: (0, 0),
            },
        );
        true
    }

    /// Remove a window. Returns `true` if removed, `false` if the
    /// window was not in this workspace. Notifies the active layout
    /// policy via `TiledLayoutPolicy::on_window_removed`; in
    /// particular, `DwindleLayout` pops its last split ratio so
    /// resize-mode adjustments never target a stale ratio that no
    /// longer maps to a tile.
    pub fn remove(&mut self, id: SurfaceId) -> bool {
        if let Some(idx) = self.windows.iter().position(|w| *w == id) {
            self.windows.remove(idx);
            self.policies.on_window_removed(self.policy, id);
            true
        } else {
            false
        }
    }

    /// Number of windows currently on this workspace.
    pub fn len(&self) -> usize {
        self.windows.len()
    }

    /// True iff this workspace contains zero windows.
    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    /// Run the active tiling policy and return per-window rectangles.
    /// `output` is the post-outer-gaps tiling area; `gaps.inner` is
    /// applied between adjacent tiles.
    pub fn tile(&mut self, output: Rect, gaps: GapConfig) -> Vec<(SurfaceId, Rect)> {
        let windows: Vec<TiledWindow> = self
            .windows
            .iter()
            .map(|id| TiledWindow {
                id: *id,
                preferred_size: (0, 0),
            })
            .collect();
        self.policies.tile(self.policy, &windows, output, gaps)
    }

    /// Adjust the focused tile along `direction` by `step` pixels via
    /// the active policy. Returns `Err(LayoutError::Unsupported)` for
    /// policies that don't support resize.
    pub fn adjust_focused(
        &mut self,
        focused: SurfaceId,
        direction: ResizeDirection,
        step: i16,
    ) -> Result<(), LayoutError> {
        self.policies
            .adjust_focused(self.policy, focused, direction, step)
    }

    /// Forward focus changes to the active policy so e.g.
    /// `TabbedLayout` updates its `focused` slot.
    pub fn on_focus_changed(&mut self, id: Option<SurfaceId>) {
        self.policies.on_focus_changed(self.policy, id);
    }

    /// Phase 72 — set the `MasterStackLayout`'s `master_ratio` for
    /// this workspace. No-op when a different policy is active (the
    /// underlying state is preserved for a later toggle back to
    /// master/stack).
    pub fn set_master_ratio(&mut self, ratio: f32) {
        self.policies.master_stack_mut().set_master_ratio(ratio);
    }
}

/// All six per-kind layout-policy states bundled into one struct so a
/// workspace can toggle between layouts without forgetting earlier
/// per-policy bookkeeping.
#[derive(Clone, Debug, Default)]
pub struct PolicySet {
    master_stack: MasterStackLayout,
    dwindle: DwindleLayout,
    spiral: DwindleLayout,
    grid: GridLayout,
    tabbed: TabbedLayout,
    fullscreen: FullscreenLayout,
}

impl PolicySet {
    pub fn new() -> Self {
        Self {
            master_stack: MasterStackLayout::new(),
            dwindle: DwindleLayout::new(),
            spiral: DwindleLayout::spiral(),
            grid: GridLayout::new(),
            tabbed: TabbedLayout::new(),
            fullscreen: FullscreenLayout::new(),
        }
    }

    /// Run the policy matching `kind` against `windows` / `output` /
    /// `gaps` and return the per-window rectangles.
    pub fn tile(
        &mut self,
        kind: PolicyKind,
        windows: &[TiledWindow],
        output: Rect,
        gaps: GapConfig,
    ) -> Vec<(SurfaceId, Rect)> {
        match kind {
            PolicyKind::MasterStack => self.master_stack.tile(windows, output, gaps),
            PolicyKind::Dwindle => self.dwindle.tile(windows, output, gaps),
            PolicyKind::Spiral => self.spiral.tile(windows, output, gaps),
            PolicyKind::Grid => self.grid.tile(windows, output, gaps),
            PolicyKind::Tabbed => self.tabbed.tile(windows, output, gaps),
            PolicyKind::Fullscreen => self.fullscreen.tile(windows, output, gaps),
        }
    }

    pub fn adjust_focused(
        &mut self,
        kind: PolicyKind,
        focused: SurfaceId,
        direction: ResizeDirection,
        step: i16,
    ) -> Result<(), LayoutError> {
        match kind {
            PolicyKind::MasterStack => self.master_stack.adjust_focused(focused, direction, step),
            PolicyKind::Dwindle => self.dwindle.adjust_focused(focused, direction, step),
            PolicyKind::Spiral => self.spiral.adjust_focused(focused, direction, step),
            PolicyKind::Grid | PolicyKind::Tabbed | PolicyKind::Fullscreen => {
                Err(LayoutError::Unsupported)
            }
        }
    }

    pub fn on_focus_changed(&mut self, kind: PolicyKind, id: Option<SurfaceId>) {
        match kind {
            PolicyKind::MasterStack => {
                TiledLayoutPolicy::on_focus_changed(&mut self.master_stack, id)
            }
            PolicyKind::Dwindle => TiledLayoutPolicy::on_focus_changed(&mut self.dwindle, id),
            PolicyKind::Spiral => TiledLayoutPolicy::on_focus_changed(&mut self.spiral, id),
            PolicyKind::Grid => TiledLayoutPolicy::on_focus_changed(&mut self.grid, id),
            PolicyKind::Tabbed => TiledLayoutPolicy::on_focus_changed(&mut self.tabbed, id),
            PolicyKind::Fullscreen => TiledLayoutPolicy::on_focus_changed(&mut self.fullscreen, id),
        }
    }

    /// Notify the active policy that a window joined the tiling set.
    /// Default trait impl is a no-op for policies without persistent
    /// per-window state; `DwindleLayout` / `MasterStackLayout` opt in.
    pub fn on_window_added(&mut self, kind: PolicyKind, window: TiledWindow) {
        match kind {
            PolicyKind::MasterStack => {
                TiledLayoutPolicy::on_window_added(&mut self.master_stack, window)
            }
            PolicyKind::Dwindle => TiledLayoutPolicy::on_window_added(&mut self.dwindle, window),
            PolicyKind::Spiral => TiledLayoutPolicy::on_window_added(&mut self.spiral, window),
            PolicyKind::Grid => TiledLayoutPolicy::on_window_added(&mut self.grid, window),
            PolicyKind::Tabbed => TiledLayoutPolicy::on_window_added(&mut self.tabbed, window),
            PolicyKind::Fullscreen => {
                TiledLayoutPolicy::on_window_added(&mut self.fullscreen, window)
            }
        }
    }

    /// Notify the active policy that a window left the tiling set.
    /// `DwindleLayout` pops its last split ratio so resize-mode does
    /// not adjust a stale split that no longer corresponds to the
    /// current window set.
    pub fn on_window_removed(&mut self, kind: PolicyKind, id: SurfaceId) {
        match kind {
            PolicyKind::MasterStack => {
                TiledLayoutPolicy::on_window_removed(&mut self.master_stack, id)
            }
            PolicyKind::Dwindle => TiledLayoutPolicy::on_window_removed(&mut self.dwindle, id),
            PolicyKind::Spiral => TiledLayoutPolicy::on_window_removed(&mut self.spiral, id),
            PolicyKind::Grid => TiledLayoutPolicy::on_window_removed(&mut self.grid, id),
            PolicyKind::Tabbed => TiledLayoutPolicy::on_window_removed(&mut self.tabbed, id),
            PolicyKind::Fullscreen => {
                TiledLayoutPolicy::on_window_removed(&mut self.fullscreen, id)
            }
        }
    }

    /// Reach the live `MasterStackLayout` so the compositor can change
    /// the master ratio via `m3ctl tile set-master-ratio <f>`.
    pub fn master_stack_mut(&mut self) -> &mut MasterStackLayout {
        &mut self.master_stack
    }
}

/// Errors returned by workspace operations.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WorkspaceError {
    /// Workspace index outside `[0, NUM_WORKSPACES)`.
    OutOfRange,
    /// Surface id is not present on any workspace.
    UnknownSurface,
    /// Surface id is already in the requested workspace.
    NoOp,
}

/// Outcome of a `switch_workspace` / `move_to_workspace` call.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct WorkspaceTransition {
    /// `true` if the active workspace changed.
    pub switched: bool,
    /// `true` if the registered window set on the new active workspace
    /// differs from the previous one (compose needs a full repaint).
    pub redraw_required: bool,
}

/// Per-output workspace manager. Owns the slot-array of
/// [`Workspace`]s plus the active-slot index.
#[derive(Clone, Debug)]
pub struct WorkspaceManager {
    workspaces: [Workspace; NUM_WORKSPACES],
    current: usize,
}

impl WorkspaceManager {
    /// Construct a manager with all 9 workspaces empty and active = 0.
    /// Every workspace defaults to `default_policy`.
    pub fn new(default_policy: PolicyKind) -> Self {
        Self {
            workspaces: core::array::from_fn(|_| Workspace::new(default_policy)),
            current: 0,
        }
    }

    /// Active workspace index (`0..NUM_WORKSPACES`).
    pub fn current_index(&self) -> usize {
        self.current
    }

    /// 1-based workspace number as exposed by the keybind and `m3ctl`
    /// (`SUPER+1` = `current_number() == 1`).
    pub fn current_number(&self) -> u8 {
        (self.current + 1) as u8
    }

    /// Borrow the active workspace.
    pub fn current(&self) -> &Workspace {
        &self.workspaces[self.current]
    }

    /// Mutably borrow the active workspace.
    pub fn current_mut(&mut self) -> &mut Workspace {
        &mut self.workspaces[self.current]
    }

    /// Borrow workspace by 0-based index.
    pub fn workspace(&self, idx: usize) -> Option<&Workspace> {
        self.workspaces.get(idx)
    }

    /// Borrow mutably by 0-based index.
    pub fn workspace_mut(&mut self, idx: usize) -> Option<&mut Workspace> {
        self.workspaces.get_mut(idx)
    }

    /// Activate the workspace with 0-based index `idx`. Returns
    /// `Err(OutOfRange)` for `idx >= NUM_WORKSPACES`; this is the
    /// graceful rejection the spec calls out ("an attempt to open a
    /// tenth workspace on a nine-slot system is rejected gracefully").
    pub fn switch_workspace(&mut self, idx: usize) -> Result<WorkspaceTransition, WorkspaceError> {
        if idx >= NUM_WORKSPACES {
            return Err(WorkspaceError::OutOfRange);
        }
        if idx == self.current {
            return Ok(WorkspaceTransition::default());
        }
        let from_set: Vec<SurfaceId> = self.workspaces[self.current].windows.clone();
        let to_set: Vec<SurfaceId> = self.workspaces[idx].windows.clone();
        self.current = idx;
        Ok(WorkspaceTransition {
            switched: true,
            redraw_required: from_set != to_set,
        })
    }

    /// Place a freshly-created surface onto the active workspace.
    /// Returns `true` if newly inserted (false on duplicate).
    pub fn insert_on_current(&mut self, id: SurfaceId) -> bool {
        self.workspaces[self.current].insert(id)
    }

    /// Remove a surface from whichever workspace contains it. Returns
    /// `Ok(idx)` of the workspace it was removed from, or
    /// `Err(UnknownSurface)` if no workspace holds it.
    pub fn remove_surface(&mut self, id: SurfaceId) -> Result<usize, WorkspaceError> {
        for (idx, ws) in self.workspaces.iter_mut().enumerate() {
            if ws.remove(id) {
                return Ok(idx);
            }
        }
        Err(WorkspaceError::UnknownSurface)
    }

    /// Move surface `id` to workspace `target`. Detaches from the
    /// source workspace's list and appends to target. Returns
    /// `Err(NoOp)` if already on target. If `follow` is `true`, the
    /// active workspace switches to `target` after the move.
    pub fn move_to_workspace(
        &mut self,
        id: SurfaceId,
        target: usize,
        follow: bool,
    ) -> Result<WorkspaceTransition, WorkspaceError> {
        if target >= NUM_WORKSPACES {
            return Err(WorkspaceError::OutOfRange);
        }
        let source = self
            .workspaces
            .iter()
            .position(|ws| ws.windows.contains(&id))
            .ok_or(WorkspaceError::UnknownSurface)?;
        if source == target {
            return Err(WorkspaceError::NoOp);
        }
        let _ = self.workspaces[source].remove(id);
        self.workspaces[target].insert(id);
        // If the source was active, the active window list changed → redraw.
        let active_changed = source == self.current || target == self.current;
        let mut transition = WorkspaceTransition {
            switched: false,
            redraw_required: active_changed,
        };
        if follow {
            let from = self.current;
            self.current = target;
            transition.switched = from != target;
            transition.redraw_required = transition.redraw_required || transition.switched;
        }
        Ok(transition)
    }

    /// Replace the active workspace's policy. Returns the previous
    /// policy so the caller can decide whether to log the change.
    pub fn set_current_layout(&mut self, policy: PolicyKind) -> PolicyKind {
        let old = self.workspaces[self.current].policy;
        self.workspaces[self.current].policy = policy;
        old
    }

    /// Iterate `(idx, workspace)` over all workspaces.
    pub fn iter(&self) -> impl Iterator<Item = (usize, &Workspace)> {
        self.workspaces.iter().enumerate()
    }

    /// Compute the active workspace's tile arrangement under the
    /// given gap config. `output` is the raw output rect; outer gaps
    /// are applied internally before delegating to the active
    /// `TiledLayoutPolicy`. Returns the per-window rectangles ready
    /// for the compose loop's blit phase.
    pub fn arrange_current(&mut self, output: Rect, gaps: GapConfig) -> Vec<(SurfaceId, Rect)> {
        self.arrange_current_with_exclusive(output, gaps, &[])
    }

    /// Phase 73 — same as [`arrange_current`], but subtracts full-edge
    /// `exclusive_zones` (e.g. the status-bar's reserved 48px strip at
    /// the top of the output) from the output rect *before* applying
    /// outer gaps, so toplevels never overlap docked Layer-shell
    /// surfaces. Zones that don't tile a full edge are ignored — the
    /// compositor caller already filters those upstream.
    pub fn arrange_current_with_exclusive(
        &mut self,
        output: Rect,
        gaps: GapConfig,
        exclusive_zones: &[Rect],
    ) -> Vec<(SurfaceId, Rect)> {
        self.arrange_workspace_with_exclusive(self.current, output, gaps, exclusive_zones)
    }

    /// Arrange an *arbitrary* workspace, not just the active one. Used
    /// by the workspace-slide path: during a slide the compositor
    /// renders both the outgoing and incoming workspaces' tiles, with
    /// per-workspace x-offsets driven by the slide's progress. The
    /// outgoing workspace's surfaces stay alive in the registry and
    /// their tiles need to be computed against this output's geometry
    /// so they can be offset off-screen.
    ///
    /// Returns an empty arrangement when `idx` is out of range — same
    /// graceful-rejection contract as [`switch_workspace`].
    pub fn arrange_workspace_with_exclusive(
        &mut self,
        idx: usize,
        output: Rect,
        gaps: GapConfig,
        exclusive_zones: &[Rect],
    ) -> Vec<(SurfaceId, Rect)> {
        if idx >= NUM_WORKSPACES {
            return Vec::new();
        }
        let usable = usable_rect(output, exclusive_zones);
        let inner = apply_outer_gaps(usable, gaps.outer);
        self.workspaces[idx].tile(inner, gaps)
    }

    /// Cycle keyboard focus through the active workspace's window
    /// list, picking the entry *after* `current_focus`. Returns the
    /// new focused id, or `None` if the workspace is empty.
    pub fn next_focus(&self, current_focus: Option<SurfaceId>) -> Option<SurfaceId> {
        let ws = &self.workspaces[self.current];
        if ws.windows.is_empty() {
            return None;
        }
        let start = current_focus
            .and_then(|c| ws.windows.iter().position(|id| *id == c))
            .map(|i| (i + 1) % ws.windows.len())
            .unwrap_or(0);
        Some(ws.windows[start])
    }
}

/// Per-frame workspace-slide context the compose pass feeds into
/// [`WorkspaceLayoutAdapter`] so the adapter can merge the outgoing
/// and incoming workspaces' tile arrangements into a single offset-
/// aware result. `None` between slides — the adapter then arranges
/// just the active workspace, byte-for-byte the same as Phase 72.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlideContext {
    /// Index of the workspace sliding *out*.
    pub from_ws: usize,
    /// X-offset applied to every tile of the outgoing workspace.
    pub from_offset_x: i32,
    /// X-offset applied to every tile of the incoming (active)
    /// workspace.
    pub to_offset_x: i32,
}

/// A [`LayoutPolicy`] adapter that delegates `arrange` to the
/// currently-active workspace's tiling policy. Lets the existing
/// Phase 56 `run_compose` flow stay byte-for-byte unchanged while the
/// new tiling logic plugs in at the trait boundary. Consumers borrow
/// a `WorkspaceManager` and a `GapConfig`; `arrange` then routes to
/// the right per-policy state.
///
/// During a workspace transition the optional [`SlideContext`] makes
/// the adapter return the merged offset arrangement: tiles for the
/// outgoing workspace shifted by `from_offset_x`, tiles for the
/// active workspace shifted by `to_offset_x`. The compose pass then
/// blits both sets at the offset positions and the user sees a real
/// slide instead of an instant snap.
pub struct WorkspaceLayoutAdapter<'a> {
    pub manager: &'a mut WorkspaceManager,
    pub gaps: GapConfig,
    pub slide: Option<SlideContext>,
}

impl<'a> LayoutPolicy for WorkspaceLayoutAdapter<'a> {
    fn arrange(
        &mut self,
        _toplevels: &[LayoutSurface],
        output: OutputGeometry,
        exclusive_zones: &[Rect],
    ) -> Vec<(SurfaceId, Rect)> {
        // The compose loop filters `iter_compose` to the union of
        // active and outgoing workspace surfaces before calling
        // `arrange`, but the legacy trait passes `toplevels` to honour
        // the call shape. We ignore the parameter and consult
        // `manager` directly — that is the source of truth for which
        // windows tile under which policy. `exclusive_zones` carries
        // the full-edge reservations declared by Layer-shell clients
        // (Phase 73 status bar / dock / panel) so toplevels never
        // overlap them.
        let active =
            self.manager
                .arrange_current_with_exclusive(output.rect, self.gaps, exclusive_zones);
        let Some(slide) = self.slide else {
            return active;
        };
        // Slide in flight — also arrange the outgoing workspace and
        // apply each side's offset. Both arrangements use the same
        // output rect and exclusive zones so the two sliding panels
        // are geometrically consistent (a surface that ends up under
        // the bar in workspace A is under the bar in workspace B's
        // arrangement too).
        let outgoing = self.manager.arrange_workspace_with_exclusive(
            slide.from_ws,
            output.rect,
            self.gaps,
            exclusive_zones,
        );
        let mut merged: Vec<(SurfaceId, Rect)> = Vec::with_capacity(active.len() + outgoing.len());
        for (sid, r) in outgoing {
            merged.push((sid, translate_rect_x(r, slide.from_offset_x)));
        }
        for (sid, r) in active {
            merged.push((sid, translate_rect_x(r, slide.to_offset_x)));
        }
        merged
    }
}

fn translate_rect_x(r: Rect, dx: i32) -> Rect {
    Rect {
        x: r.x.saturating_add(dx),
        y: r.y,
        w: r.w,
        h: r.h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_manager_starts_on_workspace_zero() {
        let m = WorkspaceManager::new(PolicyKind::Dwindle);
        assert_eq!(m.current_index(), 0);
        assert_eq!(m.current_number(), 1);
        assert_eq!(m.current().policy(), PolicyKind::Dwindle);
    }

    #[test]
    fn insert_and_remove_track_windows() {
        let mut m = WorkspaceManager::new(PolicyKind::Dwindle);
        assert!(m.insert_on_current(SurfaceId(1)));
        assert!(m.insert_on_current(SurfaceId(2)));
        assert_eq!(m.current().windows(), &[SurfaceId(1), SurfaceId(2)]);
        assert!(!m.insert_on_current(SurfaceId(1))); // dup → no insert
        assert_eq!(m.remove_surface(SurfaceId(1)).unwrap(), 0);
        assert_eq!(m.current().windows(), &[SurfaceId(2)]);
    }

    #[test]
    fn switch_to_invalid_workspace_rejected_gracefully() {
        let mut m = WorkspaceManager::new(PolicyKind::Dwindle);
        assert_eq!(
            m.switch_workspace(NUM_WORKSPACES),
            Err(WorkspaceError::OutOfRange)
        );
        // Still on workspace 0.
        assert_eq!(m.current_index(), 0);
    }

    #[test]
    fn switch_workspace_changes_active_and_signals_redraw() {
        let mut m = WorkspaceManager::new(PolicyKind::Dwindle);
        m.insert_on_current(SurfaceId(1));
        let transition = m.switch_workspace(1).unwrap();
        assert!(transition.switched);
        assert!(transition.redraw_required);
        assert_eq!(m.current_index(), 1);
        // Workspace 1 has no windows; switching back should also require redraw.
        let transition = m.switch_workspace(0).unwrap();
        assert!(transition.switched);
        assert!(transition.redraw_required);
    }

    #[test]
    fn switch_to_same_workspace_is_a_noop_transition() {
        let mut m = WorkspaceManager::new(PolicyKind::Dwindle);
        let transition = m.switch_workspace(0).unwrap();
        assert!(!transition.switched);
        assert!(!transition.redraw_required);
    }

    #[test]
    fn move_to_workspace_relocates() {
        let mut m = WorkspaceManager::new(PolicyKind::Dwindle);
        m.insert_on_current(SurfaceId(1));
        m.insert_on_current(SurfaceId(2));
        let transition = m.move_to_workspace(SurfaceId(2), 3, false).unwrap();
        assert!(!transition.switched);
        assert!(transition.redraw_required);
        assert_eq!(m.workspace(0).unwrap().windows(), &[SurfaceId(1)]);
        assert_eq!(m.workspace(3).unwrap().windows(), &[SurfaceId(2)]);
    }

    #[test]
    fn move_to_workspace_with_follow_switches() {
        let mut m = WorkspaceManager::new(PolicyKind::Dwindle);
        m.insert_on_current(SurfaceId(1));
        let transition = m.move_to_workspace(SurfaceId(1), 2, true).unwrap();
        assert!(transition.switched);
        assert!(transition.redraw_required);
        assert_eq!(m.current_index(), 2);
        assert_eq!(m.current().windows(), &[SurfaceId(1)]);
    }

    #[test]
    fn move_to_same_workspace_is_noop() {
        let mut m = WorkspaceManager::new(PolicyKind::Dwindle);
        m.insert_on_current(SurfaceId(1));
        assert_eq!(
            m.move_to_workspace(SurfaceId(1), 0, false),
            Err(WorkspaceError::NoOp)
        );
    }

    #[test]
    fn dwindle_remove_pops_split_ratio() {
        // Phase 72 review fix — `Workspace::remove` must notify the
        // active policy so DwindleLayout's persistent split-ratio
        // stack stays in sync with the window set. Resize-mode
        // `adjust_focused` writes the last ratio; if removal never
        // pops, a stale ratio gets nudged and the next tile pass
        // re-introduces it as a phantom split.
        let mut ws = Workspace::new(PolicyKind::Dwindle);
        ws.insert(SurfaceId(1));
        ws.insert(SurfaceId(2));
        ws.insert(SurfaceId(3));
        // Drive a tile pass so the dwindle layout materializes its
        // two split ratios (n=3 → 2 ratios).
        let out = Rect {
            x: 0,
            y: 0,
            w: 800,
            h: 600,
        };
        let _ = ws.tile(out, GapConfig::new(0, 0));
        // Remove a window and adjust the focused tile. If the policy
        // never saw the removal, it still carries the third ratio and
        // adjust_focused targets it instead of the surviving split.
        assert!(ws.remove(SurfaceId(3)));
        // adjust_focused operates on the policy's current ratio
        // stack; with no stale entry, this should not panic and should
        // touch the ratio that actually maps to a tile.
        ws.adjust_focused(SurfaceId(2), ResizeDirection::Right, 16)
            .expect("dwindle adjust_focused after remove");
        let rects = ws.tile(out, GapConfig::new(0, 0));
        assert_eq!(rects.len(), 2, "only two windows remain");
    }

    #[test]
    fn each_workspace_keeps_independent_policy() {
        let mut m = WorkspaceManager::new(PolicyKind::Dwindle);
        m.switch_workspace(2).unwrap();
        m.set_current_layout(PolicyKind::Grid);
        m.switch_workspace(0).unwrap();
        assert_eq!(m.current().policy(), PolicyKind::Dwindle);
        m.switch_workspace(2).unwrap();
        assert_eq!(m.current().policy(), PolicyKind::Grid);
    }
}
