# Audio

The remaster's sounds are FMOD Designer projects: event files (`*.fev`, under `sound_pc/`) that refer to FSB5 sample
banks (`*.fsb`). Sound effects and voices exist per voice language (`us`, `jp`); music has two sets (`music`, and
the original soundtrack in `music_ps2`).

## FSB5

| Offset | Type | Meaning |
|---|---|---|
| 0x00 | `char[4]` | `FSB5` |
| 0x04 | `u32` | version (1; version 0 has a 0x40-byte header) |
| 0x08 | `u32` | sample count |
| 0x0C | `u32` | size of the sample headers |
| 0x10 | `u32` | size of the name table |
| 0x14 | `u32` | size of the data |
| 0x18 | `u32` | codec (7 = IMA ADPCM in every bank of both games) |

The header is 0x3C bytes; sample headers, the name table (one `u32` offset per sample, then strings) and the data
follow. Each sample header is a `u64`: bit 0 = extra chunks follow, bits 1–4 = rate index (4000, 8000, 11000, 11025,
16000, 22050, 24000, 32000, 44100, 48000, 96000 Hz), bits 5–6 = channels (1, 2, 6, 8), bits 7–33 = data offset /
32, bits 34–63 = length in frames. Extra chunks are a `u32` (bit 0 = another follows, bits 1–24 = size, bits 25–31
= type) and their data; type 1 = channel count, 2 = rate, 3 = loop start and end (inclusive).

## IMA ADPCM

Xbox-style blocks of 36 bytes per channel give 64 frames. Per channel, a 4-byte header (`i16` predictor, `u8` step
index, 1 unused byte) is the first frame; the 32 bytes of nibbles (low nibble first, channels interleaved every 4
bytes) give the next 63 frames; the last nibble of a block is unused. Each nibble *n* changes the predictor by
±((2 · (*n* & 7) + 1) · step) / 8 with the standard IMA step and index tables. This matches FFmpeg's
`adpcm_ima_xbox` decoder sample for sample on every bank checked.

## Event projects (FEV)

`*.fev` files are RIFF (`RIFF` … `FEV `) with a `LIST`/`PROJ` holding `OBCT`, `PROP` (project name), `LGCY` (the
older binary event data: banks, categories, groups, events, sound definitions and waveforms), `EPRP`, `STRR` and
`LANG`. `STRR` is the string table: a `u32` count, `u32` offsets, then NUL-terminated strings.

Both games use only *simple events*: each event has one sound definition, named
`/__simpleevent_sounddef__/<event>_<n>` in the string table, with one waveform whose bank sample has the event's
name. The string table lists the groups first (`voice`, `music`, `loop`, `nonloop`), then the events, then the
sound definitions. The project's banks are `<project>_bankNN.fsb` beside it. Voice and sound-effect projects
exist per voice language (`/us/`, `/jp/`, with the code also in the voice file names); FFX's music exists as the
remaster's arrangement (`music/ffx_music`) and the original soundtrack (`music_ps2/ffx_music_ps2`).

Event properties in `LGCY` (volume, looping per group, 3D settings) are not read yet.
