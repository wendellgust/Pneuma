pub mod boards;
pub mod build;
pub mod config;
pub mod filters;
pub mod libs;
pub mod monitor;
pub mod native;
pub mod toolchain;
pub mod project;
pub mod ports;
pub mod state;
pub mod terminal;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("config line {line}: {msg}")]
    Config { line: usize, msg: String },
    #[error("env `{0}` not found")]
    EnvNotFound(String),
    #[error("env `{env}`: missing `{key}`")]
    MissingKey { env: String, key: &'static str },
    #[error("env `{env}`: unknown board `{board}`")]
    UnknownBoard { env: String, board: String },
    #[error("{0}")]
    Build(String),
    #[error("serial: {0}")]
    Serial(#[from] serialport::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
