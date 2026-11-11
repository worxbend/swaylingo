//! Optional per-app layout rules.
//!
//! The file is TOML. `fallback` and each `[apps]` value are a layout index or
//! a name. Names are resolved against the keyboard's `xkb_layout_names`.
//! App keys are a Wayland `app_id` or an X11 `WM_CLASS`.

use std::collections::HashMap;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::memory::DEFAULT_LAYOUT;

/// A layout token before it is matched to a Sway layout list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutToken {
    Index(u32),
    Name(String),
}

/// What the config file asked for. `fallback` is absent when the file did not set one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigFile {
    pub fallback: Option<LayoutToken>,
    pub apps: Vec<(String, LayoutToken)>,
}

/// Indexes ready to store on a window.
#[derive(Debug)]
pub struct Rules {
    pub fallback: u32,
    pub apps: HashMap<String, u32>,
}

/// Where a bad token is reported from. The fallback can be `--default`.
pub struct RuleSource<'a> {
    pub fallback_label: &'a str,
    pub apps_label: &'a str,
}

impl ConfigFile {
    fn empty() -> Self {
        Self {
            fallback: None,
            apps: Vec::new(),
        }
    }
}

/// `$XDG_CONFIG_HOME/sway-lang/config.toml`, or `~/.config/sway-lang/config.toml`.
pub fn default_path() -> Result<PathBuf, String> {
    let xdg = env::var_os("XDG_CONFIG_HOME");
    let home = env::var_os("HOME");
    config_path(xdg.as_deref(), home.as_deref())
}

pub fn config_path(xdg: Option<&OsStr>, home: Option<&OsStr>) -> Result<PathBuf, String> {
    if let Some(xdg) = xdg.filter(|xdg| !xdg.is_empty()) {
        return Ok(PathBuf::from(xdg).join("sway-lang").join("config.toml"));
    }
    let home = home.ok_or_else(|| "HOME is not set".to_string())?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("sway-lang")
        .join("config.toml"))
}

/// `--config` was set. A missing file is an error.
pub fn load_required(path: &Path) -> Result<ConfigFile, String> {
    match read_text(path)? {
        Some(text) => parse_config(&text, path),
        None => Err(format!("{}: file not found", path.display())),
    }
}

/// The default path. A missing file is an empty config.
pub fn load_optional(path: &Path) -> Result<ConfigFile, String> {
    match read_text(path)? {
        Some(text) => parse_config(&text, path),
        None => Ok(ConfigFile::empty()),
    }
}

pub fn resolve_rules(
    config: &ConfigFile,
    names: &[String],
    from: &RuleSource<'_>,
) -> Result<Rules, String> {
    let fallback = match &config.fallback {
        Some(token) => {
            resolve_token(token, names).map_err(|err| format!("{}: {err}", from.fallback_label))?
        }
        None => DEFAULT_LAYOUT,
    };
    let mut apps = HashMap::new();
    for (app, token) in &config.apps {
        let index = resolve_token(token, names)
            .map_err(|err| format!("{}: apps.{app}: {err}", from.apps_label))?;
        apps.insert(app.clone(), index);
    }
    Ok(Rules { fallback, apps })
}

fn read_text(path: &Path) -> Result<Option<String>, String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(format!("{}: {err}", path.display())),
    }
}

fn parse_config(text: &str, path: &Path) -> Result<ConfigFile, String> {
    if text.trim().is_empty() {
        return Ok(ConfigFile::empty());
    }
    let table: toml::Table = text
        .parse()
        .map_err(|err| format!("{}: {err}", path.display()))?;
    let mut fallback = None;
    let mut apps = Vec::new();
    for (key, value) in table {
        match key.as_str() {
            "fallback" => fallback = Some(parse_token(path, "fallback", &value)?),
            "apps" => apps = parse_apps(path, &value)?,
            other => return Err(format!("{}: unknown key `{other}`", path.display())),
        }
    }
    Ok(ConfigFile { fallback, apps })
}

fn parse_apps(path: &Path, value: &toml::Value) -> Result<Vec<(String, LayoutToken)>, String> {
    let Some(table) = value.as_table() else {
        return Err(format!("{}: apps must be a table", path.display()));
    };
    let mut apps = Vec::new();
    for (app, value) in table {
        if app.is_empty() {
            return Err(format!("{}: apps key is empty", path.display()));
        }
        let token = parse_token(path, &format!("apps.{app}"), value)?;
        apps.push((app.clone(), token));
    }
    Ok(apps)
}

fn parse_token(path: &Path, label: &str, value: &toml::Value) -> Result<LayoutToken, String> {
    if let Some(text) = value.as_str() {
        let text = text.trim();
        if text.is_empty() {
            return Err(format!("{}: {label} is empty", path.display()));
        }
        return Ok(LayoutToken::Name(text.to_string()));
    }
    let Some(index) = value.as_integer() else {
        return Err(format!(
            "{}: {label} must be a string or a non-negative integer",
            path.display()
        ));
    };
    let index = u32::try_from(index).map_err(|_| {
        format!(
            "{}: {label} must be a string or a non-negative integer",
            path.display()
        )
    })?;
    Ok(LayoutToken::Index(index))
}

pub(crate) fn resolve_token(token: &LayoutToken, names: &[String]) -> Result<u32, String> {
    match token {
        LayoutToken::Index(index) => index_in_range(*index, names),
        LayoutToken::Name(name) => resolve_name(name, names),
    }
}

fn resolve_name(token: &str, names: &[String]) -> Result<u32, String> {
    let token = token.trim();
    if let Some(index) = decimal_index(token) {
        return index_in_range(index, names);
    }
    if let Some(index) = exact_name(token, names) {
        return Ok(index);
    }
    if let Some(index) = short_code(token, names) {
        return Ok(index);
    }
    Err(unknown_layout(token, names))
}

fn decimal_index(token: &str) -> Option<u32> {
    if token.is_empty() || !token.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    // "01" is a name, not index 1.
    if token.len() > 1 && token.starts_with('0') {
        return None;
    }
    token.parse().ok()
}

fn index_in_range(index: u32, names: &[String]) -> Result<u32, String> {
    if (index as usize) < names.len() {
        return Ok(index);
    }
    Err(unknown_layout(&index.to_string(), names))
}

fn exact_name(token: &str, names: &[String]) -> Option<u32> {
    let index = names.iter().position(|name| name == token).or_else(|| {
        names
            .iter()
            .position(|name| name.eq_ignore_ascii_case(token))
    })?;
    u32::try_from(index).ok()
}

struct ShortCode {
    codes: &'static [&'static str],
    language: &'static str,
    prefer: Option<&'static str>,
}

const SHORT_CODES: &[ShortCode] = &[
    ShortCode {
        codes: &["us", "en"],
        language: "english",
        prefer: Some("(us)"),
    },
    ShortCode {
        codes: &["ua", "uk"],
        language: "ukrainian",
        prefer: None,
    },
    ShortCode {
        codes: &["ru"],
        language: "russian",
        prefer: None,
    },
    ShortCode {
        codes: &["de"],
        language: "german",
        prefer: None,
    },
    ShortCode {
        codes: &["pl"],
        language: "polish",
        prefer: None,
    },
    ShortCode {
        codes: &["fr"],
        language: "french",
        prefer: None,
    },
];

fn short_code(token: &str, names: &[String]) -> Option<u32> {
    let code = SHORT_CODES.iter().find(|code| {
        code.codes
            .iter()
            .any(|name| name.eq_ignore_ascii_case(token))
    })?;
    if let Some(prefer) = code.prefer
        && let Some(index) = find_name(names, code.language, Some(prefer))
    {
        return Some(index);
    }
    find_name(names, code.language, None)
}

fn find_name(names: &[String], language: &str, prefer: Option<&str>) -> Option<u32> {
    names
        .iter()
        .position(|name| {
            contains_ascii(name, language)
                && prefer.is_none_or(|prefer| contains_ascii(name, prefer))
        })
        .and_then(|index| u32::try_from(index).ok())
}

fn contains_ascii(haystack: &str, needle: &str) -> bool {
    haystack
        .to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

fn unknown_layout(token: &str, names: &[String]) -> String {
    let mut message = format!("unknown layout \"{token}\"");
    if names.is_empty() {
        message.push_str("\nknown layouts: none");
        return message;
    }
    message.push_str("\nknown layouts:");
    for (index, name) in names.iter().enumerate() {
        message.push('\n');
        message.push_str(&index.to_string());
        message.push(' ');
        message.push_str(name);
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Vec<String> {
        vec![
            "English (US)".to_string(),
            "Ukrainian".to_string(),
            "Russian".to_string(),
            "German".to_string(),
            "Polish".to_string(),
            "French".to_string(),
        ]
    }

    #[test]
    fn bad_token_error_includes_the_known_layout_name() {
        let config = parse_config("fallback = \"zz\"\n", Path::new("sway-lang.toml")).unwrap();
        let err = resolve_rules(
            &config,
            &names(),
            &RuleSource {
                fallback_label: "sway-lang.toml",
                apps_label: "sway-lang.toml",
            },
        )
        .unwrap_err();

        assert!(err.contains("sway-lang.toml"), "{err}");
        assert!(err.contains("0 English (US)"), "{err}");
        assert!(err.contains("1 Ukrainian"), "{err}");
    }

    #[test]
    fn parse_error_names_the_file() {
        let err = parse_config("fallback = [\n", Path::new("/tmp/sway-lang.toml")).unwrap_err();
        assert!(err.contains("/tmp/sway-lang.toml"), "{err}");
    }

    #[test]
    fn us_and_en_prefer_a_name_containing_us() {
        let layouts = vec![
            "English".to_string(),
            "English (UK)".to_string(),
            "English (US)".to_string(),
        ];
        assert_eq!(
            resolve_token(&LayoutToken::Name("us".into()), &layouts).unwrap(),
            2
        );
        assert_eq!(
            resolve_token(&LayoutToken::Name("en".into()), &layouts).unwrap(),
            2
        );
    }

    #[test]
    fn us_uses_english_when_no_name_contains_us() {
        let layouts = vec!["English (UK)".to_string(), "Ukrainian".to_string()];
        assert_eq!(
            resolve_token(&LayoutToken::Name("us".into()), &layouts).unwrap(),
            0
        );
    }

    #[test]
    fn short_codes_map_to_language_names() {
        let layouts = names();
        let codes = [
            ("ua", 1),
            ("uk", 1),
            ("ru", 2),
            ("de", 3),
            ("pl", 4),
            ("fr", 5),
        ];
        for (code, index) in codes {
            assert_eq!(
                resolve_token(&LayoutToken::Name(code.into()), &layouts).unwrap(),
                index,
                "{code}"
            );
        }
    }

    #[test]
    fn exact_name_and_integer_index_match() {
        let layouts = names();
        assert_eq!(
            resolve_token(&LayoutToken::Name("Ukrainian".into()), &layouts).unwrap(),
            1
        );
        assert_eq!(resolve_token(&LayoutToken::Index(1), &layouts).unwrap(), 1);
        assert_eq!(
            resolve_token(&LayoutToken::Name("1".into()), &layouts).unwrap(),
            1
        );
        let err = resolve_token(&LayoutToken::Index(9), &layouts).unwrap_err();
        assert!(err.contains("3 German"), "{err}");
    }

    #[test]
    fn exact_name_wins_over_the_short_code() {
        let layouts = vec!["us".to_string(), "English (US)".to_string()];
        assert_eq!(
            resolve_token(&LayoutToken::Name("us".into()), &layouts).unwrap(),
            0
        );
    }

    #[test]
    fn example_file_parses() {
        let config = parse_config(
            include_str!("../config.example.toml"),
            Path::new("config.example.toml"),
        )
        .unwrap();
        assert_eq!(config.fallback, Some(LayoutToken::Name("us".into())));
        let token = |app: &str| {
            config
                .apps
                .iter()
                .find(|(name, _)| name == app)
                .map(|(_, token)| token)
        };
        assert_eq!(token("Telegram"), Some(&LayoutToken::Name("ua".into())));
        assert_eq!(token("discord"), Some(&LayoutToken::Name("ua".into())));
        assert_eq!(
            token("jetbrains-idea"),
            Some(&LayoutToken::Name("us".into()))
        );
        assert_eq!(token("Code"), Some(&LayoutToken::Name("us".into())));
        assert_eq!(token("code"), Some(&LayoutToken::Name("us".into())));
    }

    #[test]
    fn apps_accept_a_string_or_an_integer() {
        let text = "fallback = 0\n\n[apps]\nTelegram = \"ua\"\nCode = 0\n";
        let config = parse_config(text, Path::new("c.toml")).unwrap();
        let rules = resolve_rules(
            &config,
            &names(),
            &RuleSource {
                fallback_label: "c.toml",
                apps_label: "c.toml",
            },
        )
        .unwrap();
        assert_eq!(rules.fallback, 0);
        assert_eq!(rules.apps["Telegram"], 1);
        assert_eq!(rules.apps["Code"], 0);
    }

    #[test]
    fn missing_explicit_config_names_the_path() {
        let path = std::env::temp_dir().join(format!("sway-lang-missing-{}", std::process::id()));
        let _ = fs::remove_file(&path);
        let err = load_required(&path).unwrap_err();
        assert!(err.contains(&path.display().to_string()), "{err}");
        assert!(err.contains("file not found"), "{err}");
    }

    #[test]
    fn missing_default_path_is_an_empty_config() {
        let path = std::env::temp_dir().join(format!("sway-lang-absent-{}", std::process::id()));
        let _ = fs::remove_file(&path);
        let config = load_optional(&path).unwrap();
        assert_eq!(config.fallback, None);
        assert!(config.apps.is_empty());
    }

    #[test]
    fn config_path_uses_xdg_then_home() {
        let xdg = config_path(Some(OsStr::new("/xdg")), Some(OsStr::new("/home/me"))).unwrap();
        assert_eq!(xdg, PathBuf::from("/xdg/sway-lang/config.toml"));
        let home = config_path(None, Some(OsStr::new("/home/me"))).unwrap();
        assert_eq!(
            home,
            PathBuf::from("/home/me/.config/sway-lang/config.toml")
        );
        let empty = config_path(Some(OsStr::new("")), Some(OsStr::new("/home/me"))).unwrap();
        assert_eq!(
            empty,
            PathBuf::from("/home/me/.config/sway-lang/config.toml")
        );
        assert!(config_path(None, None).unwrap_err().contains("HOME"));
    }
}
