//! Movies (`*.webm`): VP8 video with Vorbis audio. [`Movie`] demuxes and decodes; the caller shows pictures by
//! presentation time and feeds the audio to the mixer.

pub mod vpx;
pub mod webm;

use std::collections::VecDeque;

use lewton::audio::{PreviousWindowRight, read_audio_packet};
use lewton::header::{IdentHeader, SetupHeader, read_header_ident, read_header_setup};

pub use vpx::{Picture, Vp8Decoder, VpxError};
pub use webm::{Chunked, Demuxer, Packet, Source, Track, WebmError};

#[derive(Debug, thiserror::Error)]
pub enum MovieError {
    #[error(transparent)]
    Webm(#[from] WebmError),
    #[error(transparent)]
    Vpx(#[from] VpxError),
    #[error("vorbis: {0}")]
    Vorbis(String),
    #[error("no video track")]
    NoVideo,
}

struct Vorbis {
    ident: IdentHeader,
    setup: SetupHeader,
    pwr: PreviousWindowRight,
}

/// A movie being decoded.
pub struct Movie<S: Source> {
    demux: Demuxer<S>,
    video_track: u64,
    audio_track: Option<u64>,
    vp8: Vp8Decoder,
    vorbis: Option<Vorbis>,
    pub width: u32,
    pub height: u32,
    pub audio_rate: u32,
    pub audio_channels: u16,
    pub duration_s: f64,
    frames: VecDeque<(i64, Picture)>,
    audio: Vec<f32>,
    ended: bool,
}

impl<S: Source> std::fmt::Debug for Movie<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Movie")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

impl<S: Source> Movie<S> {
    pub fn open(source: S) -> Result<Movie<S>, MovieError> {
        let demux = Demuxer::open(source)?;
        let video = demux
            .tracks
            .iter()
            .find(|t| t.kind == 1 && t.codec == "V_VP8")
            .ok_or(MovieError::NoVideo)?
            .clone();
        let audio = demux.tracks.iter().find(|t| t.kind == 2 && t.codec == "A_VORBIS").cloned();
        let vorbis = match &audio {
            Some(t) => {
                let h = webm::xiph_headers(&t.private);
                if h.len() < 3 {
                    return Err(MovieError::Vorbis("missing headers".into()));
                }
                let ident = read_header_ident(h[0]).map_err(|e| MovieError::Vorbis(format!("{e:?}")))?;
                let setup =
                    read_header_setup(h[2], ident.audio_channels, (ident.blocksize_0, ident.blocksize_1))
                        .map_err(|e| MovieError::Vorbis(format!("{e:?}")))?;
                Some(Vorbis { ident, setup, pwr: PreviousWindowRight::new() })
            }
            None => None,
        };
        let (audio_rate, audio_channels) = vorbis
            .as_ref()
            .map_or((0, 0), |v| (v.ident.audio_sample_rate, u16::from(v.ident.audio_channels)));
        Ok(Movie {
            duration_s: demux.duration_s,
            demux,
            video_track: video.number,
            audio_track: audio.map(|t| t.number),
            vp8: Vp8Decoder::new()?,
            vorbis,
            width: video.width,
            height: video.height,
            audio_rate,
            audio_channels,
            frames: VecDeque::new(),
            audio: Vec::new(),
            ended: false,
        })
    }

    pub fn has_audio(&self) -> bool {
        self.vorbis.is_some()
    }

    pub fn ended(&self) -> bool {
        self.ended && self.frames.is_empty()
    }

    /// Decode until a picture later than `time_ns` is queued (or the movie ends). Decoded audio is collected
    /// for [`Movie::take_audio`].
    pub fn fill(&mut self, time_ns: i64) -> Result<(), MovieError> {
        while !self.ended && self.frames.back().is_none_or(|(t, _)| *t <= time_ns) {
            let Some(p) = self.demux.next_packet()? else {
                self.ended = true;
                break;
            };
            if p.track == self.video_track {
                if let Some(pic) = self.vp8.decode(&p.data)? {
                    self.frames.push_back((p.time_ns, pic));
                }
            } else if Some(p.track) == self.audio_track
                && let Some(v) = &mut self.vorbis
            {
                let decoded = read_audio_packet(&v.ident, &v.setup, &p.data, &mut v.pwr)
                    .map_err(|e| MovieError::Vorbis(format!("{e:?}")))?;
                let frames = decoded.first().map_or(0, Vec::len);
                for f in 0..frames {
                    for ch in &decoded {
                        self.audio.push(f32::from(ch[f]) / 32768.0);
                    }
                }
            }
        }
        Ok(())
    }

    /// The latest picture at or before `time_ns`, dropping older ones.
    pub fn picture_at(&mut self, time_ns: i64) -> Option<Picture> {
        let mut latest = None;
        while self.frames.front().is_some_and(|(t, _)| *t <= time_ns) {
            latest = self.frames.pop_front().map(|(_, p)| p);
        }
        latest
    }

    /// Interleaved audio decoded since the last call.
    pub fn take_audio(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.audio)
    }
}
