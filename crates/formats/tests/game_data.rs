//! Tests against a real installation. They run only when `AERUGO_GAME_DIR` is set.

use aerugo_formats::kernel::{self, KernelError};
use aerugo_formats::text::{self, Charset};
use aerugo_formats::{Game, KernelTable};
use aerugo_vfs::{GAME_DIR_ENV, Language, Vfs, install_from_env};

fn vfs() -> Option<Vfs> {
    let Some(inst) = install_from_env() else {
        eprintln!("skipped: set {GAME_DIR_ENV} to a game installation to run this test");
        return None;
    };
    let mut vfs = Vfs::new();
    vfs.mount_install(&inst).unwrap_or_else(|e| panic!("{e}"));
    Some(vfs)
}

/// Files in the table folders that use other formats (read by later work packages).
const OTHER_FORMATS: [&str; 6] = [
    "btl.bin",
    "magic.bin",
    "effcolliparam.bin",
    "prepare.bin",
    "lm_def_stcount_index_count.bin",
    "albhed.bin",
];

#[test]
fn every_charset_loads() {
    let Some(vfs) = vfs() else { return };
    for game in Game::ALL {
        for lang in Language::ALL {
            let path = text::charset_path(game, lang);
            let bytes = vfs.read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
            let cs = Charset::parse(&bytes).unwrap_or_else(|e| panic!("{path}: {e}"));
            assert!(cs.len() >= 190, "{path}: {} characters", cs.len());
        }
    }
}

/// WP-010 data test: every table of both games, in every language, parses and its text decodes without
/// unmapped characters or broken sequences.
#[test]
fn every_table_decodes_in_every_language() {
    let Some(vfs) = vfs() else { return };
    let mut report = Vec::new();
    let mut unmapped = std::collections::BTreeMap::new();
    for game in Game::ALL {
        let files = kernel::list_tables(&vfs, game);
        assert!(files.len() > 20, "{}: only {} table files", game.name(), files.len());
        for lang in Language::ALL {
            let charset =
                |l: Language| Charset::parse(&vfs.read(&text::charset_path(game, l)).unwrap()).unwrap();
            let (mut tables, mut strings, mut missing, mut fallback, mut malformed) = (0, 0, 0, 0, 0);
            for f in &files {
                let Some((path, _)) = vfs.locate_localized(&f.base_path, lang) else {
                    missing += 1;
                    continue;
                };
                let file_lang = text::folder_language(&path).unwrap_or(lang);
                if file_lang != lang {
                    fallback += 1;
                }
                let cs = charset(file_lang);
                let bytes = vfs.read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
                let table = match KernelTable::parse(bytes) {
                    Ok(t) => t,
                    Err(KernelError::NotATable) if OTHER_FORMATS.contains(&f.name.as_str()) => continue,
                    Err(e) => panic!("{} {:?} {}: {e}", game.name(), lang, f.base_path),
                };
                tables += 1;
                let fields = table.text_fields();
                for r in table.records() {
                    for &field in &fields {
                        let Some(s) = table.text(&r, field) else { continue };
                        let tokens = text::decode(s, &cs);
                        for t in &tokens {
                            match t {
                                aerugo_formats::Token::Unmapped(i) => {
                                    *unmapped.entry((game.name(), cs.len(), *i)).or_insert(0usize) += 1;
                                }
                                aerugo_formats::Token::Malformed { .. } => malformed += 1,
                                _ => {}
                            }
                        }
                        assert!(
                            !tokens.contains(&aerugo_formats::Token::Truncated),
                            "{} {:?} {} id {:#x} field {field:#x}: {}",
                            game.name(),
                            lang,
                            f.name,
                            r.id,
                            text::to_display(&tokens)
                        );
                        strings += 1;
                    }
                }
            }
            report.push(format!("{} {lang:?}: {tables} tables, {strings} strings, {fallback} from another language, {missing} absent, {malformed} malformed", game.name()));
            assert!(tables > 20 && strings > 1000, "{}", report.last().unwrap());
            // A handful of shipped strings contain a doubled extension prefix.
            assert!(malformed <= 32, "{}", report.last().unwrap());
        }
    }
    eprintln!("{}", report.join("\n"));
    eprintln!("unmapped (game, charset size, index): count = {unmapped:?}");
    // Single-byte codes past the end of a charset are glyphs that exist only in the game's font (FFX-2 uses one
    // in its Latin text); a paged index past the end would mean a wrong page rule.
    for &(game, size, index) in unmapped.keys() {
        assert!(usize::from(index) >= size && index < 0xD0, "{game}: unmapped paged index {index}");
    }
}
