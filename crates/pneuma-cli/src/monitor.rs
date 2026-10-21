//! Interactive serial monitor: raw terminal, auto-reconnect, optional timestamps.
//! Quit with Ctrl+C or Ctrl+].

use pneuma_core::{
    filters::Pipeline,
    monitor::{log_path, LogWriter, Stamper},
    ports, Error, Result,
};
use ratatui::crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode},
};
use std::{
    fs::File,
    io::{BufWriter, Read, Write},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

pub struct Opts {
    pub port: Option<String>,
    pub baud: u32,
    pub timestamps: bool,
    /// Where Ctrl+G / autosave write.
    pub log_dir: PathBuf,
    /// File-name prefix (env name or port).
    pub label: String,
    pub log_timestamps: bool,
    /// Stream everything to a file from the start.
    pub autosave: bool,
    pub pipeline: Pipeline,
    /// One-time notices shown at start (e.g. why a filter is disabled).
    pub notes: Vec<String>,
}

/// Cap on the in-memory history used by Ctrl+G (oldest bytes are dropped past this).
const MAX_HISTORY: usize = 64 * 1024 * 1024;

/// Everything shown on screen is also kept here (plain text) so Ctrl+G can save it.
struct Ctx {
    st: Stamper,
    log: LogWriter,
    history: Vec<u8>,
    truncated: bool,
    auto: Option<(PathBuf, BufWriter<File>)>,
    t0: Instant,
    pipe: Pipeline,
    last_data: Instant,
}

impl Ctx {
    fn record(&mut self, data: &[u8]) {
        let b = self.log.feed(data, self.t0.elapsed());
        if let Some((_, f)) = &mut self.auto {
            let _ = f.write_all(&b).and_then(|_| f.flush());
        }
        self.history.extend_from_slice(&b);
        if self.history.len() > MAX_HISTORY {
            let cut = self.history.len() - MAX_HISTORY / 2;
            self.history.drain(..cut);
            self.truncated = true;
        }
    }

    /// Filtered device output: to the screen and to the log.
    fn show(&mut self, out: &mut impl Write, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let _ = out.write_all(&self.st.feed(data, self.t0.elapsed()));
        let _ = out.flush();
        self.record(data);
    }

    /// Status line: shown on screen AND written to the log.
    fn say(&mut self, out: &mut impl Write, text: &str) {
        let nl = if self.st.at_line_start() { "" } else { "\r\n" };
        let _ = write!(out, "{nl}\x1b[36m-- {text} --\x1b[0m\r\n");
        let _ = out.flush();
        let lead: &[u8] = if self.log.at_line_start() { b"" } else { b"\n" };
        self.record(&[lead, format!("-- {text} --\n").as_bytes()].concat());
    }

    fn save(&mut self, dir: &std::path::Path, label: &str, out: &mut impl Write) {
        let path = log_path(dir, label);
        let res = std::fs::create_dir_all(dir).and_then(|_| std::fs::write(&path, &self.history));
        let msg = match res {
            Ok(_) => {
                let lines = self.history.iter().filter(|&&b| b == b'\n').count();
                format!("salvo: {} ({lines} linhas{})", path.display(), if self.truncated { ", início descartado: histórico passou de 64 MB" } else { "" })
            }
            Err(e) => format!("erro ao salvar {}: {e}", path.display()),
        };
        // Show only; don't write the save notice into the next saved file.
        let nl = if self.st.at_line_start() { "" } else { "\r\n" };
        let _ = write!(out, "{nl}\x1b[32m-- {msg} --\x1b[0m\r\n");
        let _ = out.flush();
    }
}

struct RawGuard;
impl RawGuard {
    fn new() -> Result<Self> {
        enable_raw_mode()?;
        Ok(RawGuard)
    }
}
impl Drop for RawGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

enum Msg {
    Data(Vec<u8>),
    Lost,
}

enum End {
    Quit,
    Lost,
    Paused,
}

/// Wait up to `d` for a quit key. True if the user asked to quit.
fn quit_within(d: Duration) -> bool {
    let end = Instant::now() + d;
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        if event::poll(left.min(Duration::from_millis(50))).unwrap_or(false) {
            if let Ok(Event::Key(k)) = event::read() {
                if is_quit(&k) {
                    return true;
                }
            }
        }
    }
    false
}

fn is_quit(k: &event::KeyEvent) -> bool {
    k.kind == KeyEventKind::Press
        && k.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(k.code, KeyCode::Char('c') | KeyCode::Char(']') | KeyCode::Char('5'))
}

fn is_save(k: &event::KeyEvent) -> bool {
    k.kind == KeyEventKind::Press && k.modifiers.contains(KeyModifiers::CONTROL) && matches!(k.code, KeyCode::Char('g'))
}

fn key_bytes(k: &event::KeyEvent) -> Option<Vec<u8>> {
    if k.kind != KeyEventKind::Press {
        return None;
    }
    Some(match k.code {
        KeyCode::Char(c) if k.modifiers.contains(KeyModifiers::CONTROL) && c.is_ascii_alphabetic() => vec![(c.to_ascii_lowercase() as u8) & 0x1f],
        KeyCode::Char(c) => c.to_string().into_bytes(),
        KeyCode::Enter => b"\n".to_vec(),
        KeyCode::Backspace => vec![0x08],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::Esc => vec![0x1b],
        _ => return None,
    })
}

fn session(mut port: Box<dyn serialport::SerialPort>, name: &str, cx: &mut Ctx, o: &Opts, out: &mut impl Write) -> End {
    let Ok(mut rd) = port.try_clone() else { return End::Lost };
    let (tx, rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let h = std::thread::spawn(move || {
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
    let end = 'outer: loop {
        while let Ok(m) = rx.try_recv() {
            match m {
                Msg::Data(d) => {
                    cx.last_data = Instant::now();
                    let f = cx.pipe.feed(&d);
                    cx.show(out, &f);
                }
                Msg::Lost => break 'outer End::Lost,
            }
        }
        if cx.pipe.has_pending() && cx.last_data.elapsed() > Duration::from_millis(80) {
            let f = cx.pipe.flush();
            cx.show(out, &f);
        }
        if ports::is_paused(name) {
            break End::Paused;
        }
        if event::poll(Duration::from_millis(10)).unwrap_or(false) {
            if let Ok(Event::Key(k)) = event::read() {
                if is_quit(&k) {
                    break End::Quit;
                }
                if is_save(&k) {
                    cx.save(&o.log_dir, &o.label, out);
                    continue;
                }
                if let Some(b) = key_bytes(&k) {
                    if port.write_all(&b).and_then(|_| port.flush()).is_err() {
                        break End::Lost;
                    }
                }
            }
        }
    };
    stop.store(true, Ordering::Relaxed);
    let _ = h.join();
    end
}

pub fn run(mut o: Opts) -> Result<()> {
    let _raw = RawGuard::new()?;
    let mut out = std::io::stdout();
    let mut cx = Ctx {
        st: Stamper::new(o.timestamps),
        log: LogWriter::new(o.log_timestamps),
        history: vec![],
        truncated: false,
        auto: None,
        t0: Instant::now(),
        pipe: std::mem::replace(&mut o.pipeline, Pipeline::passthrough()),
        last_data: Instant::now(),
    };
    for n in std::mem::take(&mut o.notes) {
        cx.say(&mut out, &n);
    }
    let mut announced_wait = false;
    let mut announced_pause = false;
    if o.autosave {
        let path = log_path(&o.log_dir, &o.label);
        match std::fs::create_dir_all(&o.log_dir).and_then(|_| File::create(&path)) {
            Ok(f) => {
                cx.auto = Some((path.clone(), BufWriter::new(f)));
                cx.say(&mut out, &format!("gravando em {}", path.display()));
            }
            Err(e) => cx.say(&mut out, &format!("autosave desativado: {}: {e}", path.display())),
        }
    }
    loop {
        let name = match &o.port {
            Some(p) => std::path::Path::new(p).exists().then(|| p.clone()),
            None => match ports::auto_port() {
                Ok(p) => Some(p),
                Err(Error::Build(m)) if m.contains("várias") => return Err(Error::Build(m)),
                Err(_) => None,
            },
        };
        if let Some(n) = name.as_ref().filter(|n| ports::is_paused(n)) {
            if !announced_pause {
                cx.say(&mut out, &format!("{n} liberada: upload em andamento..."));
                announced_pause = true;
            }
            if quit_within(Duration::from_millis(100)) {
                cx.say(&mut out, "monitor encerrado");
                return Ok(());
            }
            continue;
        }
        let opened = name.as_ref().map(|n| serialport::new(n, o.baud).timeout(Duration::from_millis(50)).open());
        match (name, opened) {
            (Some(n), Some(Ok(port))) => {
                announced_wait = false;
                if std::mem::take(&mut announced_pause) {
                    cx.say(&mut out, "upload terminou, retomando");
                }
                cx.say(&mut out, &format!("conectado: {n} @ {} baud · Ctrl+C sai · Ctrl+G salva log", o.baud));
                match session(port, &n, &mut cx, &o, &mut out) {
                    End::Quit => {
                        cx.say(&mut out, "monitor encerrado");
                        return Ok(());
                    }
                    End::Lost => cx.say(&mut out, "placa desconectada, aguardando..."),
                    End::Paused => {
                        announced_pause = true;
                        cx.say(&mut out, &format!("{n} liberada: upload em andamento..."));
                    }
                }
            }
            (name, opened) => {
                if !announced_wait {
                    let why = match (&name, opened) {
                        (Some(n), Some(Err(e))) => format!("{n}: {e}"),
                        _ => match &o.port {
                            Some(p) => format!("aguardando {p}"),
                            None => "aguardando placa USB".into(),
                        },
                    };
                    cx.say(&mut out, &why);
                    announced_wait = true;
                }
            }
        }
        if quit_within(Duration::from_millis(500)) {
            cx.say(&mut out, "monitor encerrado");
            return Ok(());
        }
    }
}
