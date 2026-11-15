# Video

Both games' movies are WebM files (`*.webm`, under `.../video/`): a Matroska container with one VP8 video track
(640×480 or 1280×720 at 29.97 frames per second) and, for story movies, one Vorbis audio track (48 kHz stereo).
Voiced movies exist once per voice language (`_us`, `_jp` suffixes).

## Container

Matroska is EBML: each element is an ID (1–4 bytes, the leading-zero count of the first byte gives the length,
marker bit kept) and a size (1–8 bytes, marker bit removed; all ones means unknown). Aerugo reads only what playback
needs:

| Element | ID | Use |
|---|---|---|
| Segment | `18538067` | top level; size may be unknown |
| Info / TimecodeScale / Duration | `1549A966` / `2AD7B1` / `4489` | time base (default 1 ms) and length |
| Tracks / TrackEntry | `1654AE6B` / `AE` | number `D7`, type `83` (1 video, 2 audio), codec `86`, private `63A2` |
| Video / PixelWidth / PixelHeight | `E0` / `B0` / `BA` | picture size |
| Cluster / Timecode | `1F43B675` / `E7` | cluster start time |
| SimpleBlock / BlockGroup / Block | `A3` / `A0` / `A1` | frames: track number (EBML size), `i16` relative time, flags |

Block flags bits 1–2 select lacing (none, Xiph, fixed, EBML). The Vorbis track's private data holds the three
Vorbis headers in Xiph lacing (count − 1, then 255-run sizes of all but the last).

## Decoding

VP8 pictures are decoded by libvpx (`vpx_codec_dec_init_ver` with the VP8 decoder interface, decoder ABI 12),
loaded at run time so building does not need it; when it is missing the movie player says so. Pictures come out as
I420 and are converted to RGBA on the CPU with BT.601 limited-range coefficients. Vorbis packets are decoded by
lewton. The first picture of a test movie is bit-exact with FFmpeg's decoder.

Playback uses the audio stream as the clock: the player decodes about 300 ms ahead, appends the audio to a
streaming mixer voice, and shows the latest picture whose time has passed. Movies without audio use wall time.
