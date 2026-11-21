//! Character face animation, as the original's PS2 texture animation player runs it.
//!
//! A character's `.chr` data may hold face animation data (in its texture block): up to 32 groups (eyes, mouth,
//! …), each with frame lists per pattern. Every frame the player steps each group of the patterns it plays and
//! reports (group, pattern, frame); the remaster shows a report as the sprite animation `G<group>` /
//! `Animation<pattern>` of the character at that frame. Groups run one of four modes: blinking (a random wait
//! of 60–149 frames, then the frame list at four and then one frame a step), frame sequences (each frame's hold
//! plus a random share of its jitter), a held frame, or scrolling.

/// A frame of a pattern: how long it holds (negative: for ever), the image and the random hold it may add.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub hold: i16,
    pub image: u8,
    pub jitter: u8,
}

/// One group of the data.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Group {
    /// The data's mode byte (0 blink, 1/3 held, 2 scroll, 4 sequence; 0xFF unused).
    pub mode: u8,
    /// Scroll steps (mode 2).
    pub scroll_step: [i32; 2],
    /// Frame lists by pattern.
    pub patterns: Vec<Vec<Frame>>,
}

/// A character's face animation data.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FaceData {
    /// The count byte of the data.
    pub count: u8,
    pub groups: Vec<Group>,
    /// Image sizes (width, height), by image.
    pub images: Vec<(i16, i16)>,
}

const GROUPS: usize = 32;
const PATTERNS: usize = 32;

impl FaceData {
    /// The face data of a `.chr` file: its texture block (header +0x18; offsets inside are relative to it) holds,
    /// from version 0x1029, a pointer at +0x20 to the data (type 2, version 1 read here).
    pub fn from_chr(d: &[u8]) -> Option<FaceData> {
        let u8_at = |o: usize| d.get(o).copied();
        let i16_at = |o: usize| d.get(o..o + 2).map(|b| i16::from_le_bytes([b[0], b[1]]));
        let u16_at = |o: usize| d.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
        let u32_at = |o: usize| d.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        let block = u32_at(0x18)? as usize;
        if block == 0 || u16_at(block + 0x10)? <= 0x1028 {
            return None;
        }
        let at = u32_at(block + 0x20)? as usize;
        if at == 0 {
            return None;
        }
        let base = block + at;
        if u8_at(base)? != 2 || u8_at(base + 2)? != 1 {
            return None;
        }
        let count = u8_at(base + 1)?;
        let images_at = base + u32_at(base + 4)? as usize;
        let groups_at = base + u32_at(base + 8)? as usize;
        let mut groups = Vec::with_capacity(GROUPS);
        let mut images = Vec::new();
        for g in 0..GROUPS {
            let mode = u8_at(base + 0xC + g)?;
            let e = groups_at + g * 0x110;
            let mut patterns = Vec::with_capacity(PATTERNS);
            if mode != 0xFF {
                for p in 0..PATTERNS {
                    let off = u32_at(e + 0x10 + p * 8)? as usize;
                    let n = i16_at(e + 0x14 + p * 8)?.max(0) as usize;
                    let frames = (0..n)
                        .map(|i| {
                            let f = base + off + 4 * i;
                            Some(Frame { hold: i16_at(f)?, image: u8_at(f + 2)?, jitter: u8_at(f + 3)? })
                        })
                        .collect::<Option<Vec<_>>>()?;
                    for f in &frames {
                        let i = usize::from(f.image);
                        if images.len() <= i {
                            images.resize(i + 1, (0, 0));
                        }
                    }
                    patterns.push(frames);
                }
            }
            let scroll_step = [u32_at(e)? as i32, u32_at(e + 4)? as i32];
            groups.push(Group { mode, scroll_step, patterns });
        }
        for (i, size) in images.iter_mut().enumerate() {
            let im = images_at + 12 * i;
            *size = (i16_at(im + 4).unwrap_or(0), i16_at(im + 6).unwrap_or(0));
        }
        Some(FaceData { count, groups, images })
    }

    fn frames(&self, g: usize, p: usize) -> &[Frame] {
        self.groups.get(g).and_then(|gr| gr.patterns.get(p)).map_or(&[], Vec::as_slice)
    }
}

/// One group's state.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GroupState {
    /// 0 sequence, 1 held, 2 blink, 3 scroll.
    pub mode: i16,
    pub frame: i32,
    /// No frames in the first pattern played.
    pub off: bool,
    acc: i16,
    jitter: i16,
    countdown: i16,
    phase: i16,
    scroll_step: [i32; 2],
    scroll: [i32; 2],
}

/// The MSVC C runtime's `rand` (the original's blink waits and holds use it).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CrtRand(pub u32);

impl CrtRand {
    pub fn rand(&mut self) -> i32 {
        self.0 = self.0.wrapping_mul(214_013).wrapping_add(2_531_011);
        ((self.0 >> 16) & 0x7FFF) as i32
    }
}

impl Default for CrtRand {
    fn default() -> CrtRand {
        CrtRand(1)
    }
}

/// A character's face animation player.
#[derive(Debug, Clone, PartialEq)]
pub struct Face {
    pub data: std::sync::Arc<FaceData>,
    /// Groups played (the data's count, or one past the last group in use).
    pub count: usize,
    pub groups: Vec<GroupState>,
    /// The pattern set (5078), 0 at the start.
    pub pattern: u8,
    /// Per pattern: bit 0 plays it after the others, bit 1 skips it.
    pub flags: [u8; 0x20],
    /// Frames a tick (1 in the field).
    pub step: i16,
}

/// What the player reports: group, pattern and frame.
pub type Report = (u8, u8, i32);

impl Face {
    /// A new player: every group's state from its mode byte.
    pub fn new(data: std::sync::Arc<FaceData>) -> Face {
        let mut groups = vec![GroupState::default(); GROUPS];
        let mut count = usize::from(data.count);
        let init = |s: &mut GroupState, mode: i16| {
            *s = GroupState { mode, ..GroupState::default() };
        };
        for g in 0..GROUPS {
            let mode = data.groups[g].mode;
            if mode == 0xFF {
                continue;
            }
            match mode {
                0 => init(&mut groups[g], 2),
                1 | 3 => {
                    init(&mut groups[g], 1);
                    Self::set_frame(&data, &mut groups, g, 1);
                }
                2 => {
                    init(&mut groups[g], 3);
                    groups[g].scroll_step = data.groups[g].scroll_step;
                }
                4 => init(&mut groups[g], 0),
                _ => {}
            }
            count = count.max(g + 1);
        }
        Face { data, count, groups, pattern: 0, flags: [0; 0x20], step: 1 }
    }

    /// Set a group's frame (group 1 sets it on every group whose mode byte is 1).
    fn set_frame(data: &FaceData, groups: &mut [GroupState], g: usize, frame: i32) {
        let reset = |s: &mut GroupState| {
            s.frame = frame;
            s.acc = 0;
            s.jitter = 0;
            s.countdown = 0;
            s.phase = 0;
            s.scroll = [0; 2];
            s.scroll_step = [0; 2];
        };
        if g != 1 {
            reset(&mut groups[g]);
        } else {
            for (i, s) in groups.iter_mut().enumerate() {
                if data.groups.get(i).is_some_and(|gr| gr.mode == 1) {
                    reset(s);
                }
            }
        }
    }

    /// Native 5078 with kind 4: set a pattern's flags; a new pattern (or flags 2) restarts every group
    /// and reports frame 0 of each.
    pub fn set_pattern(&mut self, pattern: u8, flags: u8, reports: &mut Vec<Report>) {
        if let Some(f) = self.flags.get_mut(usize::from(pattern)) {
            *f = flags;
        }
        if self.pattern == 0 || self.pattern != pattern || flags == 2 {
            self.pattern = pattern;
            for (g, s) in self.groups.iter_mut().enumerate() {
                s.scroll = [0; 2];
                s.frame = 0;
                s.acc = 0;
                s.jitter = 0;
                s.countdown = 0;
                s.phase = 0;
                reports.push((g as u8, pattern, 0));
            }
        }
    }

    /// One frame: the patterns 1, 2, 0x15 and the one set, those with flag bit 0 after the others.
    pub fn tick(&mut self, rand: &mut CrtRand, reports: &mut Vec<Report>) {
        let current = if matches!(self.pattern, 1 | 2 | 0x15) { 0xFF } else { self.pattern };
        let first = [1u8, 2, 0x15, current];
        let mut later = Vec::new();
        for (i, &p) in first.iter().enumerate() {
            if p == 0xFF {
                break;
            }
            let flags = self.flags.get(usize::from(p)).copied().unwrap_or(0);
            if flags & 2 != 0 {
                continue;
            }
            if flags & 1 == 0 {
                self.play(p, i == 0, rand, reports);
            } else {
                later.push(p);
            }
        }
        for (i, p) in later.into_iter().enumerate() {
            self.play(p, i == 0, rand, reports);
        }
    }

    fn play(&mut self, p: u8, first: bool, rand: &mut CrtRand, reports: &mut Vec<Report>) {
        let data = self.data.clone();
        let step = self.step;
        for g in 0..self.count.min(GROUPS) {
            if data.groups[g].mode == 0xFF {
                continue;
            }
            let frames = data.frames(g, usize::from(p));
            let n = frames.len() as i32;
            let s = &mut self.groups[g];
            if n == 0 {
                if first {
                    s.off = true;
                }
                continue;
            }
            let at = |i: i32| frames.get(i.max(0) as usize).copied();
            match s.mode {
                1 | 4 => {}
                2 => match s.phase {
                    0 => {
                        let r = rand.rand();
                        s.phase = 1;
                        s.frame = 0;
                        s.countdown = (r % 90) as i16 + 60;
                    }
                    1 => {
                        s.countdown -= step;
                        if s.countdown < 0 {
                            s.countdown = 4;
                            s.phase = 2;
                        }
                    }
                    2 => {
                        s.countdown -= step;
                        if s.countdown < 0 {
                            if s.frame + 1 < n {
                                s.frame += 1;
                                s.countdown = 1;
                            } else {
                                s.countdown = 0;
                                s.phase = 0;
                            }
                        }
                    }
                    _ => {}
                },
                3 => {
                    if let Some(f) = at(s.frame)
                        && f.hold >= 0
                    {
                        s.acc += step;
                        if i32::from(s.jitter) + i32::from(f.hold) < i32::from(s.acc) {
                            s.acc = 0;
                            let r = rand.rand() as u32;
                            s.frame += 1;
                            s.jitter = (((r & 0xFF) * u32::from(f.jitter)) >> 8) as i16;
                            if n <= s.frame {
                                s.frame = 0;
                            }
                        }
                    }
                    let image = at(s.frame).map_or(0, |f| usize::from(f.image));
                    let (w, h) = data.images.get(image).copied().unwrap_or((0, 0));
                    let x = s.scroll_step[0] as f32 + s.scroll[0] as f32;
                    s.scroll[0] = x as i32;
                    let y = s.scroll_step[1] as f32 + s.scroll[1] as f32;
                    s.scroll[1] = y as i32;
                    if x < 0.0 {
                        s.scroll[0] = (f32::from(w) + x) as i32;
                    }
                    if y < 0.0 {
                        s.scroll[1] = (y + f32::from(h)) as i32;
                    }
                    if f32::from(w) <= s.scroll[0] as f32 {
                        s.scroll[0] = (s.scroll[0] as f32 - f32::from(w)) as i32;
                    }
                    if f32::from(h) <= s.scroll[1] as f32 {
                        s.scroll[1] = (s.scroll[1] as f32 - f32::from(h)) as i32;
                    }
                }
                _ => {
                    if let Some(f) = at(s.frame)
                        && f.hold >= 0
                    {
                        s.acc += step;
                        if i32::from(s.jitter) + i32::from(f.hold) <= i32::from(s.acc) {
                            s.acc = 0;
                            let r = rand.rand() as u32;
                            s.frame += 1;
                            s.jitter = (((r & 0xFF) * u32::from(f.jitter)) >> 8) as i16;
                            if n <= s.frame {
                                s.frame = 0;
                            }
                        }
                    }
                }
            }
            s.off = false;
            reports.push((g as u8, p, s.frame));
        }
    }
}

/// One of the remaster's face sprite animations of a character (`PSpriteAnimationInfoChar`).
#[derive(Debug, Clone, PartialEq)]
pub struct FaceSprite {
    /// `G<group>`.
    pub group: String,
    /// `Animation<pattern>`.
    pub name: String,
    /// Sub-texture ids by frame (indexing [`CharFace::textures`]).
    pub ids: Vec<u32>,
    /// mUStart, mVStart, mUEnd, mVEnd.
    pub uv: [f32; 4],
}

/// A character's face sprites and the textures they show (its asset header's files before
/// `textureAnimationChar`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CharFace {
    pub sprites: Vec<FaceSprite>,
    pub textures: Vec<String>,
}

impl CharFace {
    /// The face sprites of the character model at `model_path` (`…/chr/<kind>/<id>/mdl/d3d11/<id>.dae.phyre`).
    pub fn load(vfs: &aerugo_vfs::Vfs, model_path: &str) -> Option<CharFace> {
        let dir = model_path.rsplit_once("/mdl/")?.0;
        let name = dir.rsplit('/').next()?;
        let header = vfs.read(&format!("{dir}/{name}.ahwin32")).ok()?;
        let (files, index) =
            super::texture_animation::asset_header(&String::from_utf8_lossy(&header), "textureAnimationChar");
        let index = index?;
        let cluster = aerugo_formats::phyre::Cluster::parse(vfs.read(files.get(index)?).ok()?).ok()?;
        let sprites = cluster
            .objects("PSpriteAnimationInfoChar")
            .into_iter()
            .map(|o| {
                let n = cluster.array_count(o, "m_subTextureIDs").unwrap_or(0) as usize;
                let ids = cluster
                    .array(o, "m_subTextureIDs", 4)
                    .map(|a| {
                        cluster
                            .array_bytes(o, a, n * 4)
                            .chunks_exact(4)
                            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                            .collect()
                    })
                    .unwrap_or_default();
                let f = |n: &str| cluster.f32(o, n).unwrap_or(0.0);
                FaceSprite {
                    group: cluster.string(o, "m_groupName").unwrap_or_default(),
                    name: cluster.string(o, "m_name").unwrap_or_default(),
                    ids,
                    uv: [f("mUStart"), f("mVStart"), f("mUEnd"), f("mVEnd")],
                }
            })
            .collect();
        Some(CharFace { sprites, textures: files[..index].to_vec() })
    }

    /// The sprite of a group's pattern, if the character has one.
    pub fn sprite(&self, group: u8, pattern: u8) -> Option<usize> {
        let (g, n) = (format!("G{group}"), format!("Animation{pattern}"));
        self.sprites.iter().position(|s| s.group == g && s.name == n)
    }

    /// The texture a sprite shows at a frame.
    pub fn texture(&self, sprite: usize, frame: i32) -> Option<&str> {
        let id = *self.sprites.get(sprite)?.ids.get(usize::try_from(frame).ok()?)?;
        self.textures.get(id as usize).map(String::as_str)
    }
}

/// One entry of a character's face layer stack: a group's sprite and frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FaceLayer {
    pub group: u8,
    pub sprite: usize,
    pub frame: i32,
}

/// The remaster's face layer stacks hold 32 entries.
const LAYER_STACK: usize = 32;

/// Pass the player's reports to the face layer stack (the remaster's character texture animations): a group whose pattern has a
/// sprite (and a texture at that frame) replaces its entry and goes on top; the oldest goes when the stack is full.
pub fn apply_reports(layers: &mut Vec<FaceLayer>, face: &CharFace, reports: &[Report]) {
    for &(group, pattern, frame) in reports {
        let Some(sprite) = face.sprite(group, pattern) else { continue };
        if face.texture(sprite, frame).is_none() {
            continue;
        }
        layers.retain(|l| l.group != group);
        if layers.len() >= LAYER_STACK {
            layers.remove(0);
        }
        layers.push(FaceLayer { group, sprite, frame });
    }
}

/// The entries a face material shows: the newest six, oldest first (all of them when fewer); a
/// material whose effect sets `CHR_TEXTURE_ANIMATION_UV` (`uv`) shows only the newest.
pub fn shown_layers(layers: &[FaceLayer], uv: bool) -> &[FaceLayer] {
    &layers[layers.len().saturating_sub(if uv { 1 } else { 6 })..]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A face material shows the newest six entries, oldest first (a UV-animated one only the newest).
    #[test]
    fn shows_the_newest_six() {
        let stack: Vec<FaceLayer> = (0..8).map(|g| FaceLayer { group: g, sprite: 0, frame: 0 }).collect();
        let groups = |l: &[FaceLayer]| l.iter().map(|l| l.group).collect::<Vec<_>>();
        assert_eq!(groups(shown_layers(&stack[..3], false)), [0, 1, 2]);
        assert_eq!(groups(shown_layers(&stack, false)), [2, 3, 4, 5, 6, 7]);
        assert_eq!(groups(shown_layers(&stack, true)), [7]);
        assert!(shown_layers(&[], true).is_empty());
    }

    #[test]
    fn crt_rand_matches_msvc() {
        let mut r = CrtRand(1);
        assert_eq!([r.rand(), r.rand(), r.rand()], [41, 18467, 6334]);
    }

    /// Tidus's face data: nine groups, the eyes (group 0) blinking, the others in frame sequences.
    #[test]
    fn reads_a_face_and_blinks() {
        let Some(inst) = aerugo_vfs::install_from_env() else {
            eprintln!("skipped: set AERUGO_GAME_DIR");
            return;
        };
        let mut vfs = aerugo_vfs::Vfs::new();
        vfs.mount_install(&inst).unwrap();
        let d = vfs.read("ffx_ps2/ffx/master/jppc/chr/pc/c001/mdl/c001.chr").unwrap();
        let data = std::sync::Arc::new(FaceData::from_chr(&d).unwrap());
        let mut face = Face::new(data.clone());
        assert_eq!(face.count, 9);
        assert_eq!(face.groups[0].mode, 2);
        assert!(face.groups[1..9].iter().all(|g| g.mode == 0));
        let (mut rand, mut reports) = (CrtRand::default(), Vec::new());
        let mut frames = Vec::new();
        for _ in 0..400 {
            reports.clear();
            face.tick(&mut rand, &mut reports);
            frames.push(face.groups[0].frame);
        }
        // The eyes stay open, then blink through their frames and open again.
        assert!(frames.iter().any(|&f| f > 0) && frames.iter().filter(|&&f| f == 0).count() > 300);
        assert!(reports.iter().any(|r| r.0 == 0));
        // Only the eyes' blink (G0 / Animation2) has a sprite in the field, as the original's stacks show.
        let sprites =
            CharFace::load(&vfs, "ffx_data/gamedata/ps3data/chr/pc/c001/mdl/d3d11/c001.dae.phyre").unwrap();
        assert_eq!(sprites.textures.len(), 23);
        let mut layers = Vec::new();
        apply_reports(&mut layers, &sprites, &reports);
        assert_eq!(layers.iter().map(|l| l.group).collect::<Vec<_>>(), [0]);
        assert_eq!(sprites.sprites[layers[0].sprite].name, "Animation2");
    }
}
