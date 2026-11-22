//! The worker records and the field's actors: a character worker takes its body's position before its turn, its
//! engine step hands the body a speed, a rise and a heading, and the other workers' positions and facings are what
//! the actors show.

use crate::workers::engine::Env;
use crate::workers::{self, Worker, p};

use super::{Actor, FieldState};

/// The engine's view of the field while one worker steps.
struct Bridge<'a> {
    actors: &'a mut Vec<Actor>,
    signals: Vec<u8>,
    once: Vec<u8>,
}

impl Bridge<'_> {
    fn actor(&mut self, w: usize) -> &mut Actor {
        if self.actors.len() <= w {
            self.actors.resize(w + 1, Actor::default());
        }
        &mut self.actors[w]
    }
}

impl Env for Bridge<'_> {
    fn chr_set_pos(&mut self, w: usize, pos: [f32; 3]) {
        self.actor(w).pos = pos;
    }
    fn chr_mode(&self, w: usize) -> i8 {
        self.actors.get(w).map_or(0, |a| a.chr_mode as i8)
    }
    fn chr_set_speed(&mut self, w: usize, v: f32) {
        self.actor(w).engine_speed = v;
    }
    fn chr_set_rise(&mut self, w: usize, v: f32) {
        self.actor(w).chr_rise = (f64::from(v) * 0.05) as f32;
    }
    fn chr_set_dir(&mut self, w: usize, v: f32) {
        self.actor(w).move_heading = v;
    }
    fn chr_dir(&self, w: usize) -> f32 {
        self.actors.get(w).map_or(0.0, |a| a.move_heading)
    }
    fn chr_yaw(&self, w: usize) -> f32 {
        self.actors.get(w).map_or(0.0, |a| a.yaw)
    }
    fn chr_set_yaw(&mut self, w: usize, v: f32) {
        // The body refuses a rotation that is not a number (and faces 0).
        self.actor(w).yaw = if v.is_finite() { v } else { 0.0 };
    }
    fn camera_point(&mut self, _handle: u32, _v: [f32; 4]) {}
    fn bind_place(&mut self, _kind: u8, _id: u16, _rot: [f32; 4], _pos: [f32; 4]) {}
    fn signal(&mut self, _w: usize, t: u8) {
        self.signals.push(t);
    }
    fn signal_once(&mut self, _w: usize, t: u8) {
        if !self.once.contains(&t) {
            self.once.push(t);
        }
    }
    fn path_point(&self, _table: u32, _cubic: bool, _t: f32) -> [f32; 3] {
        [0.0; 3]
    }
}

impl FieldState {
    /// The worker records the event's script sets up.
    pub fn setup_workers(&mut self, kinds: &[u8]) {
        self.workers.list = kinds.iter().enumerate().map(|(i, &k)| Worker::setup(k, i as u16)).collect();
    }

    /// Before a worker's turn: it keeps where it was, a character worker takes its body's place (and a pending
    /// placement puts the body on the worker first), and the player's place moves on.
    pub(super) fn worker_pre_run(&mut self, w: usize) {
        let Some(wk) = self.workers.list.get(w) else { return };
        if wk.kind == 1 && wk.chr_bound() && wk.u16(workers::PLACE) & 1 != 0 {
            let pos = wk.position();
            self.actor(w).pos = [pos[0], pos[1] - 0.1, pos[2]];
            let wk = &mut self.workers.list[w];
            wk.set_u16(workers::PLACE, wk.u16(workers::PLACE) & 0xfffe);
        }
        let body = self.actors.get(w).map(|a| (a.pos, a.chr_mode));
        let wk = &mut self.workers.list[w];
        if wk.pos().is_none() {
            return;
        }
        let pos = wk.position();
        for (k, v) in pos.into_iter().enumerate() {
            wk.set_pf(p::PREV + k as u32 * 4, v);
        }
        if wk.chr_bound()
            && let Some((bp, mode)) = body
        {
            wk.set_position(bp);
            wk.set_u8(0xac, mode);
        }
        if self.workers.sys.player == w as i16 {
            let pos = self.workers.list[w].position();
            let s = &mut self.workers.sys;
            s.player_prev = s.player_pos;
            s.player_pos = [pos[0], pos[1], pos[2], 1.0];
        }
    }

    /// A worker's engine step after its turn; returns the signal types it raises.
    pub(super) fn worker_engine_step(&mut self, w: usize, level: u8) -> Vec<u8> {
        if w >= self.workers.list.len() {
            return Vec::new();
        }
        self.workers.list[w].set_u8(workers::LEVEL, level);
        self.workers.sys.player = self.player.map_or(-1, |p| p as i16);
        self.workers.sys.talk_reach = self.talk_reach;
        let mut bridge = Bridge { actors: &mut self.actors, signals: Vec::new(), once: Vec::new() };
        self.workers.step(w, &mut bridge);
        let mut out = bridge.signals;
        out.extend(bridge.once);
        // Only the signal types the worker's mask lets through.
        let mask = self.workers.list[w].u8(workers::SIGNAL_MASK);
        out.retain(|&t| mask & (1 << t) != 0);
        self.show_worker(w);
        out
    }

    /// The actor shows a worker that has no body of its own where the worker is and how it faces.
    pub(super) fn show_worker(&mut self, w: usize) {
        let Some(wk) = self.workers.list.get(w) else { return };
        if wk.chr_bound() || wk.pos().is_none() {
            return;
        }
        let (pos, yaw) = (wk.position(), wk.pf(p::YAW));
        let a = self.actor(w);
        a.pos = pos;
        a.yaw = yaw;
    }

    /// The worker record takes the actor's binding (kind and id; a character binding has a body).
    pub(super) fn sync_bind(&mut self, w: usize) {
        let (kind, id) = self.actors.get(w).map_or((0, 0), |a| (a.bind_kind, a.bind_id));
        if let Some(wk) = self.workers.list.get_mut(w) {
            wk.set_u8(workers::BIND, kind);
            wk.set_u16(workers::BIND_ID, id);
            wk.set_u32(workers::CHR, u32::from(kind == 1));
        }
    }

    /// The talk of this frame starts: the system looks for a target while the workers step.
    pub(super) fn talk_begin(&mut self) {
        let s = &mut self.workers.sys;
        s.talk_mode = u8::from(self.talk.is_some());
        s.talk_signal = self.talk.as_ref().map_or(0, |t| t.signal);
        if s.talk_mode == 1 {
            s.talk_target = -1;
        }
    }

    /// After every worker stepped: the talk target found, with its signal.
    pub(super) fn talk_end(&mut self) -> Option<(usize, u8)> {
        let s = &self.workers.sys;
        (s.talk_mode == 1 && s.talk_target != -1).then_some((s.talk_target as usize, s.talk_signal))
    }
}
