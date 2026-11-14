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
