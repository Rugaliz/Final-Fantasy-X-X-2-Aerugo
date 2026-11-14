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
sequence (a small bytecode that orders motions) at +0x0C. Motion records (16 bytes): offset of the motion header
at +0x0C. Offsets are from the start of the file.

## Motion

| Offset | Type | Meaning |
|---|---|---|
| 0x00 | `u16` | frame count |
| 0x02 | `u16` | joint count |
| 0x04 | `u16` | rate (0x1E00 = 30 frames per second) |
| 0x06 | `u16` | event count |
| 0x08 | `u32` | channel-kind table offset (from the header) |
| 0x0C | `u32` | channel data offset (from the header) |
| 0x10 | `u32` | event table offset |

Each joint has nine channels: rotation X, Y, Z, translation X, Y, Z, scale X, Y, Z. The kind table holds two
bits per channel, 16 channels per `u32`: 0 = zero, 1 = one (4096), 2 = constant (one `i16` in the data), 3 =
animated. Values are in 1/4096 units: rotations in turns (wrapping at 4096), scales as factors, translations
multiplied by the model's scale (about 0.01 to reach HD units).

An animated channel's data starts with a `u16` byte count that includes itself, followed by a delta stream
starting from zero, one step per frame:

| Byte | Meaning |
|---|---|
| `0x00`–`0x7F` | 7-bit signed delta |
| `0x80`–`0xBF` | repeat the previous delta for this frame and the next (*byte* & 0x3F) frames |
| `0xC0`–`0xFF`, *b* | 14-bit signed delta: (*b* as signed) × 64 + (*byte* & 0x3F) |

A joint's local matrix is translation × Rz × Ry × Rx × scale, multiplied onto its parent's.
