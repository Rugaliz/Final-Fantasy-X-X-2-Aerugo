//! Fallback fonts from the operating system for scripts egui's built-in fonts lack (Japanese, Chinese,
//! Korean). Nothing is bundled; when no font is found the text shows as boxes.

use std::path::Path;

use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
use egui::{FontData, FontFamily};

/// Candidate files, best first. `.ttc` collections use face 0.
const CJK_FONTS: &[&str] = &[
    // Linux distributions
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/noto-cjk/NotoSansCJKjp-Regular.otf",
    "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
    "/usr/share/fonts/droid/DroidSansFallbackFull.ttf",
    // Windows: Japanese, then Chinese, then Korean (each family lacks some of the others' glyphs)
    "C:\\Windows\\Fonts\\YuGothM.ttc",
    "C:\\Windows\\Fonts\\msgothic.ttc",
    "C:\\Windows\\Fonts\\msyh.ttc",
    "C:\\Windows\\Fonts\\msjh.ttc",
    "C:\\Windows\\Fonts\\malgun.ttf",
];

/// How many candidates to add at most. Pan-CJK fonts make one enough on Linux; Windows needs several.
const MAX_FONTS: usize = 3;

/// Add the CJK fallback fonts found on this system as lowest-priority fallbacks. Returns how many were added.
pub fn add_cjk_fallbacks(ctx: &egui::Context) -> usize {
    let mut added = 0;
    for path in CJK_FONTS {
        if added == MAX_FONTS {
            break;
        }
        let Ok(bytes) = std::fs::read(Path::new(path)) else { continue };
        let name = format!("system-cjk-{added}");
        let families = [FontFamily::Proportional, FontFamily::Monospace]
            .into_iter()
            .map(|family| InsertFontFamily { family, priority: FontPriority::Lowest })
            .collect();
        ctx.add_font(FontInsert::new(&name, FontData::from_owned(bytes), families));
        log::info!("fallback font: {path}");
        added += 1;
        // A pan-CJK collection covers all three scripts.
        if path.contains("NotoSansCJK-Regular") {
            break;
        }
    }
    if added == 0 {
        log::warn!("no CJK font found; Japanese, Chinese and Korean text will not render");
    }
    added
}
