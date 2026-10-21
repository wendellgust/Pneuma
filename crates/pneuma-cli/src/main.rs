mod monitor;
mod pane;
mod tui;
use pneuma_core::{
    boards, build,
    config::Env,
    filters::{self, Pipeline},
    libs,
    project::{self, Project},
    ports, state, terminal, Error,
};
use std::{path::PathBuf, process::ExitCode};

const USAGE: &str = "uso: pneuma [comando] [dir] [opções]
  (nenhum) | tui [dir] [-p porta]  interface no terminal
  new <nome>                     cria um projeto novo com src/main.cpp de exemplo
  init [dir]                     cria pneuma.lua, libs/ e LIBS.md (projeto existente)
  setup [--arduino-cli]          instala a toolchain AVR nativa em ~/.pneuma (sem arduino-cli);
                                 --arduino-cli: instala o arduino-cli (ESP32, ESP8266, RP2040, lib add)
  build   [dir] [-e env]         compila
  upload  [dir] [-e env] [-p porta]
  monitor [dir] [-e env] [-p porta] [-b baud] [-t] [--log] [-f filtro]
                                 Ctrl+G salva tudo desde que abriu; --log grava direto
  logs [dir] [nome|latest]       lista os logs salvos / mostra um
  run     [dir] [-e env] [-p porta]   compila + envia + abre o monitor
  lib add <nome[@versão]>...     baixa do registro Arduino (com dependências) para libs/
  lib search <texto>             procura no registro Arduino
  debug   [dir] [-e env] [-p porta] [-P programador] [--check]
                                 gdb via arduino-cli (ESP32-S3/C3, RP2040...). AVR: não suportado
  libs [dir] [--flags]           escaneia libs/, escreve LIBS.md (--flags: imprime -I)
  check [dir|arquivo]            valida pneuma.lua ou platformio.ini
  boards | ports [--all] [--json]
  info [dir]                     JSON do projeto + portas (para editores/extensões)
Opções:  -w/--window  roda o comando em uma NOVA janela de terminal
         -t           monitor: prefixa linhas com o tempo
         -f colorize,esp32_exception_decoder | -f none   filtros do monitor";

#[derive(Default)]
struct Args {
    pos: Vec<String>,
    env: Option<String>,
    port: Option<String>,
    baud: Option<u32>,
    programmer: Option<String>,
    window: bool,
    hold: bool,
    timestamps: bool,
    log: bool,
    filters: Vec<String>,
    filters_set: bool,
    flags: bool,
    all: bool,
    check: bool,
    arduino_cli: bool,
    json: bool,
}

fn parse(rest: Vec<String>) -> pneuma_core::Result<Args> {
    let mut a = Args::default();
    let mut it = rest.into_iter();
    let bad = |m: String| Error::Build(m);
    while let Some(x) = it.next() {
        let mut val = |n: &str| it.next().ok_or_else(|| bad(format!("{n} precisa de um valor")));
        match x.as_str() {
            "-e" | "--env" => a.env = Some(val("-e")?),
            "-p" | "--port" => a.port = Some(val("-p")?),
            "-P" | "--programmer" => a.programmer = Some(val("-P")?),
            "-b" | "--baud" => a.baud = Some(val("-b")?.parse().map_err(|_| bad("baud inválido".into()))?),
            "-w" | "--window" => a.window = true,
            "--hold" => a.hold = true,
            "-t" | "--timestamps" => a.timestamps = true,
            "-f" | "--filter" => {
                a.filters_set = true;
                a.filters.extend(val("-f")?.split(',').map(str::trim).filter(|s| !s.is_empty() && *s != "none").map(String::from));
            }
            "--log" => a.log = true,
            "--flags" => a.flags = true,
            "--all" => a.all = true,
            "--check" => a.check = true,
            "--arduino-cli" => a.arduino_cli = true,
            "--json" => a.json = true,
            s if s.starts_with('-') => return Err(bad(format!("opção desconhecida: {s}"))),
            _ => a.pos.push(x),
        }
    }
    Ok(a)
}

impl Args {
    fn dir(&self) -> PathBuf {
        self.pos.first().map(PathBuf::from).unwrap_or_else(|| ".".into())
    }
}

/// `-e` wins; then the env that last worked with the connected board; then `default_env`.
fn pick_env<'a>(p: &'a Project, want: Option<&str>, port: Option<&str>) -> pneuma_core::Result<&'a Env> {
    let remembered = || {
        let port = port.map(String::from).or_else(|| ports::auto_port().ok())?;
        let name = state::last_env(&p.root, &ports::id_of(&port)?)?;
        p.envs.iter().any(|e| e.0 == name).then(|| {
            println!("env {name} (lembrado para esta placa)");
            name
        })
    };
    let name = want.map(String::from).or_else(remembered).or(p.settings.default_env.clone()).or_else(|| p.envs.first().map(|e| e.0.clone()));
    let name = name.ok_or_else(|| Error::Build("projeto sem [env]".into()))?;
    match p.envs.iter().find(|e| e.0 == name) {
        Some((_, Ok(e))) => Ok(e),
        Some((_, Err(m))) => Err(Error::Build(m.clone())),
        None => Err(Error::EnvNotFound(name)),
    }
}

fn scan(p: &Project) -> pneuma_core::Result<libs::Scan> {
    libs::scan(&p.root, &p.settings.libs_dir, &p.settings.extra_libs)
}

fn do_build(p: &Project, env: &Env) -> pneuma_core::Result<build::Plan> {
    let s = scan(p)?;
    for w in &s.warnings {
        println!("AVISO {w}");
    }
    let plan = build::prepare(p, env, &s)?;
    println!("env {} · {} · libs: {}", env.name, plan.fqbn, if plan.libs.is_empty() { "-".into() } else { plan.libs.join(", ") });
    build::compile(&plan, env.board)?;
    Ok(plan)
}

fn do_upload(plan: &build::Plan, env: &Env, port: Option<String>) -> pneuma_core::Result<String> {
    let port = match port.or(env.upload_port.clone()) {
        Some(x) => x,
        None => ports::auto_port()?,
    };
    // Ask running `pneuma monitor`s to let go of the port; they reopen it when `_pause` drops.
    let _pause = ports::pause(&port);
    let busy = ports::wait_released(&port, std::time::Duration::from_secs(3));
    if !busy.is_empty() {
        let who: Vec<String> = busy.iter().map(|(pid, c)| format!("pid {pid}: {c}")).collect();
        return Err(Error::Build(format!(
            "porta {port} ocupada ({}). Feche o monitor (Ctrl+C na janela dele) e tente de novo.",
            who.join("; ")
        )));
    }
    println!("enviando para {port}...");
    build::upload(plan, &port)?;
    Ok(port)
}

fn log_dir(p: Option<&Project>, a: &Args) -> PathBuf {
    match p {
        Some(p) => p.root.join(&p.settings.log_dir),
        None => a.dir().join("logs"),
    }
}

fn build_pipeline(env: Option<&Env>, p: Option<&Project>, a: &Args) -> pneuma_core::Result<(Pipeline, Vec<String>)> {
    filters::pipeline_for(env, p, a.filters_set.then_some(a.filters.as_slice()))
}

fn do_monitor(env: Option<&Env>, p: Option<&Project>, a: &Args) -> pneuma_core::Result<()> {
    let baud = a.baud.or(env.map(|e| e.monitor_speed)).or(p.and_then(|p| p.settings.monitor_speed)).unwrap_or(115200);
    let port = a.port.clone().or(env.and_then(|e| e.upload_port.clone()));
    let label = env
        .map(|e| e.name.clone())
        .or(port.as_deref().and_then(|p| std::path::Path::new(p).file_name()).map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "monitor".into());
    let (pipeline, notes) = build_pipeline(env, p, a)?;
    monitor::run(monitor::Opts {
        pipeline,
        notes,
        port,
        baud,
        timestamps: a.timestamps || p.is_some_and(|p| p.settings.monitor_timestamps),
        log_dir: log_dir(p, a),
        label,
        log_timestamps: p.is_none_or(|p| p.settings.log_timestamps),
        autosave: a.log || p.is_some_and(|p| p.settings.autosave),
    })
}

/// Re-run this same command in a new terminal window (without `-w`, with `--hold`).
fn relaunch(cmd: &str, raw: &[String]) -> pneuma_core::Result<()> {
    let a = parse(raw.to_vec())?;
    let dir = std::fs::canonicalize(a.dir()).map_err(|e| Error::Build(format!("{}: {e}", a.dir().display())))?;
    let exe = std::env::current_exe()?;
    let mut argv = vec![exe.display().to_string(), cmd.to_string()];
    argv.extend(raw.iter().filter(|x| !matches!(x.as_str(), "-w" | "--window")).cloned());
    // The directory is given by the wrapper's `cd`, so drop a positional dir.
    if let Some(d) = a.pos.first() {
        if let Some(i) = argv.iter().skip(2).position(|x| x == d) {
            argv.remove(i + 2);
        }
    }
    argv.push("--hold".into());
    let t = terminal::open(&dir, &argv)?;
    println!("aberto em nova janela ({t})");
    Ok(())
}

fn run() -> pneuma_core::Result<()> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let (cmd, rest) = match raw.split_first() {
        Some((c, r)) => (c.as_str(), r.to_vec()),
        None => ("tui", vec![]),
    };
    if matches!(cmd, "build" | "upload" | "monitor" | "run") && rest.iter().any(|x| x == "-w" || x == "--window") {
        return relaunch(cmd, &rest);
    }
    let a = parse(rest)?;
    match cmd {
        "info" => {
            // always JSON: this is what editor integrations read
            let ports: Vec<serde_json::Value> = ports::list(false)?
                .into_iter()
                .map(|p| serde_json::json!({ "name": p.name, "desc": p.desc, "id": p.id, "usb": p.usb }))
                .collect();
            let mut v = serde_json::json!({ "version": env!("CARGO_PKG_VERSION"), "ports": ports });
            match Project::load(&a.dir()) {
                Ok(p) => {
                    let envs: Vec<serde_json::Value> = p
                        .envs
                        .iter()
                        .map(|(n, r)| match r {
                            Ok(e) => serde_json::json!({
                                "name": n, "ok": true, "board": e.board.id, "fqbn": e.board.fqbn,
                                "family": format!("{:?}", e.board.family), "platform": e.platform,
                                "monitor_speed": e.monitor_speed, "backend": build::backend_name(&p, e),
                            }),
                            Err(m) => serde_json::json!({ "name": n, "ok": false, "error": m }),
                        })
                        .collect();
                    let remembered: serde_json::Map<String, serde_json::Value> = ports::list(false)?
                        .into_iter()
                        .filter_map(|po| state::last_env(&p.root, &po.id).map(|e| (po.name, serde_json::Value::String(e))))
                        .collect();
                    v["project"] = serde_json::json!({
                        "root": p.root, "source": p.source, "default_env": p.settings.default_env,
                        "src_dir": p.settings.src_dir, "libs_dir": p.settings.libs_dir,
                        "envs": envs, "remembered_env_by_port": remembered,
                    });
                }
                Err(e) => v["project_error"] = serde_json::Value::String(e.to_string()),
            }
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        }
        "boards" if a.json => {
            let v: Vec<serde_json::Value> = boards::BOARDS
                .iter()
                .map(|b| serde_json::json!({ "id": b.id, "name": b.name, "family": format!("{:?}", b.family), "mcu": b.mcu, "fqbn": b.fqbn }))
                .collect();
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        }
        "ports" if a.json => {
            let v: Vec<serde_json::Value> = ports::list(a.all)?
                .into_iter()
                .map(|p| serde_json::json!({ "name": p.name, "desc": p.desc, "id": p.id, "usb": p.usb }))
                .collect();
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        }
        "boards" => {
            for b in boards::BOARDS {
                println!("{:<22} {:<8} {:<11} {:<36} {}", b.id, format!("{:?}", b.family), b.mcu, b.fqbn, b.name);
            }
        }
        "ports" => {
            for p in ports::list(a.all)? {
                println!("{:<20} {}", p.name, p.desc);
            }
        }
        "check" => {
            let p = Project::load(&a.dir())?;
            println!("{}", p.source.display());
            let mut bad = 0;
            for (n, r) in &p.envs {
                match r {
                    Ok(e) => println!("ok  {:<16} {} / {} ({})", n, e.platform, e.board.id, e.framework.as_deref().unwrap_or("-")),
                    Err(m) => {
                        bad += 1;
                        println!("ERR {m}");
                    }
                }
            }
            if bad > 0 {
                std::process::exit(1);
            }
        }
        "init" => {
            let dir = a.dir();
            for l in project::scaffold(&dir, false)? {
                println!("{l}");
            }
            let p = Project::load(&dir)?;
            println!("criado {}", scan(&p)?.write_markdown()?.display());
            println!("Coloque o código em src/ (main.cpp) e cada biblioteca como pasta em libs/.");
        }
        "new" => {
            let dir = PathBuf::from(a.pos.first().ok_or_else(|| Error::Build("uso: pneuma new <nome>".into()))?);
            if dir.exists() && std::fs::read_dir(&dir).map_or(true, |mut d| d.next().is_some()) {
                return Err(Error::Build(format!("{} já existe e não está vazio", dir.display())));
            }
            for l in project::scaffold(&dir, true)? {
                println!("{l}");
            }
            let p = Project::load(&dir)?;
            scan(&p)?.write_markdown()?;
            println!("Pronto. Próximo: cd {} && pneuma   (ou: pneuma build / upload / monitor)", dir.display());
        }
        "lib" => {
            let (sub, args) = a.pos.split_first().ok_or_else(|| Error::Build("uso: pneuma lib add <nome[@versão]>... | pneuma lib search <texto>".into()))?;
            match sub.as_str() {
                "search" => build::lib_search(&args.join(" "))?,
                "add" if !args.is_empty() => {
                    let p = Project::load(&PathBuf::from("."))?;
                    let dir = p.root.join(&p.settings.libs_dir);
                    std::fs::create_dir_all(&dir)?;
                    for spec in args {
                        for (name, added) in build::lib_add(spec, &dir)? {
                            println!("{} {name}", if added { "adicionada:" } else { "já existe (mantida):" });
                        }
                    }
                    println!("-> {}", scan(&p)?.write_markdown()?.display());
                }
                _ => return Err(Error::Build("uso: pneuma lib add <nome[@versão]>... | pneuma lib search <texto>".into())),
            }
        }
        "debug" => {
            let p = Project::load(&a.dir())?;
            let env = pick_env(&p, a.env.as_deref(), a.port.as_deref())?;
            if env.board.family == boards::Family::Avr {
                return Err(Error::Build(format!(
                    "debug não é suportado em {} (AVR {}): sem JTAG/debugWIRE. Use Serial.print ou um simulador.",
                    env.board.id, env.board.mcu
                )));
            }
            build::ensure_core(env.board)?;
            if !build::debug_supported(env.board.fqbn, a.programmer.as_deref())? {
                return Err(Error::Build(format!(
                    "{} sem configuração de debug{}. Veja os programadores: arduino-cli board details -b {} --list-programmers  e rode `pneuma debug -P <programador>`.",
                    env.board.id,
                    a.programmer.as_deref().map(|p| format!(" com `-P {p}`")).unwrap_or_default(),
                    env.board.fqbn
                )));
            }
            if a.check {
                println!("ok: debug suportado para {}", env.board.id);
                return Ok(());
            }
            let s = scan(&p)?;
            let plan = build::debug_plan(&build::prepare(&p, env, &s)?);
            build::compile(&plan, env.board)?;
            build::debug(&plan, a.port.as_deref().or(env.upload_port.as_deref()), a.programmer.as_deref())?;
        }
        "libs" => {
            let p = Project::load(&a.dir())?;
            let s = scan(&p)?;
            if a.flags {
                println!("{}", s.include_flags(&[]).join(" "));
                return Ok(());
            }
            let md = s.write_markdown()?;
            for l in &s.libs {
                println!("{:<24} {}  ({} src, {} hdr)", l.name, l.path.display(), l.sources.len(), l.headers.len());
            }
            for w in &s.warnings {
                println!("AVISO {w}");
            }
            println!("-> {}", md.display());
        }
        "setup" => {
            if a.arduino_cli {
                let p = build::install_cli()?;
                println!("instalado: {}\n{}", p.display(), build::cli_version()?);
            } else {
                pneuma_core::toolchain::install_avr()?;
                println!("AVR (Uno, Nano, Mega) já funciona sem arduino-cli. Para ESP32/ESP8266/RP2040 e `lib add`: pneuma setup --arduino-cli");
            }
        }
        "build" => {
            let p = Project::load(&a.dir())?;
            do_build(&p, pick_env(&p, a.env.as_deref(), a.port.as_deref())?)?;
            println!("ok");
        }
        "upload" | "run" => {
            let p = Project::load(&a.dir())?;
            let env = pick_env(&p, a.env.as_deref(), a.port.as_deref())?;
            let plan = do_build(&p, env)?;
            let port = do_upload(&plan, env, a.port.clone())?;
            if let Some(id) = ports::id_of(&port) {
                state::set_last_env(&p.root, &id, &env.name);
            }
            if cmd == "run" {
                do_monitor(Some(env), Some(&p), &a)?;
            } else {
                println!("ok");
            }
        }
        "monitor" => {
            // Works without a project too: `pneuma monitor -p /dev/ttyUSB0 -b 115200`.
            match Project::load(&a.dir()) {
                Ok(p) => {
                    let env = pick_env(&p, a.env.as_deref(), a.port.as_deref()).ok();
                    do_monitor(env, Some(&p), &a)?
                }
                Err(_) => do_monitor(None, None, &a)?,
            }
        }
        "logs" => {
            // `pneuma logs [dir] [name|latest]`; a lone non-directory argument is the name.
            let (proj_dir, want) = match a.pos.as_slice() {
                [x] if !std::path::Path::new(x).is_dir() => (PathBuf::from("."), Some(x.clone())),
                [d] => (PathBuf::from(d), None),
                [d, n, ..] => (PathBuf::from(d), Some(n.clone())),
                [] => (PathBuf::from("."), None),
            };
            let p = Project::load(&proj_dir).ok();
            let dir = match &p {
                Some(p) => p.root.join(&p.settings.log_dir),
                None => proj_dir.join("logs"),
            };
            let all = pneuma_core::monitor::list_logs(&dir);
            match want.as_deref() {
                None => {
                    if all.is_empty() {
                        println!("nenhum log em {} (use Ctrl+G no monitor ou --log)", dir.display());
                    }
                    for (f, n) in &all {
                        println!("{:>10} B  {}", n, f.display());
                    }
                }
                Some(want) => {
                    let f = if want == "latest" {
                        all.last().map(|x| x.0.clone())
                    } else {
                        all.iter().map(|x| x.0.clone()).find(|f| f.file_name().is_some_and(|n| n.to_string_lossy().contains(want)))
                    };
                    let f = f.ok_or_else(|| Error::Build(format!("log `{want}` não encontrado em {}", dir.display())))?;
                    std::io::copy(&mut std::fs::File::open(f)?, &mut std::io::stdout())?;
                }
            }
        }
        "tui" => tui::run(a.dir(), a.port.clone())?,
        _ => println!("{USAGE}"),
    }
    Ok(())
}

fn main() -> ExitCode {
    let hold = std::env::args().any(|x| x == "--hold");
    let r = run();
    let failed = r.is_err();
    if let Err(e) = &r {
        eprintln!("erro: {e}");
    }
    let cmd = std::env::args().nth(1).unwrap_or_default();
    // In a throwaway window keep the output readable (monitor closes unless it failed).
    if hold && (failed || cmd != "monitor") {
        eprintln!("\n[Enter para fechar]");
        let _ = std::io::stdin().read_line(&mut String::new());
    }
    if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}
