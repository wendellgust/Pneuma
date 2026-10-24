//! CMake + Ninja backend: for projects that bring their own `CMakeLists.txt`
//! (Pico SDK, FreeRTOS-Kernel, ESP-IDF components...). Pneuma only drives
//! `cmake -G Ninja` / `cmake --build` and uploads the resulting artifact.
//!
//! Enabled per env with `framework = "cmake"` (alias `pico-sdk`, `freertos`).

use crate::{boards::Family, config::Env, project::Project, Error, Result};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn err<T>(m: impl Into<String>) -> Result<T> {
    Err(Error::Build(m.into()))
}

fn verbose() -> bool {
    std::env::var_os("PNEUMA_VERBOSE").is_some()
}

/// Per-env options parsed from `pneuma.lua`.
#[derive(Debug, Clone, Default)]
pub struct CmakeOpts {
    /// Directory holding `CMakeLists.txt`, relative to the project root.
    pub source_dir: String,
    /// Extra `-D...` / cmake arguments.
    pub args: Vec<String>,
    /// Build only this target (default: all).
    pub target: Option<String>,
    /// `PICO_SDK_PATH`, `FREERTOS_KERNEL_PATH`... (exported for cmake).
    pub env_vars: Vec<(String, String)>,
    /// Custom upload command; `{port} {elf} {uf2} {bin} {hex}` are replaced. Default: picotool (RP2040).
    pub upload_cmd: Vec<String>,
}

pub fn is_cmake_framework(f: Option<&str>) -> bool {
    matches!(f, Some("cmake" | "pico-sdk" | "freertos"))
}

#[derive(Debug, Clone)]
pub struct CmakeJob {
    pub source_dir: PathBuf,
    pub build_dir: PathBuf,
    pub args: Vec<String>,
    pub target: Option<String>,
    pub env_vars: Vec<(String, String)>,
    pub upload_cmd: Vec<String>,
    pub family: Family,
}

/// `None` when the env does not use cmake.
pub fn job(p: &Project, env: &Env) -> Result<Option<CmakeJob>> {
    if !is_cmake_framework(env.framework.as_deref()) {
        return Ok(None);
    }
    let o = env.cmake.clone().unwrap_or_default();
    let source_dir = p.root.join(if o.source_dir.is_empty() { "." } else { &o.source_dir });
    if !source_dir.join("CMakeLists.txt").is_file() {
        return err(format!("env `{}`: {} não tem CMakeLists.txt (use cmake_dir = \"...\")", env.name, source_dir.display()));
    }
    let mut args = o.args;
    let has = |a: &[String], k: &str| a.iter().any(|x| x.starts_with(k));
    if env.board.family == Family::Rp2040 && !has(&args, "-DPICO_BOARD") {
        args.push(format!("-DPICO_BOARD={}", if env.board.id == "rpipicow" { "pico_w" } else { "pico" }));
    }
    if !has(&args, "-DCMAKE_EXPORT_COMPILE_COMMANDS") {
        args.push("-DCMAKE_EXPORT_COMPILE_COMMANDS=ON".into());
    }
    if !env.build_flags.is_empty() {
        let f = env.build_flags.join(" ");
        args.push(format!("-DCMAKE_C_FLAGS={f}"));
        args.push(format!("-DCMAKE_CXX_FLAGS={f}"));
    }
    // SDKs cloned into ~/.pneuma/sdk/<name> are picked up when nothing else sets the variable.
    let mut env_vars = o.env_vars;
    for (var, dir) in [("PICO_SDK_PATH", "pico-sdk"), ("FREERTOS_KERNEL_PATH", "FreeRTOS-Kernel")] {
        let default = crate::build::pneuma_home().join("sdk").join(dir);
        if !env_vars.iter().any(|(k, _)| k == var) && std::env::var_os(var).is_none() && default.is_dir() {
            env_vars.push((var.into(), default.display().to_string()));
        }
    }
    Ok(Some(CmakeJob {
        source_dir,
        build_dir: p.root.join(".pneuma").join("build").join(&env.name),
        args,
        target: o.target,
        env_vars,
        upload_cmd: o.upload_cmd,
        family: env.board.family,
    }))
}

fn have(tool: &str) -> bool {
    Command::new(tool).arg("--version").output().is_ok()
}

pub fn configure_argv(j: &CmakeJob) -> Vec<String> {
    let mut v = vec!["-S".into(), j.source_dir.display().to_string(), "-B".into(), j.build_dir.display().to_string()];
    // Ninja when available; an existing build dir keeps the generator it was created with.
    if !j.build_dir.join("CMakeCache.txt").exists() && have("ninja") {
        v.push("-G".into());
        v.push("Ninja".into());
    }
    v.extend(j.args.iter().cloned());
    v
}

pub fn build_argv(j: &CmakeJob) -> Vec<String> {
    let mut v = vec!["--build".into(), j.build_dir.display().to_string(), "--parallel".into()];
    if let Some(t) = &j.target {
        v.push("--target".into());
        v.push(t.clone());
    }
    v
}

fn run(j: &CmakeJob, args: &[String], what: &str) -> Result<()> {
    if verbose() {
        println!("cmake {}", args.join(" "));
    }
    let mut c = Command::new("cmake");
    c.args(args);
    for (k, v) in &j.env_vars {
        c.env(k, v);
    }
    match c.status() {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => err(format!("{what} falhou ({s})")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => err("cmake não encontrado: instale cmake (e ninja) e o toolchain do projeto (ex.: arm-none-eabi-gcc, Pico SDK via PICO_SDK_PATH)"),
        Err(e) => err(format!("cmake: {e}")),
    }
}

/// Fails early, with the fix, when an SDK variable the CMakeLists reads is missing or wrong.
fn preflight(j: &CmakeJob) -> Result<()> {
    let text = fs::read_to_string(j.source_dir.join("CMakeLists.txt")).unwrap_or_default();
    for (var, what, file) in [
        ("PICO_SDK_PATH", "Pico SDK (git clone --recurse-submodules https://github.com/raspberrypi/pico-sdk)", "external/pico_sdk_import.cmake"),
        ("FREERTOS_KERNEL_PATH", "FreeRTOS-Kernel (git clone https://github.com/FreeRTOS/FreeRTOS-Kernel)", "tasks.c"),
    ] {
        if !text.contains(var) {
            continue;
        }
        let val = j.env_vars.iter().find(|(k, _)| k == var).map(|(_, v)| v.clone()).or_else(|| std::env::var(var).ok());
        match val {
            None => return err(format!("{var} não definido. Instale o {what} e exporte {var}=... ou defina `{}` no env de pneuma.lua", if var == "PICO_SDK_PATH" { "pico_sdk" } else { "freertos" })),
            Some(v) if !Path::new(&v).join(file).is_file() => return err(format!("{var}={v} não parece válido (falta {file})")),
            _ => {}
        }
    }
    if text.contains("pico_sdk_init") && !have("arm-none-eabi-gcc") {
        return err("arm-none-eabi-gcc não encontrado no PATH (Arch: sudo pacman -S arm-none-eabi-gcc arm-none-eabi-newlib)");
    }
    Ok(())
}

pub fn compile(j: &CmakeJob) -> Result<()> {
    preflight(j)?;
    fs::create_dir_all(&j.build_dir)?;
    run(j, &configure_argv(j), "cmake (configuração)")?;
    run(j, &build_argv(j), "compilação")
}

/// Newest file with extension `ext` directly in the build dir (the final image, not CMake internals).
fn artifact(dir: &Path, ext: &str) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == ext))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .max()
        .map(|(_, p)| p)
}

pub fn expand_upload(cmd: &[String], dir: &Path, port: &str) -> Result<Vec<String>> {
    let mut out = vec![];
    for a in cmd {
        let mut s = a.replace("{port}", port);
        for ext in ["elf", "uf2", "bin", "hex"] {
            let key = format!("{{{ext}}}");
            if s.contains(&key) {
                let f = artifact(dir, ext).ok_or_else(|| Error::Build(format!("nenhum .{ext} em {} (o build gera?)", dir.display())))?;
                s = s.replace(&key, &f.display().to_string());
            }
        }
        out.push(s);
    }
    Ok(out)
}

fn picotool(build_dir: &Path) -> Option<PathBuf> {
    if have("picotool") {
        return Some("picotool".into());
    }
    // the SDK-built copy often lacks USB support (no `load`): only use it if it knows the command
    Some(build_dir.join("_deps/picotool/picotool")).filter(|p| p.is_file() && Command::new(p).args(["help", "load"]).output().is_ok_and(|o| o.status.success()))
}

/// Mounted BOOTSEL drive (`RPI-RP2`/`RP2350`), if any. On Linux, mounts the drive through
/// `udisksctl` when it showed up as a block device but the desktop did not auto-mount it.
fn bootsel_mount() -> Option<PathBuf> {
    if let Some(m) = find_mount() {
        return Some(m);
    }
    for label in ["RPI-RP2", "RP2350"] {
        let dev = Path::new("/dev/disk/by-label").join(label);
        if dev.exists() {
            let _ = Command::new("udisksctl").args(["mount", "-b"]).arg(&dev).output();
            if let Some(m) = find_mount() {
                return Some(m);
            }
        }
    }
    None
}

fn find_mount() -> Option<PathBuf> {
    let user = std::env::var("USER").unwrap_or_default();
    for base in [format!("/run/media/{user}"), format!("/media/{user}"), "/media".into(), "/Volumes".into(), "/mnt".into()] {
        for name in ["RPI-RP2", "RP2350"] {
            let p = Path::new(&base).join(name);
            if p.join("INFO_UF2.TXT").is_file() {
                return Some(p);
            }
        }
    }
    None
}

pub fn upload(j: &CmakeJob, port: &str) -> Result<()> {
    let argv = if !j.upload_cmd.is_empty() {
        expand_upload(&j.upload_cmd, &j.build_dir, port)?
    } else if j.family == Family::Rp2040 {
        let uf2 = artifact(&j.build_dir, "uf2").ok_or_else(|| Error::Build(format!("nenhum .uf2 em {} (chame pico_add_extra_outputs no CMakeLists)", j.build_dir.display())))?;
        // 1) picotool (PATH, or the one the Pico SDK built into the build dir); -f reboots a running app into BOOTSEL
        if let Some(tool) = picotool(&j.build_dir) {
            let st = Command::new(&tool).args(["load", "-f", "-x"]).arg(&uf2).status();
            if st.is_ok_and(|s| s.success()) {
                return Ok(());
            }
            println!("picotool falhou (permissão USB/udev?); tentando copiar o .uf2 pelo drive BOOTSEL...");
        }
        // 2) BOOTSEL drive: already mounted, or reboot a running app into it with the 1200-baud touch
        let mut mount = bootsel_mount();
        if mount.is_none() && !port.is_empty() {
            println!("reiniciando {port} em BOOTSEL (1200 baud)...");
            let _ = serialport::new(port, 1200).open();
            for _ in 0..100 {
                std::thread::sleep(std::time::Duration::from_millis(200));
                if let Some(m) = bootsel_mount() {
                    mount = Some(m);
                    break;
                }
            }
        }
        let Some(m) = mount else {
            return err("não achei o drive RPI-RP2. Segure BOOTSEL ao ligar a placa e monte o drive (udisksctl mount -b /dev/sdX1), ou instale picotool com regra udev");
        };
        fs::copy(&uf2, m.join(uf2.file_name().unwrap_or_default()))?;
        println!("copiado {} -> {}", uf2.display(), m.display());
        return Ok(());
    } else {
        return err("este env não tem upload_cmd (ex.: upload_cmd = {\"esptool.py\", \"-p\", \"{port}\", \"write_flash\", \"0x0\", \"{bin}\"})");
    };
    if verbose() {
        println!("{}", argv.join(" "));
    }
    let mut c = Command::new(&argv[0]);
    c.args(&argv[1..]);
    for (k, v) in &j.env_vars {
        c.env(k, v);
    }
    let st = c.status().map_err(|e| Error::Build(format!("{}: {e}", argv[0])))?;
    if st.success() {
        Ok(())
    } else {
        err(format!("upload falhou ({st})"))
    }
}

/// Names a CMakeLists links or pulls in (`target_link_libraries`, `add_subdirectory`), with the
/// folder that provides them: SDK dirs from the job's variables, or the project's own folders.
pub fn linked_libs(j: &CmakeJob) -> Vec<(String, PathBuf)> {
    let text = fs::read_to_string(j.source_dir.join("CMakeLists.txt")).unwrap_or_default();
    let var = |k: &str| j.env_vars.iter().find(|(n, _)| n == k).map(|(_, v)| PathBuf::from(v));
    let mut out: Vec<(String, PathBuf)> = vec![];
    let mut add = |name: &str, path: PathBuf| {
        if !out.iter().any(|(n, _)| n == name) {
            out.push((name.to_string(), path));
        }
    };
    for call in calls(&text, "target_link_libraries") {
        for t in call.split_whitespace().skip(1) {
            if matches!(t, "PRIVATE" | "PUBLIC" | "INTERFACE") || t.contains('$') {
                continue;
            }
            let home = if t.starts_with("FreeRTOS") {
                var("FREERTOS_KERNEL_PATH")
            } else if t.starts_with("pico_") || t.starts_with("hardware_") || t.starts_with("tinyusb") || t.starts_with("cyw43") {
                var("PICO_SDK_PATH")
            } else {
                None
            };
            add(t, home.unwrap_or_else(|| j.source_dir.clone()));
        }
    }
    for call in calls(&text, "add_subdirectory") {
        if let Some(d) = call.split_whitespace().next().filter(|d| !d.contains('$')) {
            add(d, j.source_dir.join(d));
        }
    }
    out
}

/// Argument text of every `name( ... )` call (comments ignored).
fn calls(text: &str, name: &str) -> Vec<String> {
    let clean: String = text.lines().map(|l| l.split('#').next().unwrap_or("")).collect::<Vec<_>>().join("\n");
    let mut out = vec![];
    let mut rest = clean.as_str();
    while let Some(i) = rest.find(&format!("{name}(")).or_else(|| rest.find(&format!("{name} ("))) {
        let after = &rest[i + name.len()..];
        let Some(open) = after.find('(') else { break };
        let Some(close) = after.find(')') else { break };
        out.push(after[open + 1..close].to_string());
        rest = &after[close..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(n: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pneuma_cmake_{n}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn framework_names() {
        assert!(is_cmake_framework(Some("cmake")) && is_cmake_framework(Some("pico-sdk")) && is_cmake_framework(Some("freertos")));
        assert!(!is_cmake_framework(Some("arduino")) && !is_cmake_framework(None));
    }

    #[test]
    fn argv_shapes() {
        let j = CmakeJob {
            source_dir: "/p".into(),
            build_dir: "/p/.pneuma/build/pico".into(),
            args: vec!["-DPICO_BOARD=pico".into()],
            target: Some("app".into()),
            env_vars: vec![],
            upload_cmd: vec![],
            family: Family::Rp2040,
        };
        let c = configure_argv(&j);
        assert_eq!(&c[..4], ["-S", "/p", "-B", "/p/.pneuma/build/pico"]);
        assert_eq!(c.last().unwrap(), "-DPICO_BOARD=pico");
        assert_eq!(build_argv(&j), ["--build", "/p/.pneuma/build/pico", "--parallel", "--target", "app"]);
    }

    #[test]
    fn upload_placeholders() {
        let d = tmp("up");
        fs::write(d.join("app.uf2"), "").unwrap();
        fs::write(d.join("app.elf"), "").unwrap();
        let v = expand_upload(&["tool".into(), "-p".into(), "{port}".into(), "{elf}".into(), "{uf2}".into()], &d, "/dev/ttyACM0").unwrap();
        assert_eq!(v[2], "/dev/ttyACM0");
        assert!(v[3].ends_with("app.elf") && v[4].ends_with("app.uf2"));
        assert!(expand_upload(&["{hex}".into()], &d, "").is_err());
    }

    #[test]
    fn linked_libs_from_cmakelists() {
        let d = tmp("ll");
        fs::write(d.join("CMakeLists.txt"), "add_executable(app a.c)\ntarget_link_libraries(app pico_stdlib\n  FreeRTOS-Kernel # rtos\n  FreeRTOS-Kernel-Heap4 ${X})\nadd_subdirectory(lib/foo)\n").unwrap();
        let j = CmakeJob {
            source_dir: d.clone(),
            build_dir: d.join("b"),
            args: vec![],
            target: None,
            env_vars: vec![("FREERTOS_KERNEL_PATH".into(), "/fr".into()), ("PICO_SDK_PATH".into(), "/sdk".into())],
            upload_cmd: vec![],
            family: Family::Rp2040,
        };
        let l = linked_libs(&j);
        let names: Vec<&str> = l.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["pico_stdlib", "FreeRTOS-Kernel", "FreeRTOS-Kernel-Heap4", "lib/foo"]);
        assert_eq!(l[0].1, PathBuf::from("/sdk"));
        assert_eq!(l[1].1, PathBuf::from("/fr"));
        assert_eq!(l[3].1, d.join("lib/foo"));
    }
}
