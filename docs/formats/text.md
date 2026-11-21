# Text encoding

Both games store text as byte strings that index a per-language character list. Strings end with a `0x00` byte.

## Character lists

| Game | File |
|---|---|
| FFX | `ffx_ps2/ffx/master/jppc/ffx_encoding/ffxsjistbl_<cs>.bin` |
| FFX-2 | `ffx_ps2/ffx2/master/jppc/ffx2_encoding/ffx2sjistbl_<cs>.bin` |

`<cs>` is `us` (English, French, German, Italian, Spanish), `jp`, `kr`, `ch` (Traditional Chinese) or `cn`
(Simplified Chinese). Each file is plain UTF-8; character *n* is the *n*-th code point. The Latin list has 192
characters; the others have 1,000 to 2,100. A few lists are longer than the encoding can reach.

## Byte stream

| Lead byte | Bytes | Meaning |
|---|---|---|
| `00` | 1 | end of string |
| `01`, `02`, `05` | 1 | control code without argument |
| `03` | 1 | line break |
| `04` | 2–3 | extension prefix: the character that follows (one byte or a `2C`–`2F` pair) gets 1,040 added to its index |
| `06`–`2B` | 2 | control code with one argument byte |
| `2C`–`2F` | 2 | paged character: index = (lead − `2B`) × 208 + (second − `30`) |
| `30`–`FF` | 1 | character: index = byte − `30` |

So one byte reaches indices 0–207, a pair reaches up to 1,039 and the extension prefix up to 2,079. Only the
Chinese and Korean text uses the prefix.

Observed control codes include `0A` (colour, argument selects it), `12` and `13` (inserted values and names).
Their full meanings are documented as the message system is implemented.

## Irregularities in shipped data

- FFX-2's Latin text uses the byte `F0` (index 192), one past the end of the Latin list. It is a glyph that exists
  only in the game's font; the converter shows it as unmapped.
- A few FFX-2 Traditional Chinese strings contain a doubled prefix (`04 04 xx`). The game consumes such a sequence
  as one three-byte character, and so does the decoder (as a *malformed* token kept byte for byte).

## Font descriptions (`*.ftc`)

`FTCX` files (`menu/base.ftc` per language, extra pages per event) describe a font for the text layout; the HD
remaster draws the glyphs from its own textures (`menu_<lang>/base_ftc/d3d11/font_0_<page>` and `shadow_0_<page>`)
but takes positions and widths from these files (`aerugo_formats::ftc`).

| Offset | Type | Meaning |
|---|---|---|
| 0 | 4 bytes | `FTCX` |
| 4 | u16 | version (200 and later carry the width table) |
| 8 | u16 | font slot (0 Japanese base font, 4 base font of the other languages) |
| 0x10 | u32 | glyph count (width table length) |
| 0x20 | u32 | offset of the PS2 glyph image (empty in the PC files) |
| 0x24 | u32 | its size |
| 0x28 | u16 | PS2 image width (the base font: 128) |
| 0x2a | u16 | PS2 image height (234) |
| 0x30 | u32 | offset of the width table: one byte per glyph, in quarter texels |

Glyph `i` sits in a 14 × 18 cell at column `(i mod 18) / 2`, row `i / 18`, on page `i mod 2` (even glyphs on the
first texture, odd ones on the second). Texture coordinates are texel positions divided by the PS2 image size.
