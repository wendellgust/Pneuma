//! Optional extras `pneuma setup` can install (`--list`, `--extras`, `--freertos`).
//! Each extra is a set of git repositories cloned into `~/.pneuma/sdk/<name>`.

use crate::{build::pneuma_home, Error, Result};
use std::{path::PathBuf, process::Command};

pub struct Repo {
    pub dir: &'static str,
    pub url: &'static str,
    /// Clone submodules too (pico-sdk needs tinyusb, lwip, ...).
    pub submodules: bool,
}

pub struct Extra {
    pub id: &'static str,
    pub name: &'static str,
    pub desc: &'static str,
    pub repos: &'static [Repo],
}

pub const EXTRAS: &[Extra] = &[Extra {
    id: "freertos",
    name: "FreeRTOS for Raspberry Pi Pico",
    desc: "Pico SDK + FreeRTOS-Kernel (~400 MB, via git); builds with framework = \"cmake\"",
    repos: &[
        Repo { dir: "pico-sdk", url: "https://github.com/raspberrypi/pico-sdk", submodules: true },
        Repo { dir: "FreeRTOS-Kernel", url: "https://github.com/FreeRTOS/FreeRTOS-Kernel", submodules: false },
    ],
}];

pub fn sdk_dir() -> PathBuf {
    pneuma_home().join("sdk")
}

pub fn find(id: &str) -> Option<&'static Extra> {
    EXTRAS.iter().find(|e| e.id == id)
}

impl Extra {
    pub fn installed(&self) -> bool {
        self.repos.iter().all(|r| sdk_dir().join(r.dir).join(".git").exists())
    }

    /// Clones what is missing (never touches an existing checkout).
    pub fn install(&self) -> Result<()> {
        std::fs::create_dir_all(sdk_dir())?;
        for r in self.repos {
            let dest = sdk_dir().join(r.dir);
            if dest.join(".git").exists() {
                println!("{}: já instalado ({})", r.dir, dest.display());
                continue;
            }
            println!("clonando {} ...", r.url);
            let mut c = Command::new("git");
            c.args(["clone", "--depth", "1"]);
            if r.submodules {
                c.args(["--recurse-submodules", "--shallow-submodules"]);
            }
            let st = c.arg(r.url).arg(&dest).status().map_err(|e| Error::Build(format!("git: {e} (instale o git)")))?;
            if !st.success() {
                let _ = std::fs::remove_dir_all(&dest); // no half clones
                return Err(Error::Build(format!("git clone {} falhou", r.url)));
            }
        }
        Ok(())
    }
}

/// How to get the ARM compiler (pneuma never runs sudo itself).
pub fn compiler_hint() -> Option<&'static str> {
    if Command::new("arm-none-eabi-gcc").arg("--version").output().is_ok() {
        return None;
    }
    let has = |t: &str| Command::new(t).arg("--version").output().is_ok();
    Some(if has("pacman") {
        "sudo pacman -S arm-none-eabi-gcc arm-none-eabi-newlib cmake ninja"
    } else if has("apt") {
        "sudo apt install gcc-arm-none-eabi libnewlib-arm-none-eabi cmake ninja-build"
    } else if has("brew") {
        "brew install --cask gcc-arm-embedded && brew install cmake ninja"
    } else {
        "instale arm-none-eabi-gcc, cmake e ninja com o gerenciador do seu sistema"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry() {
        assert!(find("freertos").is_some() && find("nope").is_none());
        assert!(EXTRAS.iter().all(|e| !e.repos.is_empty()));
    }
}
