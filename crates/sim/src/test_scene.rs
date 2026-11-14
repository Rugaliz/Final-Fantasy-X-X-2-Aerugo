//! A small deterministic simulation used to check timing and interpolation visually.
//!
//! It runs at the game tick rate and contains: objects orbiting at different speeds, a spinning bar, an
//! object that teleports every two seconds (must not smear), and one that appears and disappears.

use std::f32::consts::TAU;

use crate::snapshot::{Item, Snapshot, Transform};

#[derive(Debug, Clone, Default)]
pub struct TestScene {
    tick: u64,
}

impl TestScene {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn tick_count(&self) -> u64 {
        self.tick
    }

    /// Advance one tick and return the resulting snapshot.
    pub fn step(&mut self) -> Snapshot {
        self.tick += 1;
        let t = self.tick as f32 / crate::tick::GAME_TICK_HZ as f32;
        let mut items = Vec::new();

        // Orbiters: one revolution every 2, 3 and 4 seconds.
        for (i, period) in [2.0f32, 3.0, 4.0].iter().enumerate() {
            let a = t / period * TAU;
            let r = 0.25 + 0.15 * i as f32;
            items.push(Item {
                id: 10 + i as u32,
                transform: Transform {
                    position: [r * a.cos(), r * a.sin(), 0.0],
                    ..Transform::IDENTITY.with_rotation_z(a)
                },
                color: [[1.0, 0.4, 0.3, 1.0], [0.3, 0.9, 0.4, 1.0], [0.3, 0.5, 1.0, 1.0]][i],
                discontinuous: false,
            });
        }

        // Fast spinner: 1 revolution per second (12° per tick), shows rotation interpolation clearly.
        items.push(Item {
            id: 20,
            transform: Transform { scale: [2.5, 0.4, 1.0], ..Transform::IDENTITY.with_rotation_z(t * TAU) },
            color: [1.0, 1.0, 1.0, 1.0],
            discontinuous: false,
        });

        // Teleporter: slides right, jumps back to the left every 2 seconds.
        let phase = self.tick % 60;
        items.push(Item {
            id: 30,
            transform: Transform {
                position: [-0.8 + phase as f32 * (1.6 / 59.0), -0.8, 0.0],
                ..Transform::IDENTITY
            },
            color: [1.0, 0.9, 0.2, 1.0],
            discontinuous: phase == 0,
        });

        // Blinker: present for one second out of every two.
        if (self.tick / 30).is_multiple_of(2) {
            items.push(Item {
                id: 40,
                transform: Transform { position: [0.8, 0.8, 0.0], ..Transform::IDENTITY },
                color: [0.9, 0.3, 0.9, 1.0],
                discontinuous: false,
            });
        }

        let mut s = Snapshot { tick: self.tick, items };
        s.sort();
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_flags_teleports() {
        let mut a = TestScene::new();
        let mut b = TestScene::new();
        for _ in 0..200 {
            assert_eq!(a.step(), b.step());
        }
        let mut s = TestScene::new();
        let flagged: Vec<u64> = (0..130)
            .filter_map(|_| {
                let snap = s.step();
                snap.items.iter().any(|i| i.id == 30 && i.discontinuous).then_some(snap.tick)
            })
            .collect();
        assert_eq!(flagged, [60, 120]);
    }
}
