# Data tables

The battle and menu data of both games ("kernel" files) lives in `battle/kernel/*.bin` (and, for Last Mission,
`lastmiss/kernel/*.bin`) under each language folder. A table is an array of fixed-size records, optionally followed
by a text pool. All values are little-endian. The folders also contain development leftovers (headers, lists)
and a few `.bin` files in other formats.

## FFX layout

| Offset | Type | Meaning |
|---|---|---|
| 0x00 | `i16` | number of segments (1–8) |
| 0x08 + 12·*i* | segment | see below |

Segment (12 bytes):

| Offset | Type | Meaning |
|---|---|---|
| 0x00 | `u16` | first record id |
| 0x02 | `u16` | last record id |
| 0x04 | `u16` | record size |
| 0x06 | `u16` | text pool offset, relative to the data offset |
| 0x08 | `u32` | data offset |

Record *id* starts at `data offset + (id − first id) × record size`. When the text pool offset is at least the
size of the records, the pool starts at `data offset + text pool offset`.

## FFX-2 layout

A 0x20-byte header; bytes 0x08–0x0F are zero.

| Offset | Type | Meaning |
|---|---|---|
| 0x10 | `u32` | last record id (ids start at 0) |
| 0x14 | `u32` | record size |
| 0x18 | `u32` | data size, `(last id + 1) × record size` |
| 0x1C | `u32` | data offset, always 0x20 |

The text pool follows the records.

## Text fields

Text fields are `u16` offsets into the pool, pointing at strings in the [text encoding](text.md). The files do not
say which fields are text. The viewer infers them: a `u16` field is text when, for at least 95% of records, it points
at the start of a string, and it takes enough distinct values.

## Languages

Tables are looked up like any localized file: `master/new_<xx>pc`, then `master/<xx>pc`, then the Japanese base
`master/jppc`. A table missing from a language folder falls back to the Japanese file, whose text must then be
decoded with the Japanese character list.
