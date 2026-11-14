# VBF archives

The Steam release keeps its data in three archives under `data/`: `FFX_Data.vbf`, `FFX2_Data.vbf` and
`metamenu.vbf`. This description was worked out from the files themselves. All values are little-endian.

## Header

| Offset | Type | Meaning |
|---|---|---|
| 0x00 | `char[4]` | magic `SRYK` |
| 0x04 | `u32` | header length; the file data follows it |
| 0x08 | `u64` | number of entries *n* |
| 0x10 | `[u8; 16] × n` | one 16-byte hash per entry (not needed for reading) |
| … | `record × n` | entry records, 32 bytes each (below) |
| … | `u32` | length of the name table, **including** this length field |
| … | bytes | NUL-terminated paths |
| … | `u16 × …` | block table, up to the end of the header |

## Entry record (32 bytes)

| Offset | Type | Meaning |
|---|---|---|
| 0x00 | `u32` | index of the entry's first block in the block table |
| 0x04 | `u32` | same value repeated in every archive seen |
| 0x08 | `u64` | uncompressed size |
| 0x10 | `u64` | absolute file offset of the first stored block |
| 0x18 | `u64` | offset of the path inside the name table (after the length field) |

## Blocks

A file is split into 64 KiB blocks (the last one may be shorter). For each block the block table holds the
**stored** size; `0` means 65536. Stored blocks follow each other from the entry's offset.

- stored size == uncompressed block size → the block is stored raw
- otherwise → the block is a zlib stream that inflates to the uncompressed block size

## Notes

- Paths use forward slashes and are mostly lower case; lookups should be case-insensitive.
- Zero-length entries (placeholders and temporary files left in the archives) own no blocks, and their
  offset field contains unrelated values. Ignore it.
- The largest entries are videos of several gigabytes; read them in a streaming fashion.

## Language overlays

Localized data uses two path schemes inside the archives:

- A base tree `…/master/jppc/…` plus per-language replacement trees `…/master/new_<xx>pc/…` (and a few files
  in `…/master/<xx>pc/…`). Codes: `us`, `jp`, `de`, `fr`, `it`, `sp` (Spanish), `kr`, `cn`, `ch`.
- Folders with a language suffix, for example `map_2d_ui_us`. Here Spanish is `es`.

Aerugo asks for the base path (or a path with a `{lang}` placeholder for suffix folders) and tries the
language-specific variant first, then the fallbacks (`aerugo_vfs::lang`).

