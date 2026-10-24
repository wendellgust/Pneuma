//! Unified project loading: `pneuma.lua` (preferred) or legacy `platformio.ini`.

use crate::{
    boards::{self, Family},
    config::{Config, Env},
    Error, Result,
};
use mlua::{Lua, Table, Value};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Settings {
    pub default_env: Option<String>,
    pub libs_dir: String,
    pub src_dir: String,
    pub monitor_speed: Option<u32>,
    pub upload_port: Option<String>,
    pub extra_libs: Vec<String>,
    pub monitor_timestamps: bool,
    pub monitor_filters: Vec<String>,
    pub log_dir: String,
    pub log_timestamps: bool,
    pub autosave: bool,
    pub upload_speed: Option<u32>,
    /// `auto` (native for AVR), `native` or `arduino-cli`.
    pub backend: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            default_env: None,
            libs_dir: "libs".into(),
            src_dir: "src".into(),
            monitor_speed: None,
            upload_port: None,
            extra_libs: vec![],
            monitor_timestamps: false,
            monitor_filters: vec!["colorize".into(), "esp32_exception_decoder".into()],
            log_dir: "logs".into(),
            log_timestamps: true,
            autosave: false,
            upload_speed: None,
            backend: None,
        }
    }
}

#[derive(Debug)]
pub struct Project {
    pub root: PathBuf,
    pub source: PathBuf,
    pub settings: Settings,
    pub envs: Vec<(String, std::result::Result<Env, String>)>,
}

impl Project {
    /// `path` may be a project dir, a `pneuma.lua` or a `platformio.ini`.
    pub fn load(path: &Path) -> Result<Project> {
        let (root, source) = if path.is_dir() {
            // pneuma.lua, then the legacy bmu.lua, then platformio.ini
            let found = ["pneuma.lua", "bmu.lua", "platformio.ini"].map(|n| path.join(n)).into_iter().find(|f| f.exists());
            (path.to_path_buf(), found.unwrap_or_else(|| path.join("platformio.ini")))
        } else {
            (path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_path_buf(), path.to_path_buf())
        };
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        if source.extension().is_some_and(|e| e == "lua") {
            load_lua(root, source)
        } else {
            let cfg = Config::load(&source)?;
            let envs = cfg.env_names().into_iter().map(|n| (n.to_string(), cfg.resolve(n).map_err(|e| e.to_string()))).collect();
            Ok(Project { root, source, settings: Settings::default(), envs })
        }
    }
}

fn lua_err(e: mlua::Error) -> Error {
    Error::Config { line: 0, msg: format!("lua: {e}") }
}

fn strings(t: &Table, key: &str) -> Vec<String> {
    t.get::<Table>(key)
        .map(|t| t.sequence_values::<String>().flatten().collect())
        .unwrap_or_default()
}

fn load_lua(root: PathBuf, source: PathBuf) -> Result<Project> {
    let lua = Lua::new();
    // `pneuma` global: tiny read-only context for conditionals in the config.
    let ctx = lua.create_table().map_err(lua_err)?;
    ctx.set("os", std::env::consts::OS).map_err(lua_err)?;
    ctx.set("project_dir", root.display().to_string()).map_err(lua_err)?;
    lua.globals().set("bmu", ctx.clone()).map_err(lua_err)?; // legacy name
    lua.globals().set("pneuma", ctx).map_err(lua_err)?;

    let code = std::fs::read_to_string(&source)?;
    let name = format!("@{}", source.display());
    let cfg: Value = lua.load(&code).set_name(name).eval().map_err(lua_err)?;
    let Value::Table(cfg) = cfg else {
        return Err(Error::Config { line: 0, msg: "pneuma.lua deve terminar com `return { ... }`".into() });
    };

    let mut st = Settings::default();
    st.default_env = cfg.get("default_env").ok();
    if let Ok(d) = cfg.get::<String>("libs_dir") {
        st.libs_dir = d;
    }
    if let Ok(d) = cfg.get::<String>("src_dir") {
        st.src_dir = d;
    }
    st.extra_libs = strings(&cfg, "libs");
    st.backend = cfg.get::<String>("backend").ok();
    if let Ok(m) = cfg.get::<Table>("monitor") {
        st.monitor_speed = m.get("speed").ok();
        st.monitor_timestamps = m.get("timestamps").unwrap_or(false);
        if m.contains_key("filters").unwrap_or(false) {
            st.monitor_filters = strings(&m, "filters");
        }
        if let Ok(d) = m.get::<String>("log_dir") {
            st.log_dir = d;
        }
        st.log_timestamps = m.get("log_timestamps").unwrap_or(true);
        st.autosave = m.get("autosave").unwrap_or(false);
    }
    let global_port: Option<String> = match cfg.get::<Table>("upload") {
        Ok(u) => {
            st.upload_speed = u.get("speed").ok();
            u.get("port").ok().filter(|p: &String| p != "auto")
        }
        Err(_) => None,
    };
    st.upload_port = global_port.clone();
    let global_monitor: u32 = cfg.get::<Table>("monitor").ok().and_then(|m| m.get("speed").ok()).unwrap_or(9600);
    let global_flags = strings(&cfg, "build_flags");

    let mut envs = vec![];
    if let Ok(t) = cfg.get::<Table>("env") {
        let mut named: Vec<(String, Table)> = t.pairs::<String, Table>().flatten().collect();
        named.sort_by(|a, b| a.0.cmp(&b.0));
        for (n, e) in named {
            let r = (|| -> std::result::Result<Env, String> {
                let board_id: String = e.get("board").map_err(|_| format!("env `{n}`: falta `board`"))?;
                let board = boards::find(&board_id).ok_or(format!("env `{n}`: placa desconhecida `{board_id}`"))?;
                let platform = e.get::<String>("platform").unwrap_or_else(|_| default_platform(board.family).into());
                let mut flags = global_flags.clone();
                flags.extend(strings(&e, "build_flags"));
                Ok(Env {
                    name: n.clone(),
                    platform,
                    board,
                    framework: e.get("framework").ok(),
                    upload_port: e.get("upload_port").ok().or(global_port.clone()),
                    monitor_speed: e.get("monitor_speed").unwrap_or(global_monitor),
                    build_flags: flags,
                    lib_deps: strings(&e, "libs"),
                    cmake: cmake_opts(&e),
                })
            })();
            envs.push((n, r));
        }
    }
    Ok(Project { root, source, settings: st, envs })
}

/// `cmake_dir`, `cmake_args`, `cmake_target`, `pico_sdk`, `freertos`, `env_vars`, `upload_cmd` of an env.
fn cmake_opts(e: &Table) -> Option<crate::cmake::CmakeOpts> {
    let mut o = crate::cmake::CmakeOpts { source_dir: e.get("cmake_dir").unwrap_or_default(), args: strings(e, "cmake_args"), target: e.get("cmake_target").ok(), upload_cmd: strings(e, "upload_cmd"), ..Default::default() };
    for (key, var) in [("pico_sdk", "PICO_SDK_PATH"), ("freertos", "FREERTOS_KERNEL_PATH")] {
        if let Ok(v) = e.get::<String>(key) {
            o.env_vars.push((var.into(), v));
        }
    }
    if let Ok(t) = e.get::<Table>("env_vars") {
        o.env_vars.extend(t.pairs::<String, String>().flatten());
    }
    Some(o)
}

pub fn default_platform(f: Family) -> &'static str {
    match f {
        Family::Esp32 => "espressif32",
        Family::Esp8266 => "espressif8266",
        Family::Rp2040 => "raspberrypi",
        Family::Avr => "atmelavr",
    }
}

pub const TEMPLATE: &str = r#"-- pneuma.lua — configuração do projeto. É Lua de verdade: pode usar if, funções, variáveis.
-- Global `pneuma`: pneuma.os ("linux"/"macos"/"windows"), pneuma.project_dir.

return {
  -- Ambiente usado por `pneuma build/upload` quando nenhum é dado.
  default_env = "uno",

  -- Quem compila: "auto" (AVR usa o backend nativo, sem arduino-cli; o resto usa arduino-cli),
  -- "native" ou "arduino-cli". Também: PNEUMA_BACKEND=arduino-cli.
  -- backend = "auto",

  -- Bibliotecas são PASTAS dentro deste diretório (relativo a este arquivo,
  -- nunca ao diretório atual). Rode `pneuma libs` para gerar o LIBS.md.
  libs_dir = "libs",
  src_dir  = "src",   -- código do projeto (main.cpp ou <projeto>.ino)
  -- Pastas extras fora de libs_dir (relativas a este arquivo):
  libs = { -- "../compartilhado/minha_lib",
  },

  -- Flags aplicadas a todos os ambientes.
  build_flags = { "-Wall" },

  upload = {
    port  = "auto",     -- "auto" detecta a placa; ou "/dev/ttyUSB0"
    -- speed = 921600,
  },

  monitor = {
    speed      = 115200,
    timestamps = true,  -- prefixa cada linha com a hora
    -- colorize: cor por nível (E/W/I/D). esp32_exception_decoder: traduz crash em função/arquivo:linha
    -- (usa o .elf da última compilação). {} desliga tudo; `-f none` na linha de comando também.
    filters    = { "colorize", "esp32_exception_decoder" },
    -- Ctrl+G no monitor salva tudo desde que ele abriu em <log_dir>/<env>-<data>.log
    log_dir        = "logs",
    log_timestamps = true,   -- [ss.mmm] no começo de cada linha do arquivo
    autosave       = false,  -- true: grava continuamente desde o início (como --log)
  },

  -- `platform` é opcional: deduzida da placa (esp32→espressif32, pico→raspberrypi, uno→atmelavr).
  env = {
    esp32 = { board = "esp32dev",  framework = "arduino" },
    pico  = { board = "pico",      framework = "arduino" },
    uno   = { board = "uno",       framework = "arduino",
              build_flags = { "-Os" } },
  },
}
"#;

pub const SAMPLE_MAIN: &str = r#"#include <Arduino.h>

#ifndef LED_BUILTIN
#define LED_BUILTIN 2  // generic ESP32 boards do not define it
#endif

void setup() {
  Serial.begin(115200);
  pinMode(LED_BUILTIN, OUTPUT);
}

void loop() {
  digitalWrite(LED_BUILTIN, HIGH);
  delay(500);
  digitalWrite(LED_BUILTIN, LOW);
  delay(500);
  Serial.println("ola do pneuma");
}
"#;

/// Create pneuma.lua, src/, libs/, .gitignore (never overwrites). With `sample`, also `src/main.cpp`
/// when src/ has no sources yet. Returns what was created, one line each.
pub fn scaffold(dir: &Path, sample: bool) -> Result<Vec<String>> {
    let mut made = vec![];
    std::fs::create_dir_all(dir.join("libs"))?;
    std::fs::create_dir_all(dir.join("src"))?;
    let lua = dir.join("pneuma.lua");
    if lua.exists() {
        made.push("pneuma.lua já existe, mantido.".into());
    } else {
        std::fs::write(&lua, TEMPLATE)?;
        made.push(format!("criado {}", lua.display()));
    }
    let gi = dir.join(".gitignore");
    if !gi.exists() {
        std::fs::write(&gi, ".pneuma/\nlogs/\n")?;
    }
    let has_src = std::fs::read_dir(dir.join("src"))?.flatten().any(|e| {
        e.path().extension().is_some_and(|x| matches!(x.to_str(), Some("cpp" | "c" | "ino")))
    });
    if sample && !has_src {
        let m = dir.join("src/main.cpp");
        std::fs::write(&m, SAMPLE_MAIN)?;
        made.push(format!("criado {}", m.display()));
    }
    Ok(made)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_is_valid_and_infers_platform() {
        let d = std::env::temp_dir().join(format!("pneuma_proj_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("pneuma.lua"), TEMPLATE).unwrap();
        let p = Project::load(&d).unwrap();
        assert_eq!(p.envs.len(), 3);
        assert!(p.envs.iter().all(|(_, e)| e.is_ok()));
        let uno = p.envs.iter().find(|(n, _)| n == "uno").unwrap().1.as_ref().unwrap();
        assert_eq!(uno.platform, "atmelavr");
        assert_eq!(uno.build_flags, ["-Wall", "-Os"]);
        assert_eq!(uno.monitor_speed, 115200);
        assert_eq!(p.settings.default_env.as_deref(), Some("uno"));
        assert!(p.settings.monitor_timestamps);
        assert_eq!(p.settings.log_dir, "logs");
        assert!(p.settings.log_timestamps && !p.settings.autosave);
    }

    #[test]
    fn legacy_bmu_lua_still_loads_and_new_wins() {
        let d = std::env::temp_dir().join(format!("pneuma_legacy_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("bmu.lua"), "return { default_env = 'a', env = { a = { board = 'uno' } } }").unwrap();
        assert_eq!(Project::load(&d).unwrap().settings.default_env.as_deref(), Some("a"));
        std::fs::write(d.join("pneuma.lua"), "return { default_env = 'b', env = { b = { board = 'uno' } } }").unwrap();
        assert_eq!(Project::load(&d).unwrap().settings.default_env.as_deref(), Some("b"));
    }

    #[test]
    fn scaffold_creates_sample_once() {
        let d = std::env::temp_dir().join(format!("pneuma_new_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        scaffold(&d, true).unwrap();
        let main = std::fs::read_to_string(d.join("src/main.cpp")).unwrap();
        assert!(main.contains("#include <Arduino.h>") && main.contains("Serial.begin"));
        std::fs::write(d.join("src/main.cpp"), "// mine").unwrap();
        scaffold(&d, true).unwrap();
        assert_eq!(std::fs::read_to_string(d.join("src/main.cpp")).unwrap(), "// mine");
        let p = Project::load(&d).unwrap();
        assert!(p.envs.iter().all(|(_, e)| e.is_ok()));
    }

    #[test]
    fn lua_errors_are_reported() {
        let d = std::env::temp_dir().join(format!("pneuma_proj_err_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("pneuma.lua"), "return { env = { x = { board = 'zzz' }, y = {} } }").unwrap();
        let p = Project::load(&d).unwrap();
        assert!(p.envs.iter().find(|(n, _)| n == "x").unwrap().1.as_ref().unwrap_err().contains("desconhecida"));
        assert!(p.envs.iter().find(|(n, _)| n == "y").unwrap().1.as_ref().unwrap_err().contains("falta"));
        std::fs::write(d.join("pneuma.lua"), "return {").unwrap();
        assert!(Project::load(&d).is_err());
        std::fs::write(d.join("pneuma.lua"), "return 5").unwrap();
        assert!(Project::load(&d).is_err());
    }

    #[test]
    fn falls_back_to_ini() {
        let d = std::env::temp_dir().join(format!("pneuma_proj_ini_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("platformio.ini"), "[env:a]\nplatform=p\nboard=uno\n").unwrap();
        let p = Project::load(&d).unwrap();
        assert_eq!(p.envs.len(), 1);
    }
}

#[cfg(test)]
mod cmake_tests {
    use super::*;

    #[test]
    fn lua_cmake_env_keys() {
        let d = std::env::temp_dir().join(format!("pneuma_proj_cmake_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("CMakeLists.txt"), "").unwrap();
        std::fs::write(d.join("pneuma.lua"), "return { env = { p = { board='pico', framework='cmake', cmake_args={'-DA=1'}, cmake_target='app', freertos='/fr', build_flags={'-Os'} } } }").unwrap();
        let p = Project::load(&d).unwrap();
        let e = p.envs[0].1.as_ref().unwrap();
        let j = crate::cmake::job(&p, e).unwrap().unwrap();
        assert_eq!(j.target.as_deref(), Some("app"));
        assert!(j.env_vars.contains(&("FREERTOS_KERNEL_PATH".to_string(), "/fr".to_string())));
        for want in ["-DA=1", "-DPICO_BOARD=pico", "-DCMAKE_C_FLAGS=-Os"] {
            assert!(j.args.iter().any(|a| a == want), "{want} in {:?}", j.args);
        }
        assert!(j.build_dir.ends_with(".pneuma/build/p"));
    }
}
