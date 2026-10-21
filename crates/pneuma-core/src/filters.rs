//! Monitor output filters: `colorize` (log-level colors) and
//! `esp32_exception_decoder` (turns crash addresses into `func at file:line`).
//! Both work on whole lines; a partial last line is held until it completes or
//! `flush()` is called (the monitor does that when the device goes quiet).

use crate::{boards::{Board, Family}, config::Env, project::Project, Error, Result};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

pub const KNOWN: &[&str] = &["colorize", "esp32_exception_decoder"];

const RED: &str = "\x1b[31m";
const BOLD_RED: &str = "\x1b[1;31m";
const YELLOW: &str = "\x1b[33m";
const GREEN: &str = "\x1b[32m";
const CYAN: &str = "\x1b[36m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

const CRASH_MARKERS: &[&str] = &[
    "Guru Meditation", "abort()", "assert failed", "Backtrace:", "panic'ed", "Stack smashing", "Brownout", "***ERROR***", "LoadProhibited", "StoreProhibited",
];

/// Color for one line, or None to leave it alone (already-colored lines are never touched).
pub fn line_color(line: &str) -> Option<&'static str> {
    if line.contains('\x1b') {
        return None;
    }
    let t = line.trim_start();
    if CRASH_MARKERS.iter().any(|m| t.contains(m)) {
        return Some(BOLD_RED);
    }
    let b = t.as_bytes();
    // ESP-IDF "E (123) tag: msg"  /  Arduino-style "[E] msg" / "[E][file.cpp:1]"
    let lvl = if b.len() >= 3 && b[1] == b' ' && b[2] == b'(' {
        Some(b[0])
    } else if b.len() >= 3 && b[0] == b'[' && b[2] == b']' {
        Some(b[1])
    } else if b.len() >= 3 && b[1] == b'/' {
        Some(b[0]) // Android/log style "E/tag"
    } else {
        None
    };
    match lvl? {
        b'E' => Some(RED),
        b'W' => Some(YELLOW),
        b'I' => Some(GREEN),
        b'D' => Some(CYAN),
        b'V' => Some(DIM),
        _ => None,
    }
}

fn parse_hex(s: &str) -> Option<u64> {
    u64::from_str_radix(s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))?, 16).ok()
}

/// Code addresses (ESP32 family executes from 0x4000_0000..0x5000_0000) found in a crash line.
pub fn extract_addresses(line: &str) -> Vec<String> {
    let in_code = |v: u64| (0x4000_0000..0x5000_0000).contains(&v);
    let t = line.trim();
    let mut out = vec![];
    if let Some(rest) = t.strip_prefix("Backtrace:") {
        // "0xPC:0xSP 0xPC:0xSP ..." — only the PC half is code.
        for tok in rest.split_whitespace() {
            if let Some(v) = tok.split(':').next().and_then(parse_hex).filter(|v| in_code(*v)) {
                out.push(format!("0x{v:08x}"));
            }
        }
        return out;
    }
    // Register dump: "PC      : 0x400d4f2e  PS : 0x00060030 ..." / "MEPC    : 0x42001234  RA : 0x42001230"
    let toks: Vec<&str> = t.split_whitespace().collect();
    let mut i = 0;
    while i < toks.len() {
        let (label, rest_val) = match toks[i].strip_suffix(':') {
            Some(l) => (l, toks.get(i + 1).copied()),
            None if toks.get(i + 1) == Some(&":") => (toks[i], toks.get(i + 2).copied()),
            None => {
                i += 1;
                continue;
            }
        };
        if matches!(label, "PC" | "MEPC" | "RA") {
            if let Some(v) = rest_val.and_then(parse_hex).filter(|v| in_code(*v)) {
                out.push(format!("0x{v:08x}"));
            }
        }
        i += 1;
    }
    out
}

type Resolver = Box<dyn FnMut(&[String]) -> Vec<String> + Send>;

pub struct Decoder {
    resolve: Resolver,
}

impl Decoder {
    pub fn with_resolver(r: impl FnMut(&[String]) -> Vec<String> + Send + 'static) -> Self {
        Decoder { resolve: Box::new(r) }
    }

    /// Runs `<addr2line> -pfiaC -e <elf> <addrs>`.
    pub fn addr2line(tool: PathBuf, elf: PathBuf) -> Self {
        Self::with_resolver(move |addrs| {
            let Ok(o) = Command::new(&tool).args(["-pfiaC", "-e"]).arg(&elf).args(addrs).output() else { return vec![] };
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| !l.contains("?? ??:0") && !l.trim().is_empty())
                .map(|l| l.to_string())
                .collect()
        })
    }
}

/// addr2line for this board's CPU inside `<packages>` (the arduino-cli packages dir).
pub fn find_addr2line(board: &Board, packages: &Path) -> Option<PathBuf> {
    if board.family != Family::Esp32 {
        return None;
    }
    let prefix = match board.mcu {
        "esp32" => "xtensa-esp32-elf-",
        "esp32s2" => "xtensa-esp32s2-elf-",
        "esp32s3" => "xtensa-esp32s3-elf-",
        _ => "riscv32-esp-elf-",
    };
    let want = format!("{prefix}addr2line");
    fn go(dir: &Path, want: &str, depth: u8) -> Option<PathBuf> {
        for e in std::fs::read_dir(dir).ok()?.flatten() {
            let p = e.path();
            if p.is_dir() && depth > 0 {
                if let Some(f) = go(&p, want, depth - 1) {
                    return Some(f);
                }
            } else if p.file_name().is_some_and(|n| n == want || *n == *format!("{want}.exe")) {
                return Some(p);
            }
        }
        None
    }
    go(&packages.join("esp32/tools"), &want, 5)
}

/// The sketch ELF from the last build of an env (`.pneuma/build/<env>/*.elf`).
pub fn find_elf(build_dir: &Path) -> Option<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(build_dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "elf"))
        .collect();
    v.sort();
    v.into_iter().next()
}

/// Filters for a monitor session: `explicit` (from `-f`) or `pneuma.lua`, defaulting to both.
/// The ESP32 decoder needs the board, a built .elf and the toolchain's addr2line; when one is
/// missing the decoder is off and the returned notes say why.
pub fn pipeline_for(env: Option<&Env>, p: Option<&Project>, explicit: Option<&[String]>) -> Result<(Pipeline, Vec<String>)> {
    let names: Vec<String> = match explicit {
        Some(n) => n.to_vec(),
        None => p.map(|p| p.settings.monitor_filters.clone()).unwrap_or_else(|| vec!["colorize".into(), "esp32_exception_decoder".into()]),
    };
    let mut notes = vec![];
    let mut dec = None;
    if names.iter().any(|n| n == "esp32_exception_decoder") {
        match (env, p) {
            (Some(e), Some(p)) if e.board.family == Family::Esp32 => {
                let elf = find_elf(&p.root.join(".pneuma/build").join(&e.name));
                let tool = find_addr2line(e.board, &crate::build::pneuma_home().join("arduino/packages"));
                match (elf, tool) {
                    (Some(elf), Some(tool)) => dec = Some(Decoder::addr2line(tool, elf)),
                    (None, _) => notes.push("decodificador de crash desligado: rode `pneuma build` antes (precisa do .elf)".into()),
                    (_, None) => notes.push("decodificador de crash desligado: addr2line do ESP32 não encontrado (rode `pneuma build -e <esp32>` uma vez)".into()),
                }
            }
            _ if explicit.is_some() => notes.push("esp32_exception_decoder ignorado: só vale para um env ESP32".into()),
            _ => {}
        }
    }
    Ok((Pipeline::new(&names, dec)?, notes))
}

pub struct Pipeline {
    colorize: bool,
    decoder: Option<Decoder>,
    buf: Vec<u8>,
}

impl Pipeline {
    pub fn passthrough() -> Self {
        Pipeline { colorize: false, decoder: None, buf: vec![] }
    }

    /// Validates names; `decoder` is applied only if `esp32_exception_decoder` is listed.
    pub fn new(names: &[String], decoder: Option<Decoder>) -> Result<Self> {
        for n in names {
            if !KNOWN.contains(&n.as_str()) {
                return Err(Error::Build(format!("filtro desconhecido `{n}` (disponíveis: {})", KNOWN.join(", "))));
            }
        }
        Ok(Pipeline {
            colorize: names.iter().any(|n| n == "colorize"),
            decoder: names.iter().any(|n| n == "esp32_exception_decoder").then_some(decoder).flatten(),
            buf: vec![],
        })
    }

    pub fn is_passthrough(&self) -> bool {
        !self.colorize && self.decoder.is_none()
    }

    pub fn has_pending(&self) -> bool {
        !self.buf.is_empty()
    }

    pub fn feed(&mut self, data: &[u8]) -> Vec<u8> {
        if self.is_passthrough() {
            return data.to_vec();
        }
        self.buf.extend_from_slice(data);
        let mut out = vec![];
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let raw: Vec<u8> = self.buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&raw[..raw.len() - 1]);
            let line = line.trim_end_matches('\r');
            match line_color(line).filter(|_| self.colorize) {
                Some(c) => out.extend_from_slice(format!("{c}{line}{RESET}\n").as_bytes()),
                None => out.extend_from_slice(format!("{line}\n").as_bytes()),
            }
            if let Some(d) = &mut self.decoder {
                let addrs = extract_addresses(line);
                if !addrs.is_empty() {
                    for l in (d.resolve)(&addrs) {
                        out.extend_from_slice(format!("{DIM}{CYAN}  ↳ {l}{RESET}\n").as_bytes());
                    }
                }
            }
        }
        out
    }

    /// Emit the held partial line as-is (prompts without a newline).
    pub fn flush(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(b: Vec<u8>) -> String {
        String::from_utf8(b).unwrap()
    }
    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn colors_by_level_and_leaves_colored_lines() {
        assert_eq!(line_color("E (12) wifi: boom"), Some(RED));
        assert_eq!(line_color("  W (12) x"), Some(YELLOW));
        assert_eq!(line_color("I (1) ok"), Some(GREEN));
        assert_eq!(line_color("[E][main.cpp:10] bad"), Some(RED));
        assert_eq!(line_color("Guru Meditation Error: Core 1 panic'ed"), Some(BOLD_RED));
        assert_eq!(line_color("hello world"), None);
        assert_eq!(line_color("Even though x"), None, "no false positive on plain words");
        assert_eq!(line_color("\x1b[0;32mI (1) already\x1b[0m"), None);
    }

    #[test]
    fn extracts_addresses() {
        assert_eq!(
            extract_addresses("Backtrace: 0x400d1234:0x3ffb1f50 0x400d5678:0x3ffb1f70 0x3ffb0000:0x1"),
            ["0x400d1234", "0x400d5678"]
        );
        assert_eq!(extract_addresses("PC      : 0x400d4f2e  PS      : 0x00060030  A0      : 0x800d4f1c"), ["0x400d4f2e"]);
        assert_eq!(extract_addresses("MEPC    : 0x42001234  RA      : 0x42001230  SP      : 0x3fc9a2c0"), ["0x42001234", "0x42001230"]);
        assert_eq!(extract_addresses("PC: 0x400d4f2e"), ["0x400d4f2e"]);
        assert!(extract_addresses("hello 0x400d1234 not a dump").is_empty());
        assert!(extract_addresses("PC      : 0x3ffb0000").is_empty(), "data address ignored");
    }

    #[test]
    fn pipeline_colors_buffers_and_flushes() {
        let mut p = Pipeline::new(&names(&["colorize"]), None).unwrap();
        assert_eq!(s(p.feed(b"E (1) a")), "");
        assert!(p.has_pending());
        assert_eq!(s(p.feed(b"bc\r\nplain\n")), format!("{RED}E (1) abc{RESET}\nplain\n"));
        assert_eq!(s(p.feed(b"> ")), "");
        assert_eq!(s(p.flush()), "> ");
        assert!(!p.has_pending());
        let mut off = Pipeline::passthrough();
        assert_eq!(off.feed(b"E (1) x"), b"E (1) x", "no filters = zero buffering");
    }

    #[test]
    fn decoder_appends_resolved_lines_and_validates_names() {
        let d = Decoder::with_resolver(|a| a.iter().map(|x| format!("{x}: loop() at main.cpp:12")).collect());
        let mut p = Pipeline::new(&names(&["esp32_exception_decoder"]), Some(d)).unwrap();
        let o = s(p.feed(b"Backtrace: 0x400d1234:0x3ffb1f50\nnext\n"));
        assert!(o.starts_with("Backtrace: 0x400d1234:0x3ffb1f50\n"));
        assert!(o.contains("↳ 0x400d1234: loop() at main.cpp:12"));
        assert!(o.ends_with("next\n"));
        assert!(Pipeline::new(&names(&["nope"]), None).err().unwrap().to_string().contains("colorize"));
        // decoder listed but unavailable -> plain passthrough, not a crash
        assert!(Pipeline::new(&names(&["esp32_exception_decoder"]), None).unwrap().is_passthrough());
    }
}
