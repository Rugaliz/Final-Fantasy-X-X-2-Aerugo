//! Which move and turn record each level follows. A level runs its own records unless a start hands them to the
//! levels below it (or every level up to 7); an ending move or turn hands the levels that shared it back their
//! own, carrying the position and the speeds over.

use super::{CTX_MOVE, CTX_ROT, MOVE, MOVES, ROT, ROTS, Worker, math, mv, p, rt};

impl Worker {
    /// The level's own move record (clamped as the original indexes it: by the level as given).
    pub fn own_move(&self, level: i32) -> u32 {
        MOVES + level.max(0) as u32 * MOVE
    }

    pub fn own_rot(&self, level: i32) -> u32 {
        ROTS + level.max(0) as u32 * ROT
    }

    /// The running level's own move record (natives that set up a move write there).
    pub fn cur_move(&self) -> u32 {
        MOVES + u32::from(self.level()) * MOVE
    }

    pub fn cur_rot(&self) -> u32 {
        ROTS + u32::from(self.level()) * ROT
    }

    fn level_arg(&self, level: i32) -> i32 {
        if level < 0 { i32::from(self.level()) } else { level }
    }

    /// The move of `level` (−1 the running one) ends: every level following the same record takes its own back,
    /// with the old record's target, radius, speeds and timing; a move started with a turn also ends the turn.
    pub fn move_reset(&mut self, level: i32) {
        let level = self.level_arg(level);
        let old = self.move_of(level);
        if self.u16(old + mv::MODE) & 0x4000 != 0 {
            self.rot_reset(level);
        }
        for l in 0..9 {
            let c = self.ctx(l) + CTX_MOVE;
            if self.u32(c) == old {
                let own = MOVES + l as u32 * MOVE;
                self.set_u32(c, own);
                self.set_u16(own + mv::MODE, 0);
                for f in [
                    mv::TARGET,
                    mv::TARGET + 4,
                    mv::TARGET + 8,
                    mv::RADIUS,
                    mv::F44,
                    mv::F48,
                    mv::T,
                    mv::TOTAL,
                ] {
                    let v = self.u32(old + f);
                    self.set_u32(own + f, v);
                }
            }
        }
    }

    /// The turn of `level` ends likewise.
    pub fn rot_reset(&mut self, level: i32) {
        let level = self.level_arg(level);
        let old = self.rot_of(level);
        for l in 0..9 {
            let c = self.ctx(l) + CTX_ROT;
            if self.u32(c) == old {
                let own = ROTS + l as u32 * ROT;
                self.set_u32(c, own);
                self.set_u16(own + rt::MODE, 0);
                for f in [0x20, 0x24, 0x28, 0x2c, 0x30, 0x34, rt::TOTAL, rt::T] {
                    let v = self.u32(old + f);
                    self.set_u32(own + f, v);
                }
            }
        }
    }

    /// A move starts on `level`: the level follows its own record, and so do the levels below it in `mask` (with
    /// 0x20 in `mode`: levels up to 7; with 0x100: only levels that are idle).
    pub fn set_move_mode(&mut self, level: i32, mask: u32, mode: u32) {
        let rec = self.own_move(level);
        let c = self.ctx(level) + CTX_MOVE;
        self.set_u32(c, rec);
        let top = if mode & 0x20 != 0 { 7 } else { level };
        let mut bit = 1u32;
        for l in 0..=top.max(-1) {
            let c = self.ctx(l) + CTX_MOVE;
            if (mask | 1 << (level as u32 & 31)) & bit != 0
                && (mode & 0x100 == 0 || self.u16(self.u32(c) + mv::MODE) & 0xff80 == 0)
            {
                self.set_u32(c, rec);
            }
            bit = bit.rotate_left(1);
        }
    }

    pub fn set_turn_mode(&mut self, level: i32, mask: u32, mode: u32) {
        let rec = self.own_rot(level);
        let c = self.ctx(level) + CTX_ROT;
        self.set_u32(c, rec);
        let top = if mode & 0x20 != 0 { 7 } else { level };
        let mut bit = 1u32;
        for l in 0..=top.max(-1) {
            let c = self.ctx(l) + CTX_ROT;
            if (mask | 1 << (level as u32 & 31)) & bit != 0
                && (mode & 0x100 == 0 || self.u16(self.u32(c) + rt::MODE) & 0xffe0 == 0)
            {
                self.set_u32(c, rec);
            }
            bit = bit.rotate_left(1);
        }
    }

    /// Start the running level's move: its record's id and mode, the counter cleared, then the levels it takes.
    pub fn move_start(&mut self, mask: u32, mode: u32, id: u16) {
        let m = self.cur_move();
        self.set_u16(m + mv::ID, id);
        self.set_u16(m + mv::MODE, mode as u16);
        self.set_u16(m + mv::COUNT, 0);
        self.set_move_mode(i32::from(self.level()), mask, mode);
    }

    pub fn rot_start(&mut self, mask: u32, mode: u32, id: u16) {
        let r = self.cur_rot();
        self.set_u16(r + rt::ID, id);
        self.set_u16(r + rt::MODE, mode as u16);
        self.set_u16(r + rt::COUNT, 0);
        self.set_turn_mode(i32::from(self.level()), mask, mode);
    }

    /// The running level's move goes to a point, starting from where the worker stands.
    pub fn move_set_target(&mut self, x: f32, y: f32, z: f32) {
        let m = self.cur_move();
        self.set_f32(m + mv::TARGET, x);
        self.set_f32(m + mv::TARGET + 4, y);
        self.set_f32(m + mv::TARGET + 8, z);
        let pos = self.position();
        for k in 0..3 {
            self.set_f32(m + mv::START + k * 4, pos[k as usize]);
        }
    }

    /// The running level's turn faces a point (its current record's target heading and pitch).
    pub fn rot_set_target_point(&mut self, x: f32, y: f32, z: f32) {
        let r = self.rot_of(i32::from(self.level()));
        let (yaw, pitch) = math::angles_to(self.position(), [x, y, z]);
        self.set_f32(r + rt::TARGET, yaw);
        self.set_f32(r + rt::TARGET + 4, pitch);
    }

    /// After a step: a timed turn (modes 3, 4) past its end, or a turn that reached its target heading (and pitch
    /// and roll when it turns those), ends — unless it ends with the move (0x4000). Returns the facing a character
    /// takes on.
    pub fn clear_move_mode_mask(&mut self, ctx: u32) -> Option<f32> {
        let r = self.u32(ctx + CTX_ROT);
        let mode = self.u16(r + rt::MODE);
        let chr_yaw = self.chr_bound().then(|| self.pf(p::YAW));
        let m = mode & 0x1f;
        if m > 2 {
            if m < 5 {
                if self.f32(r + rt::TOTAL) < self.f32(r + rt::T) && self.u16(r + rt::MODE) & 0x4000 == 0 {
                    self.set_u16(r + rt::MODE, self.u16(r + rt::MODE) & 0x5000);
                    self.rot_reset(-1);
                }
            } else if m == 5 {
                return chr_yaw;
            }
        }
        let mode = self.u16(r + rt::MODE);
        if self.f32(r + rt::TARGET) == self.pf(p::YAW)
            && ((mode & 0x80) == 0 || self.f32(r + rt::TARGET + 4) == self.pf(p::PITCH))
            && ((mode & 0x40) == 0 || self.f32(r + rt::TARGET + 8) == self.pf(p::ROLL))
            && mode & 0x4000 == 0
        {
            self.set_u16(r + rt::MODE, mode & 0x5000);
            self.rot_reset(-1);
        }
        chr_yaw
    }
}
