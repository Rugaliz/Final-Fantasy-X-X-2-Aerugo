//! Machine behaviour on small assembled scripts.

use std::sync::Arc;

use aerugo_formats::Game;
use aerugo_formats::script::Script;
use aerugo_script::asm::{Op, WorkerSrc, assemble};
use aerugo_script::{Natives, Phase, RequestMode, Value, Vm};

fn op(o: u8) -> Op {
    Op(o, None)
}
fn imm(v: i16) -> Op {
    Op(0x2E, Some(v as u16))
}
fn var_i32(area: u32, off: u32) -> (u32, u16) {
    (5 << 28 | area << 25 | off, 1)
}

const END: u8 = 0x3C;
const POPV: u8 = 0x20;
const PUSHV: u8 = 0x1F;

fn vm(workers: &[WorkerSrc]) -> Vm {
    let script = Script::parse(&assemble(workers)).expect("assembled script parses");
    Vm::new(Arc::new(script))
}

fn global(vm: &Vm, off: usize) -> i32 {
    i32::from_le_bytes(vm.memory.global[0][off..off + 4].try_into().unwrap())
}

#[test]
fn arithmetic_comparisons_and_variables() {
    // global[0] = (7 - 2) * 3 / 2 (int path); global[4] = 5 > 3; global[8] = 1.5 * 2.0 stored as int.
    let w = WorkerSrc {
        entries: vec![vec![
            imm(7),
            imm(2),
            op(0x15),
            imm(3),
            op(0x16),
            imm(2),
            op(0x17),
            Op(POPV, Some(0)),
            imm(5),
            imm(3),
            op(0x0A),
            Op(POPV, Some(1)),
            Op(0x2F, Some(0)),
            Op(0x2F, Some(1)),
            op(0x16),
            Op(POPV, Some(2)),
            op(END),
        ]],
        floats: vec![1.5, 2.0],
        vars: vec![var_i32(0, 0), var_i32(0, 4), var_i32(0, 8)],
        kind: 1,
        ..Default::default()
    };
    let mut m = vm(&[w]);
    let mut natives = Natives::<()>::new(Game::Ffx);
    m.frame(&mut natives.host(&mut ()));
    assert_eq!((global(&m, 0), global(&m, 4), global(&m, 8)), (7, 1, 3));
    assert!(m.faults.is_empty(), "{:?}", m.faults);
    assert_eq!(m.phase, Phase::Running);
}

#[test]
fn branches_loop_until_done() {
    // count = 0; L0: count += 1; if count < 10 goto L0
    let w = WorkerSrc {
        entries: vec![vec![
            imm(0),
            Op(POPV, Some(0)),
            Op(PUSHV, Some(0)), // L0
            imm(1),
            op(0x14),
            Op(POPV, Some(0)),
            Op(PUSHV, Some(0)),
            imm(10),
            op(0x0E),
            Op(0x56, Some(0)), // pop into A, jump if non-zero
            op(END),
        ]],
        labels: vec![(0, 2)],
        vars: vec![var_i32(0, 0)],
        kind: 1,
        ..Default::default()
    };
    let mut m = vm(&[w]);
    let mut n = Natives::<()>::new(Game::Ffx);
    m.frame(&mut n.host(&mut ()));
    assert_eq!(global(&m, 0), 10);
}

#[test]
fn frame_wait_spans_frames() {
    // wait(3), then set a flag.
    let w = WorkerSrc {
        entries: vec![vec![imm(3), Op(0x58, Some(0x0000)), imm(1), Op(POPV, Some(0)), op(END)]],
        vars: vec![var_i32(0, 0)],
        kind: 1,
        ..Default::default()
    };
    let mut m = vm(&[w]);
    let mut n = Natives::<()>::new(Game::Ffx);
    // The count runs out on the third frame; a finished wait still ends the worker's frame, so the next
    // instruction runs on the fourth.
    for frame in 0..4 {
        assert_eq!(global(&m, 0), 0, "frame {frame}");
        m.frame(&mut n.host(&mut ()));
    }
    assert_eq!(global(&m, 0), 1);
}

#[test]
fn requests_wait_for_the_end() {
    // Worker 0's entry 1 asks worker 1 to run entry 2 at level 3, waits for it to end, then writes 1.
    // Worker 1's entry 2 waits two frames and writes 7.
    let requester = WorkerSrc {
        entries: vec![
            vec![op(END)],
            vec![imm(3), imm(1), imm(2), op(0x38), op(0x2A), imm(1), Op(POPV, Some(0)), op(END)],
        ],
        vars: vec![var_i32(0, 0)],
        kind: 1,
        ..Default::default()
    };
    let target = WorkerSrc {
        entries: vec![
            vec![op(END)],
            vec![op(END)],
            vec![imm(2), Op(0x58, Some(0)), imm(7), Op(POPV, Some(0)), op(END)],
        ],
        vars: vec![var_i32(0, 4)],
        kind: 1,
        ..Default::default()
    };
    let mut m = vm(&[requester, target]);
    let mut n = Natives::<()>::new(Game::Ffx);
    m.frame(&mut n.host(&mut ()));
    assert_eq!(m.phase, Phase::Running);
    assert!(m.request(None, 0, RequestMode::Plain, 1, 1));
    let mut seen = Vec::new();
    for _ in 0..8 {
        m.frame(&mut n.host(&mut ()));
        seen.push((global(&m, 0), global(&m, 4)));
    }
    assert!(m.faults.is_empty(), "{:?}", m.faults);
    let target_done = seen.iter().position(|&(_, t)| t == 7).expect("target ran");
    let requester_done = seen.iter().position(|&(r, _)| r == 1).expect("requester resumed");
    assert!(requester_done >= target_done, "{seen:?}");
    assert_eq!(seen.last(), Some(&(1, 7)));
    // While one request is queued, another at the same level is refused.
    assert!(m.request(None, 1, RequestMode::Plain, 2, 2));
    assert!(!m.request(None, 1, RequestMode::Plain, 2, 2));
}

#[test]
fn higher_levels_preempt_and_resume() {
    // Worker 0: entry 1 (level 1) counts frames into global[0] forever; entry 2 (level 5) writes 99 to global[4]
    // and ends; then entry 1 resumes counting.
    let w = WorkerSrc {
        entries: vec![
            vec![op(END)],
            vec![
                Op(PUSHV, Some(0)), // L0
                imm(1),
                op(0x14),
                Op(POPV, Some(0)),
                imm(1),
                Op(0x58, Some(0)),
                Op(0x30, Some(0)),
            ],
            vec![imm(99), Op(POPV, Some(1)), op(END)],
        ],
        labels: vec![(1, 0)],
        vars: vec![var_i32(0, 0), var_i32(0, 4)],
        kind: 1,
        ..Default::default()
    };
    let mut m = vm(&[w]);
    let mut n = Natives::<()>::new(Game::Ffx);
    m.frame(&mut n.host(&mut ()));
    m.request(None, 0, RequestMode::Plain, 1, 1);
    for _ in 0..5 {
        m.frame(&mut n.host(&mut ()));
    }
    let before = global(&m, 0);
    assert!(before >= 3, "counting: {before}");
    m.request(None, 0, RequestMode::Plain, 5, 2);
    for _ in 0..5 {
        m.frame(&mut n.host(&mut ()));
    }
    assert_eq!(global(&m, 4), 99);
    assert!(global(&m, 0) > before + 2, "resumed counting: {} vs {before}", global(&m, 0));
    assert_eq!(m.workers[0].level, 1);
    assert!(m.faults.is_empty(), "{:?}", m.faults);
}

#[test]
fn unimplemented_natives_keep_the_stack_balanced() {
    let natives = Natives::<()>::new(Game::Ffx);
    let id = (1..0x1000u16)
        .find(|&id| {
            aerugo_formats::script::arity(Game::Ffx, id).is_some_and(|a| a.0 == 2 && a.1 == 0)
                && !natives.implemented(id)
        })
        .expect("a two-argument native");
    let w = WorkerSrc {
        entries: vec![vec![imm(1), imm(2), Op(0x58, Some(id)), imm(9), Op(POPV, Some(0)), op(END)]],
        vars: vec![var_i32(0, 0)],
        kind: 1,
        ..Default::default()
    };
    let mut m = vm(&[w]);
    let mut n = Natives::<()>::new(Game::Ffx);
    m.frame(&mut n.host(&mut ()));
    assert_eq!(global(&m, 0), 9);
    assert!(m.faults.is_empty(), "{:?}", m.faults);
    assert!(n.coverage.missing.contains(&id));
    assert_eq!(Value::Float(2.5).int(), 2);
}
