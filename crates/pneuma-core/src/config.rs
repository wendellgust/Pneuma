//! Parser for PlatformIO-style `platformio.ini` (subset: sections, key/value,
//! indented multi-line values, `;`/`#` comments, `extends`).

use crate::{boards, Error, Result};
use std::collections::BTreeMap;

#[derive(Debug, Default, Clone)]
pub struct Section {
    pub name: String,
    pub values: BTreeMap<String, String>,
}

#[derive(Debug, Default)]
pub struct Config {
    pub sections: Vec<Section>,
}

/// Fully resolved environment (after `extends` and `[env]` defaults).
#[derive(Debug)]
pub struct Env {
    pub name: String,
    pub platform: String,
    pub board: &'static boards::Board,
    pub framework: Option<String>,
    pub upload_port: Option<String>,
    pub monitor_speed: u32,
    pub build_flags: Vec<String>,
    pub lib_deps: Vec<String>,
}

impl Config {
    pub fn parse(src: &str) -> Result<Config> {
        let mut cfg = Config::default();
        let mut last_key: Option<String> = None;
        for (i, raw) in src.lines().enumerate() {
            let ln = i + 1;
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed.starts_with(';') || trimmed.starts_with('#') {
                continue;
            }
            let indented = raw.starts_with(' ') || raw.starts_with('\t');
            if indented {
                let (Some(sec), Some(key)) = (cfg.sections.last_mut(), last_key.as_ref()) else {
                    return Err(Error::Config { line: ln, msg: "continuation without key".into() });
                };
                let v = sec.values.entry(key.clone()).or_default();
                v.push('\n');
                v.push_str(trimmed);
            } else if let Some(rest) = trimmed.strip_prefix('[') {
                let name = rest
                    .strip_suffix(']')
                    .ok_or(Error::Config { line: ln, msg: "unterminated section header".into() })?;
                cfg.sections.push(Section { name: name.trim().to_string(), ..Default::default() });
                last_key = None;
            } else {
                let sec = cfg
                    .sections
                    .last_mut()
                    .ok_or(Error::Config { line: ln, msg: "key outside section".into() })?;
                let (k, v) = trimmed
                    .split_once(['=', ':'])
                    .ok_or(Error::Config { line: ln, msg: "expected `key = value`".into() })?;
                let k = k.trim().to_string();
                sec.values.insert(k.clone(), v.trim().to_string());
                last_key = Some(k);
            }
        }
        Ok(cfg)
    }

    pub fn load(path: &std::path::Path) -> Result<Config> {
        Self::parse(&std::fs::read_to_string(path)?)
    }

    pub fn env_names(&self) -> Vec<&str> {
        self.sections.iter().filter_map(|s| s.name.strip_prefix("env:")).collect()
    }

    fn section(&self, name: &str) -> Option<&Section> {
        self.sections.iter().find(|s| s.name == name)
    }

    /// Merge chain: `[env]` < extends targets < `[env:name]`.
    fn merged(&self, name: &str, depth: u8) -> Result<BTreeMap<String, String>> {
        let sec = self
            .section(&format!("env:{name}"))
            .ok_or_else(|| Error::EnvNotFound(name.into()))?;
        let mut out = BTreeMap::new();
        if let Some(common) = self.section("env") {
            out.extend(common.values.clone());
        }
        if let Some(ext) = sec.values.get("extends").filter(|_| depth < 8) {
            for parent in ext.split([',', '\n']).map(str::trim).filter(|s| !s.is_empty()) {
                if let Some(p) = self.section(parent) {
                    out.extend(p.values.clone());
                } else if let Some(p) = parent.strip_prefix("env:") {
                    out.extend(self.merged(p, depth + 1)?);
                }
            }
        }
        out.extend(sec.values.clone());
        Ok(out)
    }

    pub fn resolve(&self, name: &str) -> Result<Env> {
        let m = self.merged(name, 0)?;
        let need = |key: &'static str| {
            m.get(key).cloned().ok_or(Error::MissingKey { env: name.into(), key })
        };
        let board_id = need("board")?;
        let board = boards::find(&board_id)
            .ok_or(Error::UnknownBoard { env: name.into(), board: board_id })?;
        let list = |k: &str| -> Vec<String> {
            m.get(k)
                .map(|v| v.split(['\n']).map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect())
                .unwrap_or_default()
        };
        Ok(Env {
            name: name.into(),
            platform: need("platform")?,
            board,
            framework: m.get("framework").cloned(),
            upload_port: m.get("upload_port").cloned(),
            monitor_speed: m.get("monitor_speed").and_then(|s| s.parse().ok()).unwrap_or(9600),
            build_flags: list("build_flags"),
            lib_deps: list("lib_deps"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INI: &str = "\
; comment
[env]
framework = arduino
monitor_speed = 115200

[env:esp]
platform = espressif32
board = esp32dev
build_flags =
    -DDEBUG
    -O2

[env:esp2]
extends = env:esp
board = esp32-s3-devkitc-1
";

    #[test]
    fn parses_and_extends() {
        let c = Config::parse(INI).unwrap();
        assert_eq!(c.env_names(), ["esp", "esp2"]);
        let e = c.resolve("esp2").unwrap();
        assert_eq!(e.board.mcu, "esp32s3");
        assert_eq!(e.platform, "espressif32");
        assert_eq!(e.monitor_speed, 115200);
        assert_eq!(e.build_flags, ["-DDEBUG", "-O2"]);
    }

    #[test]
    fn missing_board_errors() {
        let c = Config::parse("[env:x]\nplatform = p\n").unwrap();
        assert!(matches!(c.resolve("x"), Err(Error::MissingKey { key: "board", .. })));
    }
}
