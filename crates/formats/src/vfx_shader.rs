//! The remaster's VFX shader table (`shaders/vfxshader_fx.txt`): which effect file draws VFX geometry with a
//! given set of shader properties (blend mode, texture, depth handling, special effects). Each line reads
//! `File.fx.phyre:PROPERTY|PROPERTY…`; the properties are found by name in the text after the colon, in the
//! engine's order (a few special ones stand alone and win over the rest). Comment lines start with `//`.

/// Property bits.
pub const ZMAT: u32 = 0x1;
pub const ALPHA_ADDITIVE: u32 = 0x2;
pub const ENABLE_TEXTURE: u32 = 0x8;
pub const ALPHA_SUBSTRACT: u32 = 0x10;
pub const ALPHA_SUBSTRACT2: u32 = 0x20;
pub const ZWRITE_ENABLE: u32 = 0x40;
pub const TK_RUNTIME_CLUT: u32 = 0x80;
pub const ALPHA_SUBSTRACT3: u32 = 0x100;
pub const ALPHA_NO_BLENDING: u32 = 0x200;
pub const EXTENDED_ALPHA: u32 = 0x400;
pub const ZTEST_DISABLE: u32 = 0x800;
pub const DTT_SPECIAL: u32 = 0x1000;
pub const SHIFT_ON_SCREEN: u32 = 0x2000;
pub const GRAY_SCALE: u32 = 0x4000;
pub const ENABLE_NCLIP: u32 = 0x8000;
pub const SCREEN_FILTER: u32 = 0x10000;
pub const POINTLIGHT: u32 = 0x20000;
pub const TEXTURE_VIDEO: u32 = 0x40000;
pub const YIQ: u32 = 0x80000;
pub const ALPHA_TEST_ENABLED: u32 = 0x100000;
pub const LOGO_TEXTURE_VIDEO: u32 = 0x200000;
pub const LOGO_SPECIAL: u32 = 0x0100_0000;
pub const NOISE_BLUR_ADDITIVE: u32 = 0x0800_0000;
pub const PERS_Z_BUF_CLEAR: u32 = 0x1000_0000;
pub const NOISE_BLUR: u32 = 0x2000_0000;
pub const Z_BUF_CLEAR: u32 = 0x4000_0000;
pub const GROUND_SPECIAL: u32 = 0x8000_0000;

/// The property bits a line's text after the colon names, found as the engine finds them (by substring, so a
/// name inside a longer one counts too).
pub fn properties(text: &str) -> u32 {
    let has = |name: &str| text.contains(name);
    // Special shaders: the first match is the whole property set.
    for (name, bits) in [
        ("VSP_GROUND_SPECIAL", GROUND_SPECIAL),
        ("VSP_Z_BUF_CLEAR", Z_BUF_CLEAR),
        ("VSP_PERS_Z_BUF_CLEAR", PERS_Z_BUF_CLEAR),
        ("VSP_DTT_SPECIAL", DTT_SPECIAL),
        ("VSP_GRAY_SCALE", GRAY_SCALE),
        ("VSP_NOISE_BLUR_ADDITIVE", NOISE_BLUR_ADDITIVE),
        ("VSP_NOISE_BLUR", NOISE_BLUR),
        ("VSP_SCREEN_FILTER", SCREEN_FILTER),
    ] {
        if has(name) {
            return bits;
        }
    }
    let mut bits = if has("VSP_ZMAT") { ZMAT } else { 0 };
    if !has("VSP_ALPHA_MODULATE") {
        if has("VSP_ALPHA_ADDITIVE") {
            bits |= ALPHA_ADDITIVE;
        } else if has("VSP_ALPHA_SUBSTRACT3") {
            bits |= ALPHA_SUBSTRACT3;
        } else if has("VSP_ALPHA_SUBSTRACT2") {
            bits |= ALPHA_SUBSTRACT2;
        } else if has("VSP_ALPHA_SUBSTRACT") {
            bits |= ALPHA_SUBSTRACT;
        }
    }
    for (name, bit) in [
        ("VSP_ENABLE_TEXTURE", ENABLE_TEXTURE),
        ("VSP_ZWRITE_ENABLE", ZWRITE_ENABLE),
        ("VSP_ALPHA_TEST_ENABLED", ALPHA_TEST_ENABLED),
        ("VSP_ZTEST_DISABLE", ZTEST_DISABLE),
        ("VSP_TK_RUNTIME_CLUT_16x16", TK_RUNTIME_CLUT),
        ("VSP_ALPHA_NO_BLENDING", ALPHA_NO_BLENDING),
        ("VSP_EXTENDED_ALPHA", EXTENDED_ALPHA),
        ("VSP_ENABLE_NCLIP", ENABLE_NCLIP),
        ("VSP_SHIFT_ON_SCREEN", SHIFT_ON_SCREEN),
        ("VSP_LOGO_SPECIAL", LOGO_SPECIAL),
        ("VSP_POINTLIGHT", POINTLIGHT),
        ("VSP_TEXTURE_VIDEO", TEXTURE_VIDEO),
        ("VSP_LOGO_TEXTURE_VIDEO", LOGO_TEXTURE_VIDEO),
        ("VSP_YIQ", YIQ),
    ] {
        if has(name) {
            bits |= bit;
        }
    }
    bits
}

/// The table: (property bits, effect file name) for every line with a colon, in file order.
pub fn parse_table(text: &str) -> Vec<(u32, String)> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with("//"))
        .filter_map(|l| l.split_once(':'))
        .map(|(file, props)| (properties(props), file.trim().to_owned()))
        .collect()
}

/// The effect file for a property set (the first line with exactly those bits).
pub fn shader_for(table: &[(u32, String)], bits: u32) -> Option<&str> {
    table.iter().find(|(b, _)| *b == bits).map(|(_, f)| f.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn properties_are_found_as_the_engine_finds_them() {
        assert_eq!(properties("VSP_ZMAT|VSP_ALPHA_MODULATE|VSP_ENABLE_TEXTURE"), ZMAT | ENABLE_TEXTURE);
        assert_eq!(properties("VSP_ALPHA_ADDITIVE|VSP_ENABLE_TEXTURE"), ALPHA_ADDITIVE | ENABLE_TEXTURE);
        assert_eq!(properties("VSP_ALPHA_SUBSTRACT2"), ALPHA_SUBSTRACT2);
        // A special shader stands alone.
        assert_eq!(properties("VSP_ZMAT|VSP_GRAY_SCALE"), GRAY_SCALE);
        // The logo's video name also contains TEXTURE_VIDEO.
        assert_eq!(properties("VSP_LOGO_TEXTURE_VIDEO"), TEXTURE_VIDEO | LOGO_TEXTURE_VIDEO);
    }

    #[test]
    fn the_table_maps_property_sets_to_effect_files() {
        let t = parse_table(
            "// comment VSP_ZMAT\nVfxDefaultShader.fx.phyre:VSP_ALPHA_MODULATE|VSP_ENABLE_TEXTURE\n\
             VfxShapeShader.fx.phyre:VSP_ZMAT|VSP_ALPHA_MODULATE|VSP_ENABLE_TEXTURE\n",
        );
        assert_eq!(shader_for(&t, ENABLE_TEXTURE), Some("VfxDefaultShader.fx.phyre"));
        assert_eq!(shader_for(&t, ZMAT | ENABLE_TEXTURE), Some("VfxShapeShader.fx.phyre"));
        assert_eq!(shader_for(&t, ALPHA_ADDITIVE), None);
    }
}
