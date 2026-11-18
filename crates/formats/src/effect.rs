//! Effects: the compiled shader clusters materials refer to (`shaders/d3d11/<name>.fx#<variant>.phyre`). Aerugo
//! does not run the compiled shaders; it reads what the fixed-function stages need (blending, depth, culling)
//! for the pass a material is drawn in, and the names of the material parameters. See `docs/formats/phyre.md`.

use crate::Game;
use crate::phyre::{Cluster, ObjectId};

/// Blend factors (Direct3D 11 numbering in the files).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlendFactor {
    Zero,
    One,
    SrcColor,
    InvSrcColor,
    SrcAlpha,
    InvSrcAlpha,
    DstAlpha,
    InvDstAlpha,
    DstColor,
    InvDstColor,
    SrcAlphaSat,
    Constant,
    InvConstant,
}

impl BlendFactor {
    fn from_d3d(v: u32) -> BlendFactor {
        match v {
            1 => BlendFactor::Zero,
            3 => BlendFactor::SrcColor,
            4 => BlendFactor::InvSrcColor,
            5 => BlendFactor::SrcAlpha,
            6 => BlendFactor::InvSrcAlpha,
            7 => BlendFactor::DstAlpha,
            8 => BlendFactor::InvDstAlpha,
            9 => BlendFactor::DstColor,
            10 => BlendFactor::InvDstColor,
            11 => BlendFactor::SrcAlphaSat,
            14 => BlendFactor::Constant,
            15 => BlendFactor::InvConstant,
            _ => BlendFactor::One,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlendOp {
    Add,
    Subtract,
    ReverseSubtract,
    Min,
    Max,
}

impl BlendOp {
    fn from_d3d(v: u32) -> BlendOp {
        match v {
            2 => BlendOp::Subtract,
            3 => BlendOp::ReverseSubtract,
            4 => BlendOp::Min,
            5 => BlendOp::Max,
            _ => BlendOp::Add,
        }
    }
}

/// Colour and alpha blend equations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Blend {
    pub src: BlendFactor,
    pub dst: BlendFactor,
    pub op: BlendOp,
    pub src_alpha: BlendFactor,
    pub dst_alpha: BlendFactor,
    pub op_alpha: BlendOp,
}

impl Blend {
    pub const ALPHA: Blend = Blend {
        src: BlendFactor::SrcAlpha,
        dst: BlendFactor::InvSrcAlpha,
        op: BlendOp::Add,
        src_alpha: BlendFactor::SrcAlpha,
        dst_alpha: BlendFactor::InvSrcAlpha,
        op_alpha: BlendOp::Add,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cull {
    None,
    Front,
    Back,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Compare {
    Never,
    Less,
    Equal,
    LessEqual,
    Greater,
    NotEqual,
    GreaterEqual,
    Always,
}

impl Compare {
    fn from_d3d(v: u32) -> Compare {
        match v {
            1 => Compare::Never,
            2 => Compare::Less,
            3 => Compare::Equal,
            5 => Compare::Greater,
            6 => Compare::NotEqual,
            7 => Compare::GreaterEqual,
            8 => Compare::Always,
            _ => Compare::LessEqual,
        }
    }
}

/// Fixed-function state of one shader pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PassState {
    pub cull: Cull,
    /// Triangles wound counter-clockwise on screen face the viewer.
    pub front_ccw: bool,
    pub depth_test: bool,
    pub depth_write: bool,
    pub depth_compare: Compare,
    pub blend: Option<Blend>,
    /// RGBA write mask (bit 0 = red).
    pub write_mask: u8,
    /// Alpha to coverage (the remaster's multisampled passes use it where their no-MSAA twins alpha-test).
    pub alpha_to_coverage: bool,
}

impl Default for PassState {
    fn default() -> PassState {
        PassState {
            cull: Cull::None,
            front_ccw: false,
            depth_test: true,
            depth_write: true,
            depth_compare: Compare::LessEqual,
            blend: None,
            write_mask: 0xF,
            alpha_to_coverage: false,
        }
    }
}

/// One scene pass an effect variant can be drawn in (`Opaque`, `Transparent`, `ZPrePass`, `Shadow`, ...).
#[derive(Debug, Clone, PartialEq)]
pub struct ScenePass {
    pub kind: String,
    /// State of each shader pass, in order.
    pub passes: Vec<PassState>,
}

/// What Aerugo reads from an effect variant.
#[derive(Debug, Clone, PartialEq)]
pub struct Effect {
    pub scene_passes: Vec<ScenePass>,
    /// Names of the material parameters the variant takes.
    pub parameters: Vec<String>,
    /// The variant's material switches (`IGNORE_FOG`, `TEXTURE_ENABLED`, …) with their values.
    pub switches: Vec<(String, String)>,
}

impl Effect {
    /// The pass a model draws in: `Opaque` or `Transparent` (whichever the variant has first), else the first
    /// pass that is not a depth or shadow pass.
    pub fn main_pass(&self) -> Option<&ScenePass> {
        let is_main = |p: &&ScenePass| p.kind == "Opaque" || p.kind == "Transparent";
        self.scene_passes.iter().find(is_main).or_else(|| {
            self.scene_passes
                .iter()
                .find(|p| !p.kind.contains("Shadow") && !p.kind.contains("ZPrePass") && !p.passes.is_empty())
        })
    }

    /// State of the first shader pass of [`Effect::main_pass`].
    pub fn main_state(&self) -> Option<PassState> {
        self.main_pass().and_then(|p| p.passes.first().copied())
    }

    /// Whether a material switch is set to "1".
    pub fn switch(&self, name: &str) -> bool {
        self.switches.iter().any(|(n, v)| n == name && v == "1")
    }

    pub fn is_transparent(&self) -> bool {
        self.main_pass().is_some_and(|p| p.kind.contains("Transparent"))
            || self.main_state().is_some_and(|s| s.blend.is_some())
    }
}

fn pass_state(c: &Cluster, pass: ObjectId) -> PassState {
    let u = |name: &str| c.u32(pass, &format!("m_state.{name}"));
    let cull = match u("m_rasterDesc.CullMode") {
        Some(2) => Cull::Front,
        Some(3) => Cull::Back,
        _ => Cull::None,
    };
    let rt = |name: &str| u(&format!("m_blendDesc.RenderTarget.{name}"));
    let blend = (rt("BlendEnable").unwrap_or(0) != 0).then(|| Blend {
        src: BlendFactor::from_d3d(rt("SrcBlend").unwrap_or(2)),
        dst: BlendFactor::from_d3d(rt("DestBlend").unwrap_or(1)),
        op: BlendOp::from_d3d(rt("BlendOp").unwrap_or(1)),
        src_alpha: BlendFactor::from_d3d(rt("SrcBlendAlpha").unwrap_or(2)),
        dst_alpha: BlendFactor::from_d3d(rt("DestBlendAlpha").unwrap_or(1)),
        op_alpha: BlendOp::from_d3d(rt("BlendOpAlpha").unwrap_or(1)),
    });
    PassState {
        cull,
        front_ccw: u("m_rasterDesc.FrontCounterClockwise").unwrap_or(0) != 0,
        depth_test: u("m_depthDesc.DepthEnable").unwrap_or(1) != 0,
        depth_write: u("m_depthDesc.DepthWriteMask").unwrap_or(1) != 0,
        depth_compare: Compare::from_d3d(u("m_depthDesc.DepthFunc").unwrap_or(4)),
        blend,
        write_mask: c
            .field(pass, "m_state.m_blendDesc.RenderTarget.RenderTargetWriteMask")
            .and_then(|b| b.first().copied())
            .unwrap_or(0xF)
            & 0xF,
        alpha_to_coverage: u("m_blendDesc.AlphaToCoverageEnable").unwrap_or(0) != 0,
    }
}

/// Read the first effect variant of an effect cluster.
pub fn read_effect(c: &Cluster) -> Option<Effect> {
    let variant = *c.objects("PEffectVariant").first()?;
    let scene_passes = c
        .array_targets(variant, "m_sceneRenderPasses")
        .into_iter()
        .map(|sp| {
            let kind = match c.pointer(sp, "m_passType") {
                Some(crate::phyre::Pointer::User(u)) => c.user_string(u),
                _ => String::new(),
            };
            let passes = c
                .array_targets(sp, "m_shaders")
                .first()
                .map(|&shader| {
                    c.array_targets(shader, "m_passes").into_iter().map(|p| pass_state(c, p)).collect()
                })
                .unwrap_or_default();
            ScenePass { kind, passes }
        })
        .collect();
    let parameters = c
        .array_targets(variant, "m_tweakableShaderParameterDefinitions")
        .into_iter()
        .filter_map(|d| c.string(d, "m_name"))
        .collect();
    let switches = c
        .objects("PMaterialSwitch")
        .into_iter()
        .filter_map(|s| Some((c.string(s, "m_name")?, c.string(s, "m_value").unwrap_or_default())))
        .collect();
    Some(Effect { scene_passes, parameters, switches })
}

/// Archive path of the cluster for an effect asset id (`PS3Data/Shaders/<name>.fx#<variant>`).
pub fn effect_cluster_path(game: Game, id: &str) -> Option<String> {
    let lower = id.to_ascii_lowercase().replace('\\', "/");
    let rest = lower.strip_prefix("ps3data/shaders/")?;
    let root = match game {
        Game::Ffx => "ffx_data",
        Game::Ffx2 => "ffx-2_data",
    };
    Some(format!("{root}/gamedata/ps3data/shaders/d3d11/{rest}.phyre"))
}

/// The effect family: the file name without the variant (`phyredefaultlitshader`).
pub fn effect_family(id: &str) -> &str {
    let name = id.rsplit(['/', '\\']).next().unwrap_or(id);
    let name = name.split('#').next().unwrap_or(name);
    name.strip_suffix(".fx").unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_families() {
        assert_eq!(
            effect_cluster_path(Game::Ffx, "PS3Data/Shaders/phyreDefaultLitShader.fx#abc").as_deref(),
            Some("ffx_data/gamedata/ps3data/shaders/d3d11/phyredefaultlitshader.fx#abc.phyre")
        );
        assert_eq!(effect_cluster_path(Game::Ffx2, "other/x.fx"), None);
        assert_eq!(effect_family("PS3Data/Shaders/phyreChrLitShader.fx#00ff"), "phyreChrLitShader");
    }

    #[test]
    fn main_pass_prefers_opaque_or_transparent() {
        let opaque = ScenePass { kind: "Opaque".into(), passes: vec![PassState::default()] };
        let shadow = ScenePass { kind: "Shadow".into(), passes: vec![PassState::default()] };
        let blended = PassState { blend: Some(Blend::ALPHA), ..PassState::default() };
        let transparent = ScenePass { kind: "Transparent".into(), passes: vec![blended] };
        let e = Effect { scene_passes: vec![shadow.clone(), opaque], parameters: vec![], switches: vec![] };
        assert_eq!(e.main_pass().unwrap().kind, "Opaque");
        assert!(!e.is_transparent());
        let e = Effect { scene_passes: vec![shadow, transparent], parameters: vec![], switches: vec![] };
        assert!(e.is_transparent());
        assert_eq!(e.main_state().unwrap().blend, Some(Blend::ALPHA));
    }
}
