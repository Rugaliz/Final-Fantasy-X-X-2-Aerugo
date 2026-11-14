//! Small reusable debug widgets.

use std::collections::VecDeque;

/// Rolling history of frame times with a plot.
#[derive(Debug, Clone)]
pub struct FrameTimeGraph {
    samples: VecDeque<f32>,
    capacity: usize,
}

impl Default for FrameTimeGraph {
    fn default() -> Self {
        Self::new(240)
    }
}

impl FrameTimeGraph {
    pub fn new(capacity: usize) -> Self {
        Self { samples: VecDeque::with_capacity(capacity), capacity: capacity.max(2) }
    }

    pub fn push(&mut self, frame_ms: f32) {
        if self.samples.len() == self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back(frame_ms);
    }

    pub fn average_ms(&self) -> f32 {
        if self.samples.is_empty() {
            0.0
        } else {
            self.samples.iter().sum::<f32>() / self.samples.len() as f32
        }
    }

    pub fn max_ms(&self) -> f32 {
        self.samples.iter().copied().fold(0.0, f32::max)
    }

    /// Draw the history as a line plot with reference lines at 60, 144 and 240 Hz.
    pub fn ui(&self, ui: &mut egui::Ui) {
        let height = 60.0;
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width().max(120.0), height), egui::Sense::hover());
        let painter = ui.painter_at(rect);
        let visuals = ui.visuals();
        painter.rect_filled(rect, 2.0, visuals.extreme_bg_color);
        let top_ms = self.max_ms().max(1000.0 / 30.0);
        let y_of = |ms: f32| rect.bottom() - (ms / top_ms).min(1.0) * rect.height();
        for (hz, label) in [(60.0f32, "60"), (144.0, "144"), (240.0, "240")] {
            let y = y_of(1000.0 / hz);
            painter.hline(
                rect.x_range(),
                y,
                egui::Stroke::new(1.0, visuals.weak_text_color().gamma_multiply(0.4)),
            );
            painter.text(
                egui::pos2(rect.right() - 2.0, y),
                egui::Align2::RIGHT_BOTTOM,
                label,
                egui::FontId::monospace(9.0),
                visuals.weak_text_color(),
            );
        }
        if self.samples.len() >= 2 {
            let dx = rect.width() / (self.capacity - 1) as f32;
            let start = self.capacity - self.samples.len();
            let points: Vec<egui::Pos2> = self
                .samples
                .iter()
                .enumerate()
                .map(|(i, &ms)| egui::pos2(rect.left() + (start + i) as f32 * dx, y_of(ms)))
                .collect();
            painter.add(egui::Shape::line(points, egui::Stroke::new(1.5, visuals.hyperlink_color)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolling_window_keeps_capacity() {
        let mut g = FrameTimeGraph::new(3);
        for ms in [1.0, 2.0, 3.0, 4.0] {
            g.push(ms);
        }
        assert_eq!(g.average_ms(), 3.0);
        assert_eq!(g.max_ms(), 4.0);
    }
}
