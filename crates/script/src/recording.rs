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

use crate::vm::TraceEvent;

pub const MAGIC: &[u8; 8] = b"AEORACL1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    Frame(u32),
    Instruction { worker: u16, level: u8, pc: u32, op: u8, operand: Option<u16> },
    Random { stream: u8, value: u32 },
    Event { index: i32, path: String },
    MapChange { target: i32, entrance: i32 },
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
    if !d.starts_with(MAGIC) {
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
                let pc = u32_at(at + 4)?;
                let op = *d.get(at + 8)?;
                if op & 0x80 != 0 {
                    let v = u16_at(at + 9)?;
                    Some((Record::Instruction { worker, level, pc, op: op & 0x7F, operand: Some(v) }, 11))
                } else {
                    Some((Record::Instruction { worker, level, pc, op, operand: None }, 9))
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
            Record::Instruction { worker, level, pc, op, operand } => {
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventRun {
    pub index: i32,
    pub path: String,
    /// Frame number of the recording where the event started.
    pub start_frame: u32,
    pub frames: Vec<Vec<TraceEvent>>,
    /// Values of random stream 2 (the scripts' stream), in order.
    pub random: Vec<u32>,
    pub map_changes: Vec<(u32, i32, i32)>,
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
            Record::Instruction { worker, level, pc, op, operand } => {
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
            Record::MapChange { target, entrance } => {
                if let Some(run) = runs.last_mut() {
                    run.map_changes.push((frame, *target, *entrance));
                }
            }
        }
    }
    runs
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
    fn parses_and_splits() {
        let mut d = MAGIC.to_vec();
        d.extend([1, 7, 0, 0, 0]);
        d.extend([4, 3, 0, 0, 0, 2, 0, b'a', b'b']);
        d.extend([2, 5, 0, 1, 0x10, 0, 0, 0, 0x2E]);
        d.extend([2, 5, 0, 1, 0x11, 0, 0, 0, 0xAE, 0x34, 0x12]);
        d.extend([3, 2, 1, 0, 0, 0]);
        d.extend([1, 8, 0, 0, 0]);
        d.extend([5, 9, 0, 0, 0, 1, 0, 0, 0]);
        d.extend([2, 5]); // cut off
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
    }
}
