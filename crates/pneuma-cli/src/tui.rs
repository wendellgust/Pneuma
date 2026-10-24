//! Terminal UI (ratatui). Tabs: Ports / Boards / Project.

use crate::pane::Pane;
use pneuma_core::{
    boards,
    libs::{self, Scan},
    ports::{self, Port},
    project::Project,
    terminal,
};
use ratatui::{
    crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Cell, List, ListState, Paragraph, Row, Table, TableState, Tabs, Wrap},
    DefaultTerminal, Frame,
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

const ACCENT: Color = Color::Cyan;
const TABS: [&str; 5] = ["Portas", "Placas", "Projeto", "Bibliotecas", "Monitor"];
const MONITOR_TAB: usize = 4;

pub struct App {
    tab: usize,
    ports: Vec<Port>,
    ports_all: bool,
    ports_state: ListState,
    boards_state: TableState,
    path: PathBuf,
    project: Result<Project, String>,
    libs: Result<Scan, String>,
    envs_state: ListState,
    libs_state: ListState,
    quit: bool,
    help: bool,
    /// Port id whose remembered env was last applied to the env selection.
    applied: Option<String>,
    pane: Pane,
    /// `pneuma tui -p <port>`: monitor this device instead of the highlighted board.
    pub forced_port: Option<String>,
    status: String,
}

impl App {
    pub fn new(path: PathBuf) -> Self {
        let mut a = App {
            tab: 0,
            ports: vec![],
            ports_all: false,
            ports_state: ListState::default(),
            boards_state: TableState::default().with_selected(0),
            path,
            project: Err(String::new()),
            libs: Err(String::new()),
            envs_state: ListState::default(),
            libs_state: ListState::default(),
            quit: false,
            help: false,
            applied: None,
            pane: Pane::new(),
            forced_port: None,
            status: String::new(),
        };
        a.refresh_ports();
        a.reload_project();
        a.apply_remembered();
        a
    }

    fn refresh_ports(&mut self) {
        self.ports = ports::list(self.ports_all).unwrap_or_default();
        let sel = self.ports_state.selected().unwrap_or(0).min(self.ports.len().saturating_sub(1));
        self.ports_state.select((!self.ports.is_empty()).then_some(sel));
        self.apply_remembered();
    }

    /// Select the env that last worked with the highlighted board (once per board).
    fn apply_remembered(&mut self) {
        let Some(id) = self.ports_state.selected().and_then(|i| self.ports.get(i)).map(|p| p.id.clone()).filter(|i| !i.is_empty()) else { return };
        if self.applied.as_ref() == Some(&id) {
            return;
        }
        let Ok(p) = &self.project else { return };
        self.applied = Some(id.clone());
        if let Some(i) = pneuma_core::state::last_env(&p.root, &id).and_then(|n| p.envs.iter().position(|e| e.0 == n)) {
            self.envs_state.select(Some(i));
        }
    }

    fn reload_project(&mut self) {
        self.project = Project::load(&self.path).map_err(|e| e.to_string());
        self.libs = match &self.project {
            Ok(p) => libs::scan_project(&p).map_err(|e| e.to_string()),
            Err(e) => Err(e.clone()),
        };
        let n = self.project.as_ref().map_or(0, |p| p.envs.len());
        let def = self.project.as_ref().ok().and_then(|p| {
            let d = p.settings.default_env.as_ref()?;
            p.envs.iter().position(|e| &e.0 == d)
        });
        let sel = self.envs_state.selected().or(def).unwrap_or(0).min(n.saturating_sub(1));
        self.envs_state.select((n > 0).then_some(sel));
        let n = self.libs.as_ref().map_or(0, |s| s.libs.len() + s.external.len());
        let sel = self.libs_state.selected().unwrap_or(0).min(n.saturating_sub(1));
        self.libs_state.select((n > 0).then_some(sel));
    }

    fn move_sel(&mut self, delta: isize) {
        fn step(cur: Option<usize>, len: usize, d: isize) -> Option<usize> {
            (len > 0).then(|| (cur.unwrap_or(0) as isize + d).rem_euclid(len as isize) as usize)
        }
        match self.tab {
            0 => {
                self.ports_state.select(step(self.ports_state.selected(), self.ports.len(), delta));
                self.apply_remembered();
            }
            1 => self.boards_state.select(step(self.boards_state.selected(), boards::BOARDS.len(), delta)),
            2 => {
                let n = self.project.as_ref().map_or(0, |p| p.envs.len());
                self.envs_state.select(step(self.envs_state.selected(), n, delta));
            }
            3 => {
                let n = self.libs.as_ref().map_or(0, |s| s.libs.len() + s.external.len());
                self.libs_state.select(step(self.libs_state.selected(), n, delta));
            }
            _ => self.pane.scroll_by(-delta),
        }
    }

    /// Which board/baud the Monitor tab should listen to, and keep the port held only while visible.
    pub fn sync_pane(&mut self) {
        let on = self.tab == MONITOR_TAB;
        if on {
            let port = self.forced_port.clone().or_else(|| self.ports_state.selected().and_then(|i| self.ports.get(i)).map(|p| p.name.clone()));
            let baud = self
                .project
                .as_ref()
                .ok()
                .and_then(|p| {
                    let env = self.envs_state.selected().and_then(|i| p.envs.get(i)).and_then(|e| e.1.as_ref().ok()).map(|e| e.monitor_speed);
                    env.or(p.settings.monitor_speed)
                })
                .unwrap_or(115200);
            self.pane.target(port, baud);
        }
        self.pane.set_active(on);
        let proj = self.project.as_ref().ok();
        let idx = self.envs_state.selected();
        self.pane.tick(&mut || {
            let env = proj.and_then(|p| idx.and_then(|i| p.envs.get(i))).and_then(|e| e.1.as_ref().ok());
            pneuma_core::filters::pipeline_for(env, proj, None).unwrap_or_else(|e| (pneuma_core::filters::Pipeline::passthrough(), vec![e.to_string()]))
        });
    }

    /// Keys for the Monitor tab. True if consumed.
    fn pane_key(&mut self, code: KeyCode, mods: KeyModifiers) -> bool {
        if let Some(buf) = &mut self.pane.input {
            match code {
                KeyCode::Esc => self.pane.input = None,
                KeyCode::Char('c' | ']') if mods.contains(KeyModifiers::CONTROL) => self.pane.input = None,
                KeyCode::Enter => {
                    let line = std::mem::take(buf);
                    self.pane.send(&line);
                }
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) => buf.push(c),
                _ => {}
            }
            return true;
        }
        match code {
            KeyCode::Enter | KeyCode::Char('i') => self.pane.input = Some(String::new()),
            KeyCode::Char('p') => self.pane.plot = !self.pane.plot,
            KeyCode::Char('c') if !mods.contains(KeyModifiers::CONTROL) => self.pane.clear(),
            KeyCode::Char('s') => {
                let (dir, label) = match &self.project {
                    Ok(p) => (p.root.join(&p.settings.log_dir), self.selected_env().unwrap_or_else(|| "monitor".into())),
                    Err(_) => (self.path.join("logs"), "monitor".into()),
                };
                self.status = self.pane.save(&dir, &label);
            }
            KeyCode::PageUp => self.pane.scroll_by(10),
            KeyCode::PageDown => self.pane.scroll_by(-10),
            KeyCode::End | KeyCode::Char('G') => self.pane.scroll_bottom(),
            KeyCode::Char('m') => self.status = "você já está no monitor (aba 5)".into(),
            _ => return false,
        }
        true
    }

    /// Run `pneuma <args>` in a new terminal window, in the project directory.
    fn window(&mut self, args: &[&str]) {
        let dir = self.project.as_ref().map(|p| p.root.clone()).unwrap_or_else(|_| PathBuf::from("."));
        let mut argv = vec![std::env::current_exe().map(|e| e.display().to_string()).unwrap_or_else(|_| "pneuma".into())];
        argv.extend(args.iter().map(|s| s.to_string()));
        argv.push("--hold".into());
        self.status = match terminal::open(&dir, &argv) {
            Ok(t) => format!("pneuma {} aberto em nova janela ({t})", args[0]),
            Err(e) => format!("erro: {e}"),
        };
    }

    fn selected_env(&self) -> Option<String> {
        let p = self.project.as_ref().ok()?;
        p.envs.get(self.envs_state.selected()?).map(|e| e.0.clone())
    }

    pub fn on_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        if self.help {
            self.help = false;
            if !matches!(code, KeyCode::Char('q') | KeyCode::Esc | KeyCode::Char('h') | KeyCode::Char('?')) {
                self.on_key(code, mods);
            }
            return;
        }
        if self.tab == MONITOR_TAB && self.pane_key(code, mods) {
            return;
        }
        match code {
            KeyCode::Char('h') | KeyCode::Char('?') | KeyCode::F(1) => self.help = true,
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => self.quit = true,
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => self.tab = (self.tab + 1) % TABS.len(),
            KeyCode::BackTab | KeyCode::Left => {
                self.tab = (self.tab + TABS.len() - 1) % TABS.len()
            }
            KeyCode::Char(c @ '1'..='5') => self.tab = c as usize - '1' as usize,
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
            KeyCode::Char('r') => {
                self.refresh_ports();
                self.reload_project();
            }
            KeyCode::Enter | KeyCode::F(5) | KeyCode::Char('x' | 'm' | 'b' | 'u') => {
                let mut c = match code {
                    KeyCode::Char(c) => c,
                    _ => 'x',
                };
                // The Monitor tab already is the monitor: "run" there would race it for the port.
                if self.tab == MONITOR_TAB && matches!(c, 'x') {
                    c = 'u';
                }
                let env = self.selected_env();
                let port = self.ports_state.selected().and_then(|i| self.ports.get(i)).filter(|_| self.tab == 0).map(|p| p.name.clone());
                let mut a: Vec<&str> = vec![match c { 'm' => "monitor", 'b' => "build", 'u' => "upload", _ => "run" }];
                if let Some(e) = env.as_deref() {
                    a.extend(["-e", e]);
                }
                if let (Some(p), true) = (port.as_deref(), c != 'b') {
                    a.extend(["-p", p]);
                }
                self.window(&a);
            }
            KeyCode::Char('a') if self.tab == 0 => {
                self.ports_all = !self.ports_all;
                self.refresh_ports();
            }
            _ => {}
        }
    }

    pub fn draw(&mut self, f: &mut Frame) {
        let [top, body, foot] =
            Layout::vertical([Constraint::Length(3), Constraint::Min(5), Constraint::Length(1)]).areas(f.area());

        let tabs = Tabs::new(TABS.iter().enumerate().map(|(i, t)| format!(" {} {t} ", i + 1)))
            .select(self.tab)
            .highlight_style(Style::new().fg(Color::Black).bg(ACCENT).bold())
            .divider("")
            .block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().fg(ACCENT))
                    .title(Line::from(vec![" PNEUMA ".bold().fg(ACCENT), "ESP32 · ESP8266 · RP2040 · Arduino ".dim()])),
            );
        f.render_widget(tabs, top);

        match self.tab {
            0 => self.draw_ports(f, body),
            1 => self.draw_boards(f, body),
            2 => self.draw_project(f, body),
            3 => self.draw_libs(f, body),
            _ => self.pane.draw(f, body, ACCENT),
        }

        let env = self.selected_env().map(|e| format!(" · env: {e}")).unwrap_or_default();
        let help = if self.tab == MONITOR_TAB {
            format!("h ajuda · Enter digitar · p gráfico · u enviar código · b compilar · q sair{env}")
        } else {
            format!("h ajuda · Enter compilar+enviar+monitor · b · u · m · q sair{env}")
        };
        let line = if self.status.is_empty() { Line::from(help.dark_gray()) } else { Line::from(self.status.clone().yellow()) };
        f.render_widget(Paragraph::new(line), foot);
        if self.help {
            self.draw_help(f);
        }
    }

    fn draw_help(&self, f: &mut Frame) {
        let k = |key: &str, d: &str| Line::from(vec![format!(" {key:<16}").bold().fg(ACCENT), Span::raw(d.to_string())]);
        let mut l = vec![
            Line::from(" Atalhos ".bold()),
            Line::raw(""),
            k("h / ? / F1", "esta ajuda (qualquer tecla fecha)"),
            k("Tab / → / l", "próxima aba"),
            k("Shift+Tab / ←", "aba anterior"),
            k("1 2 3 4 5", "ir para aba: Portas, Placas, Projeto, Bibliotecas, Monitor"),
            k("↑ ↓ / k j", "mover seleção"),
            k("r", "recarregar portas e projeto"),
            k("Enter / x / F5", "compilar + enviar + monitor, tudo numa janela (aba 5: só enviar)"),
            k("b", "compilar (nova janela)"),
            k("u", "enviar para a placa (nova janela)"),
            k("m", "monitor serial (nova janela)"),
            k("q / Esc", "sair"),
            Line::raw(""),
            Line::from(" Nesta aba ".bold()),
        ];
        match self.tab {
            0 => {
                l.push(k("a", "mostrar todas as portas / só USB"));
                l.push(k("Enter u m", "usam a porta selecionada"));
            }
            1 => l.push(Line::from("   só lista as placas suportadas".dim())),
            4 => {
                l.push(k("Enter / i", "digitar uma linha e enviar (Esc volta)"));
                l.push(k("p", "gráfico dos números de cada linha"));
                l.push(k("c / s", "limpar / salvar log em logs/"));
                l.push(k("PgUp PgDn End", "rolar / ir ao fim"));
                l.push(Line::from("   porta só fica aberta nesta aba; upload libera sozinho".dim()));
            }
            2 => l.push(k("↑ ↓", "escolher o ambiente usado por b/u/m")),
            _ => l.push(Line::from("   `pneuma libs` regenera o LIBS.md".dim())),
        }
        l.push(Line::raw(""));
        l.push(Line::from(" Enter/b/u/m usam o ambiente escolhido na aba Projeto (rodapé mostra qual) ".dim()));
        let area = f.area();
        let w = 76.min(area.width);
        let h = (l.len() as u16 + 2).min(area.height);
        let r = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
        f.render_widget(ratatui::widgets::Clear, r);
        f.render_widget(Paragraph::new(l).block(Self::panel("Ajuda").fg(ACCENT)), r);
    }

    fn panel(title: impl std::fmt::Display) -> Block<'static> {
        Block::bordered().border_type(BorderType::Rounded).title(format!(" {title} ")).dark_gray()
    }

    fn draw_ports(&mut self, f: &mut Frame, area: Rect) {
        let title = if self.ports_all { "Portas seriais (todas)" } else { "Portas seriais (USB)" };
        if self.ports.is_empty() {
            let msg = Paragraph::new(vec![
                Line::raw(""),
                Line::from("Nenhum dispositivo detectado.".yellow()),
                Line::from("Conecte a placa por USB — a lista atualiza sozinha.".dim()),
                Line::from("No Linux, sem permissão? Entre no grupo uucp/dialout.".dim()),
            ])
            .centered()
            .block(Self::panel(title));
            f.render_widget(msg, area);
            return;
        }
        let items = self.ports.iter().map(|p| {
            Line::from(vec![
                Span::styled(format!("{:<22}", p.name), Style::new().bold()),
                Span::styled(p.desc.clone(), Style::new().fg(Color::Gray)),
            ])
        });
        let list = List::new(items)
            .block(Self::panel(title))
            .highlight_style(Style::new().bg(ACCENT).fg(Color::Black))
            .highlight_symbol("▶ ");
        f.render_stateful_widget(list, area, &mut self.ports_state);
    }

    fn draw_boards(&mut self, f: &mut Frame, area: Rect) {
        let rows = boards::BOARDS.iter().map(|b| {
            let fam = match b.family {
                boards::Family::Esp32 => Cell::from("ESP32").light_red(),
                boards::Family::Esp8266 => Cell::from("ESP8266").light_magenta(),
                boards::Family::Rp2040 => Cell::from("RP2040").light_green(),
                boards::Family::Avr => Cell::from("AVR").light_blue(),
            };
            Row::new([
                Cell::from(b.id),
                fam,
                Cell::from(b.mcu),
                Cell::from(format!("{} MHz", b.f_cpu_hz / 1_000_000)),
                Cell::from(b.name),
            ])
        });
        let table = Table::new(
            rows,
            [Constraint::Length(20), Constraint::Length(8), Constraint::Length(12), Constraint::Length(9), Constraint::Min(10)],
        )
        .header(Row::new(["ID", "Família", "MCU", "CPU", "Nome"]).style(Style::new().bold().fg(ACCENT)))
        .block(Self::panel(format!("Placas suportadas ({})", boards::BOARDS.len())))
        .row_highlight_style(Style::new().bg(ACCENT).fg(Color::Black))
        .highlight_symbol("▶ ");
        f.render_stateful_widget(table, area, &mut self.boards_state);
    }

    fn draw_project(&mut self, f: &mut Frame, area: Rect) {
        let title = format!("Projeto: {}", self.path.display());
        let envs = match &self.project {
            Err(e) => {
                let hint = if self.path.exists() { e.clone() } else { "pneuma.lua / platformio.ini não encontrado.".into() };
                let msg = Paragraph::new(vec![
                    Line::raw(""),
                    Line::from(hint.red()),
                    Line::from("Rode `pneuma init` na pasta do projeto para criar pneuma.lua.".dim()),
                ])
                .centered()
                .wrap(Wrap { trim: true })
                .block(Self::panel(&title));
                f.render_widget(msg, area);
                return;
            }
            Ok(p) => &p.envs,
        };
        let [left, right] = Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)]).areas(area);
        let items = envs.iter().map(|(n, r)| match r {
            Ok(_) => Line::from(vec!["✓ ".green(), Span::raw(n.clone())]),
            Err(_) => Line::from(vec!["✗ ".red(), Span::raw(n.clone())]),
        });
        let list = List::new(items)
            .block(Self::panel("Ambientes"))
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
            .highlight_symbol("▶ ");
        f.render_stateful_widget(list, left, &mut self.envs_state);

        let detail = match self.envs_state.selected().and_then(|i| envs.get(i)) {
            None => vec![Line::from("Nenhum [env:...] no arquivo.".dim())],
            Some((_, Err(e))) => vec![Line::from(e.clone().red())],
            Some((n, Ok(e))) => {
                let kv = |k: &str, v: String| Line::from(vec![format!("{k:<14}").dim(), Span::raw(v)]);
                let mut l = vec![
                    Line::from(n.clone().bold().fg(ACCENT)),
                    Line::raw(""),
                    kv("plataforma", e.platform.clone()),
                    kv("placa", format!("{} — {}", e.board.id, e.board.name)),
                    kv("mcu", e.board.mcu.into()),
                    kv("framework", e.framework.clone().unwrap_or("-".into())),
                    kv("upload", format!("{:?}", e.board.uploader)),
                    kv("porta", e.upload_port.clone().unwrap_or("auto".into())),
                    kv("monitor", format!("{} baud", e.monitor_speed)),
                ];
                if !e.build_flags.is_empty() {
                    l.push(kv("build_flags", e.build_flags.join(" ")));
                }
                if !e.lib_deps.is_empty() {
                    l.push(kv("lib_deps", e.lib_deps.join(", ")));
                }
                if !e.lib_deps.is_empty() {
                    l.retain(|x| !x.to_string().starts_with("lib_deps"));
                    l.push(kv("libs", e.lib_deps.join(", ")));
                }
                l.push(Line::raw(""));
                l.push(Line::from("Compilar / Enviar / Monitor: em breve".dim().italic()));
                l
            }
        };
        f.render_widget(Paragraph::new(detail).wrap(Wrap { trim: false }).block(Self::panel("Detalhes")), right);
    }

    fn draw_libs(&mut self, f: &mut Frame, area: Rect) {
        let scan = match &self.libs {
            Err(e) => {
                let msg = Paragraph::new(vec![Line::raw(""), Line::from(e.clone().red())])
                    .centered()
                    .wrap(Wrap { trim: true })
                    .block(Self::panel("Bibliotecas"));
                f.render_widget(msg, area);
                return;
            }
            Ok(s) => s,
        };
        let all: Vec<(&libs::Lib, bool)> = scan.libs.iter().map(|l| (l, false)).chain(scan.external.iter().map(|l| (l, true))).collect();
        if all.is_empty() {
            let msg = Paragraph::new(vec![
                Line::raw(""),
                Line::from("Nenhuma biblioteca.".yellow()),
                Line::from(format!("Coloque cada biblioteca como pasta em {}", scan.libs_dir.display()).dim()),
            ])
            .centered()
            .block(Self::panel("Bibliotecas"));
            f.render_widget(msg, area);
            return;
        }
        let [left, right] = Layout::horizontal([Constraint::Percentage(30), Constraint::Percentage(70)]).areas(area);
        let items = all.iter().map(|(l, ext)| if *ext { Line::from(vec![Span::raw(l.name.clone()), " (cmake)".dim()]) } else { Line::raw(l.name.clone()) });
        let list = List::new(items)
            .block(Self::panel(format!("Bibliotecas ({})", all.len())))
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
            .highlight_symbol("▶ ");
        f.render_stateful_widget(list, left, &mut self.libs_state);

        let kv = |k: &str, v: String| Line::from(vec![format!("{k:<10}").dim(), Span::raw(v)]);
        let mut d = match self.libs_state.selected().and_then(|i| all.get(i)).map(|(l, _)| *l) {
            None => vec![],
            Some(l) => {
                let mut d = vec![
                    Line::from(l.name.clone().bold().fg(ACCENT)),
                    Line::raw(""),
                    kv("caminho", l.path.display().to_string()),
                ];
                for i in &l.include_dirs {
                    d.push(kv("include", i.display().to_string()));
                }
                d.push(kv("fontes", l.sources.len().to_string()));
                d.push(kv("headers", l.headers.len().to_string()));
                d
            }
        };
        d.push(Line::raw(""));
        if scan.warnings.is_empty() {
            d.push(Line::from("sem avisos".green()));
        }
        for w in &scan.warnings {
            d.push(Line::from(format!("⚠ {w}").yellow()));
        }
        d.push(Line::raw(""));
        d.push(Line::from("`pneuma libs` regenera o LIBS.md".dim().italic()));
        f.render_widget(Paragraph::new(d).wrap(Wrap { trim: false }).block(Self::panel("Detalhes")), right);
    }
}

pub fn run(path: PathBuf, port: Option<String>) -> std::io::Result<()> {
    let mut term: DefaultTerminal = ratatui::init();
    let mut app = App::new(path);
    app.forced_port = port;
    let res = event_loop(&mut term, app);
    ratatui::restore();
    res
}

fn event_loop(term: &mut DefaultTerminal, mut app: App) -> std::io::Result<()> {
    let mut last = Instant::now();
    while !app.quit {
        app.sync_pane();
        term.draw(|f| app.draw(f))?;
        if event::poll(Duration::from_millis(if app.tab == MONITOR_TAB { 30 } else { 250 }))? {
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press {
                    app.on_key(k.code, k.modifiers);
                }
            }
        }
        if last.elapsed() >= Duration::from_secs(2) {
            app.refresh_ports();
            last = Instant::now();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn screen(app: &mut App) -> String {
        let mut t = Terminal::new(TestBackend::new(90, 20)).unwrap();
        t.draw(|f| app.draw(f)).unwrap();
        t.backend().to_string()
    }

    #[test]
    fn renders_all_tabs_and_navigates() {
        let d = std::env::temp_dir().join(format!("pneuma_tui_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("libs/mylib")).unwrap();
        std::fs::write(d.join("libs/mylib/my.h"), "").unwrap();
        std::fs::write(d.join("platformio.ini"), "[env:a]\nplatform=espressif32\nboard=esp32dev\n[env:b]\nplatform=p\nboard=nope\n").unwrap();
        let mut app = App::new(d);
        assert!(screen(&mut app).contains("Portas seriais"));
        app.on_key(KeyCode::Char('2'), KeyModifiers::NONE);
        let s = screen(&mut app);
        assert!(s.contains("Placas suportadas (11)") && s.contains("megaatmega2560"));
        app.on_key(KeyCode::Tab, KeyModifiers::NONE);
        let s = screen(&mut app);
        assert!(s.contains("✓ a") && s.contains("✗ b") && s.contains("esp32dev"));
        app.on_key(KeyCode::Down, KeyModifiers::NONE);
        assert!(screen(&mut app).contains("unknown board"));
        app.on_key(KeyCode::Char('4'), KeyModifiers::NONE);
        let s = screen(&mut app);
        assert!(s.contains("Bibliotecas (1)") && s.contains("mylib") && s.contains("caminho"));
        app.on_key(KeyCode::Char('5'), KeyModifiers::NONE);
        app.pane.push_bytes(b"temp=21.5\ntemp=22.5\n");
        assert!(screen(&mut app).contains("temp=22.5"));
        app.on_key(KeyCode::Char('p'), KeyModifiers::NONE);
        assert!(app.pane.plot);
        screen(&mut app);
        app.on_key(KeyCode::Char('i'), KeyModifiers::NONE);
        app.on_key(KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(!app.quit && screen(&mut app).contains("> q"));
        app.on_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.pane.input.is_none() && !app.quit);
        app.on_key(KeyCode::Char('4'), KeyModifiers::NONE);
        app.on_key(KeyCode::Char('h'), KeyModifiers::NONE);
        let s = screen(&mut app);
        assert!(s.contains("Atalhos") && s.contains("monitor serial") && s.contains("compilar + enviar + monitor"));
        app.on_key(KeyCode::Char('x'), KeyModifiers::NONE);
        assert!(!screen(&mut app).contains("Atalhos") && !app.quit);
        app.on_key(KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(app.quit);
    }

    #[test]
    fn missing_project_shows_hint() {
        let mut app = App::new("/nonexistent/dir".into());
        app.on_key(KeyCode::Char('3'), KeyModifiers::NONE);
        assert!(screen(&mut app).contains("não encontrado"));
        app.on_key(KeyCode::Char('4'), KeyModifiers::NONE);
        screen(&mut app);
    }
}
