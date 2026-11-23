# Magic files (effects)

The remaster ships each effect as a Windows DLL, `magicFiles/FFX/magic_NNNN.dll`. Aerugo reads only the
DLL's initialised data section (`.data`), which carries the effect's data in its PS2 layout, and its
read-only data (`.rdata`, for the particle programs' names). The DLL's code is
never run: each effect's small driver is ported to Rust, and it names where in the data its files start.

All values are little-endian. Offsets are relative, as described per table; the game turns them into pointers
on first use, and Aerugo keeps them as offsets.

## OEF2 container

An OEF2 file starts on a 16-byte boundary with a directory header. A second header follows at the offset held
in `+0x3c`; it has the same counts, and its tables hold the offsets of the entries, counted from the second
header.

| Offset field | Count field (u16) | Entries |
|---|---|---|
| `+0x04` | `+0x42` | slots |
| `+0x08` | `+0x44` | image records (0x20 bytes; `+0x10` set when present) |
| `+0x0c` | `+0x46` | palette records (0x10 bytes; u16 `+2` set when present) |
| `+0x10` | `+0x48` | slots |
| `+0x14` | `+0x4a` | slots |
| `+0x18` | `+0x4c` | 0x10-byte records (kind byte; `+0xc` a size, or an earlier record's by index when negative) |
| `+0x20` | `+0x50` | particle groups |
| `+0x24` | `+0x52` | slots |

The u16 at `+0x40` is the game's "already resolved" flag (0 in the file).

## Particle group

A particle group starts with its counts: u16 `+6` parts, `+8` draw packet records (0x20 bytes), `+0xa` a u32
list, `+0xc` 8-byte pairs. Its offset fields follow, counted from the group header: `+0x10` the parts list
(u32 offsets), then `+0x14`, `+0x18` and `+0x1c` for the other three lists.

A part record holds:

- `+0`: its length in frames · 0x1000.
- `+4`: a byte; when set, the part runs until its objects are gone.
- `+8`: the offset of its program index list.
- `+0xc`: the offset of its second list (the count first, then the entries).
- `+0x10`: the first sub-part definition.

Each definition's `+0` is the offset of the next one, counted from the part record. The last element of the
chain is a terminator. A definition holds its start time at `+0x10`, then the end times at `+0x14`, `+0x18`
and `+0x1c` that decide when an object restarts or goes. It also holds a priority (u16 `+0x24`), a count of
program references (u16 `+0x26`) and the references themselves from `+0x28`, 16 bytes each:

- `+0`: an index into the effect's program table.
- `+4`: u16, the key stride.
- `+8`: the track's offset.
- `+0xc`: the offset of two work offsets.

A track is a list of keys, `stride` bytes apart, each starting with its time. An object moves on to the next
key when that key's time equals its age.

## Program table

The driver's particle program table lies in `.data`: 0x28-byte entries whose first word is the address of the
program's name in `.rdata` ("pppAccele", "pppDrawMdl3", …), then the program's nine entry points (code, not
read). A part's sub-part definitions name programs by their index in this table. Aerugo reads the names and
runs its own port of each program.

## Drivers

Each file's driver (its overlay table: set-up, run, draw, …) is code and is ported per shape (files that share
a driver differ only in where their data lies: the OEF2 file, the program table and the work buffer). The
looping-part driver (files 662 and 711) keeps one part placed in front of the camera at (−16, −13, 259) with
scale 0.0916, restarts it whenever it ends, and runs it from a frame callback the event's bit 0 starts and bit 1
ends.
