//! FMOD sample banks (FSB5, `*.fsb`) and their IMA ADPCM codec (see `docs/formats/audio.md`).

use std::ops::Range;

const RATES: [u32; 11] = [4000, 8000, 11000, 11025, 16000, 22050, 24000, 32000, 44100, 48000, 96000];

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FsbError {
    #[error("not an FSB5 bank")]
    NotFsb5,
    #[error("file ends early ({0})")]
    Truncated(&'static str),
    #[error("unsupported codec {0}")]
    Codec(u32),
}

type Result<T> = std::result::Result<T, FsbError>;

/// Codec of a bank (`mode` field). The games use IMA ADPCM throughout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Pcm16,
    ImaAdpcm,
    Other(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    pub name: String,
    pub rate: u32,
    pub channels: u16,
    /// Length in sample frames.
    pub frames: u32,
    /// Loop start and end (inclusive) in sample frames.
    pub looping: Option<(u32, u32)>,
    /// Encoded bytes in the bank.
    pub data: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bank {
    pub codec: Codec,
    pub samples: Vec<Sample>,
}

fn u32_at(d: &[u8], at: usize) -> Result<u32> {
    d.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).ok_or(FsbError::Truncated("u32"))
}

impl Bank {
    /// Bytes needed to parse a bank's header (sample headers and names), given its first 0x40 bytes.
    pub fn header_len(first: &[u8]) -> Result<usize> {
        if first.get(0..4) != Some(b"FSB5") {
            return Err(FsbError::NotFsb5);
        }
        let header = if u32_at(first, 4)? == 0 { 0x40 } else { 0x3C };
        Ok(header + u32_at(first, 12)? as usize + u32_at(first, 16)? as usize)
    }

    /// Parse from the header bytes only (see [`Bank::header_len`]); `file_len` is the whole bank's size.
    /// Sample ranges are positions in the whole file.
    pub fn parse_header(head: &[u8], file_len: usize) -> Result<Bank> {
        Bank::parse_with_len(head, file_len)
    }

    /// Decode a sample from its encoded bytes (read separately, e.g. with a ranged read).
    pub fn decode_data(&self, sample: &Sample, data: &[u8]) -> Result<Vec<i16>> {
        match self.codec {
            Codec::ImaAdpcm => {
                Ok(decode_xbox_ima(data, sample.channels.max(1) as usize, sample.frames as usize))
            }
            Codec::Pcm16 => Ok(data
                .chunks_exact(2)
                .take(sample.frames as usize * sample.channels as usize)
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect()),
            Codec::Other(m) => Err(FsbError::Codec(m)),
        }
    }

    pub fn parse(d: &[u8]) -> Result<Bank> {
        Bank::parse_with_len(d, d.len())
    }

    fn parse_with_len(d: &[u8], file_len: usize) -> Result<Bank> {
        if d.get(0..4) != Some(b"FSB5") {
            return Err(FsbError::NotFsb5);
        }
        let version = u32_at(d, 4)?;
        let count = u32_at(d, 8)? as usize;
        let headers_size = u32_at(d, 12)? as usize;
        let names_size = u32_at(d, 16)? as usize;
        let data_size = u32_at(d, 20)? as usize;
        let mode = u32_at(d, 24)?;
        let codec = match mode {
            2 => Codec::Pcm16,
            7 => Codec::ImaAdpcm,
            other => Codec::Other(other),
        };
        let header = if version == 0 { 0x40 } else { 0x3C };
        let names_at = header + headers_size;
        let data_at = names_at + names_size;
        if data_at + data_size > file_len || data_at > d.len() || count > 1 << 20 {
            return Err(FsbError::Truncated("bank"));
        }
        let mut p = header;
        let mut raw = Vec::with_capacity(count);
        for _ in 0..count {
            let v = u64::from(u32_at(d, p)?) | (u64::from(u32_at(d, p + 4)?) << 32);
            p += 8;
            let mut next = v & 1 != 0;
            let rate = RATES.get((v >> 1 & 15) as usize).copied().unwrap_or(44100);
            let mut channels = [1u16, 2, 6, 8][(v >> 5 & 3) as usize];
            let offset = ((v >> 7) & ((1 << 27) - 1)) as usize * 32;
            let frames = (v >> 34) as u32;
            let mut looping = None;
            let mut rate_override = None;
            while next {
                let c = u32_at(d, p)?;
                p += 4;
                next = c & 1 != 0;
                let size = (c >> 1 & 0xFF_FFFF) as usize;
                match c >> 25 {
                    1 => channels = u16::from(*d.get(p).ok_or(FsbError::Truncated("chunk"))?),
                    2 => rate_override = Some(u32_at(d, p)?),
                    3 => looping = Some((u32_at(d, p)?, u32_at(d, p + 4)?)),
                    _ => {}
                }
                p += size;
            }
            raw.push((rate_override.unwrap_or(rate), channels, offset, frames, looping));
        }
        let mut samples = Vec::with_capacity(count);
        for (i, &(rate, channels, offset, frames, looping)) in raw.iter().enumerate() {
            let end = raw.get(i + 1).map_or(data_size, |r| r.2);
            let name = if names_size > 0 {
                let off = u32_at(d, names_at + 4 * i)? as usize;
                let s = d.get(names_at + off..data_at).unwrap_or(&[]);
                let n = s.iter().position(|&b| b == 0).unwrap_or(s.len());
                String::from_utf8_lossy(&s[..n]).into_owned()
            } else {
                format!("{i}")
            };
            if offset > end || end > data_size {
                return Err(FsbError::Truncated("sample data"));
            }
            samples.push(Sample {
                name,
                rate,
                channels,
                frames,
                looping,
                data: data_at + offset..data_at + end,
            });
        }
        Ok(Bank { codec, samples })
    }

    /// Decode a sample to interleaved 16-bit PCM (`frames × channels` values).
    pub fn decode(&self, bank: &[u8], sample: &Sample) -> Result<Vec<i16>> {
        let data = bank.get(sample.data.clone()).ok_or(FsbError::Truncated("sample data"))?;
        match self.codec {
            Codec::ImaAdpcm => {
                Ok(decode_xbox_ima(data, sample.channels.max(1) as usize, sample.frames as usize))
            }
            Codec::Pcm16 => Ok(data
                .chunks_exact(2)
                .take(sample.frames as usize * sample.channels as usize)
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect()),
            Codec::Other(m) => Err(FsbError::Codec(m)),
        }
    }
}

const INDEX_TABLE: [i32; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];
const STEP_TABLE: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88,
    97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658,
    724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272, 2499, 2749, 3024, 3327, 3660,
    4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493, 10442, 11487, 12635, 13899, 15289, 16818,
    18500, 20350, 22385, 24623, 27086, 29794, 32767,
];

/// One IMA step: difference = (2·magnitude + 1)·step / 8.
fn ima_step(nibble: u8, predictor: &mut i32, index: &mut i32) {
    let step = STEP_TABLE[*index as usize];
    let diff = ((2 * i32::from(nibble & 7) + 1) * step) >> 3;
    if nibble & 8 != 0 {
        *predictor -= diff;
    } else {
        *predictor += diff;
    }
    *predictor = (*predictor).clamp(-32768, 32767);
    *index = (*index + INDEX_TABLE[nibble as usize]).clamp(0, 88);
}

/// Xbox IMA ADPCM: blocks of 36 bytes per channel give 64 frames each. Per channel a 4-byte header
/// (predictor, step index) is the first frame; the 32 bytes of nibbles (interleaved per channel every 4 bytes,
/// low nibble first) give the next 63 frames, the last nibble of a block being unused. This matches FFmpeg's
/// Xbox IMA decoder sample for sample.
pub fn decode_xbox_ima(data: &[u8], channels: usize, frames: usize) -> Vec<i16> {
    const BLOCK: usize = 36;
    let mut out = vec![0i16; frames * channels];
    let block_bytes = BLOCK * channels;
    let mut frame = 0usize;
    for block in data.chunks(block_bytes) {
        if frame >= frames || block.len() < 4 * channels {
            break;
        }
        for ch in 0..channels {
            let h = &block[4 * ch..4 * ch + 4];
            let mut predictor = i32::from(i16::from_le_bytes([h[0], h[1]]));
            let mut index = i32::from(h[2]).clamp(0, 88);
            if frame < frames {
                out[frame * channels + ch] = predictor as i16;
            }
            let body = &block[4 * channels..];
            // 8 groups of 4 bytes per channel, interleaved.
            'groups: for group in 0..8 {
                let at = (group * channels + ch) * 4;
                let Some(bytes) = body.get(at..at + 4) else { break };
                for (k, &byte) in bytes.iter().enumerate() {
                    for (half, nibble) in [byte & 0x0F, byte >> 4].into_iter().enumerate() {
                        let n = group * 8 + k * 2 + half;
                        if n == 63 {
                            break 'groups;
                        }
                        ima_step(nibble, &mut predictor, &mut index);
                        let f = frame + 1 + n;
                        if f < frames {
                            out[f * channels + ch] = predictor as i16;
                        }
                    }
                }
            }
        }
        frame += 64;
    }
    out
}

/// A 16-bit PCM WAV file (for exports and comparisons).
pub fn wav_pcm16(rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
    let data_len = samples.len() * 2;
    let mut w = Vec::with_capacity(44 + data_len);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&channels.to_le_bytes());
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
    w.extend_from_slice(&(channels * 2).to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&(data_len as u32).to_le_bytes());
    for s in samples {
        w.extend_from_slice(&s.to_le_bytes());
    }
    w
}

/// A WAV file holding the encoded Xbox IMA data unchanged (format tag 0x0069), for reference decoders.
pub fn wav_xbox_ima(rate: u32, channels: u16, data: &[u8]) -> Vec<u8> {
    let block_align = 36 * channels;
    let mut w = Vec::with_capacity(48 + data.len());
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(40 + data.len() as u32).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&20u32.to_le_bytes());
    w.extend_from_slice(&0x0069u16.to_le_bytes());
    w.extend_from_slice(&channels.to_le_bytes());
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * u32::from(block_align) / 64).to_le_bytes());
    w.extend_from_slice(&block_align.to_le_bytes());
    w.extend_from_slice(&4u16.to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&64u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&(data.len() as u32).to_le_bytes());
    w.extend_from_slice(data);
    w
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn ima_block_decodes_header_and_nibbles() {
        // Mono block: predictor 1000, index 0; nibbles 0x7 then 0x8.
        let mut block = vec![0u8; 36];
        block[0..2].copy_from_slice(&1000i16.to_le_bytes());
        block[4] = 0x87;
        let out = decode_xbox_ima(&block, 1, 64);
        // Header first; step 7: nibble 7 adds (15·7)>>3 = 13, index -> 8 (step 16); nibble 8 subtracts 16>>3 = 2.
        assert_eq!(&out[..3], &[1000, 1013, 1011]);
        assert_eq!(out.len(), 64);
    }

    #[test]
    fn stereo_interleaves_every_four_bytes() {
        let mut block = vec![0u8; 72];
        block[0..2].copy_from_slice(&100i16.to_le_bytes());
        block[4..6].copy_from_slice(&(-100i16).to_le_bytes());
        let out = decode_xbox_ima(&block, 2, 64);
        // Zero nibbles add 7 >> 3 = 0 at index 0: values stay at the headers.
        assert_eq!(&out[..4], &[100, -100, 100, -100]);
    }

    #[test]
    fn rejects_other_files() {
        assert_eq!(Bank::parse(b"RIFF....").unwrap_err(), FsbError::NotFsb5);
    }

    proptest! {
        #[test]
        fn never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..512), ch in 1usize..4) {
            let mut b = bytes.clone();
            if b.len() > 4 { b[..4].copy_from_slice(b"FSB5"); }
            if let Ok(bank) = Bank::parse(&b) {
                for s in &bank.samples { let _ = bank.decode(&b, s); }
            }
            let _ = decode_xbox_ima(&bytes, ch, 200);
        }
    }
}
