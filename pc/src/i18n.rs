//! Interface language. Translations live in `pc/locales/<code>.yml`; every file there becomes a
//! selectable language, with English as the fallback for missing keys.

use rust_i18n::t;
use std::path::PathBuf;

/// Available languages as (code, own name), sorted by code.
pub fn languages() -> Vec<(&'static str, String)> {
    let mut codes = rust_i18n::available_locales!();
    codes.sort_unstable();
    codes.into_iter().map(|c| (c, t!("language.name", locale = c).to_string())).collect()
}

pub fn current() -> String {
    rust_i18n::locale().to_string()
}

/// Maps a tag like `id`, `id_ID.UTF-8`, `pt-br` or `en_US` to an available locale.
fn resolve(tag: &str) -> Option<&'static str> {
    let tag = tag.split(['.', '@']).next()?.replace('_', "-");
    if tag.is_empty() || tag == "C" || tag == "POSIX" {
        return None;
    }
    let codes = rust_i18n::available_locales!();
    let lang = tag.split('-').next()?;
    codes
        .iter()
        .find(|c| c.eq_ignore_ascii_case(&tag))
        .or_else(|| codes.iter().find(|c| c.eq_ignore_ascii_case(lang)))
        .or_else(|| codes.iter().find(|c| c.split('-').next().is_some_and(|l| l.eq_ignore_ascii_case(lang))))
        .copied()
}

fn config_file() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(dir.join("acam").join("language"))
}

/// Picks the language: `--lang`, then `$ACAM_LANG`, then the choice saved by the GUI, then the
/// system locale (`$LANGUAGE`, `$LC_ALL`, `$LC_MESSAGES`, `$LANG`), then English.
pub fn init(cli: Option<&str>) -> Result<(), String> {
    if let Some(tag) = cli {
        let code = resolve(tag).ok_or_else(|| {
            let list = languages().iter().map(|(c, _)| *c).collect::<Vec<_>>().join(", ");
            t!("cli.unknown_lang", lang = tag, list = list).to_string()
        })?;
        rust_i18n::set_locale(code);
        return Ok(());
    }
    let saved = config_file().and_then(|p| std::fs::read_to_string(p).ok());
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let candidates = env("ACAM_LANG")
        .into_iter()
        .chain(saved.map(|s| s.trim().to_string()))
        .chain(env("LANGUAGE").into_iter().flat_map(|l| l.split(':').map(str::to_string).collect::<Vec<_>>()))
        .chain(["LC_ALL", "LC_MESSAGES", "LANG"].into_iter().filter_map(env));
    let code = candidates.filter_map(|c| resolve(&c)).next().unwrap_or("en");
    rust_i18n::set_locale(code);
    Ok(())
}

/// Switches language at runtime and remembers it for the next start.
pub fn set(code: &str) {
    rust_i18n::set_locale(code);
    if let Some(p) = config_file() {
        let _ = p.parent().map(std::fs::create_dir_all);
        let _ = std::fs::write(p, code);
    }
}

/// `--lang` must be known before clap builds the (translated) help, so it is read ahead of parsing.
pub fn lang_arg() -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--lang" {
            return args.next();
        }
        if let Some(v) = a.strip_prefix("--lang=") {
            return Some(v.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    #[test]
    fn resolves_system_tags() {
        assert_eq!(resolve("id_ID.UTF-8"), Some("id"));
        assert_eq!(resolve("en_US.UTF-8"), Some("en"));
        assert_eq!(resolve("ID"), Some("id"));
        assert_eq!(resolve("C"), None);
        assert_eq!(resolve("xx_YY"), None);
    }

    fn load(path: &std::path::Path) -> BTreeMap<String, String> {
        let text = std::fs::read_to_string(path).unwrap();
        let mut map = BTreeMap::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with("_version") {
                continue;
            }
            let (k, v) = line.split_once(": ").unwrap_or_else(|| panic!("{}:{}: expected `key: \"value\"`", path.display(), n + 1));
            assert!(v.starts_with('"') && v.ends_with('"'), "{}:{}: value must be quoted", path.display(), n + 1);
            assert!(map.insert(k.to_string(), v.to_string()).is_none(), "{}: duplicate key {k}", path.display());
        }
        map
    }

    fn placeholders(v: &str) -> BTreeSet<&str> {
        v.match_indices("%{").filter_map(|(i, _)| v[i + 2..].split_once('}').map(|(p, _)| p)).collect()
    }

    /// Every locale file has exactly the keys of en.yml and keeps its placeholders.
    #[test]
    fn locale_files_match_english() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
        let en = load(&dir.join("en.yml"));
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let other = load(&path);
            let name = path.file_name().unwrap().to_string_lossy();
            for (k, v) in &en {
                let o = other.get(k).unwrap_or_else(|| panic!("{name}: missing key {k}"));
                assert_eq!(placeholders(v), placeholders(o), "{name}: placeholders of {k}");
            }
            for k in other.keys() {
                assert!(en.contains_key(k), "{name}: key {k} is not in en.yml");
            }
            assert!(rust_i18n::available_locales!().contains(&&*name.trim_end_matches(".yml")), "{name} not compiled in");
        }
    }

    /// Translated code paths use keys that exist (catches typos in t!() calls).
    #[test]
    fn source_keys_exist() {
        let en = load(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("locales/en.yml"));
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for entry in std::fs::read_dir(src).unwrap() {
            let path = entry.unwrap().path();
            let text = std::fs::read_to_string(&path).unwrap();
            for (i, _) in text.match_indices("t!(\"") {
                // Skip macros that merely end in "t!", such as format!.
                if text[..i].chars().last().is_some_and(|c| c.is_alphanumeric() || c == '_') {
                    continue;
                }
                let key = text[i + 4..].split('"').next().unwrap();
                assert!(en.contains_key(key), "{}: unknown key {key}", path.display());
            }
        }
    }
}
