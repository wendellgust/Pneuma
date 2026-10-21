//! Native AVR backend: builds and uploads without arduino-cli.
//!
//! It does not hard-code any compiler flag. It reads the platform's own `platform.txt` and
//! `boards.txt` (the same files arduino-cli reads), expands their `{property}` recipes and runs
//! the resulting commands, so the command lines match arduino-cli's. Objects are cached (command
//! line + `.d` dependency files) and compiled in parallel.

use crate::{build::pneuma_home, Error, Result};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
    time::SystemTime,
};

/// `runtime.ide.version` arduino-cli reports (the `-DARDUINO=` value); sketches may test it.
const IDE_VERSION: &str = "10607";

/// `PNEUMA_VERBOSE=1` prints every command line, like `arduino-cli compile -v`.
fn verbose() -> bool {
    std::env::var_os("PNEUMA_VERBOSE").is_some()
}

fn err<T>(m: impl Into<String>) -> Result<T> {
    Err(Error::Build(m.into()))
}

// ---------- properties (Arduino platform.txt / boards.txt dialect) ----------

#[derive(Debug, Clone, Default)]
pub struct Props(BTreeMap<String, String>);

fn host_os_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "macosx"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    }
}

impl Props {
    /// `key=value` lines; `#` comments; `key.linux=` style overrides apply on that OS only.
    pub fn parse(text: &str) -> Props {
        let mut map = BTreeMap::new();
        for line in text.lines() {
            let l = line.trim();
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = l.split_once('=') {
                map.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
        let host = host_os_key();
        let overrides: Vec<(String, String, String)> = map
            .iter()
            .filter_map(|(k, v)| k.rsplit_once('.').filter(|(_, os)| matches!(*os, "linux" | "macosx" | "windows")).map(|(b, os)| (b.to_string(), os.to_string(), v.clone())))
            .collect();
        for (base, os, v) in overrides {
            map.remove(&format!("{base}.{os}"));
            if os == host {
                map.insert(base, v);
            }
        }
        Props(map)
    }

    pub fn set(&mut self, k: impl Into<String>, v: impl Into<String>) {
        self.0.insert(k.into(), v.into());
    }

    pub fn get(&self, k: &str) -> Option<&str> {
        self.0.get(k).map(String::as_str)
    }

    /// Everything under `prefix.` with the prefix removed (`tools.avrdude.` -> `cmd.path`, ...).
    pub fn with_prefix_stripped(&self, prefix: &str) -> Props {
        let p = format!("{prefix}.");
        Props(self.0.iter().filter_map(|(k, v)| k.strip_prefix(&p).map(|r| (r.to_string(), v.clone()))).collect())
    }

    pub fn merge(&mut self, other: &Props) {
        for (k, v) in &other.0 {
            self.0.insert(k.clone(), v.clone());
        }
    }

    /// Replace `{name}` recursively. An undefined name is an error: a silently empty value would
    /// hide a broken recipe.
    pub fn expand(&self, s: &str) -> Result<String> {
        let mut cur = s.to_string();
        for _ in 0..24 {
            if !cur.contains('{') {
                return Ok(cur);
            }
            let mut out = String::with_capacity(cur.len());
            let mut rest = cur.as_str();
            while let Some(i) = rest.find('{') {
                out.push_str(&rest[..i]);
                let after = &rest[i + 1..];
                let Some(j) = after.find('}') else {
                    out.push_str(&rest[i..]);
                    rest = "";
                    break;
                };
                let name = &after[..j];
                match self.0.get(name) {
                    Some(v) => out.push_str(v),
                    None => return err(format!("propriedade `{name}` indefinida (receita: {s})")),
                }
                rest = &after[j + 1..];
            }
            out.push_str(rest);
            if out == cur {
                return Ok(out);
            }
            cur = out;
        }
        err(format!("propriedades circulares em: {s}"))
    }
}

/// Properties of one board: `<id>.*` of boards.txt, then the chosen `menu.<menu>.<option>.*`.
pub fn board_props(boards_txt: &str, id: &str, menu: &[(String, String)]) -> Props {
    let all = Props::parse(boards_txt);
    let mut out = Props::default();
    let base = format!("{id}.");
    for (k, v) in &all.0 {
        if let Some(r) = k.strip_prefix(&base) {
            if !r.starts_with("menu.") {
                out.0.insert(r.to_string(), v.clone());
            }
        }
    }
    for (m, o) in menu {
        let pre = format!("{id}.menu.{m}.{o}.");
        for (k, v) in &all.0 {
            if let Some(r) = k.strip_prefix(&pre) {
                out.0.insert(r.to_string(), v.clone());
            }
        }
    }
    out
}

/// `arduino:avr:nano:cpu=atmega328` -> ("arduino","avr","nano",[("cpu","atmega328")]).
pub fn parse_fqbn(fqbn: &str) -> Result<(String, String, String, Vec<(String, String)>)> {
    let mut it = fqbn.splitn(4, ':');
    let (v, a, b) = (it.next().unwrap_or(""), it.next().unwrap_or(""), it.next().unwrap_or(""));
    if v.is_empty() || a.is_empty() || b.is_empty() {
        return err(format!("fqbn inválido: {fqbn}"));
    }
    let opts = it
        .next()
        .map(|o| o.split(',').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect())
        .unwrap_or_default();
    Ok((v.into(), a.into(), b.into(), opts))
}

/// Split a recipe into argv like a shell would (single and double quotes, no escapes).
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut has = false;
    let mut q: Option<char> = None;
    for c in s.chars() {
        match (q, c) {
            (Some(qc), c) if c == qc => q = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                q = Some(c);
                has = true;
            }
            (None, c) if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

// ---------- toolchain location ----------

fn version_key(v: &str) -> Vec<u64> {
    v.split(|c: char| !c.is_ascii_digit()).filter_map(|p| p.parse().ok()).collect()
}

fn latest_subdir(dir: &Path) -> Option<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    v.sort_by_key(|p| version_key(&p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
    v.pop()
}

/// Package roots searched for the platform and its tools: pneuma's own, then arduino-cli's.
pub fn package_roots() -> Vec<PathBuf> {
    let h = pneuma_home();
    vec![h.join("native/packages/arduino"), h.join("arduino/packages/arduino")]
}

pub struct Toolchain {
    pub platform: PathBuf,
    pub tools: BTreeMap<String, PathBuf>,
}

impl Toolchain {
    pub fn locate(arch: &str) -> Option<Toolchain> {
        for root in package_roots() {
            let Some(platform) = latest_subdir(&root.join("hardware").join(arch)) else { continue };
            let mut tools = BTreeMap::new();
            for t in fs::read_dir(root.join("tools")).into_iter().flatten().flatten() {
                if let Some(v) = latest_subdir(&t.path()) {
                    tools.insert(t.file_name().to_string_lossy().into_owned(), v);
                }
            }
            if tools.contains_key("avr-gcc") && tools.contains_key("avrdude") {
                return Some(Toolchain { platform, tools });
            }
        }
        None
    }
}

// ---------- the build ----------

#[derive(Debug, Clone)]
pub struct NativeJob {
    pub fqbn: String,
    pub sketch_dir: PathBuf,
    pub build_dir: PathBuf,
    /// Staged project libraries (`<dir>/src/...`).
    pub lib_dirs: Vec<PathBuf>,
    /// From `build_flags`: passed as compiler.c/cpp.extra_flags.
    pub extra_flags: String,
    pub project_name: String,
}

struct Ctx {
    props: Props,
    tc: Toolchain,
}

fn ctx(job: &NativeJob) -> Result<Ctx> {
    let (vendor, arch, board, menu) = parse_fqbn(&job.fqbn)?;
    let tc = Toolchain::locate(&arch).ok_or_else(|| Error::Build("toolchain AVR não instalada: rode `pneuma setup`".into()))?;
    let mut props = Props::parse(&fs::read_to_string(tc.platform.join("platform.txt"))?);
    let bp = board_props(&fs::read_to_string(tc.platform.join("boards.txt"))?, &board, &menu);
    if bp.get("name").is_none() {
        return err(format!("placa `{board}` não existe em {}", tc.platform.display()));
    }
    props.merge(&bp);
    props.set("runtime.platform.path", tc.platform.display().to_string());
    props.set("runtime.hardware.path", tc.platform.parent().and_then(|p| p.parent()).map(|p| p.display().to_string()).unwrap_or_default());
    props.set("runtime.ide.version", IDE_VERSION);
    props.set("runtime.os", host_os_key());
    for (name, dir) in &tc.tools {
        props.set(format!("runtime.tools.{name}.path"), dir.display().to_string());
        if let Some(v) = dir.file_name() {
            props.set(format!("runtime.tools.{name}-{}.path", v.to_string_lossy()), dir.display().to_string());
        }
    }
    props.set("build.arch", arch.to_uppercase());
    props.set("build.fqbn", job.fqbn.clone());
    props.set("build.path", job.build_dir.display().to_string());
    props.set("build.project_name", job.project_name.clone());
    props.set("build.source.path", job.sketch_dir.display().to_string());
    props.set("build.core.path", "{runtime.platform.path}/cores/{build.core}");
    props.set("build.variant.path", "{runtime.platform.path}/variants/{build.variant}");
    props.set("build.system.path", "{runtime.platform.path}/system");
    props.set("compiler.c.extra_flags", job.extra_flags.clone());
    props.set("compiler.cpp.extra_flags", job.extra_flags.clone());
    props.set("archive_file", "core.a");
    props.set("archive_file_path", "{build.path}/core.a");
    // -w (no warnings) is arduino-cli's default; `-Wall` etc. come in through extra_flags
    props.set("compiler.warning_flags", props.get("compiler.warning_flags.none").unwrap_or("-w").to_string());
    let _ = vendor;
    Ok(Ctx { props, tc })
}

fn ext_of(p: &Path) -> String {
    p.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default()
}

fn is_source(p: &Path) -> bool {
    matches!(ext_of(p).as_str(), "c" | "cpp" | "cc" | "cxx" | "S")
}

fn walk(dir: &Path, top_only: bool, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut v: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    v.sort();
    for p in v {
        let name = p.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        if p.is_dir() {
            if !top_only && !name.starts_with('.') && !matches!(name.as_str(), "examples" | "example" | "extras") {
                walk(&p, false, out);
            }
        } else if is_source(&p) {
            out.push(p);
        }
    }
}

fn sources_in(dir: &Path) -> Vec<PathBuf> {
    let mut v = vec![];
    walk(dir, false, &mut v);
    v
}

/// `#include <x.h>` / `#include "x.h"` names in a file (conditionals are not evaluated).
pub fn includes_of(text: &str) -> Vec<String> {
    let mut out = vec![];
    for l in text.lines() {
        let Some(r) = l.trim_start().strip_prefix('#') else { continue };
        let Some(r) = r.trim_start().strip_prefix("include") else { continue };
        let r = r.trim_start();
        let close = match r.chars().next() {
            Some('<') => '>',
            Some('"') => '"',
            _ => continue,
        };
        if let Some(end) = r[1..].find(close) {
            out.push(r[1..1 + end].to_string());
        }
    }
    out
}

#[derive(Clone)]
struct Lib {
    name: String,
    include: PathBuf,
    sources: Vec<PathBuf>,
}

fn lib_from_dir(dir: &Path) -> Option<Lib> {
    let name = dir.file_name()?.to_string_lossy().into_owned();
    let src = dir.join("src");
    if src.is_dir() {
        Some(Lib { name, sources: sources_in(&src), include: src })
    } else {
        // legacy layout: headers + sources in the root, helpers in utility/
        let mut s = vec![];
        walk(dir, true, &mut s);
        walk(&dir.join("utility"), true, &mut s);
        Some(Lib { name, include: dir.to_path_buf(), sources: s })
    }
}

/// Libraries needed by the sketch: follow `#include`s through sketch, library sources and headers.
fn resolve_libs(sketch_files: &[PathBuf], candidates: &[Lib], fixed_dirs: &[PathBuf]) -> Vec<Lib> {
    let mut chosen: Vec<Lib> = vec![];
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut queue: Vec<PathBuf> = sketch_files.to_vec();
    while let Some(f) = queue.pop() {
        if !seen.insert(f.clone()) {
            continue;
        }
        let Ok(text) = fs::read_to_string(&f) else { continue };
        for inc in includes_of(&text) {
            let mut dirs: Vec<PathBuf> = vec![f.parent().map(Path::to_path_buf).unwrap_or_default()];
            dirs.extend(fixed_dirs.iter().cloned());
            dirs.extend(chosen.iter().map(|l| l.include.clone()));
            if let Some(found) = dirs.iter().map(|d| d.join(&inc)).find(|p| p.is_file()) {
                queue.push(found);
                continue;
            }
            if let Some(l) = candidates.iter().find(|l| l.include.join(&inc).is_file()) {
                if !chosen.iter().any(|c| c.name == l.name) {
                    queue.extend(l.sources.iter().cloned());
                    chosen.push(l.clone());
                }
                queue.push(l.include.join(&inc));
            }
        }
    }
    chosen
}

fn mtime(p: &Path) -> Option<SystemTime> {
    fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// Dependencies listed in a gcc `-MMD` file (`obj: a.c b.h \` ...).
pub fn parse_depfile(text: &str) -> Vec<String> {
    let joined = text.replace("\\\r\n", " ").replace("\\\n", " ");
    let body = joined.split_once(": ").map(|x| x.1).unwrap_or("");
    let mut out = vec![];
    let mut cur = String::new();
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&' ') => {
                cur.push(' ');
                chars.next();
            }
            c if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

struct CompileJob {
    obj: PathBuf,
    argv: Vec<String>,
}

fn up_to_date(j: &CompileJob, src: &Path) -> bool {
    let cmdf = j.obj.with_extension("o.cmd");
    if fs::read_to_string(&cmdf).ok().as_deref() != Some(j.argv.join("\n").as_str()) {
        return false;
    }
    let Some(ot) = mtime(&j.obj) else { return false };
    let mut deps = vec![src.display().to_string()];
    if let Ok(d) = fs::read_to_string(j.obj.with_extension("d")) {
        deps.extend(parse_depfile(&d));
    }
    deps.iter().all(|d| mtime(Path::new(d)).is_some_and(|t| t <= ot))
}

fn recipe_for(ctx: &Ctx, src: &Path, obj: &Path, includes: &str) -> Result<Vec<String>> {
    let key = match ext_of(src).as_str() {
        "c" => "recipe.c.o.pattern",
        "S" => "recipe.S.o.pattern",
        _ => "recipe.cpp.o.pattern",
    };
    let mut p = ctx.props.clone();
    p.set("source_file", src.display().to_string());
    p.set("object_file", obj.display().to_string());
    p.set("includes", includes.to_string());
    let pattern = p.get(key).ok_or_else(|| Error::Build(format!("receita `{key}` não existe")))?.to_string();
    Ok(split_args(&p.expand(&pattern)?))
}

fn run_parallel(jobs: Vec<(CompileJob, PathBuf)>) -> Result<usize> {
    let todo: Vec<&(CompileJob, PathBuf)> = jobs.iter().filter(|(j, src)| !up_to_date(j, src)).collect();
    let next = AtomicUsize::new(0);
    let failed: Mutex<Option<String>> = Mutex::new(None);
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).min(todo.len().max(1));
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                if i >= todo.len() || failed.lock().unwrap().is_some() {
                    return;
                }
                let (j, _) = todo[i];
                if let Some(d) = j.obj.parent() {
                    let _ = fs::create_dir_all(d);
                }
                let _ = fs::remove_file(j.obj.with_extension("o.cmd"));
                if verbose() {
                    println!("{}", j.argv.join(" "));
                }
                match Command::new(&j.argv[0]).args(&j.argv[1..]).output() {
                    Ok(o) => {
                        let msg = String::from_utf8_lossy(&o.stderr).into_owned();
                        if !o.status.success() {
                            let mut f = failed.lock().unwrap();
                            if f.is_none() {
                                *f = Some(msg);
                            }
                        } else {
                            if !msg.trim().is_empty() {
                                eprint!("{}", crate::build::remap_paths(&msg));
                            }
                            let _ = fs::write(j.obj.with_extension("o.cmd"), j.argv.join("\n"));
                        }
                    }
                    Err(e) => {
                        *failed.lock().unwrap() = Some(format!("{}: {e}", j.argv[0]));
                    }
                }
            });
        }
    });
    if let Some(m) = failed.into_inner().unwrap() {
        eprint!("{}", crate::build::remap_paths(&m));
        return err("compilação falhou");
    }
    Ok(todo.len())
}

fn run_recipe(ctx: &Ctx, key: &str, extra: &[(&str, String)]) -> Result<Vec<u8>> {
    let mut p = ctx.props.clone();
    for (k, v) in extra {
        p.set(*k, v.clone());
    }
    let pattern = p.get(key).ok_or_else(|| Error::Build(format!("receita `{key}` não existe")))?.to_string();
    let argv = split_args(&p.expand(&pattern)?);
    if verbose() {
        println!("{}", argv.join(" "));
    }
    let o = Command::new(&argv[0]).args(&argv[1..]).output().map_err(|e| Error::Build(format!("{}: {e}", argv[0])))?;
    if !o.status.success() {
        eprint!("{}", String::from_utf8_lossy(&o.stderr));
        return err(format!("{key} falhou ({})", o.status));
    }
    Ok(o.stdout)
}

/// `(program bytes, data bytes)` from `avr-size -A` output, like the board's size regexes.
pub fn parse_size(out: &str) -> (u64, u64) {
    let (mut prog, mut data) = (0, 0);
    for l in out.lines() {
        let mut it = l.split_whitespace();
        let (Some(sec), Some(n)) = (it.next(), it.next().and_then(|n| n.parse::<u64>().ok())) else { continue };
        if matches!(sec, ".text" | ".data" | ".bootloader") {
            prog += n;
        }
        if matches!(sec, ".data" | ".bss" | ".noinit") {
            data += n;
        }
    }
    (prog, data)
}

pub fn compile(job: &NativeJob) -> Result<()> {
    let cx = ctx(job)?;
    let build = &job.build_dir;
    fs::create_dir_all(build)?;
    let core_dir = PathBuf::from(cx.props.expand("{build.core.path}")?);
    let variant_dir = PathBuf::from(cx.props.expand("{build.variant.path}")?);

    // sketch files (+ the `<name>.ino.cpp` translation unit arduino-cli generates from the stub .ino,
    // so the link looks the same as arduino-cli's), then the libraries they (transitively) include
    let mut sketch_files = sources_in(&job.sketch_dir);
    let ino_cpp = build.join("sketch-gen").join(format!("{}.ino.cpp", job.project_name));
    if job.sketch_dir.join(format!("{}.ino", job.project_name)).is_file() {
        fs::create_dir_all(ino_cpp.parent().unwrap())?;
        let body = "#include <Arduino.h>\n";
        if fs::read_to_string(&ino_cpp).ok().as_deref() != Some(body) {
            fs::write(&ino_cpp, body)?;
        }
        sketch_files.push(ino_cpp.clone());
        sketch_files.sort_by_key(|p| p.file_name().map(|n| n.to_os_string()));
    }
    let mut candidates: Vec<Lib> = job.lib_dirs.iter().filter_map(|d| lib_from_dir(d)).collect();
    for e in fs::read_dir(cx.tc.platform.join("libraries")).into_iter().flatten().flatten() {
        if e.path().is_dir() {
            candidates.extend(lib_from_dir(&e.path()));
        }
    }
    let mut scan_roots = sketch_files.clone();
    scan_roots.extend(
        fs::read_dir(&job.sketch_dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| matches!(ext_of(p).as_str(), "h" | "hpp" | "ino")),
    );
    let libs = resolve_libs(&scan_roots, &candidates, &[core_dir.clone(), variant_dir.clone(), job.sketch_dir.clone()]);

    let mut includes = format!("\"-I{}\" \"-I{}\"", core_dir.display(), variant_dir.display());
    for l in &libs {
        includes.push_str(&format!(" \"-I{}\"", l.include.display()));
    }

    // 1. core -> core.a
    let mut core_jobs = vec![];
    let mut core_objs = vec![];
    let mut core_srcs = sources_in(&core_dir);
    core_srcs.extend(sources_in(&variant_dir));
    for src in &core_srcs {
        let rel = src.strip_prefix(&cx.tc.platform).unwrap_or(src);
        let obj = build.join("core").join(format!("{}.o", rel.display().to_string().replace(['/', '\\'], "__")));
        core_jobs.push((CompileJob { argv: recipe_for(&cx, src, &obj, &includes)?, obj: obj.clone() }, src.clone()));
        core_objs.push(obj);
    }
    let core_changed = run_parallel(core_jobs)?;

    // 2. sketch + libraries
    let mut jobs = vec![];
    let mut objs = vec![];
    for src in &sketch_files {
        let rel = src.strip_prefix(&job.sketch_dir).or_else(|_| src.strip_prefix(build.join("sketch-gen"))).unwrap_or(src);
        let obj = build.join("sketch").join(format!("{}.o", rel.display()));
        jobs.push((CompileJob { argv: recipe_for(&cx, src, &obj, &includes)?, obj: obj.clone() }, src.clone()));
        objs.push(obj);
    }
    for l in &libs {
        for src in &l.sources {
            let rel = src.strip_prefix(&l.include).unwrap_or(src);
            let obj = build.join("libraries").join(&l.name).join(format!("{}.o", rel.display()));
            jobs.push((CompileJob { argv: recipe_for(&cx, src, &obj, &includes)?, obj: obj.clone() }, src.clone()));
            objs.push(obj);
        }
    }
    let changed = run_parallel(jobs)?;

    // 3. archive core (rebuilt from scratch whenever any core object changed)
    let archive = build.join("core.a");
    if core_changed > 0 || !archive.exists() {
        let _ = fs::remove_file(&archive);
        for o in &core_objs {
            run_recipe(&cx, "recipe.ar.pattern", &[("object_file", o.display().to_string())])?;
        }
    }

    // 4. link, hex, size
    let linked = build.join(format!("{}.hex", job.project_name));
    let elf = build.join(format!("{}.elf", job.project_name));
    if changed > 0 || core_changed > 0 || !linked.exists() || !elf.exists() {
        let object_files = objs.iter().map(|o| format!("\"{}\"", o.display())).collect::<Vec<_>>().join(" ");
        run_recipe(&cx, "recipe.c.combine.pattern", &[("object_files", object_files)])?;
        if cx.props.get("recipe.objcopy.eep.pattern").is_some() {
            run_recipe(&cx, "recipe.objcopy.eep.pattern", &[])?;
        }
        run_recipe(&cx, "recipe.objcopy.hex.pattern", &[])?;
    }
    let size = String::from_utf8_lossy(&run_recipe(&cx, "recipe.size.pattern", &[])?).into_owned();
    let (prog, data) = parse_size(&size);
    let max_prog: u64 = cx.props.get("upload.maximum_size").and_then(|v| v.parse().ok()).unwrap_or(0);
    let max_data: u64 = cx.props.get("upload.maximum_data_size").and_then(|v| v.parse().ok()).unwrap_or(0);
    if max_prog > 0 {
        println!("Sketch uses {prog} bytes ({}%) of program storage space. Maximum is {max_prog} bytes.", prog * 100 / max_prog);
        if prog > max_prog {
            return err("o programa não cabe na memória da placa");
        }
    }
    if max_data > 0 {
        println!("Global variables use {data} bytes ({}%) of dynamic memory, leaving {} bytes for local variables. Maximum is {max_data} bytes.", data * 100 / max_data, max_data.saturating_sub(data));
    }
    Ok(())
}

/// The avrdude command line for this board, from the platform's `tools.avrdude.upload.pattern`.
pub fn upload_argv(job: &NativeJob, port: &str) -> Result<Vec<String>> {
    let cx = ctx(job)?;
    let tool = cx.props.get("upload.tool.default").or(cx.props.get("upload.tool")).unwrap_or("avrdude").to_string();
    let mut p = cx.props.clone();
    p.merge(&cx.props.with_prefix_stripped(&format!("tools.{tool}")));
    p.set("serial.port", port);
    p.set("serial.port.file", Path::new(port).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    // arduino-cli's default upload: quiet, and without read-back verification (`-V`)
    let get = |p: &Props, k: &str| p.get(k).unwrap_or("").to_string();
    let (loud, quiet, noverify) = (get(&p, "upload.params.verbose"), get(&p, "upload.params.quiet"), get(&p, "upload.params.noverify"));
    p.set("upload.verbose", if verbose() { loud } else { quiet });
    p.set("upload.verify", noverify);
    let pattern = p.get("upload.pattern").ok_or_else(|| Error::Build(format!("a placa não define upload com `{tool}`")))?.to_string();
    Ok(split_args(&p.expand(&pattern)?))
}

pub fn upload(job: &NativeJob, port: &str) -> Result<()> {
    let argv = upload_argv(job, port)?;
    if verbose() {
        println!("{}", argv.join(" "));
    }
    let st = Command::new(&argv[0]).args(&argv[1..]).status().map_err(|e| Error::Build(format!("{}: {e}", argv[0])))?;
    if st.success() {
        Ok(())
    } else {
        err(format!("upload falhou ({st})"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn props_expand_nested_and_overrides() {
        let p = Props::parse("# c\na=1\nb={a}{a}\nc.linux=L\nc.macosx=M\nc.windows=W\nc=base\nd={b}-{c}\n");
        let want = match host_os_key() {
            "linux" => "11-L",
            "macosx" => "11-M",
            _ => "11-W",
        };
        assert_eq!(p.expand("{d}").unwrap(), want);
        assert!(p.expand("{nope}").is_err());
        assert_eq!(p.expand("no braces").unwrap(), "no braces");
    }

    #[test]
    fn board_menu_overrides() {
        let txt = "nano.name=Nano\nnano.build.mcu=atmega328p\nnano.menu.cpu.old.upload.speed=57600\nnano.menu.cpu.new.upload.speed=115200\nnano.upload.speed=9600\nuno.name=Uno\n";
        let b = board_props(txt, "nano", &[("cpu".into(), "old".into())]);
        assert_eq!(b.get("upload.speed"), Some("57600"));
        assert_eq!(b.get("build.mcu"), Some("atmega328p"));
        assert_eq!(board_props(txt, "nano", &[]).get("upload.speed"), Some("9600"));
        assert_eq!(board_props(txt, "uno", &[]).get("name"), Some("Uno"));
        let (v, a, b, o) = parse_fqbn("arduino:avr:nano:cpu=atmega328,x=y").unwrap();
        assert_eq!((v.as_str(), a.as_str(), b.as_str(), o.len()), ("arduino", "avr", "nano", 2));
    }

    #[test]
    fn splits_like_a_shell() {
        assert_eq!(split_args(r#""/a b/gcc" -DX='"Hi there"' "-I/p q" -c"#), ["/a b/gcc", "-DX=\"Hi there\"", "-I/p q", "-c"]);
        assert_eq!(split_args(r#"a "" b"#), ["a", "", "b"]);
    }

    #[test]
    fn includes_depfiles_and_size() {
        let src = "#include <Wire.h>\n  # include \"my lib.h\"\n// #include <no.h>\n#define X\n";
        assert_eq!(includes_of(src), ["Wire.h", "my lib.h"]);
        assert_eq!(parse_depfile("o.o: a.cpp \\\n b\\ c.h d.h\n"), ["a.cpp", "b c.h", "d.h"]);
        assert_eq!(parse_size("x.elf  :\nsection  size addr\n.data  10  0\n.text  100  0\n.bss  5  0\n.eeprom 0 0\nTotal 115\n"), (110, 15));
    }

    #[test]
    fn resolves_libs_transitively_only_when_included() {
        let d = std::env::temp_dir().join(format!("pneuma_native_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        for (p, t) in [
            ("a/src/a.h", "#include <b.h>\n"),
            ("a/src/a.cpp", "#include \"a.h\"\n"),
            ("b/src/b.h", ""),
            ("b/src/b.cpp", ""),
            ("unused/src/u.h", ""),
            ("sk/main.cpp", "#include <a.h>\n"),
        ] {
            fs::create_dir_all(d.join(p).parent().unwrap()).unwrap();
            fs::write(d.join(p), t).unwrap();
        }
        let cands: Vec<Lib> = ["a", "b", "unused"].iter().filter_map(|n| lib_from_dir(&d.join(n))).collect();
        let got = resolve_libs(&[d.join("sk/main.cpp")], &cands, &[]);
        let mut names: Vec<_> = got.iter().map(|l| l.name.clone()).collect();
        names.sort();
        assert_eq!(names, ["a", "b"]);
    }

    /// Real compile of a tiny sketch (skipped when no AVR toolchain is installed on this machine).
    #[test]
    fn compiles_a_sketch_and_reuses_objects() {
        if Toolchain::locate("avr").is_none() {
            eprintln!("skip: no AVR toolchain installed");
            return;
        }
        let d = std::env::temp_dir().join(format!("pneuma_nativebuild_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("sk")).unwrap();
        fs::write(d.join("sk/main.cpp"), "#include <Arduino.h>\n#include <Wire.h>\nvoid setup(){Wire.begin();}\nvoid loop(){}\n").unwrap();
        fs::write(d.join("sk/t.ino"), "// stub\n").unwrap();
        let job = NativeJob {
            fqbn: "arduino:avr:uno".into(),
            sketch_dir: d.join("sk"),
            build_dir: d.join("b"),
            lib_dirs: vec![],
            extra_flags: "-Wall".into(),
            project_name: "t".into(),
        };
        compile(&job).unwrap();
        let hex = d.join("b/t.hex");
        assert!(hex.is_file() && d.join("b/libraries/Wire/Wire.cpp.o").is_file());
        let t0 = mtime(&d.join("b/core/main.cpp.o")).or_else(|| mtime(&d.join("b/core/cores__arduino__main.cpp.o"))).unwrap();
        let t1 = mtime(&hex).unwrap();
        compile(&job).unwrap();
        assert_eq!(mtime(&hex).unwrap(), t1, "nothing changed: no relink");
        assert_eq!(mtime(&d.join("b/core/cores__arduino__main.cpp.o")).unwrap_or(t0), t0);
        let argv = upload_argv(&job, "/dev/ttyUSB0").unwrap();
        assert!(argv.iter().any(|a| a == "-patmega328p") && argv.iter().any(|a| a.ends_with("b/t.hex:i")), "{argv:?}");
    }

    #[test]
    fn version_ordering() {
        assert!(version_key("1.8.10") > version_key("1.8.9"));
        assert!(version_key("7.3.0-atmel3.6.1-arduino7") > version_key("7.3.0-atmel3.6.1-arduino5"));
    }
}
