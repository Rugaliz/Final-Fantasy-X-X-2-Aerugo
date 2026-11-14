//! The timing test scene: a 30 Hz simulation drawn at display rate through snapshot interpolation.

use std::time::Duration;

use aerugo_debug_ui::egui;
use aerugo_gfx::Quad;
use aerugo_sim::test_scene::TestScene;
use aerugo_sim::{FixedTimestep, GAME_TICK_HZ, Item, SnapshotPair};

/// Base half-size of a scene item before its scale is applied.
const ITEM_HALF: f32 = 0.05;

#[derive(Debug)]
pub struct SceneState {
    scene: TestScene,
    pair: SnapshotPair,
    timestep: FixedTimestep,
    items: Vec<Item>,
    pub interpolate: bool,
    pub paused: bool,
    /// Simulation speed multiplier (slow motion below 1).
    pub time_scale: f32,
    step_once: bool,
    pub last_alpha: f32,
    pub dropped_frames: u64,
}

impl Default for SceneState {
    fn default() -> Self {
        Self::new()
    }
}

impl SceneState {
    pub fn new() -> Self {
        let mut scene = TestScene::new();
        let mut pair = SnapshotPair::default();
        pair.push(scene.step());
        pair.push(scene.step());
        Self {
            scene,
            pair,
            timestep: FixedTimestep::new(GAME_TICK_HZ, 8),
            items: Vec::new(),
            interpolate: true,
            paused: false,
            time_scale: 1.0,
            step_once: false,
            last_alpha: 0.0,
            dropped_frames: 0,
        }
    }

    pub fn tick_count(&self) -> u64 {
        self.scene.tick_count()
    }

    /// Advance by real elapsed time and return the quads to draw.
    pub fn update(&mut self, elapsed: Duration, out: &mut Vec<Quad>) {
        let alpha = if self.paused {
            if std::mem::take(&mut self.step_once) {
                self.pair.push(self.scene.step());
            }
            self.last_alpha
        } else {
            let steps = self.timestep.advance(elapsed.mul_f32(self.time_scale));
            for _ in 0..steps.ticks {
                self.pair.push(self.scene.step());
            }
            if steps.dropped_time {
                self.dropped_frames += 1;
            }
            steps.alpha
        };
        self.last_alpha = alpha;
        self.pair.frame(alpha, self.interpolate, &mut self.items);
        out.clear();
        out.extend(self.items.iter().map(item_to_quad));
    }

    pub fn controls_ui(&mut self, ui: &mut egui::Ui) {
        ui.checkbox(&mut self.interpolate, "Interpolate between ticks");
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.paused, "Pause");
            if ui.add_enabled(self.paused, egui::Button::new("Step one tick")).clicked() {
                self.step_once = true;
            }
        });
        ui.add(egui::Slider::new(&mut self.time_scale, 0.05..=2.0).text("speed").logarithmic(true));
        ui.label(format!(
            "ticks: {}   alpha: {:.2}   catch-up drops: {}",
            self.tick_count(),
            self.last_alpha,
            self.dropped_frames
        ));
        ui.label(
            egui::RichText::new(
                "The yellow square jumps back every 2 s; it must never smear across the screen.",
            )
            .small()
            .weak(),
        );
    }
}

fn item_to_quad(item: &Item) -> Quad {
    let t = &item.transform;
    Quad {
        center: [t.position[0], t.position[1]],
        half_size: [ITEM_HALF * t.scale[0], ITEM_HALF * t.scale[1]],
        rotation: t.rotation_z(),
        color: item.color,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_quads_and_advances_at_tick_rate() {
        let mut s = SceneState::new();
        let start = s.tick_count();
        let mut quads = Vec::new();
        // One second at 240 Hz.
        for _ in 0..240 {
            s.update(Duration::from_micros(4167), &mut quads);
        }
        let ticks = s.tick_count() - start;
        assert!((29..=31).contains(&ticks), "{ticks} ticks in one second");
        assert!(!quads.is_empty());
    }

    #[test]
    fn pause_freezes_and_step_advances_one_tick() {
        let mut s = SceneState::new();
        let mut quads = Vec::new();
        s.paused = true;
        let t0 = s.tick_count();
        s.update(Duration::from_secs(1), &mut quads);
        assert_eq!(s.tick_count(), t0);
        s.step_once = true;
        s.update(Duration::from_secs(1), &mut quads);
        assert_eq!(s.tick_count(), t0 + 1);
    }
}
