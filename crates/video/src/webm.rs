//! A small WebM (Matroska) demuxer: tracks and the blocks of each cluster, read through a random-access
//! byte source so large movies need not be in memory.

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WebmError {
    #[error("not a WebM file")]
    NotWebm,
    #[error("bad element at {0}")]
    Bad(u64),
    #[error("read failed: {0}")]
    Read(String),
}

type Result<T> = std::result::Result<T, WebmError>;

/// Random-access input.
pub trait Source {
    fn len(&self) -> u64;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn read_at(&mut self, offset: u64, len: usize) -> std::result::Result<Vec<u8>, String>;
}

impl Source for Vec<u8> {
    fn len(&self) -> u64 {
        self.len() as u64
    }

    fn read_at(&mut self, offset: u64, len: usize) -> std::result::Result<Vec<u8>, String> {
        let a = (offset as usize).min(self.as_slice().len());
        let b = a.saturating_add(len).min(self.as_slice().len());
        Ok(self[a..b].to_vec())
    }
}

/// A source read in large chunks, for inputs where each read is expensive (compressed archives).
pub struct Chunked<F: FnMut(u64, usize) -> std::result::Result<Vec<u8>, String>> {
    read: F,
    len: u64,
    chunk: usize,
    cached: Option<(u64, Vec<u8>)>,
}

impl<F: FnMut(u64, usize) -> std::result::Result<Vec<u8>, String>> std::fmt::Debug for Chunked<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Chunked").field("len", &self.len).finish_non_exhaustive()
    }
}

impl<F: FnMut(u64, usize) -> std::result::Result<Vec<u8>, String>> Chunked<F> {
    pub fn new(len: u64, chunk: usize, read: F) -> Self {
        Chunked { read, len, chunk: chunk.max(4096), cached: None }
    }
}

impl<F: FnMut(u64, usize) -> std::result::Result<Vec<u8>, String>> Source for Chunked<F> {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&mut self, offset: u64, len: usize) -> std::result::Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(len);
        let mut pos = offset;
        let end = (offset + len as u64).min(self.len);
        while pos < end {
            let base = pos / self.chunk as u64 * self.chunk as u64;
            if self.cached.as_ref().is_none_or(|(b, _)| *b != base) {
                let data = (self.read)(base, self.chunk)?;
                self.cached = Some((base, data));
            }
            let (b, data) = self.cached.as_ref().expect("filled above");
            let from = (pos - b) as usize;
            if from >= data.len() {
                break;
            }
            let take = ((end - pos) as usize).min(data.len() - from);
            out.extend_from_slice(&data[from..from + take]);
            pos += take as u64;
        }
        Ok(out)
    }
}

const EBML: u32 = 0x1A45_DFA3;
const SEGMENT: u32 = 0x1853_8067;
const INFO: u32 = 0x1549_A966;
const TIMECODE_SCALE: u32 = 0x2A_D7B1;
const DURATION: u32 = 0x4489;
const TRACKS: u32 = 0x1654_AE6B;
const TRACK_ENTRY: u32 = 0xAE;
const TRACK_NUMBER: u32 = 0xD7;
const TRACK_TYPE: u32 = 0x83;
const CODEC_ID: u32 = 0x86;
const CODEC_PRIVATE: u32 = 0x63A2;
const VIDEO: u32 = 0xE0;
const PIXEL_WIDTH: u32 = 0xB0;
const PIXEL_HEIGHT: u32 = 0xBA;
const AUDIO: u32 = 0xE1;
const SAMPLING_FREQUENCY: u32 = 0xB5;
const CHANNELS: u32 = 0x9F;
const CLUSTER: u32 = 0x1F43_B675;
const TIMECODE: u32 = 0xE7;
const SIMPLE_BLOCK: u32 = 0xA3;
const BLOCK_GROUP: u32 = 0xA0;
const BLOCK: u32 = 0xA1;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Track {
    pub number: u64,
    /// 1 video, 2 audio.
    pub kind: u64,
    pub codec: String,
    pub private: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub rate: f64,
    pub channels: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub track: u64,
    /// Presentation time in nanoseconds.
    pub time_ns: i64,
    pub keyframe: bool,
    pub data: Vec<u8>,
}

/// Element header: id, size (None = unknown), data start.
fn header(buf: &[u8]) -> Option<(u32, Option<u64>, usize)> {
    let b0 = *buf.first()?;
    let id_len = b0.leading_zeros() as usize + 1;
    if id_len > 4 {
        return None;
    }
    let mut id = 0u32;
    for &b in buf.get(..id_len)? {
        id = id << 8 | u32::from(b);
    }
    let s0 = *buf.get(id_len)?;
    let size_len = s0.leading_zeros() as usize + 1;
    if size_len > 8 {
        return None;
    }
    let mut size = u64::from(s0) & ((1u64 << (8 - size_len)) - 1);
    let mut all_ones = size == (1u64 << (8 - size_len)) - 1;
    for &b in buf.get(id_len + 1..id_len + size_len)? {
        size = size << 8 | u64::from(b);
        all_ones &= b == 0xFF;
    }
    Some((id, (!all_ones).then_some(size), id_len + size_len))
}

fn uint(d: &[u8]) -> u64 {
    d.iter().take(8).fold(0, |a, &b| a << 8 | u64::from(b))
}

fn float(d: &[u8]) -> f64 {
    match d.len() {
        4 => f64::from(f32::from_be_bytes([d[0], d[1], d[2], d[3]])),
        8 => f64::from_be_bytes(d.try_into().expect("8 bytes")),
        _ => 0.0,
    }
}

/// Children of an in-memory master element: (id, data).
fn children(d: &[u8]) -> Vec<(u32, &[u8])> {
    let mut out = Vec::new();
    let mut p = 0;
    while let Some((id, size, hl)) = header(&d[p..]) {
        let size = size.unwrap_or((d.len() - p - hl) as u64) as usize;
        let start = p + hl;
        let end = (start + size).min(d.len());
        out.push((id, &d[start..end]));
        p = end;
        if p >= d.len() {
            break;
        }
    }
    out
}

/// A WebM file opened for sequential reading of its packets.
#[derive(Debug)]
pub struct Demuxer<S: Source> {
    source: S,
    pub tracks: Vec<Track>,
    /// Nanoseconds per timecode tick.
    pub timecode_scale: u64,
    pub duration_s: f64,
    segment_end: u64,
    /// Next element to read inside the segment.
    pos: u64,
    queue: std::collections::VecDeque<Packet>,
    segment_start: u64,
}

impl<S: Source> Demuxer<S> {
    pub fn open(mut source: S) -> Result<Demuxer<S>> {
        let head = source.read_at(0, 64).map_err(WebmError::Read)?;
        let (id, size, hl) = header(&head).ok_or(WebmError::NotWebm)?;
        if id != EBML {
            return Err(WebmError::NotWebm);
        }
        let mut pos = hl as u64 + size.ok_or(WebmError::Bad(0))?;
        let seg = source.read_at(pos, 16).map_err(WebmError::Read)?;
        let (id, size, hl) = header(&seg).ok_or(WebmError::Bad(pos))?;
        if id != SEGMENT {
            return Err(WebmError::NotWebm);
        }
        pos += hl as u64;
        let segment_start = pos;
        let segment_end = size.map_or(source.len(), |s| (pos + s).min(source.len()));
        let mut d = Demuxer {
            source,
            tracks: Vec::new(),
            timecode_scale: 1_000_000,
            duration_s: 0.0,
            segment_end,
            pos,
            queue: Default::default(),
            segment_start,
        };
        d.read_headers()?;
        Ok(d)
    }

    /// Read elements up to the first cluster: info and tracks.
    fn read_headers(&mut self) -> Result<()> {
        let mut p = self.pos;
        while p < self.segment_end {
            let h = self.source.read_at(p, 16).map_err(WebmError::Read)?;
            let Some((id, size, hl)) = header(&h) else { return Err(WebmError::Bad(p)) };
            if id == CLUSTER {
                self.pos = p;
                return Ok(());
            }
            let size = size.ok_or(WebmError::Bad(p))?;
            if id == INFO || id == TRACKS {
                let body = self.source.read_at(p + hl as u64, size as usize).map_err(WebmError::Read)?;
                if id == INFO {
                    let mut duration = 0.0;
                    for (cid, cd) in children(&body) {
                        match cid {
                            TIMECODE_SCALE => self.timecode_scale = uint(cd).max(1),
                            DURATION => duration = float(cd),
                            _ => {}
                        }
                    }
                    self.duration_s = duration * self.timecode_scale as f64 / 1e9;
                } else {
                    for (cid, cd) in children(&body) {
                        if cid == TRACK_ENTRY {
                            self.tracks.push(parse_track(cd));
                        }
                    }
                }
            }
            p += hl as u64 + size;
        }
        self.pos = p;
        Ok(())
    }

    /// Restart from the first cluster.
    pub fn rewind(&mut self) -> Result<()> {
        self.queue.clear();
        self.pos = self.segment_start;
        self.tracks.clear();
        self.read_headers()
    }

    /// The next packet of any track, in file order.
    pub fn next_packet(&mut self) -> Result<Option<Packet>> {
        loop {
            if let Some(p) = self.queue.pop_front() {
                return Ok(Some(p));
            }
            if self.pos >= self.segment_end {
                return Ok(None);
            }
            let h = self.source.read_at(self.pos, 16).map_err(WebmError::Read)?;
            let Some((id, size, hl)) = header(&h) else { return Err(WebmError::Bad(self.pos)) };
            let size = size.unwrap_or(self.segment_end - self.pos - hl as u64);
            if id == CLUSTER {
                let body =
                    self.source.read_at(self.pos + hl as u64, size as usize).map_err(WebmError::Read)?;
                self.read_cluster(&body);
            }
            self.pos += hl as u64 + size;
        }
    }

    fn read_cluster(&mut self, body: &[u8]) {
        let mut base = 0i64;
        for (id, d) in children(body) {
            match id {
                TIMECODE => base = uint(d) as i64,
                SIMPLE_BLOCK => self.read_block(d, base, None),
                BLOCK_GROUP => {
                    for (gid, gd) in children(d) {
                        if gid == BLOCK {
                            self.read_block(gd, base, Some(false));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn read_block(&mut self, d: &[u8], cluster_time: i64, keyframe: Option<bool>) {
        let Some(&b0) = d.first() else { return };
        let n = b0.leading_zeros() as usize + 1;
        if n > 8 || d.len() < n + 3 {
            return;
        }
        let track = uint(&d[..n]) & ((1u64 << (8 * n - n)) - 1);
        let rel = i64::from(i16::from_be_bytes([d[n], d[n + 1]]));
        let flags = d[n + 2];
        let time_ns = (cluster_time + rel) * self.timecode_scale as i64;
        let key = keyframe.unwrap_or(flags & 0x80 != 0);
        let body = &d[n + 3..];
        for frame in unlace(body, flags >> 1 & 3) {
            self.queue.push_back(Packet { track, time_ns, keyframe: key, data: frame.to_vec() });
        }
    }
}

fn parse_track(d: &[u8]) -> Track {
    let mut t = Track::default();
    for (id, v) in children(d) {
        match id {
            TRACK_NUMBER => t.number = uint(v),
            TRACK_TYPE => t.kind = uint(v),
            CODEC_ID => t.codec = String::from_utf8_lossy(v).into_owned(),
            CODEC_PRIVATE => t.private = v.to_vec(),
            VIDEO => {
                for (vid, vv) in children(v) {
                    match vid {
                        PIXEL_WIDTH => t.width = uint(vv) as u32,
                        PIXEL_HEIGHT => t.height = uint(vv) as u32,
                        _ => {}
                    }
                }
            }
            AUDIO => {
                for (aid, av) in children(v) {
                    match aid {
                        SAMPLING_FREQUENCY => t.rate = float(av),
                        CHANNELS => t.channels = uint(av) as u32,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    t
}

/// Split a block's frames according to its lacing (0 none, 1 Xiph, 2 fixed, 3 EBML).
fn unlace(body: &[u8], lacing: u8) -> Vec<&[u8]> {
    if lacing == 0 || body.is_empty() {
        return vec![body];
    }
    let count = body[0] as usize + 1;
    let mut p = 1;
    let mut sizes = Vec::with_capacity(count);
    match lacing {
        1 => {
            for _ in 0..count - 1 {
                let mut s = 0usize;
                while let Some(&b) = body.get(p) {
                    p += 1;
                    s += b as usize;
                    if b != 255 {
                        break;
                    }
                }
                sizes.push(s);
            }
        }
        2 => {
            let each = (body.len() - 1) / count;
            sizes = vec![each; count - 1];
        }
        _ => {
            // EBML lacing: first size as a vint, then signed differences.
            let read_vint = |p: &mut usize| -> Option<(u64, usize)> {
                let b0 = *body.get(*p)?;
                let n = b0.leading_zeros() as usize + 1;
                if n > 8 {
                    return None;
                }
                let v = uint(body.get(*p..*p + n)?) & ((1u64 << (7 * n)) - 1);
                *p += n;
                Some((v, n))
            };
            let Some((first, _)) = read_vint(&mut p) else { return vec![body] };
            sizes.push(first as usize);
            let mut last = first as i64;
            for _ in 1..count - 1 {
                let Some((raw, n)) = read_vint(&mut p) else { return vec![body] };
                let bias = (1i64 << (7 * n - 1)) - 1;
                last += raw as i64 - bias;
                sizes.push(last.max(0) as usize);
            }
        }
    }
    let mut out = Vec::with_capacity(count);
    for s in sizes {
        let end = (p + s).min(body.len());
        out.push(&body[p..end]);
        p = end;
    }
    out.push(&body[p.min(body.len())..]);
    out
}

/// Split Xiph-laced codec private data (Vorbis headers) into its packets.
pub fn xiph_headers(private: &[u8]) -> Vec<&[u8]> {
    if private.is_empty() {
        return Vec::new();
    }
    let n = private[0] as usize + 1;
    let mut p = 1;
    let mut sizes = Vec::new();
    for _ in 0..n - 1 {
        let mut s = 0usize;
        while let Some(&b) = private.get(p) {
            p += 1;
            s += b as usize;
            if b != 255 {
                break;
            }
        }
        sizes.push(s);
    }
    let mut out = Vec::new();
    for s in sizes {
        let end = (p + s).min(private.len());
        out.push(&private[p..end]);
        p = end;
    }
    out.push(&private[p.min(private.len())..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn el(id: u32, body: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        let idb = id.to_be_bytes();
        let skip = idb.iter().position(|&b| b != 0).unwrap_or(3);
        v.extend_from_slice(&idb[skip..]);
        // 8-byte size.
        v.push(0x01);
        v.extend_from_slice(&(body.len() as u64).to_be_bytes()[1..]);
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn demuxes_tracks_and_blocks() {
        let mut track = el(TRACK_NUMBER, &[1]);
        track.extend(el(TRACK_TYPE, &[1]));
        track.extend(el(CODEC_ID, b"V_VP8"));
        track.extend(el(VIDEO, &[el(PIXEL_WIDTH, &[2, 0]), el(PIXEL_HEIGHT, &[1, 0])].concat()));
        let tracks = el(TRACKS, &el(TRACK_ENTRY, &track));
        let block = [&[0x81u8, 0x00, 0x10, 0x80][..], b"frame"].concat();
        let cluster = el(CLUSTER, &[el(TIMECODE, &[100]), el(SIMPLE_BLOCK, &block)].concat());
        let file = [el(EBML, &[]), el(SEGMENT, &[tracks, cluster].concat())].concat();
        let mut d = Demuxer::open(file).unwrap();
        assert_eq!(d.tracks[0].codec, "V_VP8");
        assert_eq!((d.tracks[0].width, d.tracks[0].height), (512, 256));
        let p = d.next_packet().unwrap().unwrap();
        assert_eq!((p.track, p.time_ns, p.keyframe), (1, 116 * 1_000_000, true));
        assert_eq!(p.data, b"frame");
        assert!(d.next_packet().unwrap().is_none());
    }

    #[test]
    fn xiph_lacing() {
        let private = [2u8, 3, 255, 1, b'a', b'b', b'c']
            .iter()
            .copied()
            .chain(std::iter::repeat_n(b'x', 256))
            .chain(*b"zz")
            .collect::<Vec<_>>();
        let h = xiph_headers(&private);
        assert_eq!(h[0], b"abc");
        assert_eq!(h[1].len(), 256);
        assert_eq!(h[2], b"zz");
    }

    proptest! {
        #[test]
        fn never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
            if let Ok(mut d) = Demuxer::open(bytes) {
                for _ in 0..64 {
                    match d.next_packet() { Ok(Some(_)) => {}, _ => break }
                }
            }
            let _ = unlace(&[3, 1, 2, 3, 4, 5, 6], 1);
        }
    }
}
