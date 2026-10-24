//! Build/upload backend. Delegates compilation to `arduino-cli` (isolated data dir
//! under `~/.pneuma`, never touching the Arduino IDE's) but owns everything that
//! PlatformIO gets wrong: libraries come from `libs/` folders, staged into
//! `<project>/.pneuma/` with absolute paths, so include resolution never depends on cwd.

use crate::{
    boards::Board,
    config::Env,
    libs::{Lib, Scan, SKIP_DIRS},
    project::Project,
    Error, Result,
};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn err<T>(m: impl Into<String>) -> Result<T> {
    Err(Error::Build(m.into()))
}

/// `~/.pneuma` (override: `PNEUMA_HOME`, legacy `BMU_HOME`). A legacy `~/.bmu` is moved once.
pub fn pneuma_home() -> PathBuf {
    static HOME: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        if let Some(h) = std::env::var_os("PNEUMA_HOME").or_else(|| std::env::var_os("BMU_HOME")) {
            return PathBuf::from(h);
        }
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return PathBuf::from(".pneuma") };
        let (new, old) = (home.join(".pneuma"), home.join(".bmu"));
        if !new.exists() && old.is_dir() {
            match fs::rename(&old, &new) {
                Ok(_) => eprintln!("pneuma: {} movido para {}", old.display(), new.display()),
                Err(e) => {
                    eprintln!("pneuma: não consegui mover {} ({e}); usando a pasta antiga", old.display());
                    return old;
                }
            }
        }
        new
    })
    .clone()
}

/// Per-project work dir `.pneuma/`; a legacy `.bmu/` is renamed and `.gitignore` updated.
fn work_dir(root: &Path) -> PathBuf {
    let (new, old) = (root.join(".pneuma"), root.join(".bmu"));
    if !new.exists() && old.is_dir() {
        let _ = fs::rename(&old, &new);
    }
    let gi = root.join(".gitignore");
    if let Ok(s) = fs::read_to_string(&gi) {
        if s.lines().any(|l| l.trim() == ".bmu/") && !s.lines().any(|l| l.trim() == ".pneuma/") {
            let _ = fs::write(&gi, format!("{}{}.pneuma/\n", s, if s.ends_with('\n') { "" } else { "\n" }));
        }
    }
    new
}

pub fn find_cli() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("PNEUMA_ARDUINO_CLI").or_else(|| std::env::var_os("BMU_ARDUINO_CLI")).map(PathBuf::from).filter(|p| p.is_file()) {
        return Some(p);
    }
    let local = pneuma_home().join("bin/arduino-cli");
    if local.is_file() {
        return Some(local);
    }
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join("arduino-cli")).find(|p| p.is_file())
}

fn need_cli() -> Result<PathBuf> {
    find_cli().ok_or_else(|| Error::Build("arduino-cli não encontrado (só AVR roda sem ele). Rode `pneuma setup --arduino-cli`.".into()))
}

fn cli_command(cli: &Path) -> Command {
    let data = pneuma_home().join("arduino");
    let mut c = Command::new(cli);
    c.env("ARDUINO_DIRECTORIES_DATA", &data)
        .env("ARDUINO_DIRECTORIES_USER", data.join("user"))
        .env("ARDUINO_DIRECTORIES_DOWNLOADS", data.join("staging"));
    c
}

fn run_inherit(mut c: Command, what: &str) -> Result<()> {
    let st = c.status().map_err(|e| Error::Build(format!("{what}: {e}")))?;
    if st.success() {
        Ok(())
    } else {
        err(format!("{what} falhou ({st})"))
    }
}

/// Like `run_inherit`, but compiler paths are rewritten from the staged copies to the user's files.
fn run_remapped(mut c: Command, what: &str) -> Result<()> {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    c.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| Error::Build(format!("{what}: {e}")))?;
    let (out, errp) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let t = std::thread::spawn(move || {
        for l in BufReader::new(errp).split(b'\n').flatten() {
            eprintln!("{}", remap_paths(&String::from_utf8_lossy(&l)));
        }
    });
    for l in BufReader::new(out).split(b'\n').flatten() {
        println!("{}", remap_paths(&String::from_utf8_lossy(&l)));
    }
    let _ = t.join();
    let st = child.wait().map_err(|e| Error::Build(format!("{what}: {e}")))?;
    if st.success() {
        Ok(())
    } else {
        err(format!("{what} falhou ({st})"))
    }
}

/// Download arduino-cli into `~/.pneuma/bin` (needs `curl` and `tar`).
pub fn install_cli() -> Result<PathBuf> {
    let os = match std::env::consts::OS {
        "linux" => "Linux",
        "macos" => "macOS",
        o => return err(format!("instalação automática não suportada em {o}; instale arduino-cli manualmente")),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "64bit",
        "aarch64" => "ARM64",
        a => return err(format!("arquitetura {a} não suportada pelo `setup`")),
    };
    let bin = pneuma_home().join("bin");
    fs::create_dir_all(&bin)?;
    let url = format!("https://downloads.arduino.cc/arduino-cli/arduino-cli_latest_{os}_{arch}.tar.gz");
    let tgz = bin.join("arduino-cli.tgz");
    let mut curl = Command::new("curl");
    curl.args(["-fL", "--progress-bar", "-o"]).arg(&tgz).arg(&url);
    run_inherit(curl, "curl")?;
    let mut tar = Command::new("tar");
    tar.arg("xzf").arg(&tgz).arg("-C").arg(&bin).arg("arduino-cli");
    run_inherit(tar, "tar")?;
    let _ = fs::remove_file(&tgz);
    Ok(bin.join("arduino-cli"))
}

pub fn cli_version() -> Result<String> {
    let cli = need_cli()?;
    let o = cli_command(&cli).arg("version").output()?;
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn core_installed(cli: &Path, core: &str) -> bool {
    cli_command(cli)
        .args(["core", "list"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().any(|l| l.split_whitespace().next() == Some(core)))
        .unwrap_or(false)
}

/// Marker written only after a positive `core list` or a finished install; trusted only while the
/// core's hardware dir still exists (an aborted download never leaves a marker).
fn core_marker(core: &str) -> PathBuf {
    pneuma_home().join("cores-ok").join(core.replace(':', "_"))
}

fn core_dir(core: &str) -> PathBuf {
    let (v, a) = core.split_once(':').unwrap_or((core, ""));
    pneuma_home().join("arduino/packages").join(v).join("hardware").join(a)
}

fn mark_core(core: &str) {
    let m = core_marker(core);
    if let Some(d) = m.parent() {
        let _ = fs::create_dir_all(d);
    }
    let _ = fs::write(m, "");
}

pub fn ensure_core(board: &Board) -> Result<()> {
    let core = board.core();
    if core_marker(core).exists() && core_dir(core).is_dir() {
        return Ok(());
    }
    let cli = need_cli()?;
    if core_installed(&cli, core) {
        mark_core(core);
        return Ok(());
    }
    let size = match board.family {
        crate::boards::Family::Esp32 => "~1.5 GB",
        crate::boards::Family::Esp8266 => "~300 MB",
        crate::boards::Family::Rp2040 => "~300 MB",
        crate::boards::Family::Avr => "~50 MB",
    };
    println!("Instalando suporte a {} ({size}, só na primeira vez; pode demorar)...", board.core());
    let urls: Vec<&str> = board.index_url().into_iter().collect();
    let mut up = cli_command(&cli);
    up.args(["core", "update-index"]);
    if !urls.is_empty() {
        up.arg("--additional-urls").arg(urls.join(","));
    }
    run_inherit(up, "core update-index")?;
    let mut ins = cli_command(&cli);
    ins.args(["core", "install", board.core()]);
    if !urls.is_empty() {
        ins.arg("--additional-urls").arg(urls.join(","));
    }
    run_inherit(ins, "core install")?;
    mark_core(core);
    Ok(())
}

// ---------- library registry ----------

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for e in fs::read_dir(from)?.flatten() {
        let (p, name) = (e.path(), e.file_name());
        if name == ".git" {
            continue;
        }
        if p.is_dir() {
            copy_dir(&p, &to.join(&name))?;
        } else {
            fs::copy(&p, to.join(&name))?;
        }
    }
    Ok(())
}

/// `arduino-cli lib search <query>`, printed as is.
pub fn lib_search(query: &str) -> Result<()> {
    let cli = need_cli()?;
    let mut up = cli_command(&cli);
    up.args(["lib", "update-index"]);
    run_inherit(up, "lib update-index")?;
    let mut c = cli_command(&cli);
    c.args(["lib", "search", query]);
    run_inherit(c, "lib search")
}

/// Download `spec` (`Name` or `Name@1.2.3`) and its dependencies from the Arduino registry into an
/// isolated scratch dir, then copy each library folder into `libs_dir` as a plain folder.
/// Existing folders are never overwritten. Returns `(name, added)` per library.
pub fn lib_add(spec: &str, libs_dir: &Path) -> Result<Vec<(String, bool)>> {
    let cli = need_cli()?;
    let scratch = pneuma_home().join("tmp").join(format!("lib-{}", std::process::id()));
    let _ = fs::remove_dir_all(&scratch);
    fs::create_dir_all(&scratch)?;
    let result = (|| {
        let mut up = cli_command(&cli);
        up.args(["lib", "update-index"]);
        run_inherit(up, "lib update-index")?;
        let mut c = cli_command(&cli);
        c.env("ARDUINO_DIRECTORIES_USER", &scratch).args(["lib", "install", spec]);
        run_inherit(c, "lib install")?;
        let mut done = vec![];
        let mut dirs: Vec<_> = fs::read_dir(scratch.join("libraries")).map_err(|_| Error::Build(format!("`{spec}` não instalou nada")))?.flatten().collect();
        dirs.sort_by_key(|e| e.file_name());
        for e in dirs.into_iter().filter(|e| e.path().is_dir()) {
            let name = e.file_name().to_string_lossy().into_owned();
            let dest = libs_dir.join(&name);
            if dest.exists() {
                done.push((name, false));
            } else {
                copy_dir(&e.path(), &dest)?;
                done.push((name, true));
            }
        }
        Ok(done)
    })();
    let _ = fs::remove_dir_all(&scratch);
    result
}

// ---------- staging ----------

enum Content {
    File(PathBuf),
    Text(String),
}

fn collect(dir: &Path, skip_noise: bool) -> Vec<PathBuf> {
    fn go(base: &Path, dir: &Path, skip: bool, out: &mut Vec<PathBuf>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_ascii_lowercase();
            if name.starts_with('.') {
                continue;
            }
            if p.is_dir() {
                if !(skip && SKIP_DIRS.contains(&name.as_str())) {
                    go(base, &p, skip, out);
                }
            } else if let Ok(r) = p.strip_prefix(base) {
                out.push(r.to_path_buf());
            }
        }
    }
    let mut v = vec![];
    go(dir, dir, skip_noise, &mut v);
    v.sort();
    v
}

/// Make `dest_root` contain exactly `items`. Files are rewritten only when the
/// content differs, so mtimes stay stable and arduino-cli can reuse its object cache.
fn sync(dest_root: &Path, items: Vec<(PathBuf, Content)>) -> Result<()> {
    let mut keep: HashSet<PathBuf> = HashSet::new();
    for (rel, c) in items {
        let dst = dest_root.join(&rel);
        let bytes = match &c {
            Content::File(p) => fs::read(p)?,
            Content::Text(t) => t.clone().into_bytes(),
        };
        if fs::read(&dst).map(|cur| cur != bytes).unwrap_or(true) {
            if let Some(d) = dst.parent() {
                fs::create_dir_all(d)?;
            }
            fs::write(&dst, bytes)?;
        }
        keep.insert(rel);
    }
    for rel in collect(dest_root, false) {
        if !keep.contains(&rel) {
            let _ = fs::remove_file(dest_root.join(rel));
        }
    }
    Ok(())
}

/// Arduino "recursive layout" (`src/` + `library.properties`) built from any of
/// our accepted layouts: flat, `src/`, or `include/`+`src/`.
fn stage_lib(l: &Lib, dest: &Path) -> Result<()> {
    let (src, inc) = (l.path.join("src"), l.path.join("include"));
    let mut items: Vec<(PathBuf, Content)> = vec![];
    let to_src = |rel: PathBuf| PathBuf::from("src").join(rel);
    if src.is_dir() {
        items.extend(collect(&src, true).into_iter().map(|r| (to_src(r.clone()), Content::File(src.join(r)))));
    } else {
        for r in collect(&l.path, true) {
            if !r.starts_with("include") && r != Path::new("library.properties") {
                items.push((to_src(r.clone()), Content::File(l.path.join(r))));
            }
        }
    }
    if inc.is_dir() {
        items.extend(collect(&inc, true).into_iter().map(|r| (to_src(r.clone()), Content::File(inc.join(r)))));
    }
    items.push((
        "library.properties".into(),
        Content::Text(format!("name={}\nversion=0.0.0\nauthor=pneuma\nmaintainer=pneuma\nsentence=staged by pneuma\nparagraph=staged by pneuma\ncategory=Other\nurl=\narchitectures=*\n", l.name)),
    ));
    sync(dest, items)
}

fn sketch_name(root: &Path) -> String {
    let n: String = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "sketch".into())
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    if n.starts_with(|c: char| c.is_ascii_digit()) { format!("p{n}") } else { n }
}

fn stage_sketch(p: &Project, name: &str, dest: &Path) -> Result<()> {
    let src = p.root.join(&p.settings.src_dir);
    if !src.is_dir() {
        return err(format!("pasta de código não existe: {} (crie-a com main.cpp ou {name}.ino)", src.display()));
    }
    let files = collect(&src, false);
    let want = PathBuf::from(format!("{name}.ino"));
    let inos: Vec<&PathBuf> = files.iter().filter(|f| f.parent().is_some_and(|p| p.as_os_str().is_empty()) && f.extension().is_some_and(|e| e == "ino")).collect();
    let rename_from = (!files.contains(&want) && inos.len() == 1).then(|| inos[0].clone());
    let mut items: Vec<(PathBuf, Content)> = vec![];
    for f in &files {
        let dst = if Some(f) == rename_from.as_ref() { want.clone() } else { f.clone() };
        items.push((dst, Content::File(src.join(f))));
    }
    if !files.contains(&want) && rename_from.is_none() {
        if !files.iter().any(|f| matches!(f.extension().and_then(|e| e.to_str()), Some("cpp" | "c" | "cc"))) {
            return err(format!("nenhum código em {}", src.display()));
        }
        items.push((want, Content::Text("// stub gerado pelo pneuma: o código real está nos .cpp ao lado\n".into())));
    }
    sync(dest, items)
}

#[derive(Debug)]
#[derive(Clone)]
pub struct Plan {
    /// Set when the native AVR backend builds this plan (no arduino-cli).
    pub native: Option<crate::native::NativeJob>,
    /// Set for `framework = "cmake"` envs (CMake + Ninja, no arduino-cli).
    pub cmake: Option<crate::cmake::CmakeJob>,
    pub fqbn: String,
    pub sketch_dir: PathBuf,
    pub build_dir: PathBuf,
    pub libs: Vec<String>,
    pub compile_args: Vec<String>,
}

pub fn prepare(p: &Project, env: &Env, scan: &Scan) -> Result<Plan> {
    if let Some(job) = crate::cmake::job(p, env)? {
        return Ok(Plan { native: None, cmake: Some(job.clone()), fqbn: "cmake".into(), sketch_dir: job.source_dir, build_dir: job.build_dir, libs: vec![], compile_args: vec![] });
    }
    let work = work_dir(&p.root);
    let name = sketch_name(&p.root);
    let sketch_dir = work.join("sketch").join(&env.name).join(&name);
    let build_dir = work.join("build").join(&env.name);
    stage_sketch(p, &name, &sketch_dir)?;

    let chosen: Vec<&Lib> = if env.lib_deps.is_empty() {
        scan.libs.iter().collect()
    } else {
        let mut v = vec![];
        for n in &env.lib_deps {
            match scan.libs.iter().find(|l| &l.name == n) {
                Some(l) => v.push(l),
                None => return err(format!("env `{}`: biblioteca `{n}` não existe em {}", env.name, scan.libs_dir.display())),
            }
        }
        v
    };
    let mut args: Vec<String> = vec!["compile".into(), "--fqbn".into(), env.board.fqbn.into(), "--build-path".into(), build_dir.display().to_string()];
    for l in &chosen {
        let dest = work.join("libs").join(&l.name);
        stage_lib(l, &dest)?;
        args.push("--library".into());
        args.push(dest.display().to_string());
    }
    if !env.build_flags.is_empty() {
        let f = env.build_flags.join(" ");
        for k in ["compiler.c.extra_flags", "compiler.cpp.extra_flags"] {
            args.push("--build-property".into());
            args.push(format!("{k}={f}"));
        }
    }
    args.push(sketch_dir.display().to_string());
    let native = pick_native(p, env, &sketch_dir)?.then(|| crate::native::NativeJob {
        fqbn: env.board.fqbn.into(),
        sketch_dir: sketch_dir.clone(),
        build_dir: work.join("build").join(format!("{}-native", env.name)),
        lib_dirs: chosen.iter().map(|l| work.join("libs").join(&l.name)).collect(),
        extra_flags: env.build_flags.join(" "),
        project_name: name.clone(),
    });
    set_remap(&p.root, &p.settings.src_dir, &p.settings.libs_dir);
    Ok(Plan { native, cmake: None, fqbn: env.board.fqbn.into(), sketch_dir, build_dir, libs: chosen.iter().map(|l| l.name.clone()).collect(), compile_args: args })
}

/// Native backend for AVR unless disabled; a real `.ino` (needs prototype generation) needs arduino-cli.
fn pick_native(p: &Project, env: &Env, sketch_dir: &Path) -> Result<bool> {
    let want = std::env::var("PNEUMA_BACKEND").ok().or_else(|| p.settings.backend.clone()).unwrap_or_else(|| "auto".into());
    let real_ino = fs::read_dir(sketch_dir).into_iter().flatten().flatten().any(|e| {
        e.path().extension().is_some_and(|x| x == "ino")
            && fs::read_to_string(e.path()).is_ok_and(|t| t.lines().any(|l| !l.trim().is_empty() && !l.trim_start().starts_with("//")))
    });
    let avr = env.board.family == crate::boards::Family::Avr;
    match want.as_str() {
        "arduino-cli" => Ok(false),
        "native" if !avr => err(format!("backend nativo só existe para AVR; `{}` usa arduino-cli", env.board.id)),
        "native" if real_ino => err("arquivos .ino com código precisam do arduino-cli (geração de protótipos): use backend = \"arduino-cli\" ou renomeie para .cpp"),
        "native" => Ok(true),
        "auto" => Ok(avr && !real_ino),
        o => err(format!("backend desconhecido `{o}` (use auto, native ou arduino-cli)")),
    }
}

/// Which backend a `build` for this env would use (for display; ignores the `.ino` fallback).
pub fn backend_name(p: &Project, env: &Env) -> &'static str {
    if crate::cmake::is_cmake_framework(env.framework.as_deref()) {
        return "cmake";
    }
    let want = std::env::var("PNEUMA_BACKEND").ok().or_else(|| p.settings.backend.clone()).unwrap_or_else(|| "auto".into());
    if want != "arduino-cli" && env.board.family == crate::boards::Family::Avr {
        "native"
    } else {
        "arduino-cli"
    }
}

/// Installs the native AVR toolchain on first use.
pub fn ensure_native() -> Result<()> {
    if crate::native::Toolchain::locate("avr").is_some() {
        return Ok(());
    }
    println!("Instalando a toolchain AVR (~250 MB, só na primeira vez)...");
    crate::toolchain::install_avr()
}

pub fn compile(plan: &Plan, board: &Board) -> Result<()> {
    if let Some(c) = &plan.cmake {
        return crate::cmake::compile(c);
    }
    if let Some(n) = &plan.native {
        ensure_native()?;
        return crate::native::compile(n);
    }
    ensure_core(board)?;
    let cli = need_cli()?;
    let mut c = cli_command(&cli);
    c.args(&plan.compile_args);
    run_remapped(c, "compilação")
}

pub fn upload_args(plan: &Plan, port: &str) -> Vec<String> {
    ["upload", "-p", port, "--fqbn", &plan.fqbn, "--input-dir", &plan.build_dir.display().to_string()]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

pub fn upload(plan: &Plan, port: &str) -> Result<()> {
    if let Some(c) = &plan.cmake {
        return crate::cmake::upload(c, port);
    }
    if let Some(n) = &plan.native {
        return crate::native::upload(n, port);
    }
    let cli = need_cli()?;
    let mut c = cli_command(&cli);
    c.args(upload_args(plan, port));
    run_inherit(c, "upload")
}

// ---------- compiler messages: staged copies -> the user's files ----------

static REMAP: std::sync::Mutex<Option<(PathBuf, String, String)>> = std::sync::Mutex::new(None);

fn set_remap(root: &Path, src_dir: &str, libs_dir: &str) {
    *REMAP.lock().unwrap() = Some((root.to_path_buf(), src_dir.to_string(), libs_dir.to_string()));
}

/// gcc reports `.pneuma/sketch/<env>/<name>/x.cpp` and `.pneuma/libs/<lib>/src/y.h` (staged copies);
/// point those at `src/x.cpp` and `libs/<lib>/...` so editors and terminals open the real file.
pub fn remap_paths(text: &str) -> String {
    let Some((root, src_dir, libs_dir)) = REMAP.lock().unwrap().clone() else { return text.to_string() };
    remap_with(text, &root, &src_dir, &libs_dir)
}

fn remap_with(text: &str, root: &Path, src_dir: &str, libs_dir: &str) -> String {
    let work = root.join(".pneuma").display().to_string();
    let end_of_path = |s: &str| s.find(|c: char| matches!(c, ':' | ' ' | '\n' | '"' | '\'' | ')' | '\t')).unwrap_or(s.len());
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let sk = rest.find(&format!("{work}/sketch/"));
        let lb = rest.find(&format!("{work}/libs/"));
        let (i, is_sketch) = match (sk, lb) {
            (Some(a), Some(b)) if a < b => (a, true),
            (Some(_), Some(b)) => (b, false),
            (Some(a), None) => (a, true),
            (None, Some(b)) => (b, false),
            (None, None) => break,
        };
        out.push_str(&rest[..i]);
        let after = &rest[i + work.len() + if is_sketch { "/sketch/".len() } else { "/libs/".len() }..];
        let n = end_of_path(after);
        let path = &after[..n];
        let mapped = if is_sketch {
            // <env>/<name>/<rel>
            let mut parts = path.splitn(3, '/');
            match (parts.next(), parts.next(), parts.next()) {
                (Some(_), Some(_), Some(rel)) => Some(root.join(src_dir).join(rel)),
                _ => None,
            }
        } else {
            // <lib>/src/<rel>  ->  the first of libs/<lib>/{src,include,.}/<rel> that exists
            let mut parts = path.splitn(2, '/');
            match (parts.next(), parts.next()) {
                (Some(lib), Some(r)) => {
                    let rel = r.strip_prefix("src/").unwrap_or(r);
                    ["src", "include", ""].iter().map(|d| root.join(libs_dir).join(lib).join(d).join(rel)).find(|p| p.is_file())
                }
                _ => None,
            }
        };
        match mapped {
            Some(p) => out.push_str(&p.display().to_string()),
            None => out.push_str(&rest[i..i + (rest.len() - after.len()) - i + n]),
        }
        rest = &after[n..];
    }
    out.push_str(rest);
    out
}

// ---------- debug ----------

/// Same sketch, built with `--optimize-for-debug` into its own build dir (keeps normal cache intact).
pub fn debug_plan(plan: &Plan) -> Plan {
    let dir = plan.build_dir.with_file_name(format!("{}-debug", plan.build_dir.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()));
    let (old, new) = (plan.build_dir.display().to_string(), dir.display().to_string());
    let mut args: Vec<String> = plan.compile_args.iter().map(|a| if *a == old { new.clone() } else { a.clone() }).collect();
    let at = args.len().saturating_sub(1);
    args.insert(at, "--optimize-for-debug".into());
    Plan { build_dir: dir, compile_args: args, ..plan.clone() }
}

/// Does arduino-cli know a debug setup for this board (+ programmer)?
pub fn debug_supported(fqbn: &str, programmer: Option<&str>) -> Result<bool> {
    let cli = need_cli()?;
    let mut c = cli_command(&cli);
    c.args(["debug", "check", "--json", "--fqbn", fqbn]);
    if let Some(p) = programmer {
        c.args(["--programmer", p]);
    }
    let o = c.output().map_err(|e| Error::Build(format!("debug check: {e}")))?;
    // `{"debugging_supported": true|false}`
    let out: String = String::from_utf8_lossy(&o.stdout).split_whitespace().collect();
    Ok(out.contains("\"debugging_supported\":true"))
}

/// Interactive gdb session through arduino-cli (it starts the debug server, e.g. OpenOCD).
pub fn debug(plan: &Plan, port: Option<&str>, programmer: Option<&str>) -> Result<()> {
    let cli = need_cli()?;
    let mut c = cli_command(&cli);
    c.args(["debug", "--fqbn", &plan.fqbn, "--input-dir", &plan.build_dir.display().to_string()]);
    if let Some(p) = port {
        c.args(["--port", p]);
    }
    if let Some(p) = programmer {
        c.args(["--programmer", p]);
    }
    c.arg(plan.sketch_dir.display().to_string());
    run_inherit(c, "debug")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{libs, project::Project};

    #[test]
    fn debug_plan_uses_own_dir_and_flag() {
        let plan = Plan {
            native: None,
            cmake: None,
            fqbn: "x:y:z".into(),
            sketch_dir: "/p/.pneuma/sketch/e/p".into(),
            build_dir: "/p/.pneuma/build/e".into(),
            libs: vec![],
            compile_args: ["compile", "--build-path", "/p/.pneuma/build/e", "/p/.pneuma/sketch/e/p"].map(String::from).to_vec(),
        };
        let d = debug_plan(&plan);
        assert_eq!(d.build_dir, std::path::PathBuf::from("/p/.pneuma/build/e-debug"));
        assert_eq!(d.compile_args, ["compile", "--build-path", "/p/.pneuma/build/e-debug", "--optimize-for-debug", "/p/.pneuma/sketch/e/p"]);
        assert_eq!(plan.compile_args[2], "/p/.pneuma/build/e");
    }

    fn proj(name: &str) -> (PathBuf, Project) {
        let d = std::env::temp_dir().join(format!("pneuma_build_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("src")).unwrap();
        fs::create_dir_all(d.join("libs/flat")).unwrap();
        fs::create_dir_all(d.join("libs/both/include")).unwrap();
        fs::create_dir_all(d.join("libs/both/src")).unwrap();
        fs::write(d.join("src/main.cpp"), "void setup(){}\nvoid loop(){}\n").unwrap();
        fs::write(d.join("libs/flat/f.h"), "").unwrap();
        fs::write(d.join("libs/flat/f.cpp"), "").unwrap();
        fs::write(d.join("libs/both/include/b.h"), "").unwrap();
        fs::write(d.join("libs/both/src/b.cpp"), "").unwrap();
        fs::write(d.join("pneuma.lua"), "return { build_flags={'-DX=1'}, env = { u = { board='uno', libs={'flat'}, build_flags={'-Os'} }, all = { board='uno' }, bad = { board='uno', libs={'nope'} } } }").unwrap();
        let p = Project::load(&d).unwrap();
        (d, p)
    }
    fn env<'a>(p: &'a Project, n: &str) -> &'a Env {
        p.envs.iter().find(|(x, _)| x == n).unwrap().1.as_ref().unwrap()
    }

    #[test]
    fn stages_sketch_libs_and_builds_args() {
        let (_d, p) = proj("ok");
        let scan = libs::scan(&p.root, "libs", &[]).unwrap();
        let plan = prepare(&p, env(&p, "u"), &scan).unwrap();
        assert_eq!(plan.libs, ["flat"]);
        assert!(plan.sketch_dir.join("main.cpp").is_file());
        let stub = plan.sketch_dir.file_name().unwrap().to_string_lossy().into_owned() + ".ino";
        assert!(plan.sketch_dir.join(stub).is_file());
        let lib = p.root.join(".pneuma/libs/flat");
        assert!(lib.join("src/f.h").is_file() && lib.join("library.properties").is_file());
        assert!(plan.compile_args.windows(2).any(|w| w[0] == "--library" && w[1] == lib.display().to_string()));
        assert!(plan.compile_args.contains(&"compiler.cpp.extra_flags=-DX=1 -Os".to_string()));
        assert!(plan.compile_args.iter().all(|a| !a.starts_with("..")));
        // all libs, include/ merged into staged src/
        let plan = prepare(&p, env(&p, "all"), &scan).unwrap();
        assert_eq!(plan.libs, ["both", "flat"]);
        assert!(p.root.join(".pneuma/libs/both/src/b.h").is_file() && p.root.join(".pneuma/libs/both/src/b.cpp").is_file());
    }

    #[test]
    fn restage_is_stable_and_removes_stale() {
        let (d, p) = proj("stale");
        let scan = libs::scan(&p.root, "libs", &[]).unwrap();
        prepare(&p, env(&p, "u"), &scan).unwrap();
        let f = d.join(".pneuma/libs/flat/src/f.cpp");
        let m1 = fs::metadata(&f).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        prepare(&p, env(&p, "u"), &scan).unwrap();
        assert_eq!(fs::metadata(&f).unwrap().modified().unwrap(), m1, "unchanged file must not be rewritten");
        fs::remove_file(d.join("libs/flat/f.cpp")).unwrap();
        let scan = libs::scan(&p.root, "libs", &[]).unwrap();
        prepare(&p, env(&p, "u"), &scan).unwrap();
        assert!(!f.exists(), "stale staged file removed");
    }

    #[test]
    fn errors_are_clear() {
        let (d, p) = proj("err");
        let scan = libs::scan(&p.root, "libs", &[]).unwrap();
        let e = prepare(&p, env(&p, "bad"), &scan).unwrap_err().to_string();
        assert!(e.contains("`nope`"));
        fs::remove_dir_all(d.join("src")).unwrap();
        assert!(prepare(&p, env(&p, "u"), &scan).unwrap_err().to_string().contains("pasta de código"));
    }

    #[test]
    fn single_ino_is_renamed_to_sketch_name() {
        let (d, p) = proj("ino");
        fs::remove_file(d.join("src/main.cpp")).unwrap();
        fs::write(d.join("src/blink.ino"), "void setup(){}\nvoid loop(){}\n").unwrap();
        let scan = libs::scan(&p.root, "libs", &[]).unwrap();
        let plan = prepare(&p, env(&p, "u"), &scan).unwrap();
        let n = plan.sketch_dir.file_name().unwrap().to_string_lossy().into_owned();
        assert!(plan.sketch_dir.join(format!("{n}.ino")).is_file() && !plan.sketch_dir.join("blink.ino").exists());
    }

    #[test]
    fn compiler_paths_point_at_user_files() {
        let d = std::env::temp_dir().join(format!("pneuma_remap_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("libs/dht/include")).unwrap();
        fs::write(d.join("libs/dht/include/dht.h"), "").unwrap();
        let w = d.join(".pneuma");
        let msg = format!(
            "{w}/sketch/uno/p/code.cpp:44:3: error: 'delay' was not declared\nIn file included from {w}/libs/dht/src/dht.h:1,\n  see {w}/libs/zzz/src/gone.h:2: x",
            w = w.display()
        );
        let out = remap_with(&msg, &d, "src", "libs");
        assert!(out.contains(&format!("{}/src/code.cpp:44:3: error", d.display())));
        assert!(out.contains(&format!("{}/libs/dht/include/dht.h:1,", d.display())));
        assert!(out.contains(&format!("{}/libs/zzz/src/gone.h:2", w.display())), "unmapped stays as is: {out}");
    }

    #[test]
    fn upload_args_shape() {
        let plan = Plan { native: None, cmake: None, fqbn: "arduino:avr:uno".into(), sketch_dir: "s".into(), build_dir: "/b".into(), libs: vec![], compile_args: vec![] };
        assert_eq!(upload_args(&plan, "/dev/ttyACM0"), ["upload", "-p", "/dev/ttyACM0", "--fqbn", "arduino:avr:uno", "--input-dir", "/b"]);
    }
}
