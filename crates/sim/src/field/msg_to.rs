//! Message windows as the original's message module runs them: per window a frame (its open and close steps by
//! frame type), a text (pages revealed character by character) and a choice record, stepped once a frame after
//! the event step; the event side's window state follows the frame each frame, and a press while a window waits
//! sets the system block's advance bit.
//!
//! Frame states: 0 none, 1 opening, 2 shown, 3 closing, 4 closed. Frame type 0 grows from its left edge by
//! 0x2aa / 0x1000 of its width a frame and shrinks the same way; types 1 and 2 open at once and close after their
//! countdown. The text's handler follows the frame's state: it reveals once the frame is shown.
//!
//! A page runs up to the next byte below 3 (0 the end, 1 and 2 page breaks). Its count: glyphs, {0b:xx} and
//! {09:c0+n} one each. The reveal adds speed × the rate (30, or 25 for language types 2–6) to an accumulator a
//! frame and shows one character per 30 (the 25 divisor belongs to a game-language switch that is 0 in a release
//! build); the speed is the value native 0259 sets (2 at start). The draw pass applies the text's
//! codes as they are reached: {09:30} reveals the page at once (each drawn glyph shows the next), {09:3n} sets the
//! rate n, {09:c0+n} waits n frames.

use super::{FieldState, Window};

/// Frame handlers by state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FrameStep {
    #[default]
    None,
    Open,
    Shown,
    Close,
    Closed,
}

/// The window's frame object (the original's 0x7c record).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Frame {
    /// +0: the frame is up.
    pub on: bool,
    /// +4 frame type, +6 the open fraction (of 0x1000), +8 state, +0xc the close countdown, +0x10 the width.
    pub kind: u16,
    pub t: i16,
    pub state: i16,
    pub count: i16,
    pub width: i16,
    /// Left edge, the right edge drawn and the one it grows to (+0x28, +0x50, +0x30).
    pub x0: f32,
    pub x1: f32,
    pub target_x1: f32,
    pub step: FrameStep,
}

/// Text handlers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TextStep {
    #[default]
    None,
    Init,
    Reveal,
    Auto,
    Closing,
    Closed,
}

/// The window's text object (the original's 0x130 record).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Text {
    pub on: bool,
    /// +0x2c the phase (1 revealing, 2 page shown, 3 closing, 4 closed) and +0x2e the state the waits read (1
    /// revealing, 2 page shown with more to come, 3 all shown).
    pub phase: u16,
    pub state: u16,
    /// The page: byte offsets of its first byte and of the byte below 3 that ends it.
    pub start: usize,
    pub end: Option<usize>,
    /// +0xa0 shown, +0xa4 shown at the last draw, +0xa8 the page's count, +0xac the rate, +0xb0 the
    /// accumulator, +0xb4 frames to wait.
    pub shown: i32,
    pub drawn_shown: i32,
    pub count: i32,
    pub rate: i32,
    pub acc: i32,
    pub wait: i32,
    pub step: TextStep,
    /// +0x11c: the handler the next page resumes with.
    pub resume: TextStep,
    /// +0x12: pages go on by themselves (flags 0x400).
    pub auto_next: bool,
    /// +0x1e: the text has choices.
    pub choices: bool,
    /// +8 the text type (flags & 0xf), +0x1a the size type, +0xc (flags 0x1000, or the settings' bit 8 clear) and
    /// +0x10 (flags 0x4000): together with the settings' subtitle bit they decide whether a dialogue is drawn.
    pub kind: u16,
    pub size: u16,
    pub flag_c: bool,
    pub forced: bool,
    /// Left out of this frame's draw (a dialogue subtitle with subtitles off).
    pub hidden: bool,
}

/// The window's choice record (the original's 0x138 record): +1 state (2 chosen), +2 highlighted, +3 the one
/// cancel picks, +4 / +5 the first and the count, +6 picks wanted, +7 picks made, +0x18 the picks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Choice {
    pub state: u8,
    pub highlighted: i8,
    pub cancel: i8,
    pub first: i8,
    pub count: i8,
    pub wanted: u8,
    pub picks: Vec<i8>,
}

/// The message module's part of a window.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct To {
    /// +0: open; +6: the active window (input goes to it).
    pub open: bool,
    pub active: bool,
    pub frame: Frame,
    pub text: Text,
    pub choice: Choice,
}

/// The menu pad's repeat (8b7cd0): per button the time held and whether it fired, at 30 frames a second.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MenuPad {
    pub last_held: u32,
    pub time: [f32; 16],
    pub fired: [u8; 16],
    /// This frame: pressed, and pressed or repeating.
    pub pressed: u32,
    pub repeat: u32,
}

const FRAME_TIME: f32 = 1.0 / 30.0;

/// The save image's settings word (785290).
const SETTINGS: usize = 0x3d0c;

/// Left plus right insets by frame type (table c59ed8).
const FRAME_INSETS: [i16; 9] = [40, 64, 32, 0, 32, 32, 32, 32, 32];

impl MenuPad {
    fn frame(&mut self, held: u32, stick: [f32; 2]) {
        let mut held = held & 0xfff;
        // The stick past a quarter of its travel reads as the d-pad.
        if stick[1] > 0.5 {
            held |= 0x1000;
        }
        if stick[1] < -0.5 {
            held |= 0x4000;
        }
        if stick[0] < -0.5 {
            held |= 0x8000;
        }
        if stick[0] > 0.5 {
            held |= 0x2000;
        }
        let pressed = (held ^ self.last_held) & held;
        let mut repeat = 0;
        for i in 0..16 {
            let bit = 1u32 << i;
            if bit == 0x800 || bit == 0x100 {
                continue;
            }
            if pressed & bit != 0 && self.fired[i] == 0 {
                repeat |= bit;
            }
            if held & bit == 0 {
                self.fired[i] = 0;
                self.time[i] = 0.0;
            } else {
                self.fired[i] = self.fired[i].wrapping_add(1);
                self.time[i] += FRAME_TIME;
            }
            if self.time[i] > 0.233_333_32 {
                repeat |= bit;
                self.fired[i] = 4;
                self.time[i] = f32::from_bits(0x3e08_8888);
            }
        }
        self.last_held = held;
        self.pressed = pressed;
        self.repeat = repeat;
    }
}

/// Element kinds of the text stream (8df610): 1 end or page break, 2 a code with an argument byte, 3 a macro, 4
/// anything else (a glyph).
fn kind(b: u8) -> u8 {
    match b {
        0..=3 => 1,
        7..=0xb | 0xd | 0xe | 0x10 => 2,
        0x12..=0x23 => 3,
        _ => 4,
    }
}

/// Bytes an element takes (8dfd80).
fn step_len(bytes: &[u8]) -> usize {
    match bytes.first() {
        None | Some(0) => 0,
        Some(4) => {
            if bytes.get(1).is_some_and(|&b| b < 0x30) {
                3
            } else {
                2
            }
        }
        Some(6..=0x0b | 0x0d | 0x0e | 0x10 | 0x12..=0x2f) => 2,
        Some(_) => 1,
    }
}

/// The page from `start`: its end (the next byte below 3) and its count (8b7f40, 8b80b0); `name` gives the
/// length of a {13:30+n} character name.
pub fn page(bytes: &[u8], start: usize, name: &dyn Fn(u8) -> i32) -> (Option<usize>, i32) {
    let mut at = start;
    let mut count = 0;
    loop {
        let Some(&b) = bytes.get(at) else { return (None, count) };
        if b < 3 {
            return (Some(at), count);
        }
        match kind(b) {
            2 => {
                let arg = bytes.get(at + 1).copied().unwrap_or(0);
                if b == 0x10 && (0x50..0x60).contains(&arg) {
                    log::error!("GAP: message name element {{10:{arg:02x}}} (its length) is not ported");
                }
                if b == 0xb || (b == 9 && arg > 0xbf) {
                    count += 1;
                }
            }
            3 => {
                let arg = bytes.get(at + 1).copied().unwrap_or(0);
                if b == 0x13 && arg >= 0x30 {
                    count += name(arg - 0x30);
                } else {
                    log::error!("GAP: message macro {{{b:02x}:{arg:02x}}} (its length) is not ported");
                }
            }
            4 => count += 1,
            _ => {}
        }
        at += step_len(&bytes[at..]).max(1);
    }
}

/// The rate per character for the language type (8de8c0): 30 or 25; other types leave it (0).
fn language_rate(language_type: Option<u32>, rate: i32) -> i32 {
    match language_type {
        Some(0 | 1 | 0x10..=0x12) => 0x1e,
        Some(2..=6) => 0x19,
        _ => rate,
    }
}

/// A character name's length in the save image (its slot, up to the NUL; page leads and the extension prefix
/// start two-byte characters).
fn name_len(image: &[u8], chr: u8) -> i32 {
    let at = super::NAMES + usize::from(chr) * super::NAME_SLOT;
    let slot = image.get(at..at + super::NAME_SLOT).unwrap_or_default();
    slot.iter().take_while(|&&b| b != 0).filter(|&&b| !matches!(b, 0x04 | 0x2C..=0x2F)).count() as i32
}

fn bytes_of(g: &FieldState, w: usize) -> Vec<u8> {
    g.windows[w].message.and_then(|m| g.messages.get(m)).map(|m| m.bytes.clone()).unwrap_or_default()
}

/// A new page (8de370): the page after the current one (or the first), shown from its start.
fn new_page(g: &mut FieldState, w: usize, image: &[u8]) {
    let bytes = bytes_of(g, w);
    let t = &mut g.windows[w].to.text;
    if let Some(end) = t.end {
        t.start = end + 1;
    }
    let (end, count) = page(&bytes, t.start, &|chr| name_len(image, chr));
    t.end = end;
    t.shown = 0;
    t.drawn_shown = 0;
    t.count = count;
    if t.rate == 0 && bytes.get(t.start).is_some_and(|&b| b != 0) {
        t.shown += 1;
    }
    t.phase = 1;
    t.state = 1;
    t.step = t.resume;
    // A choice text's records (8b9a60): one per {10:xx} marker on the page (not {10:ff}, not the name markers
    // 0x50–0x5f); the first is 0.
    if t.choices {
        let (start, end) = (t.start, t.end.unwrap_or(bytes.len()).min(bytes.len()));
        let mut at = start;
        let mut n = 0i8;
        while at < end {
            if bytes[at] == 0x10
                && bytes.get(at + 1).is_some_and(|&a| a != 0xff && !(0x50..0x60).contains(&a))
            {
                n += 1;
            }
            at += step_len(&bytes[at..]).max(1);
        }
        let c = &mut g.windows[w].to.choice;
        c.first = 0;
        c.count = n;
    }
}

/// Whether the page is the text's last (the byte after it is the end).
fn last_page(bytes: &[u8], t: &Text) -> bool {
    t.end.is_none_or(|e| bytes.get(e).is_none_or(|&b| b == 0))
}

/// The text's next page when there is one (8b8ca0).
fn next_page(g: &mut FieldState, w: usize, image: &[u8]) {
    let bytes = bytes_of(g, w);
    if !last_page(&bytes, &g.windows[w].to.text) {
        new_page(g, w, image);
    }
}

/// Open the window's message module part (8ada30 … 8ade10 of native 006a): `mode` is the atel record's mode
/// byte, `choices` its choice count.
pub fn open(g: &mut FieldState, w: usize, image: &[u8]) {
    let language_type = g.language_type;
    let win = &mut g.windows[w];
    let kind = u16::from(win.style.unwrap_or(0));
    let flags = win.flags | win.word28;
    win.to = To::default();
    let to = &mut win.to;
    to.frame.kind = kind;
    // Text flags (8b8bb0): the text type, 0x4000, the size type (0x2000; with 0x8000 3, else 1; type 3: 3 with
    // 0x8000, else 1), 0x1000 (or the settings' bit 8 clear), 0x400 pages go on by themselves.
    let settings =
        image.get(SETTINGS..SETTINGS + 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    to.text.kind = (flags & 0xf) as u16;
    to.text.forced = flags & 0x4000 != 0;
    to.text.size = if flags & 0x2000 != 0 { if flags & 0x8000 != 0 { 3 } else { 1 } } else { 0 };
    if to.text.kind == 3 {
        to.text.size = if flags & 0x8000 != 0 { 3 } else { 1 };
    }
    to.text.flag_c = flags & 0x1000 != 0 || settings & 0x100 == 0;
    to.text.auto_next = flags & 0x400 != 0;
    // Choices without the number entry (8b0020).
    if win.mode & 2 != 0 && win.mode & 0x40 == 0 {
        to.text.choices = true;
        to.choice.highlighted = win.first_choice as i8;
        to.choice.cancel = win.cancel_choice as i8;
        to.choice.wanted = 1;
    }
    to.open = true;
    for x in &mut g.windows {
        x.to.active = false;
    }
    g.windows[w].to.active = true;
    // The frame opens (906c80): its init handler, then the open step.
    // The frame's width only decides on which step frame type 0 is fully open or closed: the 7th and the 6th for
    // every width a window can have (its insets alone are 40, the screen 512); the frame is measured here as its
    // insets.
    let f = &mut g.windows[w].to.frame;
    f.on = true;
    f.x0 = 0.0;
    f.width = FRAME_INSETS[usize::from(kind).min(8)];
    f.target_x1 = f32::from(f.width);
    match f.kind {
        0 => {
            f.t = 0;
            f.x1 = f.x0;
        }
        1 | 2 => {
            f.count = 0;
            f.x1 = f.target_x1;
        }
        k => log::error!("GAP: message frame type {k} (its open and close steps) is not ported"),
    }
    f.state = 1;
    f.step = FrameStep::Open;
    // The text opens (8b8560): init, then its first page.
    let t = &mut g.windows[w].to.text;
    t.on = true;
    t.phase = 0;
    t.shown = 0;
    t.wait = 0;
    t.rate = language_rate(language_type, t.rate);
    t.state = 1;
    t.step = TextStep::None;
    t.resume = TextStep::None;
    t.start = 0;
    t.end = None;
    new_page(g, w, image);
}

/// msg_close (8640f0): unless closed or closing, the event side's state 3 and the frame and text close.
pub fn close(g: &mut FieldState, w: usize) {
    let win = &mut g.windows[w];
    if matches!(win.atel_state, 0 | 3) {
        return;
    }
    win.atel_state = 3;
    let f = &mut win.to.frame;
    if f.on {
        f.state = 3;
        f.step = FrameStep::Close;
    }
    let t = &mut win.to.text;
    if t.on {
        t.phase = 3;
        t.step = TextStep::Closing;
    }
}

/// The press check before the block's workers (867950): a confirm (or the second button) while a window waits on
/// it sets the advance bit (system byte 0 bit 2); the bit lasts one frame.
pub fn press(g: &mut FieldState, pressed: u32) {
    let s = &mut g.workers.sys;
    s.flags0 &= !4 & !0x10;
    if pressed & 0xa0 != 0 && waiting(g) == 1 {
        g.workers.sys.flags0 |= 4;
    }
}

/// 86c8b0: a window opening or shown that waits for a press (1), or that takes choices (2).
pub fn waiting(g: &FieldState) -> u8 {
    for w in &g.windows {
        if matches!(w.atel_state, 1 | 2) {
            if w.mode & 0x22 != 0 {
                return 2;
            }
            if w.word16 != 0 || w.mode & 0x10 != 0 {
                return 1;
            }
        }
    }
    0
}

/// The window part of AtelUpdateWindowStatus (after the workers): the event side's state follows the frame (4
/// reads as 0); the last shown window that takes input (mode 0x32) becomes the active one (window 0 when none).
///
/// The user control part: a window up with mode bit 1 (choices) is "a choice is up"; unless the script mode word
/// (+0x1a) is set, byte 1 bit 2 follows it (when byte 1 bit 3 allows; else it is cleared) and when it changes the
/// control is checked again (UcOff, then UcOn unless blocked when the kept byte 0 had 0x22; byte 0 bit 1 kept);
/// byte 3 bit 4 = not (control on and byte 1 bit 2 clear). (The block's +0x1ea countdown that clears byte 3 bit 7
/// is not set by any ported native.)
pub fn status(g: &mut FieldState) {
    let mut active = 0;
    let mut choice_up = false;
    for (i, w) in g.windows.iter_mut().enumerate() {
        w.atel_state = if w.to.frame.state == 4 { 0 } else { w.to.frame.state };
        if w.atel_state != 0 && w.mode & 2 != 0 {
            choice_up = true;
        }
        if w.atel_state == 2 && w.mode & 0x32 != 0 {
            active = i;
        }
    }
    if g.workers.sys.mode1a == 0 {
        let s = &mut g.workers.sys;
        let b1 = s.flags1;
        if (b1 >> 2 & 1 != 0) != choice_up {
            s.flags1 = if b1 & 8 == 0 { b1 & !4 } else { b1 & !4 | u8::from(choice_up) << 2 };
            g.control_recheck();
        }
        let s = &mut g.workers.sys;
        let off = s.flags1 & 4 != 0 || s.flags0 & 2 == 0;
        s.mode3 = s.mode3 & !0x10 | u8::from(off) << 4;
    }
    for (i, w) in g.windows.iter_mut().enumerate() {
        w.to.active = i == active;
    }
}

/// One frame step: returns whether the state changed (the text then takes the state's handler).
fn frame_step(f: &mut Frame) -> bool {
    let grow = |f: &mut Frame| {
        let v = i32::from(f.width) * i32::from(f.t);
        f.x1 = (v / 0x1000) as f32 + f.x0;
    };
    match (f.kind, f.step) {
        (0, FrameStep::Open) => {
            f.t = (f.t + 0x2aa).min(0x1000);
            grow(f);
            if f.target_x1 > f.x1 {
                return false;
            }
            f.state = 2;
            f.step = FrameStep::Shown;
            true
        }
        (0, FrameStep::Close) => {
            f.t = (f.t - 0x2aa).max(0);
            grow(f);
            if f.x1 <= f.x0 {
                f.state = 4;
                f.step = FrameStep::Closed;
                return true;
            }
            false
        }
        (0, FrameStep::Closed) => {
            f.on = false;
            f.state = 0;
            f.step = FrameStep::None;
            true
        }
        (_, FrameStep::Open) => {
            f.state = 2;
            f.step = FrameStep::Shown;
            true
        }
        (_, FrameStep::Close) => {
            f.count -= 1;
            if f.count < 1 {
                f.state = 4;
                f.step = FrameStep::Closed;
                return true;
            }
            false
        }
        (_, FrameStep::Closed) => {
            f.on = false;
            f.step = FrameStep::None;
            true
        }
        _ => false,
    }
}

/// The text's handler for a frame state (the text handler table: 2 reveal, 3 closing, 4 closed; 0 the init
/// again, 1 none).
fn text_handler(state: i16) -> TextStep {
    match state {
        0 => TextStep::Init,
        2 => TextStep::Reveal,
        3 => TextStep::Closing,
        4 => TextStep::Closed,
        _ => TextStep::None,
    }
}

/// The reveal's step (8df570).
fn advance(t: &mut Text, speed: f32) {
    if t.wait != 0 {
        t.wait -= 1;
        return;
    }
    t.acc = (f64::from(t.rate as f32 * speed) + f64::from(t.acc)) as i32;
    if t.acc >= 0x1e {
        t.shown += t.acc / 0x1e;
        t.acc %= 0x1e;
    }
}

/// The message module's frame (8ab910): the menu pad, then each open window's frame and text steps and the active
/// window's input.
pub fn step(g: &mut FieldState, held: u32, image: &[u8]) {
    let stick = g.input.stick;
    g.menu_pad.frame(held, stick);
    let speed = g.values_5b98[0];
    for w in 0..g.windows.len() {
        if !g.windows[w].to.open {
            continue;
        }
        let changed = g.windows[w].to.frame.on && frame_step(&mut g.windows[w].to.frame);
        text_step(g, w, speed, image);
        if changed {
            let h = text_handler(g.windows[w].to.frame.state);
            let t = &mut g.windows[w].to.text;
            if t.on && h != TextStep::None {
                t.step = h;
            }
        }
        if g.windows[w].to.active {
            input(g, w, image);
        }
    }
}

fn text_step(g: &mut FieldState, w: usize, speed: f32, image: &[u8]) {
    if !g.windows[w].to.text.on {
        return;
    }
    match g.windows[w].to.text.step {
        TextStep::Reveal => {
            let bytes = bytes_of(g, w);
            let t = &mut g.windows[w].to.text;
            t.shown = t.shown.clamp(0, t.count);
            if t.shown >= t.count {
                t.phase = 2;
                t.state = if last_page(&bytes, t) { 3 } else { 2 };
                t.step = TextStep::Auto;
                t.resume = TextStep::Reveal;
            }
            advance(t, speed);
            t.shown = t.shown.clamp(0, t.count);
        }
        TextStep::Auto => {
            if g.windows[w].to.text.auto_next {
                next_page(g, w, image);
            }
        }
        TextStep::Closing => g.windows[w].to.text.phase = 4,
        TextStep::Closed => {
            let t = &mut g.windows[w].to.text;
            t.on = false;
            t.phase = 0;
            t.state = 0;
        }
        TextStep::Init => {
            let language_type = g.language_type;
            let t = &mut g.windows[w].to.text;
            t.shown = 0;
            t.wait = 0;
            t.rate = language_rate(language_type, t.rate);
            t.state = 1;
            t.step = TextStep::None;
        }
        TextStep::None => {}
    }
}

/// The active window's input (8b6b70: once its page is shown): plain text goes to the next page on confirm
/// (8b64a0); choices move, cancel and pick (8b64e0).
fn input(g: &mut FieldState, w: usize, image: &[u8]) {
    if g.windows[w].to.text.phase != 2 {
        return;
    }
    let (pressed, repeat) = (g.menu_pad.pressed, g.menu_pad.repeat);
    if !g.windows[w].to.text.choices {
        if pressed & 0x20 != 0 {
            next_page(g, w, image);
        }
        return;
    }
    if let Some(e) = g.windows[w].number.as_mut() {
        // The number entry (8b62b0).
        e.step(repeat, pressed);
        g.windows[w].to.choice.state = e.state;
        return;
    }
    let c = &mut g.windows[w].to.choice;
    let before = c.highlighted;
    if pressed & 0x40 != 0 {
        c.highlighted = c.cancel;
        return;
    }
    if pressed & 0x20 != 0 {
        c.picks.push(before);
        if c.picks.len() == usize::from(c.wanted) {
            c.state = 2;
        }
        return;
    }
    if repeat & 0x6000 != 0 {
        c.highlighted += 1;
    }
    if repeat & 0x9000 != 0 {
        c.highlighted -= 1;
    }
    if c.count <= c.highlighted {
        c.highlighted = c.first;
    }
    if c.highlighted < c.first {
        c.highlighted = c.count - 1;
    }
}

/// The draw pass's effects on the reveal (TODrawMessageWindow → 8dea40 / 8dee00 → TOSetDrawInfo): the page's
/// elements up to the shown count; at speed 0 each drawn glyph shows the next.
pub fn draw_pass(g: &mut FieldState, image: &[u8]) {
    let settings =
        image.get(SETTINGS..SETTINGS + 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    for w in 0..g.windows.len() {
        let t = &mut g.windows[w].to.text;
        // TODrawMessageWindow leaves out dialogue subtitles (text type 2, size type 1 or 3, not forced) while the
        // settings' subtitle bit (0x200) is clear.
        t.hidden = settings & 0x200 == 0 && t.kind == 2 && matches!(t.size, 1 | 3) && !t.forced && !t.flag_c;
        let win = &g.windows[w];
        if !win.to.text.on || win.to.text.hidden || win.to.text.shown <= 0 {
            continue;
        }
        let bytes = bytes_of(g, w);
        let t = &mut g.windows[w].to.text;
        let mut at = t.start;
        let mut drawn = 0;
        while drawn < t.shown {
            let Some(&b) = bytes.get(at) else { break };
            if b < 3 {
                break;
            }
            let arg = bytes.get(at + 1).copied().unwrap_or(0);
            match kind(b) {
                4 => {
                    drawn += 1;
                    if t.wait == 0 && t.rate == 0 && t.shown < t.count {
                        t.shown += 1;
                    }
                }
                2 if b == 9 && arg > 0xbf => {
                    if t.shown != t.drawn_shown && drawn + 1 == t.shown {
                        t.wait = i32::from(arg) - 0xc0;
                    }
                    drawn += 1;
                }
                2 if b == 9 => {
                    if drawn >= t.drawn_shown {
                        t.rate = if arg == 0x30 { 0 } else { i32::from(arg) - 0x30 };
                    }
                }
                2 if b == 0xb => drawn += 1,
                _ => {}
            }
            at += step_len(&bytes[at..]).max(1);
        }
        t.drawn_shown = t.shown;
    }
}

impl Window {
    /// The text state the waits read (8b7ca0).
    pub fn text_state(&self) -> u16 {
        self.to.text.state
    }
}
