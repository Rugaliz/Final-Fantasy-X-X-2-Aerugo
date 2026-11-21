//! The remaster's texture animations of a map: materials whose `m_texAnimID` names an animation show one of the
//! map's swap textures, picked by the animation's frame.
//!
//! The map's asset header (`<map>.ahwin32`) lists its files in load order; the files before the one it names
//! `textureAnimation` are the swap textures, and a frame's sub-texture id indexes them. That file holds the
//! animations (`PSpriteAnimationInfo`: name, interval, sub-texture ids). Every animation starts playing at frame 0
//! and steps a frame each time its elapsed time reaches the interval (the elapsed time then restarts at 0). A PS2
//! texture animation slot whose keyframes pick a frame stops the animation of the same name and sets its frame —
//! when one of the map's materials names that animation (the original looks the name up among its animated
//! materials); otherwise the animation plays on.

use aerugo_formats::phyre::Cluster;
use aerugo_vfs::Vfs;

/// One animation and where it stands.
#[derive(Debug, Clone, PartialEq)]
pub struct TextureAnimation {
    pub name: String,
    pub interval: f32,
    /// Sub-texture ids, one per frame.
    pub frames: Vec<u32>,
    pub elapsed: f32,
    pub frame: u32,
    pub playing: bool,
}

/// A map's texture animations and its swap textures.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TextureAnimations {
    /// The swap textures (archive paths) in load order.
    pub textures: Vec<String>,
    pub animations: Vec<TextureAnimation>,
    /// The animation names the map's materials give (`m_texAnimID`, without the characters' `Mat_CTA` ones).
    pub materials: Vec<String>,
}

/// The files of an asset header in load order and the index it names `textureAnimation`.
fn asset_header(text: &str) -> (Vec<String>, Option<usize>) {
    let mut index = None;
    let mut files = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("const Phyre::PUInt32 textureAnimation =") {
            index = rest.trim().trim_end_matches(';').trim().parse().ok();
        }
        let line = line.trim_start_matches(',');
        if let Some(path) = line.strip_prefix('"').and_then(|l| l.strip_suffix('"'))
            && path.starts_with('/')
        {
            files.push(path.trim_start_matches('/').to_ascii_lowercase());
        }
    }
    (files, index)
}

impl TextureAnimations {
    /// The texture animations of the map model at `map_model` (`…/map/<area>/<map>/mdl/d3d11/<map>.dae.phyre`), if
    /// its asset header names any.
    pub fn load(vfs: &Vfs, map_model: &str) -> Option<TextureAnimations> {
        let dir = map_model.rsplit_once("/mdl/")?.0;
        let name = dir.rsplit('/').next()?;
        let header = vfs.read(&format!("{dir}/{name}.ahwin32")).ok()?;
        let (files, index) = asset_header(&String::from_utf8_lossy(&header));
        let index = index?;
        let cluster = Cluster::parse(vfs.read(files.get(index)?).ok()?).ok()?;
        let animations = cluster
            .objects("PSpriteAnimationInfo")
            .into_iter()
            .map(|o| {
                let n = cluster.array_count(o, "m_subTextureIDs").unwrap_or(0) as usize;
                let frames = cluster
                    .array(o, "m_subTextureIDs", 4)
                    .map(|a| {
                        cluster
                            .array_bytes(o, a, n * 4)
                            .chunks_exact(4)
                            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                            .collect()
                    })
                    .unwrap_or_default();
                TextureAnimation {
                    name: cluster.string(o, "m_name").unwrap_or_default(),
                    interval: cluster.f32(o, "m_timeInterval").unwrap_or(0.0),
                    frames,
                    elapsed: 0.0,
                    frame: 0,
                    playing: true,
                }
            })
            .collect();
        // The map's animated materials.
        let model = Cluster::parse(vfs.read(map_model).ok()?).ok()?;
        let mut materials = Vec::new();
        for m in model.objects("PMaterial") {
            let id = model.string(m, "m_texAnimID").unwrap_or_default();
            if !id.is_empty() && !id.contains("Mat_CTA") && !materials.contains(&id) {
                materials.push(id);
            }
        }
        Some(TextureAnimations { textures: files[..index].to_vec(), animations, materials })
    }

    /// The first animation named `name`.
    pub fn find(&self, name: &str) -> Option<&TextureAnimation> {
        self.animations.iter().find(|a| a.name == name)
    }

    /// A PS2 texture animation slot picked `frame` of the animation named `name`: it stops and shows that frame.
    pub fn set_frame(&mut self, name: &str, frame: u32) {
        if !self.materials.iter().any(|m| m == name) {
            return;
        }
        if let Some(a) = self.animations.iter_mut().find(|a| a.name == name) {
            a.playing = false;
            if (frame as usize) < a.frames.len() {
                a.frame = frame;
            }
        }
    }

    /// Advance the playing animations by `dt` seconds.
    pub fn update(&mut self, dt: f32) {
        for a in self.animations.iter_mut().filter(|a| a.playing) {
            a.elapsed += dt;
            if a.interval <= a.elapsed {
                a.elapsed = 0.0;
                let n = a.frames.len() as u32;
                if n > 0 {
                    a.frame = (a.frame + 1 + n) % n;
                }
            }
        }
    }

    /// The swap texture an animation shows now.
    pub fn texture(&self, name: &str) -> Option<&str> {
        let a = self.find(name)?;
        let id = *a.frames.get(a.frame as usize)?;
        self.textures.get(id as usize).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_an_asset_header() {
        let text = "const Phyre::PUInt32  = 0;\nconst Phyre::PUInt32 textureAnimation = 1;\n\
                    const Phyre::PChar *g_fileNames[] = {\n\"/FFX_Data/A/t_000.dds.phyre\"\n,\"/FFX_Data/A/x.ags.phyre\"\n};";
        let (files, index) = asset_header(text);
        assert_eq!(files, ["ffx_data/a/t_000.dds.phyre", "ffx_data/a/x.ags.phyre"]);
        assert_eq!(index, Some(1));
    }

    #[test]
    fn plays_and_stops() {
        let mut t = TextureAnimations {
            materials: vec!["n".into()],
            textures: vec!["a".into(), "b".into(), "c".into()],
            animations: vec![TextureAnimation {
                name: "n".into(),
                interval: 0.5,
                frames: vec![0, 1, 2],
                elapsed: 0.0,
                frame: 0,
                playing: true,
            }],
        };
        let mut shown = Vec::new();
        for _ in 0..32 {
            t.update(1.0 / 30.0);
            shown.push(t.animations[0].frame);
        }
        // 1/30 added up in f32 reaches 0.5 on the 15th step.
        assert_eq!(shown.iter().position(|&f| f == 1), Some(14));
        assert_eq!(shown.iter().position(|&f| f == 2), Some(29));
        t.set_frame("n", 0);
        t.update(1.0);
        assert_eq!((t.animations[0].frame, t.texture("n")), (0, Some("a")));
    }

    /// znkd10: thirty swap textures before its animation file, and the animation its water material names.
    #[test]
    fn reads_a_maps_animations() {
        let Some(inst) = aerugo_vfs::install_from_env() else {
            eprintln!("skipped: set AERUGO_GAME_DIR");
            return;
        };
        let mut vfs = Vfs::new();
        vfs.mount_install(&inst).unwrap();
        let t = TextureAnimations::load(
            &vfs,
            "ffx_data/gamedata/ps3data/map/znkd/znkd10/mdl/d3d11/znkd10.dae.phyre",
        )
        .unwrap();
        assert_eq!(t.textures.len(), 30);
        assert!(t.textures[0].ends_with("texswap/d3d11/12032_20_000_000_4_128_128_000.dds.phyre"));
        let a = t.find("12032_20_000_000").unwrap();
        assert!(!a.frames.is_empty() && a.interval > 0.0);
        assert!(vfs.read(t.texture("12032_20_000_000").unwrap()).is_ok());
    }
}
