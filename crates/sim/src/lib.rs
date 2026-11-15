//! Simulation runtime shared by both games.
//!
//! Game logic runs at a fixed tick ([`tick::GAME_TICK_HZ`]) and publishes a [`snapshot::Snapshot`] per tick.
//! Rendering interpolates between snapshots, so any display rate shows smooth motion while the logic keeps
//! the original timing. This crate has no graphics, audio or window dependencies.

pub mod control;
pub mod field;
pub mod save;
pub mod snapshot;
pub mod test_scene;
pub mod tick;
pub mod trigger;

pub use snapshot::{Item, Snapshot, Transform, interpolate};
pub use tick::{FixedTimestep, GAME_TICK_HZ, Steps};

/// Keeps the last two snapshots of a simulation.
#[derive(Debug, Clone, Default)]
pub struct SnapshotPair {
    pub prev: Snapshot,
    pub cur: Snapshot,
}

impl SnapshotPair {
    pub fn push(&mut self, snapshot: Snapshot) {
        self.prev = std::mem::replace(&mut self.cur, snapshot);
    }

    /// Items to draw: interpolated, or the current snapshot when interpolation is off.
    pub fn frame(&self, alpha: f32, interpolate_on: bool, out: &mut Vec<Item>) {
        if interpolate_on {
            interpolate(&self.prev, &self.cur, alpha, out);
        } else {
            out.clear();
            out.extend_from_slice(&self.cur.items);
        }
    }
}
