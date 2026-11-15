# Scripts

Both games drive maps, cutscenes and battles with bytecode scripts run by a small stack machine. Event scripts are
`*.ebp` files under `event/obj/<xx>/<map>/`; battle scripts are `battle/btl/<name>/<name>.bin` (encounters) and
`battle/mon/_m<n>/m<n>.bin` (monsters). Same-named files under the per-language `new_*` folders hold battle text, not
scripts. A few unused FFX-2 map scripts are empty files.

## Containers

An event script starts with `EV01` and a `u32` offset to the script (0x40). A battle file is a chunk container: a
`u32` chunk count (1–32) and `u32` chunk offsets; chunk 0 is the script. All offsets below are relative to the start
of the script.

| Offset | Type | Meaning |
|---|---|---|
| 0x2C | `u32` | end of the code (start of the next section; 0 when there is none) |
| 0x30 | `u32` | start of the code; instruction addresses are relative to it |
| 0x34 | `u16` | worker count |
| 0x38 | `u32[]` | worker descriptor offsets |

## Workers

A worker is one script entity (a character, the camera, a battle actor, a controller). Its descriptor:

| Offset | Type | Meaning |
|---|---|---|
| 0x00 | `u8` | kind |
| 0x02 | `u16` | variable count |
| 0x04 | `u16` | integer constant count |
| 0x06 | `u16` | float constant count |
| 0x08 | `u16` | entry count |
| 0x0A | `u16` | label count |
| 0x14 | `u32` | variable table: 8-byte entries |
| 0x18 | `u32` | integer constants (`i32`) |
| 0x1C | `u32` | float constants (`f32`) |
| 0x20 | `u32` | entry points (`u32` code offsets) |
| 0x24 | `u32` | jump labels (`u32` code offsets) |

A variable entry is a `u32` and a `u16` element count. The `u32` holds the element type in bits 28–31 (0 `u8`, 1
`i8`, 2 `u16`, 3 `i16`, 4 `u32`, 5 `i32`, 6 `f32`, 7 single bytes used as flags), the storage area in bits 25–27 (0 and 1 global blocks, 2–4 the worker's data areas, 5 its registers, 6 the script's shared data),
and the byte offset in that area in bits 0–23 (bit 24 is unused). Indexed accesses clamp the index to the element count.

## Instructions

One opcode byte; if bit 7 is set, a little-endian `u16` operand follows. Opcodes run 0x00–0x7A. The operand is,
depending on the opcode, a variable index, a label index, a native id, an index into the integer or float constants,
a signed 16-bit immediate, a worker index or a global index. Stack slots are tagged integer or float; arithmetic
promotes to float when either side is a float.

| Opcodes | Group |
|---|---|
| 0x01–0x1C | logic, comparison, bit and arithmetic operators |
| 0x1F–0x24, 0x27 | variable loads and stores, array elements, element addresses |
| 0x25–0x2C | the float, A and X registers, `dup` |
| 0x2D–0x2F | push integer constant, immediate, float constant |
| 0x30–0x32, 0x55–0x57 | jumps, conditional on the A register (optionally popped first) |
| 0x33, 0x34 | call another worker's code, return |
| 0x35, 0x58 | call a native (keep or drop its result) |
| 0x36–0x3B, 0x45–0x53, 0x77, 0x78 | requests: ask another worker to run an entry, optionally waiting for it to start or end |
| 0x3C–0x3F, 0x54 | end the current entry (optionally restart or continue at another entry) |
| 0x59–0x74 | worker registers: 4 integer and 10 float |

Natives are engine functions with ids `group << 12 | index`. A native may run over several frames (start, then poll
until done) and returns an integer or a float. Aerugo names natives in its own table, grouped as system, math,
menu, actor, camera, battle, map / particles, debug, movie, effect and dungeon; unnamed natives show as
`<group>.<id>`.

The disassembler follows control flow from every entry and label, so data embedded in the code is never decoded.
Every script of both games decodes without reaching an invalid opcode or a label out of range.
