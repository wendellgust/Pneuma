//! Serial monitor helpers (pure, testable). The interactive loop lives in the CLI.

use std::time::Duration;

/// Normalizes line endings to CRLF (raw terminal) and optionally prefixes every
/// line with the time since the monitor started: `[ 12.345] `.
pub struct Stamper {
    timestamps: bool,
    at_start: bool,
}

impl Stamper {
    pub fn new(timestamps: bool) -> Self {
        Stamper { timestamps, at_start: true }
    }

    pub fn at_line_start(&self) -> bool {
        self.at_start
    }

    pub fn feed(&mut self, data: &[u8], elapsed: Duration) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len() + 16);
        for &b in data {
            match b {
                b'\r' => {}
                b'\n' => {
                    out.extend_from_slice(b"\r\n");
                    self.at_start = true;
                }
                _ => {
                    if self.at_start && self.timestamps {
                        out.extend_from_slice(format!("\x1b[2m[{:>7.3}]\x1b[0m ", elapsed.as_secs_f64()).as_bytes());
                    }
                    self.at_start = false;
                    out.push(b);
                }
            }
        }
        out
    }
}

/// Plain-text log writer: LF newlines, ANSI escape sequences stripped,
/// optional `[ss.mmm] ` prefix per line (no colors, so the file is grep-friendly).
pub struct LogWriter {
    timestamps: bool,
    at_start: bool,
    /// 0 = normal, 1 = after ESC, 2 = inside CSI (`ESC [ ... final`)
    esc: u8,
}

impl LogWriter {
    pub fn new(timestamps: bool) -> Self {
        LogWriter { timestamps, at_start: true, esc: 0 }
    }

    pub fn at_line_start(&self) -> bool {
        self.at_start
    }

    pub fn feed(&mut self, data: &[u8], elapsed: Duration) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len() + 16);
        for &b in data {
            match self.esc {
                1 => self.esc = if b == b'[' { 2 } else { 0 },
                2 => {
                    if (0x40..=0x7e).contains(&b) {
                        self.esc = 0;
                    }
                }
                _ => match b {
                    0x1b => self.esc = 1,
                    b'\r' => {}
                    b'\n' => {
                        out.push(b'\n');
                        self.at_start = true;
                    }
                    _ => {
                        if self.at_start && self.timestamps {
                            out.extend_from_slice(format!("[{:>7.3}] ", elapsed.as_secs_f64()).as_bytes());
                        }
                        self.at_start = false;
                        out.push(b);
                    }
                },
            }
        }
        out
    }
}

/// Local date-time for file names, e.g. `20261013-143005` (uses `date`; falls back to epoch seconds).
pub fn timestamp_name() -> String {
    std::process::Command::new("date")
        .arg("+%Y%m%d-%H%M%S")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0).to_string()
        })
}

/// `<dir>/<label>-<datetime>.log`, never overwriting an existing file.
pub fn log_path(dir: &std::path::Path, label: &str) -> std::path::PathBuf {
    let label: String = label.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    let base = format!("{}-{}", if label.is_empty() { "monitor" } else { &label }, timestamp_name());
    let mut p = dir.join(format!("{base}.log"));
    let mut n = 1;
    while p.exists() {
        p = dir.join(format!("{base}-{n}.log"));
        n += 1;
    }
    p
}

/// Log files in `dir`, oldest first.
pub fn list_logs(dir: &std::path::Path) -> Vec<(std::path::PathBuf, u64)> {
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "log"))
        .filter_map(|p| Some((std::fs::metadata(&p).ok()?.len(), p)))
        .map(|(l, p)| (p, l))
        .collect();
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crlf_and_timestamps_across_chunks() {
        let mut s = Stamper::new(false);
        assert_eq!(s.feed(b"a\r\nb\n", Duration::ZERO), b"a\r\nb\r\n");
        assert!(s.at_line_start());
        let mut s = Stamper::new(true);
        let t = Duration::from_millis(1500);
        let a = String::from_utf8(s.feed(b"hel", t)).unwrap();
        assert!(a.contains("[  1.500]") && a.ends_with("hel"));
        let b = String::from_utf8(s.feed(b"lo\nx", t)).unwrap();
        assert_eq!(b.matches("[  1.500]").count(), 1, "no stamp mid-line, one on the next line");
    }

    #[test]
    fn log_strips_ansi_across_chunks_and_uses_lf() {
        let mut l = LogWriter::new(false);
        let mut out = l.feed(b"\x1b[0;32mI (12) ok\x1b", Duration::ZERO);
        out.extend(l.feed(b"[0m\r\nnext\n", Duration::ZERO));
        assert_eq!(String::from_utf8(out).unwrap(), "I (12) ok\nnext\n");
        let mut l = LogWriter::new(true);
        let o = String::from_utf8(l.feed(b"a\nb", Duration::from_millis(2500))).unwrap();
        assert_eq!(o, "[  2.500] a\n[  2.500] b");
    }

    #[test]
    fn log_path_is_unique_and_sanitized() {
        let d = std::env::temp_dir().join(format!("pneuma_logs_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let a = log_path(&d, "/dev/ttyUSB0");
        assert!(a.file_name().unwrap().to_string_lossy().starts_with("_dev_ttyUSB0-"));
        std::fs::write(&a, "x").unwrap();
        let b = log_path(&d, "/dev/ttyUSB0");
        assert_ne!(a, b);
        std::fs::write(&b, "yy").unwrap();
        let l = list_logs(&d);
        assert_eq!(l.len(), 2);
        assert!(list_logs(&d.join("nope")).is_empty());
    }
}
