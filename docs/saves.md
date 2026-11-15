# Saves

Scripts read and write the game's state by offset in the **save image**: the first global memory block of the
script machine (FFX: 0x68C0 bytes). A save is that image plus where to resume.

## Aerugo's container (`.aesv`)

| Field | Type |
|---|---|
| magic `AERUGOSV` | 8 bytes |
| version (1) | `u16` |
| game (0 FFX, 1 FFX-2) | `u8` |
| has position | `u8` |
| game time in ticks | `u64` |
| player position | 3 × `f32` |
| event script path | `u16` length + UTF-8 |
| save image | `u32` length + bytes |
| CRC-32 (IEEE) of everything before it | `u32` |

Saves are written through a temporary file and renamed, so an interrupted write never replaces a good save.
Loading checks the magic, the version and the checksum.

## Importing a Steam release save

A save file of the Steam release holds a 0x40-byte header followed by the save image. Import copies the image; the
place to resume is not read from the header yet.
