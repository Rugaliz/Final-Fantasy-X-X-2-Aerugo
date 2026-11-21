# Menu shapes

Message windows, choice cursors and menu frames are drawn from **shapes**: lists of rectangles cut from
one of three menu images. The shape tables are not stored in any data file; they are compiled into the
game executables. The PC `FFX.exe` and the PS2 disc's boot executable (checked against the Japanese
International disc, `SLPS_250.88`) contain identical copies. Aerugo reads them from the user's own
executable at run time (`aerugo_formats::shape`), either `FFX.exe` in the install folder or a PS2 disc
image (`aerugo_vfs::ps2`, which finds the executable through `SYSTEM.CNF`). Nothing is copied into this
repository.

## Table layout (little endian)

| Offset | Type | Meaning |
|---|---|---|
| 0 | u32 | shape count *n* |
| 4 | u32 × *n* | offset of each shape from the table start; the first is `4 + 4n`, shapes are packed back to back |

Each shape:

| Offset | Type | Meaning |
|---|---|---|
| 0 | u32 | piece count *c* |
| 4 | 12 bytes × *c* | pieces |

Each piece:

| Offset | Type | Meaning |
|---|---|---|
| 0 | u8 | u (texels in the PS2-size image) |
| 1 | u8 | v |
| 2 | s8 | x offset from the shape position |
| 3 | s8 | y offset |
| 4 | u16 | width |
| 6 | u16 | height |
| 8 | u16 | 1 = mirrored horizontally (left and right edges swapped) |
| 10 | u16 | 1 = mirrored vertically |

## The menu banks

Three tables follow each other (with a few bytes of alignment padding between them) and hold 85, 110 and 25
shapes. Aerugo finds them by that layout. Shape numbers used by the game code select a bank:

| Shape numbers | Bank | Image | PS2 image size |
|---|---|---|---|
| 0–198, 200–398 | first (85) | `meswin` | 256 × 256 |
| 400–598 | second (110) | `battle` | 256 × 256 |
| 600–649 | third (25) | `icon` | 256 × 128 |

The HD images (`menu*/d3d11/<name>.dds.phyre`) have the same layout at a higher resolution, so texture
coordinates are the PS2 texel positions divided by the PS2 image size.

The executables contain three more tables in the same layout (75, 7 and 7 shapes) used by another menu
module.
