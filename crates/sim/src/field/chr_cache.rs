//! The character data cache's entries as the scripts see them: up to 100, each a type (1 a character's data, 2 a
//! character's resident motion file), an id, and whether its read is still running. A read the scripts request
//! makes an entry unless one exists; the waits poll an entry's state; a purge drops the read entries no one holds.
//! Reads here complete at once (the original's asynchronous reads may finish before the first poll).

/// One entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub ty: u8,
    pub id: u32,
    /// The read is still running.
    pub reading: bool,
    /// Held (a purge keeps it).
    pub held: bool,
    /// Its use (low 3 bits; 5, set when made, and 7 are kept by a purge).
    pub use_kind: u8,
    /// A purge came while it was being read.
    pub purge_pending: bool,
}

/// The entries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChrCache {
    pub entries: Vec<Entry>,
}

/// Entries the cache holds.
pub const SLOTS: usize = 100;

impl ChrCache {
    /// An entry's state: 0 read, 1 being read, 2 none.
    pub fn state(&self, ty: u8, id: u32) -> i32 {
        match self.entries.iter().find(|e| e.ty == ty && e.id == id) {
            Some(e) => i32::from(e.reading),
            None => 2,
        }
    }

    /// Request a read (`exists`: the file is there to read); nothing when the entry exists or the cache is full.
    pub fn request(&mut self, ty: u8, id: u32, exists: bool) {
        if self.state(ty, id) != 2 || !exists || self.entries.len() >= SLOTS {
            return;
        }
        self.entries.push(Entry { ty, id, reading: false, held: false, use_kind: 5, purge_pending: false });
    }

    /// Drop the read entries that are not held and not of use 5 or 7; one still being read is marked instead.
    pub fn purge(&mut self) {
        self.entries.retain_mut(|e| {
            let keep_kind = matches!(e.use_kind & 7, 5 | 7);
            if e.held || keep_kind {
                return true;
            }
            if e.reading {
                e.purge_pending = true;
                return true;
            }
            false
        });
    }
}

use aerugo_script::natives::{Native, Natives};
use aerugo_script::vm::Value;

use super::FieldState;

impl FieldState {
    /// A character's data can be read: listed by its type and its file not empty.
    fn chr_data_exists(&self, id: u16) -> bool {
        let (Some(vfs), Some(game)) = (self.vfs.as_ref(), self.game) else { return false };
        super::chr_listed(vfs, game, id)
            && super::chr_path(game, id).and_then(|p| vfs.size(&p)).is_some_and(|n| n != 0)
    }

    /// A character's resident motion file `n` can be read: listed, and neither empty nor a bare header.
    fn chr_motion_exists(&self, id: u16, n: u16) -> bool {
        let (Some(vfs), Some(game)) = (self.vfs.as_ref(), self.game) else { return false };
        let path = super::ps2_chr_dir(game, id).map(|d| format!("{d}/mot/resident{n}.mgrp"));
        super::chr_listed(vfs, game, id)
            && path.and_then(|p| vfs.size(&p)).is_some_and(|s| s != 0 && s != 0x10)
    }
}

/// 504e read a character's data, 504f wait for it; 5061 read a character's resident motion file, 5063 wait for
/// it; 4020 purge the cache.
pub fn register(n: &mut Natives<FieldState>) {
    n.register(
        0x504E,
        Native::result(|g, c| {
            let id = c.stack.pop_int() as u32;
            let exists = g.chr_data_exists(id as u16);
            g.chr_cache.request(1, id, exists);
            Value::Int(0)
        }),
    );
    n.register(
        0x504F,
        Native::waiting(
            |_, c| c.scratch[0] = c.stack.pop_int(),
            |g, c| u32::from(g.chr_cache.state(1, c.scratch[0] as u32) == 0),
        ),
    );
    n.register(
        0x5061,
        Native::result(|g, c| {
            let m = c.stack.pop_int() as u32;
            let id = c.stack.pop_int() as u32;
            let exists = g.chr_motion_exists(id as u16, m as u16);
            if exists && g.chr_cache.state(2, m << 16 | id) == 2 {
                g.read_motion_file((0, m as u16, id as u16));
            }
            g.chr_cache.request(2, m << 16 | id, exists);
            Value::Int(0)
        }),
    );
    n.register(
        0x5063,
        Native::waiting(
            |_, c| {
                c.scratch[1] = c.stack.pop_int();
                c.scratch[0] = c.stack.pop_int();
            },
            |g, c| u32::from(g.chr_cache.state(2, (c.scratch[1] as u32) << 16 | c.scratch[0] as u32) == 0),
        ),
    );
    n.register(
        0x4020,
        Native::result(|g, _| {
            g.chr_cache.purge();
            Value::Int(0)
        }),
    );
}
