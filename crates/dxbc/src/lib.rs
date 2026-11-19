//! Translation of the games' compiled Direct3D 11 shaders (DXBC, Shader Model 4–5) to SPIR-V, through
//! dxbc-spirv (the translator DXVK uses; vendored in `third_party/dxbc-spirv`). Aerugo runs the games' own
//! shaders this way instead of re-creating them.
//!
//! Descriptor layout of the translated shaders, as DXVK lays out a Direct3D 11 stage: one descriptor set per
//! stage (vertex and compute 0, pixel 1, geometry 2, hull 3, domain 4); in it constant buffer `b<n>` at binding
//! n, sampler `s<n>` at [`BINDING_SAMPLER`] + n, shader resource `t<n>` at [`BINDING_SRV`] + n and unordered
//! access view `u<n>` at [`BINDING_UAV`] + n. Vertex inputs and inter-stage values sit at the location of their
//! register (`v<n>`, `o<n>`). A pixel shader's sample count is specialization constant 0. Clip space is
//! Direct3D's: draw with a viewport of negative height, as DXVK does.

use std::ffi::{CStr, c_char};

/// First binding of the samplers in a stage's set.
pub const BINDING_SAMPLER: u32 = 16;
/// First binding of the shader resources (textures, buffers) in a stage's set.
pub const BINDING_SRV: u32 = 32;
/// First binding of the unordered access views in a stage's set.
pub const BINDING_UAV: u32 = 160;
/// Descriptor set of the vertex stage.
pub const SET_VERTEX: u32 = 0;
/// Descriptor set of the pixel stage.
pub const SET_PIXEL: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a DXBC container")]
    NotDxbc,
    #[error("translation failed: {0}")]
    Translate(String),
}

unsafe extern "C" {
    fn aerugo_dxbc_to_spirv(
        code: *const u8,
        size: usize,
        prev: *const u8,
        prev_size: usize,
        float_controls: u32,
        words: *mut *mut u32,
        count: *mut usize,
        error: *mut *mut c_char,
    ) -> i32;
    fn aerugo_dxbc_free(words: *mut u32);
    fn aerugo_dxbc_free_text(text: *mut c_char);
}

fn is_dxbc(code: &[u8]) -> bool {
    code.len() >= 32 && code.starts_with(b"DXBC")
}

/// The device's 32-bit float controls (`VkPhysicalDeviceFloatControlsProperties`); the translated shaders ask
/// for Direct3D's rounding and denormal behaviour where the device offers it, as DXVK does.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FloatControls {
    pub independent_denorm: bool,
    pub rte32: bool,
    pub rtz32: bool,
    pub flush_denorm32: bool,
    pub preserve_denorm32: bool,
    pub preserve_inf_nan32: bool,
}

impl FloatControls {
    fn bits(&self) -> u32 {
        [
            self.independent_denorm,
            self.rte32,
            self.rtz32,
            self.flush_denorm32,
            self.preserve_denorm32,
            self.preserve_inf_nan32,
        ]
        .iter()
        .enumerate()
        .map(|(i, &b)| u32::from(b) << i)
        .sum()
    }
}

/// Translates one shader to SPIR-V words. `previous` is the vertex shader a pixel shader is drawn with: its
/// outputs are matched with the pixel shader's inputs (pass `None` for other stages).
pub fn to_spirv(code: &[u8], previous: Option<&[u8]>, float: &FloatControls) -> Result<Vec<u32>, Error> {
    if !is_dxbc(code) || previous.is_some_and(|p| !is_dxbc(p)) {
        return Err(Error::NotDxbc);
    }
    let (prev_ptr, prev_len) = previous.map_or((std::ptr::null(), 0), |p| (p.as_ptr(), p.len()));
    let mut words: *mut u32 = std::ptr::null_mut();
    let mut count = 0usize;
    let mut error: *mut c_char = std::ptr::null_mut();
    // SAFETY: the pointers and lengths describe live slices; the out-parameters are valid for writes.
    let status = unsafe {
        aerugo_dxbc_to_spirv(
            code.as_ptr(),
            code.len(),
            prev_ptr,
            prev_len,
            float.bits(),
            &mut words,
            &mut count,
            &mut error,
        )
    };
    if status != 0 {
        let message = if error.is_null() {
            String::from("unknown error")
        } else {
            // SAFETY: on failure the shim sets `error` to a NUL-terminated string it allocated.
            let text = unsafe { CStr::from_ptr(error) }.to_string_lossy().into_owned();
            // SAFETY: `error` came from the shim's allocator and is freed once.
            unsafe { aerugo_dxbc_free_text(error) };
            text
        };
        return Err(Error::Translate(message));
    }
    // SAFETY: on success `words` points at `count` initialised words the shim allocated.
    let out = unsafe { std::slice::from_raw_parts(words, count) }.to_vec();
    // SAFETY: `words` came from the shim's allocator and is freed once, after the copy.
    unsafe { aerugo_dxbc_free(words) };
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_other_data() {
        assert!(matches!(to_spirv(b"not a shader", None, &FloatControls::default()), Err(Error::NotDxbc)));
    }

    /// Every pass of every effect of both games translates, pixel shaders linked to their vertex shaders.
    #[test]
    fn translates_every_game_shader() {
        let Some(inst) = aerugo_vfs::install_from_env() else {
            eprintln!("skipped: set AERUGO_GAME_DIR");
            return;
        };
        let mut vfs = aerugo_vfs::Vfs::new();
        vfs.mount_install(&inst).unwrap();
        let paths: Vec<String> = vfs
            .archives()
            .flat_map(|(_, a)| a.entries().iter().map(|e| e.path.clone()))
            .filter(|p| p.contains("/shaders/d3d11/") && p.ends_with(".phyre"))
            .collect();
        let (mut pairs, mut seen, mut failures) = (0, std::collections::HashSet::new(), Vec::new());
        for path in &paths {
            let cluster = aerugo_formats::phyre::Cluster::parse(vfs.read(path).unwrap()).unwrap();
            let Some(effect) = aerugo_formats::effect::read_effect(&cluster) else { continue };
            for sp in &effect.scene_passes {
                for p in &sp.programs {
                    let (Some(vs), Some(ps)) = (&p.vertex, &p.fragment) else { continue };
                    if !seen.insert((vs.clone(), ps.clone())) {
                        continue;
                    }
                    pairs += 1;
                    for (code, prev) in [(vs, None), (ps, Some(vs.as_slice()))] {
                        match to_spirv(code, prev, &FloatControls::default()) {
                            Ok(w) if w.first() == Some(&0x0723_0203) => {}
                            Ok(_) => failures.push(format!("{path} ({}): no SPIR-V header", sp.kind)),
                            Err(e) => failures.push(format!("{path} ({}): {e}", sp.kind)),
                        }
                    }
                }
            }
        }
        eprintln!("{pairs} distinct vertex/pixel pairs in {} effect clusters", paths.len());
        assert!(pairs > 1000);
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}
