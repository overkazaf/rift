//! Multi-window bookkeeping: stable window ids, pane-id namespacing, the
//! window registry (OS handle <-> stable id, focus order). All pure, no
//! winit types, so the routing rules are unit-tested.
//!
//! ## Architecture (see also `WindowState` in `app/mod.rs`)
//!
//! * [`WinId`] is a process-lifetime stable `u64` (never reused). The first
//!   window is 0.
//! * Every pane id embeds its window: `pane_id(win, local)`. Window 0 keeps
//!   the plain ids 0, 1, 2 ..., so single-window behaviour (MCP pane ids,
//!   `RIFT_PANE_ID`) is unchanged. Global registries keyed by pane id
//!   (agents, review checkpoints, workflow queues, MCP) therefore stay valid
//!   across windows and can find the owning window with [`pane_window`].
//! * [`WindowRegistry`] owns the id <-> OS-window mapping, the focus order
//!   (most recently focused first) and what gets focus when a window closes.

use std::collections::HashMap;
use std::hash::Hash;

/// Stable id of a window; unique for the life of the process.
pub type WinId = u64;

/// Bits of a pane id below this shift are the pane's number within its window.
pub const WIN_SHIFT: u32 = usize::BITS / 2;

/// Pane id of the `local`-th pane created in window `win`.
pub const fn pane_id(win: WinId, local: usize) -> usize {
    ((win as usize) << WIN_SHIFT) | local
}

/// The window a pane id belongs to.
pub const fn pane_window(pane: usize) -> WinId {
    (pane >> WIN_SHIFT) as WinId
}

/// Number of the pane within its window.
#[cfg(test)]
pub const fn pane_local(pane: usize) -> usize {
    pane & ((1usize << WIN_SHIFT) - 1)
}

/// Windows of the app, keyed two ways: the stable [`WinId`] and the OS handle
/// `K` (`winit::window::WindowId` in the app, plain integers in tests).
#[derive(Debug)]
pub struct WindowRegistry<K> {
    /// Creation order.
    order: Vec<WinId>,
    os: HashMap<WinId, K>,
    by_os: HashMap<K, WinId>,
    /// Most recently focused first; every live window appears exactly once.
    mru: Vec<WinId>,
    focused: Option<WinId>,
    next: WinId,
}

impl<K: Copy + Eq + Hash> Default for WindowRegistry<K> {
    fn default() -> Self {
        Self { order: Vec::new(), os: HashMap::new(), by_os: HashMap::new(), mru: Vec::new(), focused: None, next: 0 }
    }
}

impl<K: Copy + Eq + Hash> WindowRegistry<K> {
    /// Allocate a window that has no OS window yet (the app's first window
    /// exists before the event loop delivers `resumed`). The first window
    /// becomes focused.
    pub fn reserve(&mut self) -> WinId {
        let id = self.next;
        self.next += 1;
        self.order.push(id);
        self.mru.push(id);
        if self.focused.is_none() {
            self.focused = Some(id);
        }
        id
    }

    /// Attach the OS handle to a reserved window.
    pub fn bind(&mut self, id: WinId, os: K) {
        if !self.order.contains(&id) {
            return;
        }
        if let Some(old) = self.os.insert(id, os) {
            self.by_os.remove(&old);
        }
        self.by_os.insert(os, id);
    }

    /// Register a new window that already has an OS handle. It is *not*
    /// focused until the OS reports focus (or [`focus`](Self::focus) is called).
    #[cfg(test)]
    pub fn insert(&mut self, os: K) -> WinId {
        let id = self.reserve();
        self.bind(id, os);
        id
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// Ids in creation order.
    pub fn ids(&self) -> &[WinId] {
        &self.order
    }

    pub fn contains(&self, id: WinId) -> bool {
        self.order.contains(&id)
    }

    pub fn id_of(&self, os: &K) -> Option<WinId> {
        self.by_os.get(os).copied()
    }

    pub fn os_of(&self, id: WinId) -> Option<K> {
        self.os.get(&id).copied()
    }

    pub fn focused(&self) -> Option<WinId> {
        self.focused
    }

    /// Make `id` the focused window (moves it to the front of the focus order).
    pub fn focus(&mut self, id: WinId) -> bool {
        if !self.contains(id) {
            return false;
        }
        self.mru.retain(|w| *w != id);
        self.mru.insert(0, id);
        self.focused = Some(id);
        true
    }

    /// Remove a window. When it was focused, focus passes to the window that
    /// was focused before it. Returns the new focused window, if any.
    pub fn remove(&mut self, id: WinId) -> Option<WinId> {
        self.order.retain(|w| *w != id);
        self.mru.retain(|w| *w != id);
        if let Some(os) = self.os.remove(&id) {
            self.by_os.remove(&os);
        }
        if self.focused == Some(id) {
            self.focused = self.mru.first().copied();
        }
        self.focused
    }
}

// ── active / parked window states ───────────────────────────────────────

/// What the app needs from a per-window state to run the swap-in model.
pub trait WindowSlot {
    fn id(&self) -> WinId;
    /// Carry process-wide data that physically rides along with the active
    /// window from `prev` (the window that was active until now) to `self`.
    fn take_shared_from(&mut self, prev: &mut Self);
}

/// Make window `id` the active one by swapping it with `active`. The previous
/// active state is parked. False for an unknown id; true also when `id` is
/// already active.
pub fn activate<S: WindowSlot>(active: &mut S, parked: &mut HashMap<WinId, S>, id: WinId) -> bool {
    if active.id() == id {
        return true;
    }
    let Some(mut next) = parked.remove(&id) else { return false };
    std::mem::swap(active, &mut next);
    // `next` now holds the previously active window.
    active.take_shared_from(&mut next);
    parked.insert(next.id(), next);
    true
}

/// Remove window `id` from the registry and return its state. When it is the
/// active window, the window the registry now focuses becomes active (and
/// inherits the shared data). Refuses (None) to remove the last window or an
/// unknown one: the caller quits the app instead.
pub fn remove_window<K: Copy + Eq + Hash, S: WindowSlot>(
    reg: &mut WindowRegistry<K>,
    active: &mut S,
    parked: &mut HashMap<WinId, S>,
    id: WinId,
) -> Option<S> {
    if reg.len() <= 1 || !reg.contains(id) {
        return None;
    }
    let next = reg.remove(id);
    if active.id() == id {
        let mut replacement = next.and_then(|n| parked.remove(&n))?;
        std::mem::swap(active, &mut replacement);
        // `replacement` is the closing window now.
        active.take_shared_from(&mut replacement);
        Some(replacement)
    } else {
        parked.remove(&id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_zero_keeps_plain_pane_ids() {
        assert_eq!(pane_id(0, 0), 0);
        assert_eq!(pane_id(0, 7), 7);
        assert_eq!(pane_window(7), 0);
    }

    #[test]
    fn pane_ids_round_trip_and_never_collide_across_windows() {
        let a = pane_id(1, 5);
        let b = pane_id(2, 5);
        assert_ne!(a, b);
        assert_ne!(a, pane_id(0, 5));
        assert_eq!((pane_window(a), pane_local(a)), (1, 5));
        assert_eq!((pane_window(b), pane_local(b)), (2, 5));
        // High local numbers stay inside their window.
        let big = pane_id(3, (1usize << WIN_SHIFT) - 1);
        assert_eq!((pane_window(big), pane_local(big)), (3, (1usize << WIN_SHIFT) - 1));
    }

    #[test]
    fn first_window_is_reserved_then_bound() {
        let mut r = WindowRegistry::<u32>::default();
        let w0 = r.reserve();
        assert_eq!(w0, 0);
        assert_eq!(r.focused(), Some(0));
        assert_eq!(r.id_of(&100), None);
        r.bind(w0, 100);
        assert_eq!(r.id_of(&100), Some(0));
        assert_eq!(r.os_of(0), Some(100));
    }

    #[test]
    fn insert_assigns_increasing_stable_ids_and_never_reuses() {
        let mut r = WindowRegistry::<u32>::default();
        let a = r.insert(10);
        let b = r.insert(20);
        let c = r.insert(30);
        assert_eq!((a, b, c), (0, 1, 2));
        assert_eq!(r.ids(), &[0, 1, 2]);
        r.remove(b);
        let d = r.insert(40);
        assert_eq!(d, 3, "ids are never reused");
        assert_eq!(r.ids(), &[0, 2, 3]);
        assert_eq!(r.id_of(&20), None);
        assert_eq!(r.id_of(&40), Some(3));
    }

    #[test]
    fn new_window_does_not_steal_focus_until_told() {
        let mut r = WindowRegistry::<u32>::default();
        let a = r.insert(10);
        let b = r.insert(20);
        assert_eq!(r.focused(), Some(a));
        assert!(r.focus(b));
        assert_eq!(r.focused(), Some(b));
        assert!(!r.focus(99));
        assert_eq!(r.focused(), Some(b));
    }

    #[test]
    fn closing_the_focused_window_refocuses_the_previous_one() {
        let mut r = WindowRegistry::<u32>::default();
        let a = r.insert(10);
        let b = r.insert(20);
        let c = r.insert(30);
        r.focus(c);
        r.focus(a);
        r.focus(b);
        // focus order: b, a, c
        assert_eq!(r.remove(b), Some(a));
        assert_eq!(r.focused(), Some(a));
        assert_eq!(r.remove(a), Some(c));
        // Removing an unfocused window keeps the focus.
        let d = r.insert(40);
        assert_eq!(r.remove(d), Some(c));
        // Last window: nothing left to focus.
        assert_eq!(r.remove(c), None);
        assert_eq!(r.len(), 0);
        assert_eq!(r.focused(), None);
    }

    #[test]
    fn os_lookup_follows_rebinding_and_removal() {
        let mut r = WindowRegistry::<u32>::default();
        let a = r.insert(1);
        r.bind(a, 5);
        assert_eq!(r.id_of(&1), None);
        assert_eq!(r.id_of(&5), Some(a));
        r.remove(a);
        assert_eq!(r.id_of(&5), None);
        assert!(!r.contains(a));
    }

    // ── swap-in model ──

    #[derive(Debug)]
    struct Fake {
        id: WinId,
        /// Rides along with the active window.
        shared: Vec<&'static str>,
        own: &'static str,
    }

    impl WindowSlot for Fake {
        fn id(&self) -> WinId {
            self.id
        }
        fn take_shared_from(&mut self, prev: &mut Self) {
            std::mem::swap(&mut self.shared, &mut prev.shared);
        }
    }

    fn three() -> (WindowRegistry<u32>, Fake, HashMap<WinId, Fake>) {
        let mut reg = WindowRegistry::default();
        for os in [10, 20, 30] {
            reg.insert(os);
        }
        let active = Fake { id: 0, shared: vec!["autopilot"], own: "w0" };
        let mut parked = HashMap::new();
        parked.insert(1, Fake { id: 1, shared: vec![], own: "w1" });
        parked.insert(2, Fake { id: 2, shared: vec![], own: "w2" });
        (reg, active, parked)
    }

    #[test]
    fn activate_swaps_states_and_carries_shared_data() {
        let (_, mut active, mut parked) = three();
        assert!(activate(&mut active, &mut parked, 2));
        assert_eq!((active.id, active.own), (2, "w2"));
        assert_eq!(active.shared, ["autopilot"], "shared data follows the active window");
        assert!(parked[&0].shared.is_empty() && parked.len() == 2);
        // Activating the active window or an unknown one changes nothing.
        assert!(activate(&mut active, &mut parked, 2));
        assert!(!activate(&mut active, &mut parked, 77));
        assert_eq!(active.id, 2);
        // Round trip.
        assert!(activate(&mut active, &mut parked, 0));
        assert_eq!((active.own, active.shared.as_slice()), ("w0", &["autopilot"][..]));
    }

    #[test]
    fn removing_the_active_window_hands_over_to_the_focused_one_with_shared_data() {
        let (mut reg, mut active, mut parked) = three();
        reg.focus(2);
        reg.focus(0);
        let gone = remove_window(&mut reg, &mut active, &mut parked, 0).expect("removed");
        assert_eq!(gone.own, "w0");
        // Focus order was 0, 2, 1: window 2 takes over and keeps the shared data.
        assert_eq!((active.id, active.own), (2, "w2"));
        assert_eq!(active.shared, ["autopilot"]);
        assert!(gone.shared.is_empty());
        assert_eq!(reg.ids(), &[1, 2]);
        assert_eq!(parked.len(), 1);
    }

    #[test]
    fn removing_a_parked_window_leaves_the_active_one_alone() {
        let (mut reg, mut active, mut parked) = three();
        let gone = remove_window(&mut reg, &mut active, &mut parked, 1).expect("removed");
        assert_eq!(gone.own, "w1");
        assert_eq!((active.id, active.shared.as_slice()), (0, &["autopilot"][..]));
        assert_eq!(reg.ids(), &[0, 2]);
    }

    #[test]
    fn the_last_window_is_never_removed_here() {
        let mut reg = WindowRegistry::<u32>::default();
        reg.insert(1);
        let mut active = Fake { id: 0, shared: vec![], own: "only" };
        let mut parked = HashMap::new();
        assert!(remove_window(&mut reg, &mut active, &mut parked, 0).is_none());
        assert_eq!(reg.len(), 1);
        assert!(remove_window(&mut reg, &mut active, &mut parked, 9).is_none());
    }
}
