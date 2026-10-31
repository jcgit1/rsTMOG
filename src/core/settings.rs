//! Persistent settings: TOML at `<config_dir>/rstmog/config.toml`.
//! Unknown keys in an existing file survive round-trips (we parse into a
//! map, merge our known fields, and write back in a stable order).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Known, typed settings. Add new fields with serde defaults so old
/// config files keep loading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// "dark" | "green" | "amber" | "blue" | "mono" | "light"
    pub theme: String,
    /// sampling cadence: 500 | 1000 | 2000 ms
    pub refresh_ms: u32,
    /// process table sort: pid|name|cpu|mem|user|threads
    pub proc_sort: String,
    pub proc_sort_desc: bool,
    pub proc_tree: bool,
    pub proc_follow: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: "dark".into(),
            refresh_ms: 1000,
            proc_sort: "cpu".into(),
            proc_sort_desc: true,
            proc_tree: false,
            proc_follow: false,
        }
    }
}

/// Config file location: `$XDG_CONFIG_HOME` (via `dirs`) or
/// `~/.config`, then `rstmog/config.toml`.
pub fn default_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("rstmog")
        .join("config.toml")
}

/// Load; missing file → Default. A corrupt file is returned as an Err
/// (the GUI shows a warning and falls back to defaults; we never silently
/// discard a user's settings).
pub fn load(path: &Path) -> Result<Settings, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(e) => Err(format!("read {}: {e}", path.display())),
    }
}

/// Serialize with unknown keys preserved: we round-trip through a
/// toml::Value map so user-added future keys survive.
pub fn save(path: &Path, s: &Settings) -> Result<(), String> {
    let mut map: toml::Table = match std::fs::read_to_string(path)
        .ok()
        .and_then(|t| t.parse::<toml::Table>().ok())
    {
        Some(existing) => existing,
        None => toml::Table::new(),
    };
    let known = s.clone().into_table();
    for (k, v) in known {
        map.insert(k, v);
    }
    let text = toml::to_string_pretty(&map).map_err(|e| format!("serialize: {e}"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create dir: {e}"))?;
    }
    // atomic-ish: write temp then rename
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("write: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

impl Settings {
    fn into_table(self) -> toml::Table {
        let mut t = toml::Table::new();
        t.insert("theme".into(), self.theme.into());
        t.insert("refresh_ms".into(), self.refresh_ms.into());
        t.insert("proc_sort".into(), self.proc_sort.into());
        t.insert("proc_sort_desc".into(), self.proc_sort_desc.into());
        t.insert("proc_tree".into(), self.proc_tree.into());
        t.insert("proc_follow".into(), self.proc_follow.into());
        t
    }
}

/// Validate-and-normalize incoming values (used by the settings dialog):
/// refresh clamps to the supported ladder, theme must be one of ours.
pub fn normalize(s: &mut Settings) {
    const THEMES: [&str; 6] = ["dark", "green", "amber", "blue", "mono", "light"];
    if !THEMES.contains(&s.theme.as_str()) {
        s.theme = "dark".into();
    }
    s.refresh_ms = match s.refresh_ms {
        0..=700 => 500,
        701..=1500 => 1000,
        _ => 2000,
    };
    const SORTS: [&str; 6] = ["pid", "name", "cpu", "mem", "user", "threads"];
    if !SORTS.contains(&s.proc_sort.as_str()) {
        s.proc_sort = "cpu".into();
    }
}

/// For tests: parse a map form directly (bypasses files).
#[allow(dead_code)]
fn as_map(s: &Settings) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    m.insert("theme".into(), s.theme.clone());
    m.insert("refresh_ms".into(), s.refresh_ms.to_string());
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rstmog-settings-test-{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn missing_file_is_defaults() {
        let s = load(Path::new("/nonexistent/rstmog/config.toml")).unwrap();
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn round_trip_exact() {
        let dir = tmpdir("rt");
        let path = dir.join("config.toml");
        let s = Settings {
            theme: "amber".into(),
            refresh_ms: 500,
            proc_tree: true,
            ..Default::default()
        };
        save(&path, &s).unwrap();
        let back = load(&path).unwrap();
        assert_eq!(back, s);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unknown_keys_survive() {
        let dir = tmpdir("unk");
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "theme = \"green\"\nrefresh_ms = 2000\nfuture_widget.enabled = true\n",
        )
        .unwrap();
        let mut s = load(&path).unwrap();
        s.theme = "blue".into();
        save(&path, &s).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("future_widget"), "unknown key lost:\n{text}");
        let back = load(&path).unwrap();
        assert_eq!(back.theme, "blue");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupt_file_errors_not_silent() {
        let dir = tmpdir("corrupt");
        let path = dir.join("config.toml");
        std::fs::write(&path, "this is = = not toml").unwrap();
        assert!(load(&path).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn partial_file_uses_defaults() {
        let dir = tmpdir("partial");
        let path = dir.join("config.toml");
        std::fs::write(&path, "theme = \"mono\"\n").unwrap();
        let s = load(&path).unwrap();
        assert_eq!(s.theme, "mono");
        assert_eq!(s.refresh_ms, 1000); // serde(default)
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn normalize_ladders() {
        let mut s = Settings {
            theme: "hotpink".into(),
            refresh_ms: 750,
            proc_sort: "bogus".into(),
            ..Default::default()
        };
        normalize(&mut s);
        assert_eq!(s.theme, "dark");
        assert_eq!(s.refresh_ms, 1000);
        assert_eq!(s.proc_sort, "cpu");
        let mut s2 = Settings {
            refresh_ms: 100,
            ..Default::default()
        };
        normalize(&mut s2);
        assert_eq!(s2.refresh_ms, 500);
    }

    #[test]
    fn save_creates_app_dir() {
        let dir = tmpdir("mkdir");
        let path = dir.join("nested").join("deeper").join("config.toml");
        save(&path, &Settings::default()).unwrap();
        assert!(path.exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
