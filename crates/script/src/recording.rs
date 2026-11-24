//! Recordings of the original game's script machine, for checking Aerugo against it. A recording is a stream of
//! records (little endian) after the magic `AEORACL1`:
//!
//! | Tag | Record |
//! |---|---|
//! | 1 | frame number `u32` |
//! | 2 | instruction: worker `u16`, level `u8`, pc `u32` (from the code start), opcode byte, `u16` operand when the opcode byte has bit 7 |
//! | 3 | random number: stream `u8`, value `u32` |
//! | 4 | event start: index `i32`, path (`u16` length + bytes) |
//! | 5 | map change: target `i32`, entrance `i32` |
//! | 0x0B | battle state: in battle `u8`, pending `u8`, area `u16`, group `u8`, formation `u8` |

use crate::vm::TraceEvent;

pub const MAGIC: &[u8; 8] = b"AEORACL1";
/// Version 2 adds the worker's stack depth (before the instruction) to instruction records.
pub const MAGIC_V2: &[u8; 8] = b"AEORACL2";
/// Version 3 adds native results and polls, input and worker positions per frame.
pub const MAGIC_V3: &[u8; 8] = b"AEORACL3";

/// A worker's place at the start of a frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorkerPosition {
    pub worker: u16,
    pub kind: u8,
    pub pos: [f32; 3],
    pub pitch: f32,
    pub yaw: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Record {
    Frame(u32),
    Instruction {
        worker: u16,
        level: u8,
        pc: u32,
        op: u8,
        operand: Option<u16>,
        depth: Option<u8>,
    },
    Random {
        stream: u8,
        value: u32,
    },
    Event {
        index: i32,
        path: String,
    },
    MapChange {
        target: i32,
        entrance: i32,
    },
    /// A finished native call's result (tag 1 int, 2 float; the value's raw bits).
    NativeResult {
        worker: u16,
        native: u16,
        tag: u8,
        value: u32,
    },
    /// A native's poll result flags.
    NativePoll {
        worker: u16,
        native: u16,
        flags: u8,
    },
    /// Input at the start of a frame: the scripts' pad words, then pad 0's state block.
    Input(Vec<u8>),
    Positions(Vec<WorkerPosition>),
    /// The active camera's two points (the pair the player's heading comes from) at the start of a frame.
    Camera([f32; 4], [f32; 4]),
    /// The battle state when it changes: in a battle, a battle pending (1 random, 2 scripted), and the battle's
    /// area index, group and formation.
    Battle {
        active: bool,
        pending: u8,
        area: u16,
        group: u8,
        formation: u8,
    },
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RecordingError {
    #[error("not a recording")]
    NotARecording,
    #[error("unknown record {0} at {1:#x}")]
    UnknownRecord(u8, usize),
}

/// Parse a recording. A record cut off at the end (the game was closed mid-write) ends the list.
pub fn parse(d: &[u8]) -> Result<Vec<Record>, RecordingError> {
    let v3 = d.starts_with(MAGIC_V3);
    let v2 = d.starts_with(MAGIC_V2) || v3;
    if !d.starts_with(MAGIC) && !v2 {
        return Err(RecordingError::NotARecording);
    }
    let mut out = Vec::new();
    let mut at = MAGIC.len();
    let u16_at = |a: usize| d.get(a..a + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |a: usize| d.get(a..a + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    while at < d.len() {
        let tag = d[at];
        let rec = match tag {
            1 => u32_at(at + 1).map(|f| (Record::Frame(f), 5)),
            2 => (|| {
                let worker = u16_at(at + 1)?;
                let level = *d.get(at + 3)?;
                let (depth, at) = if v2 { (Some(*d.get(at + 4)?), at + 1) } else { (None, at) };
                let extra = usize::from(v2);
                let pc = u32_at(at + 4)?;
                let op = *d.get(at + 8)?;
                if op & 0x80 != 0 {
                    let v = u16_at(at + 9)?;
                    Some((
                        Record::Instruction { worker, level, pc, op: op & 0x7F, operand: Some(v), depth },
                        11 + extra,
                    ))
                } else {
                    Some((Record::Instruction { worker, level, pc, op, operand: None, depth }, 9 + extra))
                }
            })(),
            3 => (|| Some((Record::Random { stream: *d.get(at + 1)?, value: u32_at(at + 2)? }, 6)))(),
            4 => (|| {
                let index = u32_at(at + 1)? as i32;
                let n = usize::from(u16_at(at + 5)?);
                let path = String::from_utf8_lossy(d.get(at + 7..at + 7 + n)?).into_owned();
                Some((Record::Event { index, path }, 7 + n))
            })(),
            5 => (|| {
                Some((
                    Record::MapChange { target: u32_at(at + 1)? as i32, entrance: u32_at(at + 5)? as i32 },
                    9,
                ))
            })(),
            6 => (|| {
                Some((
                    Record::NativeResult {
                        worker: u16_at(at + 1)?,
                        native: u16_at(at + 3)?,
                        tag: *d.get(at + 5)?,
                        value: u32_at(at + 6)?,
                    },
                    10,
                ))
            })(),
            7 => (|| {
                Some((
                    Record::NativePoll {
                        worker: u16_at(at + 1)?,
                        native: u16_at(at + 3)?,
                        flags: *d.get(at + 5)?,
                    },
                    6,
                ))
            })(),
            8 => (|| {
                let n = usize::from(u16_at(at + 1)?);
                Some((Record::Input(d.get(at + 3..at + 3 + n)?.to_vec()), 3 + n))
            })(),
            9 => (|| {
                let n = usize::from(u16_at(at + 1)?);
                let mut v = Vec::with_capacity(n);
                for k in 0..n {
                    let o = at + 3 + k * 23;
                    let f = |i: usize| u32_at(o + 3 + 4 * i).map(f32::from_bits);
                    v.push(WorkerPosition {
                        worker: u16_at(o)?,
                        kind: *d.get(o + 2)?,
                        pos: [f(0)?, f(1)?, f(2)?],
                        pitch: f(3)?,
                        yaw: f(4)?,
                    });
                }
                Some((Record::Positions(v), 3 + n * 23))
            })(),
            0x0A => (|| {
                let f = |i: usize| u32_at(at + 1 + 4 * i).map(f32::from_bits);
                Some((Record::Camera([f(0)?, f(1)?, f(2)?, f(3)?], [f(4)?, f(5)?, f(6)?, f(7)?]), 33))
            })(),
            0x0B => (|| {
                Some((
                    Record::Battle {
                        active: *d.get(at + 1)? != 0,
                        pending: *d.get(at + 2)?,
                        area: u16_at(at + 3)?,
                        group: *d.get(at + 5)?,
                        formation: *d.get(at + 6)?,
                    },
                    7,
                ))
            })(),
            t => return Err(RecordingError::UnknownRecord(t, at)),
        };
        let Some((r, n)) = rec else { break };
        out.push(r);
        at += n;
    }
    Ok(out)
}

/// Write records in the recording format (for tests and for recording Aerugo's own runs).
pub fn encode(records: &[Record]) -> Vec<u8> {
    let mut d = MAGIC.to_vec();
    for r in records {
        match r {
            Record::Frame(f) => {
                d.push(1);
                d.extend(f.to_le_bytes());
            }
            Record::Instruction { worker, level, pc, op, operand, .. } => {
                d.push(2);
                d.extend(worker.to_le_bytes());
                d.push(*level);
                d.extend(pc.to_le_bytes());
                match operand {
                    Some(v) => {
                        d.push(op | 0x80);
                        d.extend(v.to_le_bytes());
                    }
                    None => d.push(*op),
                }
            }
            Record::Random { stream, value } => {
                d.push(3);
                d.push(*stream);
                d.extend(value.to_le_bytes());
            }
            Record::Event { index, path } => {
                d.push(4);
                d.extend(index.to_le_bytes());
                d.extend((path.len() as u16).to_le_bytes());
                d.extend(path.as_bytes());
            }
            Record::NativeResult { .. }
            | Record::NativePoll { .. }
            | Record::Input(_)
            | Record::Positions(_)
            | Record::Camera(..)
            | Record::Battle { .. } => {}
            Record::MapChange { target, entrance } => {
                d.push(5);
                d.extend(target.to_le_bytes());
                d.extend(entrance.to_le_bytes());
            }
        }
    }
    d
}

/// One event's stretch of a recording: its start, then per frame the instructions and random numbers.
#[derive(Debug, Clone, Default)]
pub struct EventRun {
    pub index: i32,
    pub path: String,
    /// Frame number of the recording where the event started.
    pub start_frame: u32,
    pub frames: Vec<Vec<TraceEvent>>,
    /// Values of random stream 2 (the scripts' stream), in order.
    pub random: Vec<u32>,
    pub map_changes: Vec<(u32, i32, i32)>,
    /// What the original's natives reported, per (worker, native), in order.
    pub replay: crate::natives::Replay,
    /// Worker positions recorded in each frame (version 3; empty before).
    pub positions: Vec<Vec<WorkerPosition>>,
    /// Input recorded at the start of each frame (version 3; empty before): the scripts' pad words, then pad 0.
    pub inputs: Vec<Vec<u8>>,
    /// The active camera's two points per frame (recordings that have them).
    pub cameras: Vec<Option<([f32; 4], [f32; 4])>>,
    /// Battle state changes: (frame in the run, in a battle).
    pub battles: Vec<(u32, bool)>,
}

/// Split a recording into event runs. Each frame record starts a frame (the recorder writes it when the frame
/// begins); instructions before a run's first frame record get a frame of their own.
pub fn event_runs(records: &[Record]) -> Vec<EventRun> {
    let mut runs: Vec<EventRun> = Vec::new();
    let mut frame = 0u32;
    for r in records {
        match r {
            Record::Frame(f) => {
                frame = *f;
                if let Some(run) = runs.last_mut() {
                    run.frames.push(Vec::new());
                }
            }
            Record::Event { index, path } => runs.push(EventRun {
                index: *index,
                path: path.clone(),
                start_frame: frame,
                ..EventRun::default()
            }),
            Record::Instruction { worker, level, pc, op, operand, .. } => {
                if let Some(run) = runs.last_mut() {
                    if run.frames.is_empty() {
                        run.frames.push(Vec::new());
                    }
                    let f = (run.frames.len() - 1) as u64;
                    run.frames.last_mut().expect("just ensured").push(TraceEvent {
                        frame: f,
                        worker: *worker,
                        level: *level,
                        pc: *pc,
                        op: *op,
                        operand: *operand,
                    });
                }
            }
            Record::Random { stream, value } => {
                if *stream == 2
                    && let Some(run) = runs.last_mut()
                {
                    run.random.push(*value);
                }
            }
            Record::NativeResult { worker, native, tag, value } => {
                if let Some(run) = runs.last_mut() {
                    run.replay.results.entry((*worker, *native)).or_default().push_back((*tag, *value));
                }
            }
            Record::NativePoll { worker, native, flags } => {
                if let Some(run) = runs.last_mut() {
                    run.replay.polls.entry((*worker, *native)).or_default().push_back(*flags);
                }
            }
            Record::Input(d) => {
                if let Some(run) = runs.last_mut() {
                    let f = run.frames.len().saturating_sub(1);
                    if run.inputs.len() <= f {
                        run.inputs.resize(f + 1, Vec::new());
                    }
                    run.inputs[f] = d.clone();
                }
            }
            Record::Battle { active, .. } => {
                if let Some(run) = runs.last_mut() {
                    let f = run.frames.len().saturating_sub(1) as u32;
                    run.battles.push((f, *active));
                }
            }
            Record::Camera(a, b) => {
                if let Some(run) = runs.last_mut() {
                    let f = run.frames.len().saturating_sub(1);
                    if run.cameras.len() <= f {
                        run.cameras.resize(f + 1, None);
                    }
                    run.cameras[f] = Some((*a, *b));
                }
            }
            Record::Positions(p) => {
                if let Some(run) = runs.last_mut() {
                    let f = run.frames.len().saturating_sub(1);
                    if run.positions.len() <= f {
                        run.positions.resize(f + 1, Vec::new());
                    }
                    run.positions[f] = p.clone();
                }
            }
            Record::MapChange { target, entrance } => {
                if let Some(run) = runs.last_mut() {
                    run.map_changes.push((frame, *target, *entrance));
                }
            }
        }
    }
    runs
}

/// Argument counts of natives measured from a version-2 recording: for each native call that finished, the
/// stack depth before the call minus the depth before the worker's next instruction (plus the pushed result).
/// Returns, per native id, every distinct count seen and how often.
pub fn measured_arity(
    records: &[Record],
) -> std::collections::BTreeMap<u16, std::collections::BTreeMap<i32, u32>> {
    use std::collections::{BTreeMap, HashMap};
    let mut out: BTreeMap<u16, BTreeMap<i32, u32>> = BTreeMap::new();
    // Last native call per (worker, level): id, opcode, depth before, pc.
    let mut pending: HashMap<(u16, u8), (u16, u8, u8, u32)> = HashMap::new();
    for r in records {
        let Record::Instruction { worker, level, pc, op, operand, depth: Some(depth) } = *r else { continue };
        let key = (worker, level);
        if let Some((id, call_op, before, call_pc)) = pending.remove(&key) {
            // A call still polling repeats at the same pc; count only once the worker moved on.
            if pc == call_pc && op == call_op {
                pending.insert(key, (id, call_op, before, call_pc));
                continue;
            }
            let pushed = i32::from(call_op == 0x35);
            *out.entry(id).or_default().entry(i32::from(before) - i32::from(depth) + pushed).or_default() +=
                1;
        }
        if matches!(op, 0x35 | 0x58)
            && let Some(id) = operand
        {
            pending.insert(key, (id, op, depth, pc));
        }
    }
    out
}

/// Where two instruction streams first differ.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    pub frame: usize,
    /// Index within the frame's instructions.
    pub index: usize,
    pub expected: Option<TraceEvent>,
    pub got: Option<TraceEvent>,
}

/// Compare frame by frame (frame numbers ignored); `None` when every frame matches.
pub fn first_divergence(expected: &[Vec<TraceEvent>], got: &[Vec<TraceEvent>]) -> Option<Divergence> {
    let key = |t: &TraceEvent| (t.worker, t.level, t.pc, t.op, t.operand);
    for (f, (e, g)) in expected.iter().zip(got).enumerate() {
        let n = e.len().max(g.len());
        for i in 0..n {
            let (a, b) = (e.get(i), g.get(i));
            if a.map(key) != b.map(key) {
                return Some(Divergence { frame: f, index: i, expected: a.copied(), got: b.copied() });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_records_parse() {
        let mut d = MAGIC_V3.to_vec();
        d.push(0x0A);
        for v in [1.0f32, 2.0, 3.0, 1.0, 4.0, 5.0, 6.0, 1.0] {
            d.extend(v.to_le_bytes());
        }
        assert_eq!(parse(&d).unwrap(), vec![Record::Camera([1.0, 2.0, 3.0, 1.0], [4.0, 5.0, 6.0, 1.0])]);
        let mut d = MAGIC_V3.to_vec();
        d.extend([0x0B, 1, 2, 0x34, 0x12, 1, 3]);
        assert_eq!(
            parse(&d).unwrap(),
            vec![Record::Battle { active: true, pending: 2, area: 0x1234, group: 1, formation: 3 }]
        );
    }

    #[test]
    fn parses_version_3_records() {
        let mut d = MAGIC_V3.to_vec();
        d.extend([6, 3, 0, 0x01, 0x10, 2]);
        d.extend(1.5f32.to_bits().to_le_bytes());
        d.extend([7, 3, 0, 0x00, 0x00, 5]);
        d.extend([8, 2, 0, 0xAA, 0xBB]);
        d.extend([9, 1, 0, 4, 0, 1]);
        for v in [1.0f32, -2.0, 3.0, 0.0, 0.5] {
            d.extend(v.to_bits().to_le_bytes());
        }
        let r = parse(&d).unwrap();
        assert_eq!(r[0], Record::NativeResult { worker: 3, native: 0x1001, tag: 2, value: 1.5f32.to_bits() });
        assert_eq!(r[1], Record::NativePoll { worker: 3, native: 0, flags: 5 });
        assert_eq!(r[2], Record::Input(vec![0xAA, 0xBB]));
        let Record::Positions(p) = &r[3] else { panic!() };
        assert_eq!((p[0].worker, p[0].kind, p[0].pos, p[0].yaw), (4, 1, [1.0, -2.0, 3.0], 0.5));
    }

    #[test]
    fn parses_and_splits() {
        let mut d = MAGIC.to_vec();
        d.extend([1, 7, 0, 0, 0]);
        d.extend([4, 3, 0, 0, 0, 2, 0, b'a', b'b']);
        d.extend([2, 5, 0, 1, 0x10, 0, 0, 0, 0x2E]);
        d.extend([2, 5, 0, 1, 0x11, 0, 0, 0, 0xAE, 0x34, 0x12]);
        d.extend([3, 2, 1, 0, 0, 0]);
        d.extend([1, 8, 0, 0, 0]);
        d.extend([5, 9, 0, 0, 0, 1, 0, 0, 0]);
        d.extend([2, 5]); // Cut off
        let recs = parse(&d).unwrap();
        assert_eq!(recs.len(), 7);
        let runs = event_runs(&recs);
        assert_eq!(runs.len(), 1);
        let r = &runs[0];
        assert_eq!((r.index, r.path.as_str(), r.start_frame), (3, "ab", 7));
        assert_eq!(r.frames.len(), 2);
        assert_eq!(r.frames[0].len(), 2);
        assert_eq!(r.frames[0][1].operand, Some(0x1234));
        assert_eq!(r.frames[0][1].op, 0x2E);
        assert_eq!(r.random, vec![1]);
        assert_eq!(r.map_changes, vec![(8, 9, 1)]);
        assert_eq!(first_divergence(&r.frames, &r.frames), None);
        let mut other = r.frames.clone();
        other[0][1].pc += 1;
        let d = first_divergence(&r.frames, &other).unwrap();
        assert_eq!((d.frame, d.index), (0, 1));
        assert!(parse(b"nope").is_err());
        assert_eq!(parse(&encode(&recs)).unwrap(), recs);
        // Version 2: depth before each instruction; a call taking 2 and pushing a result leaves depth 2 -> 1.
        let mut d = MAGIC_V2.to_vec();
        d.extend([2, 0, 0, 0, 2, 0, 0, 0, 0, 0xB5, 0x01, 0x10]); // Depth 2, call 0x1001
        d.extend([2, 0, 0, 0, 1, 3, 0, 0, 0, 0xA0, 0, 0]); // Depth 1 after
        let recs = parse(&d).unwrap();
        let m = measured_arity(&recs);
        assert_eq!(m[&0x1001].keys().copied().collect::<Vec<_>>(), vec![2]);
    }
}
