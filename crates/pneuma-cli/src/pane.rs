//! Serial monitor + plotter embedded in the TUI (tab "Monitor").
//!
//! The port is only held while the tab is active, and is released whenever `pneuma upload` asks for it
//! (see `ports::is_paused`), so build/upload from any window keeps working.

use pneuma_core::{filters::Pipeline, monitor::log_path, ports};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    symbols,
    text::{Line, Span},
    widgets::{Axis, Block, BorderType, Chart, Dataset, GraphType, Paragraph},
    Frame,
};
use std::{
    collections::VecDeque,
    io::{Read, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

const MAX_LINES: usize = 5000;
const MAX_POINTS: usize = 300;
const MAX_SERIES: usize = 6;
const SERIES_COLORS: [Color; MAX_SERIES] = [Color::Cyan, Color::Yellow, Color::Green, Color::Magenta, Color::LightRed, Color::LightBlue];

enum Msg {
    Data(Vec<u8>),
    Lost,
}

struct Conn {
    name: String,
    writer: Box<dyn serialport::SerialPort>,
    rx: mpsc::Receiver<Msg>,
    stop: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.reader.take() {
            let _ = h.join();
        }
    }
}

/// One finished line: styled pieces (from the filters' ANSI colors) + its plain text.
struct Row {
    plain: String,
    spans: Vec<(Style, String)>,
}

pub struct Pane {
    /// Wanted device (None = the single USB board) and baud.
    want: Option<String>,
    baud: u32,
    active: bool,
    conn: Option<Conn>,
    last_try: Option<Instant>,
    pub status: String,
    lines: VecDeque<Row>,
    /// Unfinished line: (style, raw bytes) pieces; bytes stay raw so UTF-8 split across reads is safe.
    partial: Vec<(Style, Vec<u8>)>,
    cur: Style,
    csi: Vec<u8>,
    esc: Esc,
    pipeline: Pipeline,
    last_data: Instant,
    series: Vec<VecDeque<f64>>,
    pub plot: bool,
    /// `Some` while typing a line to send.
    pub input: Option<String>,
    /// Lines scrolled up from the bottom.
    scroll: usize,
}

#[derive(Default, Clone, Copy, PartialEq)]
enum Esc {
    #[default]
    None,
    Saw,
    Csi,
}

/// All numbers in a line, in order (`temp=23.5 hum:40` -> [23.5, 40]).
pub fn numbers(line: &str) -> Vec<f64> {
    let mut out = vec![];
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut Vec<f64>| {
        if cur.bytes().any(|b| b.is_ascii_digit()) {
            if let Ok(v) = cur.parse::<f64>() {
                out.push(v);
            }
        }
        cur.clear();
    };
    for c in line.chars() {
        let sign = (c == '-' || c == '+') && cur.is_empty();
        if c.is_ascii_digit() || c == '.' || sign {
            cur.push(c);
        } else {
            flush(&mut cur, &mut out);
        }
    }
    flush(&mut cur, &mut out);
    out
}

fn color256(n: u16) -> Color {
    Color::Indexed(n.min(255) as u8)
}

/// Apply an SGR parameter string (`1;31`, `38;5;208`, `0`...) to a style.
fn apply_sgr(mut st: Style, params: &str) -> Style {
    let nums: Vec<u16> = if params.is_empty() { vec![0] } else { params.split(';').map(|p| p.parse().unwrap_or(0)).collect() };
    let mut i = 0;
    while i < nums.len() {
        let n = nums[i];
        match n {
            0 => st = Style::new(),
            1 => st = st.add_modifier(Modifier::BOLD),
            2 => st = st.add_modifier(Modifier::DIM),
            3 => st = st.add_modifier(Modifier::ITALIC),
            4 => st = st.add_modifier(Modifier::UNDERLINED),
            22 => st = st.remove_modifier(Modifier::BOLD | Modifier::DIM),
            30..=37 => st = st.fg(color256(n - 30)),
            90..=97 => st = st.fg(color256(n - 90 + 8)),
            39 => st.fg = None,
            40..=47 => st = st.bg(color256(n - 40)),
            100..=107 => st = st.bg(color256(n - 100 + 8)),
            49 => st.bg = None,
            38 | 48 => {
                let fg = n == 38;
                match nums.get(i + 1) {
                    Some(5) if i + 2 < nums.len() => {
                        let c = color256(nums[i + 2]);
                        st = if fg { st.fg(c) } else { st.bg(c) };
                        i += 2;
                    }
                    Some(2) if i + 4 < nums.len() => {
                        let c = Color::Rgb(nums[i + 2] as u8, nums[i + 3] as u8, nums[i + 4] as u8);
                        st = if fg { st.fg(c) } else { st.bg(c) };
                        i += 4;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        i += 1;
    }
    st
}

impl Pane {
    pub fn new() -> Self {
        Pane {
            baud: 115200,
            want: None,
            active: false,
            conn: None,
            last_try: None,
            status: String::new(),
            lines: VecDeque::new(),
            partial: vec![],
            cur: Style::new(),
            csi: vec![],
            esc: Esc::None,
            pipeline: Pipeline::passthrough(),
            last_data: Instant::now(),
            series: vec![],
            plot: false,
            input: None,
            scroll: 0,
        }
    }

    /// Called when the wanted board/baud may have changed.
    pub fn target(&mut self, port: Option<String>, baud: u32) {
        if self.want != port || self.baud != baud {
            self.conn = None;
            self.last_try = None;
        }
        self.want = port;
        self.baud = baud;
    }

    /// The tab became (in)active: hold the port only while visible.
    pub fn set_active(&mut self, on: bool) {
        self.active = on;
        if !on {
            if self.conn.take().is_some() {
                self.status = "porta liberada (saiu da aba)".into();
            }
            self.input = None;
        }
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.partial.clear();
        self.cur = Style::new();
        self.series.clear();
        self.scroll = 0;
    }

    /// Feed device bytes through the filters (colors, crash decoder), then into lines.
    pub fn push_device(&mut self, data: &[u8]) {
        self.last_data = Instant::now();
        let out = self.pipeline.feed(data);
        self.push_bytes(&out);
    }

    /// Feed (possibly ANSI-colored) text: SGR colors become styles, other escapes are dropped.
    pub fn push_bytes(&mut self, data: &[u8]) {
        for &b in data {
            match self.esc {
                Esc::Saw => {
                    self.esc = if b == b'[' { Esc::Csi } else { Esc::None };
                    self.csi.clear();
                    continue;
                }
                Esc::Csi => {
                    if (0x40..=0x7e).contains(&b) {
                        self.esc = Esc::None;
                        if b == b'm' {
                            let params = String::from_utf8_lossy(&self.csi).into_owned();
                            self.cur = apply_sgr(self.cur, &params);
                        }
                    } else {
                        self.csi.push(b);
                    }
                    continue;
                }
                Esc::None => {}
            }
            match b {
                0x1b => self.esc = Esc::Saw,
                b'\r' => {}
                b'\n' => self.finish_line(),
                _ => match self.partial.last_mut() {
                    Some((st, bytes)) if *st == self.cur => bytes.push(b),
                    _ => self.partial.push((self.cur, vec![b])),
                },
            }
        }
    }

    fn finish_line(&mut self) {
        let spans: Vec<(Style, String)> = std::mem::take(&mut self.partial).into_iter().map(|(s, b)| (s, String::from_utf8_lossy(&b).into_owned())).collect();
        let plain: String = spans.iter().map(|s| s.1.as_str()).collect();
        let nums = numbers(&plain);
        if !nums.is_empty() && nums.len() <= MAX_SERIES {
            while self.series.len() < nums.len() {
                self.series.push(VecDeque::new());
            }
            for (i, v) in nums.into_iter().enumerate() {
                let q = &mut self.series[i];
                q.push_back(v);
                if q.len() > MAX_POINTS {
                    q.pop_front();
                }
            }
        }
        self.lines.push_back(Row { plain, spans });
        if self.lines.len() > MAX_LINES {
            self.lines.pop_front();
        }
        if self.scroll > 0 {
            self.scroll = (self.scroll + 1).min(self.lines.len());
        }
    }

    fn note(&mut self, text: &str) {
        self.status = text.to_string();
        let t = format!("-- {text} --");
        self.lines.push_back(Row { plain: t.clone(), spans: vec![(Style::new().fg(Color::Cyan), t)] });
    }

    /// A fresh filter chain per connection: the .elf only exists after a build.
    pub fn set_pipeline(&mut self, p: Pipeline, notes: Vec<String>) {
        self.pipeline = p;
        for n in notes {
            self.note(&n);
        }
    }

    /// Drain device data, honour upload pauses, (re)connect. Call often while active.
    pub fn tick(&mut self, mk_pipeline: &mut dyn FnMut() -> (Pipeline, Vec<String>)) {
        let mut lost = false;
        let mut got = vec![];
        if let Some(c) = &self.conn {
            while let Ok(m) = c.rx.try_recv() {
                match m {
                    Msg::Data(d) => got.push(d),
                    Msg::Lost => lost = true,
                }
            }
        }
        for d in got {
            self.push_device(&d);
        }
        // Prompts without a newline: show what the filters are holding once the device goes quiet.
        if self.pipeline.has_pending() && self.last_data.elapsed() > Duration::from_millis(80) {
            let f = self.pipeline.flush();
            self.push_bytes(&f);
        }
        if let Some(name) = self.conn.as_ref().map(|c| c.name.clone()) {
            if ports::is_paused(&name) {
                self.conn = None;
                self.note(&format!("{name} liberada: upload em andamento..."));
            } else if lost {
                self.conn = None;
                self.note("placa desconectada, aguardando...");
            }
            return;
        }
        if !self.active || self.last_try.is_some_and(|t| t.elapsed() < Duration::from_millis(500)) {
            return;
        }
        self.last_try = Some(Instant::now());
        let name = match &self.want {
            Some(p) => std::path::Path::new(p).exists().then(|| p.clone()),
            None => ports::auto_port().ok(),
        };
        let Some(name) = name else {
            if self.status != "aguardando placa USB" {
                self.status = "aguardando placa USB".into();
            }
            return;
        };
        if ports::is_paused(&name) {
            return;
        }
        match self.open(&name) {
            Ok(c) => {
                self.conn = Some(c);
                self.note(&format!("conectado: {name} @ {} baud", self.baud));
                let (pipe, notes) = mk_pipeline();
                self.set_pipeline(pipe, notes);
            }
            Err(e) => {
                let m = format!("{name}: {e}");
                if self.status != m {
                    self.status = m;
                }
            }
        }
    }

    fn open(&self, name: &str) -> Result<Conn, String> {
        let port = serialport::new(name, self.baud).timeout(Duration::from_millis(50)).open().map_err(|e| e.to_string())?;
        let mut rd = port.try_clone().map_err(|e| e.to_string())?;
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let reader = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while !stop2.load(Ordering::Relaxed) {
                match rd.read(&mut buf) {
                    Ok(0) => {}
                    Ok(n) => {
                        if tx.send(Msg::Data(buf[..n].to_vec())).is_err() {
                            return;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                    Err(_) => {
                        let _ = tx.send(Msg::Lost);
                        return;
                    }
                }
            }
        });
        Ok(Conn { name: name.to_string(), writer: port, rx, stop, reader: Some(reader) })
    }

    pub fn send(&mut self, text: &str) {
        let Some(c) = &mut self.conn else {
            self.status = "sem conexão: nada enviado".into();
            return;
        };
        let r = c.writer.write_all(format!("{text}\n").as_bytes()).and_then(|_| c.writer.flush());
        self.status = match r {
            Ok(_) => format!("enviado: {text}"),
            Err(e) => format!("erro ao enviar: {e}"),
        };
    }

    /// Plain-text log of everything shown; never overwrites.
    pub fn save(&self, dir: &std::path::Path, label: &str) -> String {
        let path = log_path(dir, label);
        let body: String = self.lines.iter().map(|l| format!("{}\n", l.plain)).collect();
        match std::fs::create_dir_all(dir).and_then(|_| std::fs::write(&path, body)) {
            Ok(_) => format!("salvo: {} ({} linhas)", path.display(), self.lines.len()),
            Err(e) => format!("erro ao salvar {}: {e}", path.display()),
        }
    }

    pub fn scroll_by(&mut self, delta: isize) {
        let max = self.lines.len();
        self.scroll = (self.scroll as isize + delta).clamp(0, max as isize) as usize;
    }

    pub fn scroll_bottom(&mut self) {
        self.scroll = 0;
    }

    pub fn draw(&self, f: &mut Frame, area: Rect, accent: Color) {
        let conn = match &self.conn {
            Some(c) => format!("{} @ {}", c.name, self.baud),
            None => self.status.clone(),
        };
        let title = format!(" Monitor · {conn} ");
        let block = Block::bordered().border_type(BorderType::Rounded).title(title).border_style(Style::new().fg(accent));
        let inner = block.inner(area);
        f.render_widget(block, area);
        let hint = match &self.input {
            Some(_) => "Enter envia · Esc volta".to_string(),
            None => "Enter/i digitar · p gráfico · c limpar · s salvar · PgUp/PgDn rolar".to_string(),
        };
        let [body, foot] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
        let (text_area, plot_area) = if self.plot {
            let [t, p] = Layout::vertical([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(body);
            (t, Some(p))
        } else {
            (body, None)
        };
        self.draw_text(f, text_area);
        if let Some(p) = plot_area {
            self.draw_plot(f, p);
        }
        let line = match &self.input {
            Some(s) => Line::from(vec!["> ".bold().fg(accent), ratatui::text::Span::raw(format!("{s}█"))]),
            None => Line::from(hint.dark_gray()),
        };
        f.render_widget(Paragraph::new(line), foot);
    }

    fn draw_text(&self, f: &mut Frame, area: Rect) {
        let h = area.height as usize;
        let has_partial = !self.partial.is_empty();
        let total = self.lines.len() + usize::from(has_partial);
        let end = total.saturating_sub(self.scroll);
        let start = end.saturating_sub(h);
        let rows: Vec<Line> = (start..end)
            .map(|i| match self.lines.get(i) {
                Some(r) => Line::from(r.spans.iter().map(|(st, t)| Span::styled(t.clone(), *st)).collect::<Vec<_>>()),
                None => Line::from(self.partial.iter().map(|(st, b)| Span::styled(String::from_utf8_lossy(b).into_owned(), *st)).collect::<Vec<_>>()),
            })
            .collect();
        f.render_widget(Paragraph::new(rows), area);
    }

    fn draw_plot(&self, f: &mut Frame, area: Rect) {
        let n = self.series.iter().map(|s| s.len()).max().unwrap_or(0);
        if n < 2 {
            f.render_widget(Paragraph::new(Line::from("gráfico: aguardando linhas com números (ex.: `temp=23.5,hum=40`)".dim())), area);
            return;
        }
        let data: Vec<Vec<(f64, f64)>> = self.series.iter().map(|s| s.iter().enumerate().map(|(i, v)| (i as f64, *v)).collect()).collect();
        let (mut lo, mut hi) = (f64::MAX, f64::MIN);
        for v in data.iter().flatten().map(|p| p.1) {
            lo = lo.min(v);
            hi = hi.max(v);
        }
        if (hi - lo).abs() < f64::EPSILON {
            lo -= 1.0;
            hi += 1.0;
        }
        let names: Vec<String> = (1..=data.len()).map(|i| format!("#{i}")).collect();
        let sets: Vec<Dataset> = data
            .iter()
            .enumerate()
            .map(|(i, d)| {
                Dataset::default()
                    .name(names[i].as_str())
                    .marker(symbols::Marker::Braille)
                    .graph_type(GraphType::Line)
                    .style(Style::new().fg(SERIES_COLORS[i % MAX_SERIES]))
                    .data(d)
            })
            .collect();
        let chart = Chart::new(sets)
            .x_axis(Axis::default().bounds([0.0, (n - 1) as f64]))
            .y_axis(Axis::default().bounds([lo, hi]).labels([format!("{lo:.1}"), format!("{hi:.1}")]));
        f.render_widget(chart, area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn extracts_numbers() {
        assert_eq!(numbers("temp=23.5 hum:40"), [23.5, 40.0]);
        assert_eq!(numbers("a -3, b +4.5e"), [-3.0, 4.5]);
        assert!(numbers("sem numeros - aqui").is_empty());
    }

    #[test]
    fn lines_ansi_and_series() {
        let mut p = Pane::new();
        p.push_bytes(b"\x1b[32mhello 1\x1b[0m\r\nx=2,y=3\nresto");
        let plain: Vec<&str> = p.lines.iter().map(|r| r.plain.as_str()).collect();
        assert_eq!(plain, ["hello 1", "x=2,y=3"]);
        assert_eq!(p.lines[0].spans[0].0.fg, Some(Color::Indexed(2)));
        assert_eq!(p.partial.len(), 1);
        assert_eq!(String::from_utf8_lossy(&p.partial[0].1), "resto");
        assert_eq!(p.series.len(), 2);
        assert_eq!(p.series[0], [1.0, 2.0]);
        assert_eq!(p.series[1], [3.0]);
    }

    #[test]
    fn filters_color_and_decode_in_the_pane() {
        let dec = pneuma_core::filters::Decoder::with_resolver(|a| a.iter().map(|x| format!("func_for_{x} at main.cpp:7")).collect());
        let pipe = Pipeline::new(&["colorize".to_string(), "esp32_exception_decoder".to_string()], Some(dec)).unwrap();
        let mut p = Pane::new();
        p.set_pipeline(pipe, vec!["nota".into()]);
        p.push_device(b"E (12) wifi: boom\nBacktrace: 0x40001234:0x3ffb0000\nprompt> ");
        let red = p.lines.iter().find(|r| r.plain.contains("boom")).unwrap();
        assert_eq!(red.spans[0].0.fg, Some(Color::Indexed(1)));
        assert!(p.lines.iter().any(|r| r.plain.contains("func_for_0x40001234 at main.cpp:7")));
        // numbers come from plain text only: SGR codes (31, 0) must not become data points
        assert!(p.series.iter().flatten().all(|v| *v != 31.0));
        // idle flush shows the held prompt
        assert!(p.partial.is_empty());
        std::thread::sleep(Duration::from_millis(100));
        p.tick(&mut || (Pipeline::passthrough(), vec![]));
        assert_eq!(String::from_utf8_lossy(&p.partial[0].1), "prompt> ");
    }

    #[test]
    fn caps_history_and_draws_plot() {
        let mut p = Pane::new();
        for i in 0..(MAX_LINES + 50) {
            p.push_bytes(format!("v={}\n", i % 7).as_bytes());
        }
        assert_eq!(p.lines.len(), MAX_LINES);
        assert_eq!(p.series[0].len(), MAX_POINTS);
        p.plot = true;
        p.input = Some("ola".into());
        let mut t = Terminal::new(TestBackend::new(80, 24)).unwrap();
        t.draw(|f| p.draw(f, f.area(), Color::Cyan)).unwrap();
        let s = t.backend().to_string();
        assert!(s.contains("Monitor") && s.contains("v=") && s.contains("> ola") && s.contains("#1"));
    }

    #[test]
    fn save_never_overwrites() {
        let d = std::env::temp_dir().join(format!("pneuma_pane_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let mut p = Pane::new();
        p.push_bytes(b"a\nb\n");
        let m1 = p.save(&d, "uno");
        let m2 = p.save(&d, "uno");
        assert!(m1.starts_with("salvo") && m2.starts_with("salvo"));
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 2);
    }
}
