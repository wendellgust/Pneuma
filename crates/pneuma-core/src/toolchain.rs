//! Installs the AVR platform + tools straight from Arduino's package index (needs `curl`, `tar`
//! and `sha256sum`/`shasum`; no arduino-cli involved).

use crate::{build::pneuma_home, Error, Result};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const INDEX_URL: &str = "https://downloads.arduino.cc/packages/package_index.json";

fn err<T>(m: impl Into<String>) -> Result<T> {
    Err(Error::Build(m.into()))
}

pub fn native_root() -> PathBuf {
    pneuma_home().join("native/packages/arduino")
}

/// Hosts to try, in order, for the `systems[].host` field of the index.
pub fn host_triplets(os: &str, arch: &str) -> Vec<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => vec!["x86_64-pc-linux-gnu", "x86_64-linux-gnu"],
        ("linux", "aarch64") => vec!["aarch64-linux-gnu"],
        ("macos", "x86_64") => vec!["x86_64-apple-darwin", "i386-apple-darwin11"],
        ("macos", "aarch64") => vec!["arm64-apple-darwin", "x86_64-apple-darwin", "i386-apple-darwin11"],
        _ => vec![],
    }
}

fn vkey(v: &str) -> Vec<u64> {
    v.split(|c: char| !c.is_ascii_digit()).filter_map(|p| p.parse().ok()).collect()
}

pub struct Download {
    pub url: String,
    pub file: String,
    pub sha256: String,
}

pub struct Plan {
    pub version: String,
    pub platform: Download,
    /// (tool name, version, download)
    pub tools: Vec<(String, String, Download)>,
}

fn dl(v: &Value) -> Option<Download> {
    Some(Download {
        url: v.get("url")?.as_str()?.to_string(),
        file: v.get("archiveFileName")?.as_str()?.to_string(),
        sha256: v.get("checksum")?.as_str()?.strip_prefix("SHA-256:")?.to_lowercase(),
    })
}

/// Latest `arduino:<arch>` platform and the download of each tool it depends on, for this host.
pub fn plan_from_index(index: &str, arch: &str, hosts: &[&str]) -> Result<Plan> {
    let root: Value = serde_json::from_str(index).map_err(|e| Error::Build(format!("package_index.json inválido: {e}")))?;
    let pkg = root["packages"].as_array().and_then(|a| a.iter().find(|p| p["name"] == "arduino")).ok_or_else(|| Error::Build("pacote `arduino` não está no índice".into()))?;
    let plat = pkg["platforms"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["architecture"] == arch)
        .max_by_key(|p| vkey(p["version"].as_str().unwrap_or("")))
        .ok_or_else(|| Error::Build(format!("plataforma `{arch}` não está no índice")))?;
    let platform = dl(plat).ok_or_else(|| Error::Build("entrada da plataforma incompleta no índice".into()))?;
    let mut tools = vec![];
    for dep in plat["toolsDependencies"].as_array().into_iter().flatten() {
        if dep["packager"] != "arduino" {
            continue;
        }
        let (name, ver) = (dep["name"].as_str().unwrap_or(""), dep["version"].as_str().unwrap_or(""));
        let tool = pkg["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|t| t["name"] == name && t["version"] == ver)
            .ok_or_else(|| Error::Build(format!("ferramenta {name} {ver} não está no índice")))?;
        let sys = hosts
            .iter()
            .find_map(|h| tool["systems"].as_array().into_iter().flatten().find(|s| s["host"] == *h))
            .ok_or_else(|| Error::Build(format!("{name} não tem build para este sistema ({hosts:?})")))?;
        tools.push((name.to_string(), ver.to_string(), dl(sys).ok_or_else(|| Error::Build(format!("entrada de {name} incompleta")))?));
    }
    Ok(Plan { version: plat["version"].as_str().unwrap_or("").to_string(), platform, tools })
}

fn run(mut c: Command, what: &str) -> Result<()> {
    let st = c.status().map_err(|e| Error::Build(format!("{what}: {e} (instalado?)")))?;
    if st.success() {
        Ok(())
    } else {
        err(format!("{what} falhou ({st})"))
    }
}

fn sha256_of(file: &Path) -> Result<String> {
    let (bin, args): (&str, &[&str]) = if cfg!(target_os = "macos") { ("shasum", &["-a", "256"]) } else { ("sha256sum", &[]) };
    let o = Command::new(bin).args(args).arg(file).output().map_err(|e| Error::Build(format!("{bin}: {e}")))?;
    Ok(String::from_utf8_lossy(&o.stdout).split_whitespace().next().unwrap_or("").to_lowercase())
}

/// Download (checksummed) and unpack one archive; its single top-level folder becomes `dest`.
fn fetch(d: &Download, dest: &Path, tmp: &Path) -> Result<()> {
    let file = tmp.join(&d.file);
    println!("baixando {} ...", d.file);
    let mut c = Command::new("curl");
    c.args(["-fL", "--retry", "3", "--progress-bar", "-o"]).arg(&file).arg(&d.url);
    run(c, "curl")?;
    let got = sha256_of(&file)?;
    if got != d.sha256 {
        let _ = fs::remove_file(&file);
        return err(format!("checksum de {} não confere (esperado {}, veio {got})", d.file, d.sha256));
    }
    let x = tmp.join(format!("x-{}", d.file));
    let _ = fs::remove_dir_all(&x);
    fs::create_dir_all(&x)?;
    let mut t = Command::new("tar");
    t.arg("xf").arg(&file).arg("-C").arg(&x);
    run(t, "tar (para .tar.bz2 precisa do bzip2 instalado)")?;
    let tops: Vec<PathBuf> = fs::read_dir(&x)?.flatten().map(|e| e.path()).collect();
    let top = match tops.as_slice() {
        [one] if one.is_dir() => one.clone(),
        _ => x.clone(),
    };
    let _ = fs::remove_dir_all(dest);
    if let Some(p) = dest.parent() {
        fs::create_dir_all(p)?;
    }
    fs::rename(&top, dest)?;
    let _ = fs::remove_dir_all(&x);
    let _ = fs::remove_file(&file);
    Ok(())
}

/// Install `arduino:avr` + avr-gcc + avrdude under `~/.pneuma/native`. Skips what is present.
pub fn install_avr() -> Result<()> {
    if let Some(tc) = crate::native::Toolchain::locate("avr") {
        println!("toolchain AVR já instalada em {} — nada a baixar", tc.platform.display());
        return Ok(());
    }
    let root = native_root();
    let tmp = pneuma_home().join("native/tmp");
    fs::create_dir_all(&tmp)?;
    let idx = tmp.join("package_index.json");
    println!("baixando o índice de pacotes do Arduino ...");
    let mut c = Command::new("curl");
    c.args(["-fsSL", "--retry", "3", "-o"]).arg(&idx).arg(INDEX_URL);
    run(c, "curl")?;
    let hosts = host_triplets(std::env::consts::OS, std::env::consts::ARCH);
    if hosts.is_empty() {
        return err(format!("sistema {}/{} não suportado pela instalação nativa", std::env::consts::OS, std::env::consts::ARCH));
    }
    let plan = plan_from_index(&fs::read_to_string(&idx)?, "avr", &hosts)?;
    let pdir = root.join("hardware/avr").join(&plan.version);
    if pdir.join("platform.txt").is_file() {
        println!("plataforma avr {} já instalada", plan.version);
    } else {
        fetch(&plan.platform, &pdir, &tmp)?;
    }
    for (name, ver, d) in &plan.tools {
        let tdir = root.join("tools").join(name).join(ver);
        if tdir.is_dir() && fs::read_dir(&tdir).is_ok_and(|mut r| r.next().is_some()) {
            println!("{name} {ver} já instalado");
        } else {
            fetch(d, &tdir, &tmp)?;
        }
    }
    let _ = fs::remove_dir_all(&tmp);
    println!("toolchain AVR pronta em {}", root.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const INDEX: &str = r#"{"packages":[{"name":"other","platforms":[],"tools":[]},{"name":"arduino",
      "platforms":[
        {"architecture":"avr","version":"1.8.6","url":"u1","archiveFileName":"avr-1.8.6.tar.bz2","checksum":"SHA-256:AA","toolsDependencies":[]},
        {"architecture":"avr","version":"1.8.10","url":"https://x/avr-1.8.10.tar.bz2","archiveFileName":"avr-1.8.10.tar.bz2","checksum":"SHA-256:ABCD",
         "toolsDependencies":[{"packager":"arduino","name":"avr-gcc","version":"7.3.0"},{"packager":"other","name":"zzz","version":"1"}]},
        {"architecture":"samd","version":"9.9.9","url":"s","archiveFileName":"s","checksum":"SHA-256:00","toolsDependencies":[]}],
      "tools":[{"name":"avr-gcc","version":"7.3.0","systems":[
        {"host":"i686-linux-gnu","url":"w","archiveFileName":"w","checksum":"SHA-256:11"},
        {"host":"x86_64-pc-linux-gnu","url":"https://x/gcc.tar.bz2","archiveFileName":"gcc.tar.bz2","checksum":"SHA-256:22"}]}]}]}"#;

    #[test]
    fn picks_latest_platform_and_host_tool() {
        let p = plan_from_index(INDEX, "avr", &host_triplets("linux", "x86_64")).unwrap();
        assert_eq!(p.version, "1.8.10");
        assert_eq!((p.platform.file.as_str(), p.platform.sha256.as_str()), ("avr-1.8.10.tar.bz2", "abcd"));
        assert_eq!(p.tools.len(), 1);
        assert_eq!((p.tools[0].0.as_str(), p.tools[0].2.url.as_str()), ("avr-gcc", "https://x/gcc.tar.bz2"));
        assert!(plan_from_index(INDEX, "avr", &["riscv-none"]).is_err());
        assert!(plan_from_index(INDEX, "nope", &["x86_64-pc-linux-gnu"]).is_err());
    }
}
