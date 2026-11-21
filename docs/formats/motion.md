# Motions

Character and object animation comes from the original PS2 data: motion groups (`*.mgrp`) under
`ffx_ps2/<game>/master/jppc/chr/<kind>/<id>/mot/` (and `event/obj/…` for event objects). The HD skeletons use
the same joints: HD joint `SkeletonNode_<n>` is PS2 joint *n*. All values are little-endian.

## Motion group

| Offset | Type | Meaning |
|---|---|---|
| 0x0C | `u32` | offset of the group descriptor (empty groups are 16 bytes) |

Descriptor: `u16` at +4 character id, `i16` at +8 entry count, `i16` at +0x0A motion count, `u32` at +0x0C entry
table offset, `u32` at +0x10 motion table offset. Entries (16 bytes): motion number at +0, offset of its
sequence (a small bytecode that orders motions) at +0x0C and its jump table (`i16` offsets from the sequence) at
+0x08. Motion records (16 bytes): offset of the motion header at +0x0C and of a frame table (`i16` frame numbers
the sequence picks by index) at +0x08. Offsets are from the start of the file.

### Sequences

Numbers in a sequence are two bytes, low first, with the low byte sign-extended over the high one.

| Code | Arguments | Meaning |
|---|---|---|
| 0 | | end |
| 1 | record, plays, from, to | play a frame range of a motion record: from `frames[from]` to `frames[to]` (one frame more unless `to` or `plays` is 1), `plays` times (0: for ever) |
| 2 | | wait until the range is done |
| 3 | n | wait n frames |
| 4 | i | jump to the sequence start + `jumps[i]` |
| 5 | | stop |
| 6 | n | wait until the play count a script sets reaches n |

A motion id is the owning character's id in the high 16 bits and an entry number in the low ones.

## Motion

| Offset | Type | Meaning |
|---|---|---|
| 0x00 | `u16` | frame count |
| 0x02 | `u16` | joint count |
| 0x04 | `u16` | rate (0x1E00 = 30 frames per second) |
| 0x06 | `u16` | event count |
| 0x08 | `u32` | channel-kind table offset (from the header) |
| 0x0C | `u32` | channel data offset (from the header) |
| 0x10 | `u32` | event table offset (from the header) |

Events: `u8` kind (1 sound, 2 character callback, 3 face pattern, 5 effect), `u8` extra words, `i16` start frame,
`i16` end frame, `u16` id, then the extra words (4 bytes each).

Each joint has nine channels: rotation X, Y, Z, translation X, Y, Z, scale X, Y, Z. The kind table holds two
bits per channel, 16 channels per `u32`: 0 = zero, 1 = one (4096), 2 = constant (one `i16` in the data), 3 =
animated. Values are in 1/4096 units: rotations in turns (wrapping at 4096), scales as factors, translations
multiplied by the model's scale (the `.chr` info block's float at +0x20 × 0.001; 0.01 for nearly every character).

An animated channel's data starts with a `u16` byte count that includes itself, followed by a delta stream
starting from zero, one step per frame:

| Byte | Meaning |
|---|---|
| `0x00`–`0x7F` | 7-bit signed delta |
| `0x80`–`0xBF` | repeat the previous delta for this frame and the next (*byte* & 0x3F) frames |
| `0xC0`–`0xFF`, *b* | 14-bit signed delta: (*b* as signed) × 64 + (*byte* & 0x3F) |

A joint's local matrix is translation × Rz × Ry × Rx × scale, multiplied onto its parent's.
