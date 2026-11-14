//! SDL3 events → egui `RawInput`, and egui `PlatformOutput` → SDL3.

use std::time::Instant;

use aerugo_platform::sdl3;
use egui::{Key, Modifiers, PointerButton, Pos2, RawInput, Rect, Vec2};
use sdl3::event::{Event, WindowEvent};
use sdl3::keyboard::{Keycode, Mod};
use sdl3::mouse::{Cursor, MouseButton, SystemCursor};
use sdl3::video::Window;

/// Output of one egui frame, ready for the renderer.
#[derive(Debug, Default)]
pub struct PreparedFrame {
    pub textures: egui::TexturesDelta,
    pub primitives: Vec<egui::ClippedPrimitive>,
    pub pixels_per_point: f32,
}

/// Holds the egui context and the input gathered since the last frame.
pub struct EguiSdl {
    pub ctx: egui::Context,
    raw: RawInput,
    start: Instant,
    modifiers: Modifiers,
    /// User zoom on top of the display scale.
    pub ui_scale: f32,
    pixels_per_point: f32,
    /// Window units → physical pixels.
    pixel_density: f32,
    cursor_icon: egui::CursorIcon,
    cursor: Option<Cursor>,
}

impl std::fmt::Debug for EguiSdl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EguiSdl").field("pixels_per_point", &self.pixels_per_point).finish_non_exhaustive()
    }
}

impl Default for EguiSdl {
    fn default() -> Self {
        Self::new()
    }
}

impl EguiSdl {
    pub fn new() -> Self {
        Self {
            ctx: egui::Context::default(),
            raw: RawInput::default(),
            start: Instant::now(),
            modifiers: Modifiers::default(),
            ui_scale: 1.0,
            pixels_per_point: 1.0,
            pixel_density: 1.0,
            cursor_icon: egui::CursorIcon::Default,
            cursor: None,
        }
    }

    /// Convert a position in SDL window units to egui points.
    fn to_points(&self, x: f32, y: f32) -> Pos2 {
        let k = self.pixel_density / self.pixels_per_point;
        Pos2::new(x * k, y * k)
    }

    /// Feed one SDL event. Returns `true` when egui wants this input (the game should ignore it).
    pub fn handle_event(&mut self, event: &Event) -> bool {
        let wants_pointer = self.ctx.egui_wants_pointer_input() || self.ctx.is_pointer_over_egui();
        let wants_keyboard = self.ctx.egui_wants_keyboard_input();
        match event {
            Event::MouseMotion { x, y, .. } => {
                let pos = self.to_points(*x, *y);
                self.raw.events.push(egui::Event::PointerMoved(pos));
                wants_pointer
            }
            Event::MouseButtonDown { mouse_btn, x, y, .. } | Event::MouseButtonUp { mouse_btn, x, y, .. } => {
                let pressed = matches!(event, Event::MouseButtonDown { .. });
                if let Some(button) = map_button(*mouse_btn) {
                    let pos = self.to_points(*x, *y);
                    self.raw.events.push(egui::Event::PointerButton {
                        pos,
                        button,
                        pressed,
                        modifiers: self.modifiers,
                    });
                }
                wants_pointer
            }
            Event::MouseWheel { x, y, .. } => {
                self.raw.events.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta: Vec2::new(*x, *y),
                    phase: egui::TouchPhase::Move,
                    modifiers: self.modifiers,
                });
                wants_pointer
            }
            Event::KeyDown { keycode, keymod, repeat, .. } | Event::KeyUp { keycode, keymod, repeat, .. } => {
                let pressed = matches!(event, Event::KeyDown { .. });
                self.modifiers = map_modifiers(*keymod);
                let Some(key) = keycode.and_then(map_key) else { return wants_keyboard };
                if pressed && self.modifiers.command {
                    match key {
                        Key::C => self.raw.events.push(egui::Event::Copy),
                        Key::X => self.raw.events.push(egui::Event::Cut),
                        Key::V => {
                            // Clipboard text arrives through `set_clipboard_text` from the app (needs SDL handles).
                        }
                        _ => {}
                    }
                }
                self.raw.events.push(egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed,
                    repeat: *repeat,
                    modifiers: self.modifiers,
                });
                wants_keyboard
            }
            Event::TextInput { text, .. } => {
                if !text.chars().all(char::is_control) {
                    self.raw.events.push(egui::Event::Text(text.clone()));
                }
                wants_keyboard
            }
            Event::Window { win_event, .. } => {
                match win_event {
                    WindowEvent::FocusGained => self.raw.events.push(egui::Event::WindowFocused(true)),
                    WindowEvent::FocusLost => self.raw.events.push(egui::Event::WindowFocused(false)),
                    WindowEvent::MouseLeave => self.raw.events.push(egui::Event::PointerGone),
                    _ => {}
                }
                false
            }
            _ => false,
        }
    }

    /// Paste text (call when the user pressed the paste shortcut and the clipboard has text).
    pub fn paste(&mut self, text: String) {
        self.raw.events.push(egui::Event::Paste(text));
    }

    /// True when Ctrl/Cmd+V is the event; the app then reads the clipboard and calls [`Self::paste`].
    pub fn is_paste(event: &Event) -> bool {
        matches!(
            event,
            Event::KeyDown { keycode: Some(Keycode::V), keymod, .. }
                if keymod.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD | Mod::LGUIMOD | Mod::RGUIMOD)
        )
    }

    /// Run one egui frame and tessellate its output. `window` provides sizes and receives cursor changes.
    pub fn run(
        &mut self,
        window: &Window,
        clipboard: &sdl3::clipboard::ClipboardUtil,
        ui: impl FnMut(&mut egui::Ui),
    ) -> PreparedFrame {
        let (w_units, _) = window.size();
        let (w_px, h_px) = window.size_in_pixels();
        self.pixel_density = if w_units > 0 { w_px as f32 / w_units as f32 } else { 1.0 };
        self.pixels_per_point = (window.display_scale() * self.ui_scale).max(0.25);

        let mut raw = std::mem::take(&mut self.raw);
        raw.time = Some(self.start.elapsed().as_secs_f64());
        raw.screen_rect = Some(Rect::from_min_size(
            Pos2::ZERO,
            Vec2::new(w_px as f32, h_px as f32) / self.pixels_per_point,
        ));
        raw.focused = true;
        raw.max_texture_side = Some(8192);
        if let Some(vp) = raw.viewports.get_mut(&raw.viewport_id) {
            vp.native_pixels_per_point = Some(self.pixels_per_point);
        } else {
            raw.viewports.insert(
                raw.viewport_id,
                egui::ViewportInfo {
                    native_pixels_per_point: Some(self.pixels_per_point),
                    ..Default::default()
                },
            );
        }

        let output = self.ctx.run_ui(raw, ui);
        self.apply_platform_output(&output.platform_output, clipboard);
        let primitives = self.ctx.tessellate(output.shapes, output.pixels_per_point);
        PreparedFrame {
            textures: output.textures_delta,
            primitives,
            pixels_per_point: output.pixels_per_point,
        }
    }

    fn apply_platform_output(
        &mut self,
        out: &egui::PlatformOutput,
        clipboard: &sdl3::clipboard::ClipboardUtil,
    ) {
        for cmd in &out.commands {
            if let egui::OutputCommand::CopyText(text) = cmd
                && let Err(e) = clipboard.set_clipboard_text(text)
            {
                log::warn!("clipboard: {e}");
            }
        }
        if out.cursor_icon != self.cursor_icon {
            self.cursor_icon = out.cursor_icon;
            let system = map_cursor(out.cursor_icon);
            match Cursor::from_system(system) {
                Ok(c) => {
                    c.set();
                    self.cursor = Some(c);
                }
                Err(e) => log::debug!("cursor: {e}"),
            }
        }
    }
}

fn map_button(b: MouseButton) -> Option<PointerButton> {
    Some(match b {
        MouseButton::Left => PointerButton::Primary,
        MouseButton::Right => PointerButton::Secondary,
        MouseButton::Middle => PointerButton::Middle,
        MouseButton::X1 => PointerButton::Extra1,
        MouseButton::X2 => PointerButton::Extra2,
        MouseButton::Unknown => return None,
    })
}

fn map_modifiers(m: Mod) -> Modifiers {
    let alt = m.intersects(Mod::LALTMOD | Mod::RALTMOD);
    let ctrl = m.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD);
    let shift = m.intersects(Mod::LSHIFTMOD | Mod::RSHIFTMOD);
    let gui = m.intersects(Mod::LGUIMOD | Mod::RGUIMOD);
    let mac = cfg!(target_os = "macos");
    Modifiers { alt, ctrl, shift, mac_cmd: mac && gui, command: if mac { gui } else { ctrl } }
}

fn map_cursor(c: egui::CursorIcon) -> SystemCursor {
    use egui::CursorIcon as C;
    match c {
        C::Text | C::VerticalText => SystemCursor::IBeam,
        C::PointingHand => SystemCursor::Hand,
        C::Wait => SystemCursor::Wait,
        C::Progress => SystemCursor::WaitArrow,
        C::Crosshair => SystemCursor::Crosshair,
        C::NotAllowed | C::NoDrop => SystemCursor::No,
        C::Move | C::AllScroll | C::Grab | C::Grabbing => SystemCursor::SizeAll,
        C::ResizeHorizontal | C::ResizeEast | C::ResizeWest | C::ResizeColumn => SystemCursor::SizeWE,
        C::ResizeVertical | C::ResizeNorth | C::ResizeSouth | C::ResizeRow => SystemCursor::SizeNS,
        C::ResizeNeSw | C::ResizeNorthEast | C::ResizeSouthWest => SystemCursor::SizeNESW,
        C::ResizeNwSe | C::ResizeNorthWest | C::ResizeSouthEast => SystemCursor::SizeNWSE,
        _ => SystemCursor::Arrow,
    }
}

fn map_key(k: Keycode) -> Option<Key> {
    use Keycode as K;
    Some(match k {
        K::Down => Key::ArrowDown,
        K::Left => Key::ArrowLeft,
        K::Right => Key::ArrowRight,
        K::Up => Key::ArrowUp,
        K::Escape => Key::Escape,
        K::Tab => Key::Tab,
        K::Backspace => Key::Backspace,
        K::Return | K::KpEnter => Key::Enter,
        K::Space => Key::Space,
        K::Insert => Key::Insert,
        K::Delete => Key::Delete,
        K::Home => Key::Home,
        K::End => Key::End,
        K::PageUp => Key::PageUp,
        K::PageDown => Key::PageDown,
        K::Minus | K::KpMinus => Key::Minus,
        K::Plus | K::KpPlus => Key::Plus,
        K::Equals => Key::Equals,
        K::Comma => Key::Comma,
        K::Period => Key::Period,
        K::Slash => Key::Slash,
        K::Backslash => Key::Backslash,
        K::Semicolon => Key::Semicolon,
        K::Apostrophe => Key::Quote,
        K::Grave => Key::Backtick,
        K::LeftBracket => Key::OpenBracket,
        K::RightBracket => Key::CloseBracket,
        K::_0 | K::Kp0 => Key::Num0,
        K::_1 | K::Kp1 => Key::Num1,
        K::_2 | K::Kp2 => Key::Num2,
        K::_3 | K::Kp3 => Key::Num3,
        K::_4 | K::Kp4 => Key::Num4,
        K::_5 | K::Kp5 => Key::Num5,
        K::_6 | K::Kp6 => Key::Num6,
        K::_7 | K::Kp7 => Key::Num7,
        K::_8 | K::Kp8 => Key::Num8,
        K::_9 | K::Kp9 => Key::Num9,
        K::A => Key::A,
        K::B => Key::B,
        K::C => Key::C,
        K::D => Key::D,
        K::E => Key::E,
        K::F => Key::F,
        K::G => Key::G,
        K::H => Key::H,
        K::I => Key::I,
        K::J => Key::J,
        K::K => Key::K,
        K::L => Key::L,
        K::M => Key::M,
        K::N => Key::N,
        K::O => Key::O,
        K::P => Key::P,
        K::Q => Key::Q,
        K::R => Key::R,
        K::S => Key::S,
        K::T => Key::T,
        K::U => Key::U,
        K::V => Key::V,
        K::W => Key::W,
        K::X => Key::X,
        K::Y => Key::Y,
        K::Z => Key::Z,
        K::F1 => Key::F1,
        K::F2 => Key::F2,
        K::F3 => Key::F3,
        K::F4 => Key::F4,
        K::F5 => Key::F5,
        K::F6 => Key::F6,
        K::F7 => Key::F7,
        K::F8 => Key::F8,
        K::F9 => Key::F9,
        K::F10 => Key::F10,
        K::F11 => Key::F11,
        K::F12 => Key::F12,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_is_ctrl_off_mac() {
        let m = map_modifiers(Mod::LCTRLMOD | Mod::RSHIFTMOD);
        assert!(m.ctrl && m.shift && !m.alt);
        assert_eq!(m.command, !cfg!(target_os = "macos"));
    }

    #[test]
    fn keys_and_buttons_map() {
        assert_eq!(map_key(Keycode::F3), Some(Key::F3));
        assert_eq!(map_key(Keycode::Kp7), Some(Key::Num7));
        assert_eq!(map_key(Keycode::CapsLock), None);
        assert_eq!(map_button(MouseButton::Right), Some(PointerButton::Secondary));
    }
}
