//! The PS2 map's display objects and their keyframe animation, as the original runs them.
//!
//! The map object data of a PS2 map package (`mapout.vpa`, the section its `MAP1` header names at +0x14) holds the
//! map's display objects, keyframe tracks and texture animation frames. Tracks are attached to a display object or
//! a texture animation slot (by map setup and by scripts) and played in up to 64 keyframe slots once a frame,
//! after the scripts; the results move and turn map parts, set their morph weights and pick texture animation
//! frames. The clock counts 60 Hz ticks: two per main-loop frame.

/// A key of a track (0x80 bytes in the data).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Key {
    /// Bit 0: the track ends here (the slot stops).
    pub flags: u32,
    /// 0 linear, 1 cubic Hermite with the tangents, 2 the end value.
    pub interpolation: u16,
    /// Length in ticks.
    pub duration: u32,
    /// What the track drives (the first key's kind is the track's): 0 position, 1 rotation, 2 morph weights,
    /// 3 texture animation frame.
    pub kind: u32,
    pub tangent_start: [f32; 4],
    pub tangent_end: [f32; 4],
    pub start: [f32; 4],
    pub end: [f32; 4],
}

/// A display object (a movable map part): where it rests, its rotation order and the tracks map setup attaches.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayObject {
    pub rotation: [f32; 4],
    pub position: [f32; 4],
    /// Which order the rotation's angles apply in (0–5).
    pub rotation_order: u16,
    pub tracks: Vec<u16>,
}

/// A texture animation slot: the name of each frame's texture animation and the tracks map setup attaches.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TextureSlot {
    pub frames: Vec<String>,
    pub tracks: Vec<u32>,
}

/// The map object data.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MapObjects {
    pub objects: Vec<DisplayObject>,
    /// Keyframe tracks by index (type-8 tracks, which the original keeps apart, are not read yet).
    pub tracks: Vec<Option<Vec<Key>>>,
    pub texture_slots: Vec<TextureSlot>,
}

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    d.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]))
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    d.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn vec4_at(d: &[u8], at: usize) -> Option<[f32; 4]> {
    let mut v = [0.0; 4];
    for (i, x) in v.iter_mut().enumerate() {
        *x = f32::from_bits(u32_at(d, at + 4 * i)?);
    }
    Some(v)
}

fn key_at(d: &[u8], at: usize) -> Option<Key> {
    Some(Key {
        flags: u32_at(d, at)?,
        interpolation: u16_at(d, at + 2)?,
        duration: u32_at(d, at + 4)?,
        kind: u32_at(d, at + 8)?,
        tangent_start: vec4_at(d, at + 0x10)?,
        tangent_end: vec4_at(d, at + 0x20)?,
        start: vec4_at(d, at + 0x30)?,
        end: vec4_at(d, at + 0x40)?,
    })
}

impl MapObjects {
    /// Read the map object data of a PS2 map package: chunks of a 0x40-byte header (type, body size, …) and
    /// their body; type 0 a display object, 5 a keyframe track, 6 a texture animation frame.
    pub fn from_map_package(vpa: &[u8]) -> Option<MapObjects> {
        if !vpa.starts_with(b"MAP1") {
            return None;
        }
        let section = u32_at(vpa, 0x14)? as usize;
        if u32_at(vpa, section)? != 0x0021_4365 || u32_at(vpa, section + 4)? != 0x110 {
            return None;
        }
        let chunks = u32_at(vpa, section + 0xC)?;
        let mut m = MapObjects::default();
        // Tracks of either type take the next index unless the header names one.
        let mut next_track = 0usize;
        let mut at = section + 0x40;
        for _ in 0..chunks {
            let (kind, size) = (u32_at(vpa, at)?, u32_at(vpa, at + 4)? as usize);
            let body = at + 0x40;
            match kind {
                0 => {
                    let count = u32_at(vpa, body + 0x34)? as usize;
                    let tracks =
                        (0..count.min(4)).map(|i| u16_at(vpa, body + 0x38 + 2 * i)).collect::<Option<_>>()?;
                    m.objects.push(DisplayObject {
                        rotation: vec4_at(vpa, body + 0x10)?,
                        position: vec4_at(vpa, body + 0x20)?,
                        rotation_order: u16_at(vpa, body + 0x30)?,
                        tracks,
                    });
                }
                5 | 8 => {
                    let (index, keys) = if u32_at(vpa, at + 0x20)? == 0 {
                        (next_track, size >> if kind == 5 { 7 } else { 5 })
                    } else {
                        (u32_at(vpa, at + 0x24)? as usize, u32_at(vpa, at + 0x28)? as usize)
                    };
                    next_track += 1;
                    if m.tracks.len() <= index {
                        m.tracks.resize(index + 1, None);
                    }
                    if kind == 5 {
                        m.tracks[index] =
                            Some((0..keys).map(|k| key_at(vpa, body + 0x80 * k)).collect::<Option<_>>()?);
                    }
                }
                6 => {
                    let slot = u32_at(vpa, body)? as usize;
                    if m.texture_slots.len() <= slot {
                        m.texture_slots.resize(slot + 1, TextureSlot::default());
                    }
                    // The frame's texture record follows the header (+0x80); the remaster names the animation
                    // after its words +0x24 and +0x34.
                    let (a, b) = (u32_at(vpa, at + 0x80 + 0x24)?, u32_at(vpa, at + 0x80 + 0x34)?);
                    let name = format!(
                        "{:05}_{:02}_{:03}_{:03}",
                        a & 0xFFFF,
                        a >> 24,
                        b & 0xFFFF,
                        (b >> 16) & 0x7FF
                    );
                    let s = &mut m.texture_slots[slot];
                    s.frames.push(name);
                    let count = u32_at(vpa, at + 0x44)?;
                    if count != 0 {
                        s.tracks = (0..count.min(4) as usize)
                            .map(|i| u32_at(vpa, at + 0x48 + 4 * i))
                            .collect::<Option<_>>()?;
                    }
                }
                _ => {}
            }
            at = body + size;
        }
        Some(m)
    }

    fn track(&self, index: usize) -> Option<&[Key]> {
        self.tracks.get(index)?.as_deref().filter(|k| !k.is_empty())
    }
}

/// One of the 64 keyframe slots.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Slot {
    active: bool,
    /// Non-zero: play the track once, then free the slot.
    once: u16,
    kind: u16,
    target: u16,
    track: u16,
    key: u16,
    start: u32,
    now: u32,
    /// The current key (an index into the track), if any.
    current: Option<usize>,
}

/// What the keyframes set on a display object.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ObjectPose {
    pub position: Option<[f32; 4]>,
    /// The rotation's angles and the order they apply in.
    pub rotation: Option<([f32; 4], u16)>,
    pub morph: Option<[f32; 2]>,
}

/// The keyframe slots and what they last set.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Keyframes {
    slots: Vec<Slot>,
    count: u32,
    /// Per display object.
    pub poses: Vec<ObjectPose>,
    /// Per texture animation slot: the frame the keyframes last picked.
    pub texture_frames: Vec<Option<u32>>,
    /// The slots whose frame the keyframes set on the last update (passed on to the remaster's animations).
    pub texture_frames_set: Vec<bool>,
}

const SLOTS: usize = 64;

impl Keyframes {
    pub fn new(objects: &MapObjects) -> Keyframes {
        Keyframes {
            slots: vec![Slot::default(); SLOTS],
            count: 0,
            poses: vec![ObjectPose::default(); objects.objects.len()],
            texture_frames: vec![None; objects.texture_slots.len()],
            texture_frames_set: vec![false; objects.texture_slots.len()],
        }
    }

    /// Map setup: every display object's tracks, then every texture slot's (played on, from `clock`).
    pub fn setup(&mut self, objects: &MapObjects, clock: u32) {
        for (i, o) in objects.objects.iter().enumerate() {
            for &t in &o.tracks {
                self.attach_object(objects, i, usize::from(t), 0, clock);
            }
        }
        for (i, s) in objects.texture_slots.iter().enumerate() {
            for &t in &s.tracks {
                self.attach_texture(objects, i, t as usize, 0, clock);
            }
        }
    }

    fn free_slot(&self) -> Option<usize> {
        self.slots.iter().position(|s| !s.active)
    }

    /// Stop the slots playing `kind` on display object `target`.
    fn detach(&mut self, target: usize, kind: u16) {
        let mut seen = 0;
        for s in self.slots.iter_mut() {
            if !s.active {
                continue;
            }
            if usize::from(s.target) == target && s.kind == kind {
                *s = Slot { start: 0, now: 0, ..Slot::default() };
                self.count = self.count.wrapping_sub(1);
            }
            seen += 1;
            if self.count <= seen {
                return;
            }
        }
    }

    /// Attach track `track` to display object `target` (scripts 0198/0199 and map setup); `once` non-zero plays
    /// it once.
    pub fn attach_object(
        &mut self,
        objects: &MapObjects,
        target: usize,
        track: usize,
        once: u16,
        clock: u32,
    ) {
        let Some(keys) = objects.track(track) else { return };
        if target >= objects.objects.len() {
            return;
        }
        let Some(i) = self.free_slot() else { return };
        let kind = keys[0].kind as u16;
        self.detach(target, kind);
        self.slots[i] = Slot {
            active: true,
            once,
            kind,
            target: target as u16,
            track: track as u16,
            key: 0,
            start: clock,
            now: clock,
            current: Some(0),
        };
        self.count += 1;
    }

    /// Attach track `track` to texture animation slot `target`.
    pub fn attach_texture(
        &mut self,
        objects: &MapObjects,
        target: usize,
        track: usize,
        once: u16,
        clock: u32,
    ) {
        let Some(keys) = objects.track(track) else { return };
        if target >= objects.texture_slots.len() {
            return;
        }
        let Some(i) = self.free_slot() else { return };
        self.slots[i] = Slot {
            active: true,
            once,
            kind: keys[0].kind as u16,
            target: target as u16,
            track: track as u16,
            key: 0,
            start: clock,
            now: clock,
            current: Some(0),
        };
        self.count += 1;
    }

    /// One frame: every active slot sets its target from the current key at the time it last advanced to, then
    /// advances to `clock`.
    pub fn update(&mut self, objects: &MapObjects, clock: u32) {
        self.texture_frames_set.iter_mut().for_each(|s| *s = false);
        // The original keeps the last share through slots without a current key.
        let mut t = 0.0f32;
        for i in 0..SLOTS {
            let s = self.slots[i];
            if !s.active {
                continue;
            }
            let keys = objects.track(usize::from(s.track)).unwrap_or(&[]);
            let key = s.current.and_then(|k| keys.get(k));
            if let Some(k) = key {
                let duration = k.duration as f32;
                t = if duration == 0.0 {
                    0.0
                } else {
                    (s.now.wrapping_sub(s.start) as f32 / duration).min(1.0)
                };
            }
            let target = usize::from(s.target);
            match (s.kind, key) {
                (0, Some(k)) if target < self.poses.len() => {
                    let mut p = vec4(k, t);
                    p[3] = 1.0;
                    self.poses[target].position = Some(p);
                    self.advance(objects, i, clock);
                }
                (1, Some(k)) if target < self.poses.len() => {
                    let mut r = vec4(k, t);
                    r[3] = 1.0;
                    let order = objects.objects[target].rotation_order;
                    self.poses[target].rotation = Some((r, order));
                    self.advance(objects, i, clock);
                }
                (2, Some(k)) if target < self.poses.len() => {
                    self.poses[target].morph = Some(vec2(k, t));
                    self.advance(objects, i, clock);
                }
                // (The original tests bit 16 of the slot's 16-bit flags here, which is never set.)
                (3, Some(k)) if target < self.texture_frames.len() => {
                    let frame = k.end[0] as i32;
                    if frame >= 0 && objects.texture_slots[target].frames.get(frame as usize).is_some() {
                        self.texture_frames[target] = Some(frame as u32);
                        self.texture_frames_set[target] = true;
                    }
                    self.advance(objects, i, clock);
                }
                _ => {}
            }
        }
    }

    /// Move slot `i` on to `clock`: through the keys whose time is over, looping or ending the track.
    fn advance(&mut self, objects: &MapObjects, i: usize, clock: u32) {
        let keys = objects.track(usize::from(self.slots[i].track)).unwrap_or(&[]);
        let s = &mut self.slots[i];
        if clock < s.start {
            s.start = clock;
        }
        s.now = clock;
        let Some(mut k) = s.current else { return };
        let mut start = s.start;
        let Some(mut key) = keys.get(k) else { return };
        if clock < key.duration.wrapping_add(start) {
            return;
        }
        loop {
            if key.flags & 1 != 0 {
                *s = Slot { start: clock, now: clock, ..Slot::default() };
                return;
            }
            s.key += 1;
            if usize::from(s.key) < keys.len() {
                s.start = key.duration.wrapping_add(start);
                k += 1;
                s.current = Some(k);
            } else if s.once != 0 {
                if s.current.and_then(|c| keys.get(c)).map_or(0, |c| c.duration).wrapping_add(s.start) < clock
                {
                    *s = Slot { start: clock, now: clock, ..Slot::default() };
                    self.count = self.count.wrapping_sub(1);
                }
                return;
            } else {
                s.start = key.duration.wrapping_add(start);
                k = 0;
                s.current = Some(0);
                s.key = 0;
            }
            key = &keys[k];
            if key.duration == 0 {
                return;
            }
            start = s.start;
            if clock < key.duration.wrapping_add(start) {
                return;
            }
        }
    }
}

/// A key's value at share `t` of its time (four components).
fn vec4(k: &Key, t: f32) -> [f32; 4] {
    let (a, b) = (k.start, k.end);
    match k.interpolation {
        0 => [
            a[0] + t * (b[0] - a[0]),
            (b[1] - a[1]) * t + a[1],
            (b[2] - a[2]) * t + a[2],
            (b[3] - a[3]) * t + a[3],
        ],
        1 => {
            let (m0, m1) = (k.tangent_start, k.tangent_end);
            let (h00, h01, h10, h11) = hermite(t);
            [
                h00 * a[0] + h01 * b[0] + h10 * m0[0] + h11 * m1[0],
                m1[1] * h11 + m0[1] * h10 + a[1] * h00 + b[1] * h01,
                m1[2] * h11 + m0[2] * h10 + a[2] * h00 + b[2] * h01,
                h10 * m0[3] + b[3] * h01 + a[3] * h00 + m1[3] * h11,
            ]
        }
        2 => b,
        // Other values leave the target as it was in the original; the slot still advances.
        _ => b,
    }
}

/// A key's first two components at share `t` (morph weights).
fn vec2(k: &Key, t: f32) -> [f32; 2] {
    let (a, b) = (k.start, k.end);
    match k.interpolation {
        0 => [(b[0] - a[0]) * t + a[0], (b[1] - a[1]) * t + a[1]],
        1 => {
            let (m0, m1) = (k.tangent_start, k.tangent_end);
            let (h00, h01, h10, h11) = hermite(t);
            [
                h00 * a[0] + h01 * b[0] + h10 * m0[0] + h11 * m1[0],
                h10 * m0[1] + b[1] * h01 + a[1] * h00 + m1[1] * h11,
            ]
        }
        _ => [b[0], b[1]],
    }
}

/// The cubic Hermite weights of the start, the end and the two tangents.
fn hermite(t: f32) -> (f32, f32, f32, f32) {
    let u = t - 1.0;
    ((t + t + 1.0) * u * u, (3.0 - (t + t)) * t * t, (1.0 - t) * (1.0 - t) * t, u * t * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(duration: u32, kind: u32, end: f32) -> Key {
        Key {
            flags: 0,
            interpolation: 0,
            duration,
            kind,
            tangent_start: [0.0; 4],
            tangent_end: [0.0; 4],
            start: [0.0; 4],
            end: [end, 0.0, 0.0, 0.0],
        }
    }

    fn frames() -> MapObjects {
        MapObjects {
            objects: Vec::new(),
            tracks: vec![Some((0..3).map(|i| key(5, 3, i as f32)).collect())],
            texture_slots: vec![TextureSlot { frames: vec!["a".into(); 3], tracks: vec![0] }],
        }
    }

    #[test]
    fn texture_frames_step_per_key_and_loop() {
        let m = frames();
        let mut k = Keyframes::new(&m);
        k.setup(&m, 0);
        let mut seen = Vec::new();
        for frame in 0..10u32 {
            k.update(&m, frame * 2);
            seen.push(k.texture_frames[0].unwrap());
        }
        // A key lasts 5 ticks (2.5 frames); the frame set is the key current before this frame's advance.
        assert_eq!(seen, [0, 0, 0, 0, 1, 1, 2, 2, 2, 0]);
    }

    /// bjyt02's map object data: 51 display objects, a 30-frame texture animation played by track 4 (keys of 5
    /// ticks, frames 0–29) and the turning and morphing objects.
    #[test]
    fn reads_a_maps_objects() {
        let Some(inst) = aerugo_vfs::install_from_env() else {
            eprintln!("skipped: set AERUGO_GAME_DIR");
            return;
        };
        let mut vfs = aerugo_vfs::Vfs::new();
        vfs.mount_install(&inst).unwrap();
        let vpa = vfs.read("ffx_ps2/ffx/master/jppc/map/bjyt/bjyt02/bin/mapout.vpa").unwrap();
        let m = MapObjects::from_map_package(&vpa).unwrap();
        assert_eq!(m.objects.len(), 51);
        assert_eq!(m.objects[49].tracks, [2]);
        let turn = m.track(2).unwrap();
        assert_eq!((turn.len(), turn[0].kind, turn[0].duration), (1, 1, 16000));
        assert_eq!(m.texture_slots.len(), 1);
        let slot = &m.texture_slots[0];
        assert_eq!(slot.frames.len(), 30);
        assert!(slot.frames.iter().all(|f| f == "12032_19_000_000"));
        assert_eq!(slot.tracks, [4]);
        let frames = m.track(4).unwrap();
        assert_eq!(frames.len(), 30);
        assert!(
            frames.iter().enumerate().all(|(i, k)| k.kind == 3 && k.duration == 5 && k.end[0] == i as f32)
        );
        let mut k = Keyframes::new(&m);
        k.setup(&m, 0);
        for frame in 0..40 {
            k.update(&m, frame * 2);
        }
        assert!(k.texture_frames[0].is_some());
        assert!(k.poses[49].rotation.is_some() && k.poses[0].morph.is_some());
    }

    #[test]
    fn once_tracks_free_their_slot() {
        let mut m = frames();
        m.texture_slots[0].tracks.clear();
        let mut k = Keyframes::new(&m);
        k.attach_texture(&m, 0, 0, 1, 0);
        assert_eq!(k.count, 1);
        for frame in 0..12u32 {
            k.update(&m, frame * 2);
        }
        assert_eq!(k.count, 0);
        assert!(k.slots.iter().all(|s| !s.active));
    }
}
