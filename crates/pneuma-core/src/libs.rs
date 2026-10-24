//! Libraries are plain folders under `<project>/libs/`. Every path is resolved
//! from the project root (never the cwd) and stored absolute + canonical, so
//! include paths cannot drift depending on where `pneuma` was launched.

use crate::Result;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

const SRC_EXT: &[&str] = &["c", "cc", "cpp", "cxx", "s"];
const HDR_EXT: &[&str] = &["h", "hh", "hpp", "hxx"];
/// Folders that never contribute to a build.
pub(crate) const SKIP_DIRS: &[&str] = &["examples", "example", "test", "tests", "extras", "docs", "doc", "keywords", ".git"];

#[derive(Debug, Clone)]
pub struct Lib {
    pub name: String,
    /// Absolute, canonical.
    pub path: PathBuf,
    /// Absolute include directories (`-I`).
    pub include_dirs: Vec<PathBuf>,
    pub sources: Vec<PathBuf>,
    pub headers: Vec<PathBuf>,
}

#[derive(Debug, Default)]
pub struct Scan {
    pub root: PathBuf,
    pub libs_dir: PathBuf,
    pub libs: Vec<Lib>,
    /// Libraries a CMake env links (SDK, FreeRTOS...): listed for the user, never staged.
    pub external: Vec<Lib>,
    pub warnings: Vec<String>,
}

fn ext_in(p: &Path, set: &[&str]) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| set.contains(&e.to_ascii_lowercase().as_str()))
}

fn walk(dir: &Path, out_src: &mut Vec<PathBuf>, out_hdr: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_ascii_lowercase();
        if p.is_dir() {
            if !SKIP_DIRS.contains(&name.as_str()) && !name.starts_with('.') {
                walk(&p, out_src, out_hdr);
            }
        } else if ext_in(&p, SRC_EXT) {
            out_src.push(p);
        } else if ext_in(&p, HDR_EXT) {
            out_hdr.push(p);
        }
    }
}

/// Layout detection: `include/`+`src/`, else `src/`, else the folder itself.
pub fn load_lib(path: &Path) -> Result<Lib> {
    let path = fs::canonicalize(path)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let (inc, src) = (path.join("include"), path.join("src"));
    let (include_dirs, code_root) = match (inc.is_dir(), src.is_dir()) {
        (true, true) => (vec![inc, src.clone()], src),
        (true, false) => (vec![inc.clone()], path.clone()),
        (false, true) => (vec![src.clone()], src),
        (false, false) => (vec![path.clone()], path.clone()),
    };
    let (mut sources, mut headers) = (vec![], vec![]);
    walk(&code_root, &mut sources, &mut headers);
    if include_dirs.contains(&path.join("include")) {
        walk(&path.join("include"), &mut vec![], &mut headers);
    }
    headers.sort();
    headers.dedup();
    Ok(Lib { name, path, include_dirs, sources, headers })
}

/// Scan `<root>/<libs_dir>` plus explicit `extra` folders (relative to `root`).
pub fn scan(root: &Path, libs_dir: &str, extra: &[String]) -> Result<Scan> {
    let root = fs::canonicalize(root)?;
    let dir = root.join(libs_dir);
    let mut s = Scan { root: root.clone(), libs_dir: dir.clone(), ..Default::default() };
    let mut paths: Vec<PathBuf> = vec![];
    match fs::read_dir(&dir) {
        Ok(rd) => paths.extend(rd.flatten().map(|e| e.path()).filter(|p| {
            p.is_dir() && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'))
        })),
        Err(_) => s.warnings.push(format!("pasta de bibliotecas não existe: {}", dir.display())),
    }
    for e in extra {
        let p = root.join(e);
        if p.is_dir() {
            paths.push(p);
        } else {
            s.warnings.push(format!("biblioteca extra não encontrada: {}", p.display()));
        }
    }
    paths.sort();
    for p in paths {
        let lib = load_lib(&p)?;
        if lib.name.contains(char::is_whitespace) {
            s.warnings.push(format!("`{}`: espaços no nome da pasta quebram -I em vários toolchains; renomeie", lib.name));
        }
        if lib.sources.is_empty() && lib.headers.is_empty() {
            s.warnings.push(format!("`{}`: nenhum .h/.c/.cpp encontrado em {}", lib.name, lib.path.display()));
        }
        s.libs.push(lib);
    }
    // Same header relative to its include dir in two libs => silent wrong-include bugs.
    let mut seen: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for l in &s.libs {
        for h in &l.headers {
            let rel = l
                .include_dirs
                .iter()
                .find_map(|d| h.strip_prefix(d).ok())
                .map(|r| r.to_string_lossy().into_owned())
                .unwrap_or_else(|| h.file_name().unwrap().to_string_lossy().into_owned());
            seen.entry(rel).or_default().push(&l.name);
        }
    }
    for (h, owners) in seen.iter().filter(|(_, o)| o.len() > 1) {
        s.warnings.push(format!("header `{h}` existe em {}: o include pode pegar a errada", owners.join(", ")));
    }
    Ok(s)
}

/// `scan` plus, for `framework = "cmake"` envs, the libraries their CMakeLists links.
pub fn scan_project(p: &crate::project::Project) -> Result<Scan> {
    let mut s = scan(&p.root, &p.settings.libs_dir, &p.settings.extra_libs)?;
    for (_, e) in &p.envs {
        let Ok(e) = e else { continue };
        let Ok(Some(job)) = crate::cmake::job(p, e) else { continue };
        for (name, path) in crate::cmake::linked_libs(&job) {
            if s.external.iter().all(|l| l.name != name) {
                let inc = path.join("include");
                s.external.push(Lib { name, include_dirs: if inc.is_dir() { vec![inc] } else { vec![] }, path, sources: vec![], headers: vec![] });
            }
        }
    }
    if !s.external.is_empty() {
        s.warnings.retain(|w| !w.starts_with("pasta de bibliotecas não existe"));
    }
    Ok(s)
}

impl Scan {
    fn rel(&self, p: &Path) -> String {
        p.strip_prefix(&self.root).unwrap_or(p).display().to_string()
    }

    /// `-I` flags (absolute) for the given libs (all if `only` is empty).
    pub fn include_flags(&self, only: &[String]) -> Vec<String> {
        self.libs
            .iter()
            .filter(|l| only.is_empty() || only.contains(&l.name))
            .flat_map(|l| l.include_dirs.iter().map(|d| format!("-I{}", d.display())))
            .collect()
    }

    /// Deterministic (no timestamps) so it diffs cleanly in git.
    pub fn to_markdown(&self) -> String {
        let mut o = String::from("# Bibliotecas\n\n");
        o.push_str("> Gerado por `pneuma libs`. Não edite à mão — adicione/remova pastas em `");
        o.push_str(&self.rel(&self.libs_dir));
        o.push_str("/` e rode de novo.\n\n");
        o.push_str(&format!("- Projeto: `{}`\n- Pasta de bibliotecas: `{}`\n- Total: {}\n\n", self.root.display(), self.libs_dir.display(), self.libs.len()));
        if !self.libs.is_empty() {
            o.push_str("| Biblioteca | Caminho (relativo) | Include dirs | Fontes | Headers |\n|---|---|---|---:|---:|\n");
            for l in &self.libs {
                let inc: Vec<String> = l.include_dirs.iter().map(|d| format!("`{}`", self.rel(d))).collect();
                o.push_str(&format!("| {} | `{}` | {} | {} | {} |\n", l.name, self.rel(&l.path), inc.join("<br>"), l.sources.len(), l.headers.len()));
            }
            o.push('\n');
        }
        for l in &self.libs {
            o.push_str(&format!("## {}\n\n- Caminho absoluto: `{}`\n", l.name, l.path.display()));
            for d in &l.include_dirs {
                o.push_str(&format!("- Include: `{}`\n", d.display()));
            }
            if !l.headers.is_empty() {
                let hs: Vec<String> = l.headers.iter().take(8).map(|h| format!("`{}`", h.file_name().unwrap().to_string_lossy())).collect();
                let more = l.headers.len().saturating_sub(8);
                o.push_str(&format!("- Headers: {}{}\n", hs.join(", "), if more > 0 { format!(" (+{more})") } else { String::new() }));
            }
            o.push_str("- Uso no código: `#include <...>` relativo a um dos Include acima\n\n");
        }
        if !self.external.is_empty() {
            o.push_str("## Via CMake\n\n| Biblioteca | Origem |\n|---|---|\n");
            for l in &self.external {
                o.push_str(&format!("| {} | `{}` |\n", l.name, l.path.display()));
            }
            o.push('\n');
        }
        o.push_str("## Avisos\n\n");
        if self.warnings.is_empty() {
            o.push_str("Nenhum.\n");
        } else {
            for w in &self.warnings {
                o.push_str(&format!("- ⚠ {w}\n"));
            }
        }
        o
    }

    pub fn write_markdown(&self) -> Result<PathBuf> {
        let p = self.root.join("LIBS.md");
        fs::write(&p, self.to_markdown())?;
        Ok(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pneuma_libs_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }
    fn touch(p: PathBuf) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, "").unwrap();
    }

    #[test]
    fn detects_layouts_and_absolute_paths() {
        let r = tmp("layouts");
        touch(r.join("libs/flat/flat.h"));
        touch(r.join("libs/flat/flat.cpp"));
        touch(r.join("libs/srcd/src/a.cpp"));
        touch(r.join("libs/srcd/src/a.h"));
        touch(r.join("libs/srcd/examples/demo.cpp"));
        touch(r.join("libs/both/include/b.h"));
        touch(r.join("libs/both/src/b.c"));
        let s = scan(&r, "libs", &[]).unwrap();
        let names: Vec<_> = s.libs.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["both", "flat", "srcd"]);
        let get = |n: &str| s.libs.iter().find(|l| l.name == n).unwrap();
        assert_eq!(get("srcd").sources.len(), 1, "examples/ ignored");
        assert_eq!(get("both").include_dirs.len(), 2);
        assert_eq!(get("both").headers.len(), 1);
        assert_eq!(get("flat").include_dirs, [get("flat").path.clone()]);
        assert!(s.libs.iter().all(|l| l.path.is_absolute()));
        assert!(s.include_flags(&[]).iter().all(|f| f.starts_with("-I/")));
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
    }

    #[test]
    fn warns_duplicates_empty_and_missing_dir() {
        let r = tmp("warn");
        touch(r.join("libs/a/util.h"));
        touch(r.join("libs/b/util.h"));
        fs::create_dir_all(r.join("libs/empty")).unwrap();
        let s = scan(&r, "libs", &["nope".into()]).unwrap();
        let w = s.warnings.join("\n");
        assert!(w.contains("util.h") && w.contains("a, b"));
        assert!(w.contains("`empty`") && w.contains("nope"));
        let md = s.to_markdown();
        assert!(md.contains("| a |") && md.contains("## Avisos") && md.contains("⚠"));
        let r2 = tmp("nolibs");
        assert!(scan(&r2, "libs", &[]).unwrap().warnings[0].contains("não existe"));
    }
}
