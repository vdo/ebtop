//! btop-compatible color themes.
//!
//! Loads btop `.theme` files with the same semantics as btop (see
//! `src/btop_theme.cpp` upstream): same keys, color formats, defaults,
//! fallbacks and gradient generation. btop's own theme files are bundled, and
//! installed/user btop themes are picked up too, so any btop theme works.

use std::collections::HashMap;
use std::ffi::CStr;
use std::fs;
use std::path::{Path, PathBuf};

use ratatui::style::Color;

mod bundled {
    include!(concat!(env!("OUT_DIR"), "/themes.rs"));
}

/// btop's built-in "Default" theme (btop v1.4.7).
const DEFAULT: &[(&str, &str)] = &[
    ("main_bg", "#00"),
    ("main_fg", "#cc"),
    ("title", "#ee"),
    ("hi_fg", "#b54040"),
    ("selected_bg", "#6a2f2f"),
    ("selected_fg", "#ee"),
    ("inactive_fg", "#40"),
    ("graph_text", "#60"),
    ("meter_bg", "#40"),
    ("proc_misc", "#0de756"),
    ("cpu_box", "#556d59"),
    ("mem_box", "#6c6c4b"),
    ("net_box", "#5c588d"),
    ("proc_box", "#805252"),
    ("div_line", "#30"),
    ("temp_start", "#4897d4"),
    ("temp_mid", "#5474e8"),
    ("temp_end", "#ff40b6"),
    ("cpu_start", "#77ca9b"),
    ("cpu_mid", "#cbc06c"),
    ("cpu_end", "#dc4c4c"),
    ("free_start", "#384f21"),
    ("free_mid", "#b5e685"),
    ("free_end", "#dcff85"),
    ("cached_start", "#163350"),
    ("cached_mid", "#74e6fc"),
    ("cached_end", "#26c5ff"),
    ("available_start", "#4e3f0e"),
    ("available_mid", "#ffd77a"),
    ("available_end", "#ffb814"),
    ("used_start", "#592b26"),
    ("used_mid", "#d9626d"),
    ("used_end", "#ff4769"),
    ("download_start", "#291f75"),
    ("download_mid", "#4f43a3"),
    ("download_end", "#b0a9de"),
    ("upload_start", "#620665"),
    ("upload_mid", "#7d4180"),
    ("upload_end", "#dcafde"),
    ("process_start", "#80d0a3"),
    ("process_mid", "#dcd179"),
    ("process_end", "#d45454"),
];

/// Keys btop leaves unset (then falls back) instead of taking the default.
const OPTIONAL: &[&str] = &["meter_bg", "process_start", "process_mid", "process_end", "graph_text"];

/// 101-step color ramp, indexed by percent like btop's.
#[derive(Clone)]
pub struct Gradient(Vec<Color>);

impl Gradient {
    /// Color at `t` in [0, 1].
    pub fn at(&self, t: f64) -> Color {
        let i = (t.clamp(0.0, 1.0) * 100.0).round() as usize;
        self.0[i.min(self.0.len() - 1)]
    }

    fn solid(c: Color) -> Self {
        Self(vec![c; 101])
    }

    /// Linear start->end, or start->mid->end in two halves (btop's algorithm).
    fn rgb(start: (u8, u8, u8), mid: Option<(u8, u8, u8)>, end: Option<(u8, u8, u8)>) -> Self {
        let Some(end) = end else { return Self::solid(rgb(start)) };
        let lerp = |a: (u8, u8, u8), b: (u8, u8, u8), i: i32, n: i32| {
            let c = |x: u8, y: u8| (x as i32 + i * (y as i32 - x as i32) / n) as u8;
            Color::Rgb(c(a.0, b.0), c(a.1, b.1), c(a.2, b.2))
        };
        Self(
            (0..=100)
                .map(|i| match mid {
                    None => lerp(start, end, i, 100),
                    Some(m) if i <= 50 => lerp(start, m, i, 50),
                    Some(m) => lerp(m, end, i - 50, 50),
                })
                .collect(),
        )
    }

    /// Stepped ramp for the 16-color TTY theme.
    fn steps(start: Color, mid: Option<Color>, end: Color) -> Self {
        Self(
            (0..=100)
                .map(|i| match mid {
                    None if i <= 50 => start,
                    None => end,
                    Some(_) if i <= 33 => start,
                    Some(m) if i <= 66 => m,
                    Some(_) => end,
                })
                .collect(),
        )
    }
}

fn rgb((r, g, b): (u8, u8, u8)) -> Color {
    Color::Rgb(r, g, b)
}

#[derive(Clone)]
pub struct Theme {
    pub name: String,
    /// `None` = terminal default (transparent).
    pub bg: Option<Color>,
    pub fg: Color,
    pub title: Color,
    pub hi: Color,
    pub selected_bg: Color,
    pub selected_fg: Color,
    pub inactive: Color,
    pub graph_text: Color,
    pub meter_bg: Color,
    pub proc_misc: Color,
    pub div_line: Color,
    pub cpu_box: Color,
    pub mem_box: Color,
    pub net_box: Color,
    pub proc_box: Color,
    pub cpu: Gradient,
    pub free: Gradient,
    pub cached: Gradient,
    pub used: Gradient,
    pub download: Gradient,
    pub upload: Gradient,
    pub process: Gradient,
}

/// Parses a btop color value: `#RRGGBB`, `#BW` (greyscale) or `R G B`.
fn parse_color(v: &str) -> Option<(u8, u8, u8)> {
    let v = v.trim();
    if let Some(hex) = v.strip_prefix('#') {
        let byte = |s: &str| u8::from_str_radix(s, 16).ok();
        return match hex.len() {
            2 => byte(hex).map(|b| (b, b, b)),
            6 => Some((byte(&hex[0..2])?, byte(&hex[2..4])?, byte(&hex[4..6])?)),
            _ => None,
        };
    }
    let parts: Vec<u8> =
        v.split_whitespace().map(|p| p.parse::<i64>().map(|n| n.clamp(0, 255) as u8)).collect::<Result<_, _>>().ok()?;
    match parts[..] {
        [r, g, b] => Some((r, g, b)),
        _ => None,
    }
}

/// Reads `theme[key]="value"` lines the way btop's loader does.
fn parse_file(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim_start();
        if line.starts_with('#') {
            continue;
        }
        let Some(rest) = line.split_once('[').map(|(_, r)| r) else { continue };
        let Some((key, rest)) = rest.split_once(']') else { continue };
        if !DEFAULT.iter().any(|(k, _)| *k == key) {
            continue;
        }
        let Some((_, value)) = rest.split_once('=') else { continue };
        let value = value.trim_start();
        let value = match value.strip_prefix('"') {
            Some(v) => v.split('"').next().unwrap_or(""),
            None => value.trim_end(),
        };
        out.insert(key.to_string(), value.to_string());
    }
    out
}

impl Theme {
    /// Builds a theme from btop key/value pairs, applying btop's defaults and
    /// fallbacks. `background` mirrors btop's `theme_background` option.
    fn from_values(name: &str, source: &HashMap<String, String>, background: bool) -> Self {
        let mut colors: HashMap<&str, Option<(u8, u8, u8)>> = HashMap::new();
        for &(key, default) in DEFAULT {
            let value = source.get(key).map(String::as_str);
            let parsed = match value {
                // empty mid/end: gradient without that stop; empty bg: terminal default
                Some("") if key.ends_with("_mid") || key.ends_with("_end") || key == "main_bg" => {
                    colors.insert(key, None);
                    continue;
                }
                Some(v) => parse_color(v),
                None => None,
            };
            match parsed {
                Some(c) => {
                    colors.insert(key, Some(c));
                }
                None if !OPTIONAL.contains(&key) => {
                    colors.insert(key, parse_color(default));
                }
                None => {}
            }
        }
        let get = |k: &str| colors.get(k).copied().flatten();
        let fallback = |k: &str, other: &str| get(k).or_else(|| get(other));
        let solid = |k: &str| rgb(get(k).unwrap_or((0xcc, 0xcc, 0xcc)));
        let inactive = solid("inactive_fg");
        let gradient = |base: &str| {
            let (start, mid, end) = if base == "process" && get("process_start").is_none() {
                (get("cpu_start"), get("cpu_mid"), get("cpu_end"))
            } else {
                (get(&format!("{base}_start")), get(&format!("{base}_mid")), get(&format!("{base}_end")))
            };
            Gradient::rgb(start.unwrap_or((0xcc, 0xcc, 0xcc)), mid, end)
        };
        Theme {
            name: name.to_string(),
            bg: if background { get("main_bg").map(rgb) } else { None },
            fg: solid("main_fg"),
            title: solid("title"),
            hi: solid("hi_fg"),
            selected_bg: solid("selected_bg"),
            selected_fg: solid("selected_fg"),
            inactive,
            graph_text: fallback("graph_text", "inactive_fg").map_or(inactive, rgb),
            meter_bg: fallback("meter_bg", "inactive_fg").map_or(inactive, rgb),
            proc_misc: solid("proc_misc"),
            div_line: solid("div_line"),
            cpu_box: solid("cpu_box"),
            mem_box: solid("mem_box"),
            net_box: solid("net_box"),
            proc_box: solid("proc_box"),
            cpu: gradient("cpu"),
            free: gradient("free"),
            cached: gradient("cached"),
            used: gradient("used"),
            download: gradient("download"),
            upload: gradient("upload"),
            process: gradient("process"),
        }
    }

    /// btop's built-in 16-color TTY theme.
    fn tty(background: bool) -> Self {
        use Color::*;
        let g = |s, m, e| Gradient::steps(s, m, e);
        Theme {
            name: "TTY".into(),
            bg: background.then_some(Black),
            fg: Gray,
            title: White,
            hi: LightRed,
            selected_bg: Red,
            selected_fg: White,
            inactive: DarkGray,
            graph_text: DarkGray,
            meter_bg: DarkGray,
            proc_misc: LightGreen,
            div_line: DarkGray,
            cpu_box: Green,
            mem_box: Yellow,
            net_box: Magenta,
            proc_box: Red,
            cpu: g(LightGreen, Some(LightYellow), LightRed),
            free: g(Green, None, LightGreen),
            cached: g(Cyan, None, LightCyan),
            used: g(Red, None, LightRed),
            download: g(Blue, None, LightBlue),
            upload: g(Magenta, None, LightMagenta),
            process: g(Green, Some(Yellow), Red),
        }
    }

    pub fn default_theme(background: bool) -> Self {
        let values = DEFAULT.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        Self::from_values("Default", &values, background)
    }
}

// ---------------------------------------------------------------- discovery

#[derive(Clone)]
enum Source {
    Default,
    Tty,
    Bundled(&'static str),
    File(PathBuf),
}

#[derive(Clone)]
pub struct Entry {
    pub name: String,
    source: Source,
}

impl Entry {
    pub fn origin(&self) -> String {
        match &self.source {
            Source::Default | Source::Tty => "built-in".into(),
            Source::Bundled(_) => "bundled".into(),
            Source::File(p) => p.display().to_string(),
        }
    }

    pub fn load(&self, background: bool) -> Theme {
        match &self.source {
            Source::Default => Theme::default_theme(background),
            Source::Tty => Theme::tty(background),
            Source::Bundled(text) => Theme::from_values(&self.name, &parse_file(text), background),
            Source::File(path) => match fs::read_to_string(path) {
                Ok(text) => Theme::from_values(&self.name, &parse_file(&text), background),
                Err(_) => Theme::default_theme(background),
            },
        }
    }
}

/// Home directories to search: the invoking user's (under sudo/pkexec) first,
/// then root's own.
fn homes() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let pw_home = |pw: *mut libc::passwd| {
        (!pw.is_null()).then(|| PathBuf::from(unsafe { CStr::from_ptr((*pw).pw_dir) }.to_string_lossy().into_owned()))
    };
    if let Ok(user) = std::env::var("SUDO_USER")
        && let Ok(c) = std::ffi::CString::new(user)
        && let Some(h) = pw_home(unsafe { libc::getpwnam(c.as_ptr()) })
    {
        out.push(h);
    }
    if let Some(uid) = std::env::var("PKEXEC_UID").ok().and_then(|u| u.parse().ok())
        && let Some(h) = pw_home(unsafe { libc::getpwuid(uid) })
    {
        out.push(h);
    }
    if let Some(h) = std::env::var_os("HOME") {
        out.push(h.into());
    }
    out.dedup();
    out
}

fn config_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for home in homes() {
        dirs.push(home.join(".config"));
    }
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME") {
        dirs.insert(0, x.into());
    }
    dirs.dedup();
    dirs
}

/// Theme directories in btop's priority order (user before system).
fn theme_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for cfg in config_dirs() {
        dirs.push(cfg.join("ebtop/themes"));
        dirs.push(cfg.join("btop/themes"));
    }
    dirs.push("/usr/local/share/btop/themes".into());
    dirs.push("/usr/share/btop/themes".into());
    dirs
}

/// Every available theme: Default and TTY first, then the rest by name. A
/// user or system theme overrides a bundled one with the same name.
pub fn available() -> Vec<Entry> {
    let mut found: Vec<Entry> = Vec::new();
    let mut from_files: Vec<Entry> = Vec::new();
    for dir in theme_dirs() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        let mut files: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
        files.sort();
        for path in files {
            if path.extension().is_some_and(|e| e == "theme")
                && let Some(stem) = path.file_stem().map(|s| s.to_string_lossy().into_owned())
                && !from_files.iter().any(|e| e.name == stem)
            {
                from_files.push(Entry { name: stem, source: Source::File(path) });
            }
        }
    }
    found.push(Entry { name: "Default".into(), source: Source::Default });
    found.push(Entry { name: "TTY".into(), source: Source::Tty });
    let mut rest = from_files;
    for (name, text) in bundled::THEMES {
        if !rest.iter().any(|e| e.name == *name) {
            rest.push(Entry { name: name.to_string(), source: Source::Bundled(text) });
        }
    }
    rest.sort_by_key(|e| e.name.to_lowercase());
    found.extend(rest);
    found
}

/// Finds a theme by name (as btop: file stem, file name or full path). A path
/// to a `.theme` file outside the theme directories also works.
pub fn find(entries: &[Entry], wanted: &str) -> Option<Entry> {
    let wanted = wanted.trim();
    entries
        .iter()
        .find(|e| {
            e.name == wanted
                || matches!(&e.source, Source::File(p)
                    if p.as_os_str() == wanted || p.file_name().is_some_and(|f| f == wanted))
                || matches!(&e.source, Source::Bundled(_) if format!("{}.theme", e.name) == wanted)
        })
        .cloned()
        .or_else(|| {
            let p = Path::new(wanted);
            p.is_file().then(|| Entry {
                name: p.file_stem().map_or(wanted.to_string(), |s| s.to_string_lossy().into_owned()),
                source: Source::File(p.to_path_buf()),
            })
        })
}

/// Theme settings from config files: `color_theme` and `theme_background`,
/// read from ebtop's own config first, then btop's, so ebtop follows the
/// user's btop theme by default.
pub fn configured() -> (Option<String>, Option<bool>) {
    let mut theme = None;
    let mut background = None;
    for cfg in config_dirs() {
        for file in [cfg.join("ebtop/ebtop.conf"), cfg.join("btop/btop.conf")] {
            let Ok(text) = fs::read_to_string(&file) else { continue };
            for line in text.lines() {
                let Some((k, v)) = line.split_once('=') else { continue };
                let v = v.trim().trim_matches('"');
                match k.trim() {
                    "color_theme" if theme.is_none() && !v.is_empty() => theme = Some(v.to_string()),
                    "theme_background" if background.is_none() => {
                        background = Some(matches!(v.to_ascii_lowercase().as_str(), "true" | "1" | "yes"))
                    }
                    _ => {}
                }
            }
        }
    }
    (theme, background)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_btop_color_formats() {
        assert_eq!(parse_color("#ff8000"), Some((255, 128, 0)));
        assert_eq!(parse_color("#90"), Some((0x90, 0x90, 0x90)));
        assert_eq!(parse_color("10 20 300"), Some((10, 20, 255)));
        assert_eq!(parse_color("#12345"), None);
        assert_eq!(parse_color("nope"), None);
    }

    #[test]
    fn parses_theme_lines_like_btop() {
        let t = parse_file(
            "# comment\ntheme[main_bg]=\"\"\ntheme[main_fg]=\"#F8F8F2\" #b05475\"\n\
             theme[title]=255 0 0\ntheme[bogus]=\"#fff\"\n",
        );
        assert_eq!(t.get("main_bg").map(String::as_str), Some(""));
        assert_eq!(t.get("main_fg").map(String::as_str), Some("#F8F8F2"));
        assert_eq!(t.get("title").map(String::as_str), Some("255 0 0"));
        assert!(!t.contains_key("bogus"));
    }

    #[test]
    fn applies_btop_defaults_and_fallbacks() {
        let src: HashMap<String, String> = [
            ("main_bg", ""),
            ("inactive_fg", "#101010"),
            ("cpu_start", "#000000"),
            ("cpu_mid", ""),
            ("cpu_end", "#646464"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let t = Theme::from_values("t", &src, true);
        assert_eq!(t.bg, None, "empty main_bg means terminal default");
        assert_eq!(t.meter_bg, Color::Rgb(16, 16, 16), "meter_bg falls back to inactive_fg");
        assert_eq!(t.graph_text, Color::Rgb(16, 16, 16), "graph_text falls back to inactive_fg");
        assert_eq!(t.title, Color::Rgb(0xee, 0xee, 0xee), "missing keys take btop's default");
        // two-stop gradient, and process falls back to cpu
        assert_eq!(t.cpu.at(0.5), Color::Rgb(50, 50, 50));
        assert_eq!(t.process.at(1.0), Color::Rgb(100, 100, 100));
    }

    #[test]
    fn bundled_themes_match_btop_and_parse() {
        assert_eq!(bundled::THEMES.len(), 41, "all themes shipped with btop v1.4.7");
        for (name, text) in bundled::THEMES {
            let values = parse_file(text);
            // every btop theme defines the core colors
            for key in ["main_fg", "title", "hi_fg", "cpu_box", "cpu_start"] {
                let v = values.get(key).unwrap_or_else(|| panic!("{name}: missing {key}"));
                assert!(parse_color(v).is_some(), "{name}: bad {key} {v:?}");
            }
            let entry = Entry { name: name.to_string(), source: Source::Bundled(text) };
            assert_eq!(entry.load(true).name, *name);
        }
    }

    #[test]
    fn every_available_theme_loads() {
        let entries = available();
        assert!(entries.len() >= 43, "Default, TTY and the 41 btop themes");
        for e in &entries {
            let t = e.load(true);
            assert_eq!(t.name, e.name);
        }
        assert!(find(&entries, "nord").is_some());
        assert!(find(&entries, "nord.theme").is_some());
        assert!(find(&entries, "Default").is_some());
    }
}
