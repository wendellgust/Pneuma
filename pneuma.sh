#!/usr/bin/env bash
# Launcher: compila (só se mudou algo) e executa o PNEUMA.
#   ./pneuma.sh                  abre a TUI (usa ./platformio.ini)
#   ./pneuma.sh init             cria pneuma.lua + libs/ + LIBS.md
#   ./pneuma.sh libs [--flags]   escaneia libs/, gera LIBS.md
#   ./pneuma.sh setup            instala a toolchain AVR (1x); --arduino-cli: instala o arduino-cli
#   ./pneuma.sh build|upload|monitor|run [-e env] [-p porta] [-w]
#   ./pneuma.sh logs [nome|latest]   lista/mostra logs salvos do monitor
#   ./pneuma.sh tui|check [dir|arquivo]
#   ./pneuma.sh boards|ports
#   ./pneuma.sh test             roda os testes
#   ./pneuma.sh install          instala `pneuma` em ~/.cargo/bin
set -euo pipefail

ROOT="$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")" && pwd)"
BIN="$ROOT/target/release/pneuma"
# O projeto do usuário é o diretório de onde o script foi chamado.
cd "${PNEUMA_CWD:-${BMU_CWD:-$PWD}}"

command -v cargo >/dev/null || { echo "erro: cargo não encontrado. Instale Rust: https://rustup.rs" >&2; exit 1; }

case "${1:-}" in
  test)    exec cargo test --manifest-path "$ROOT/Cargo.toml" ;;
  install) exec cargo install --path "$ROOT/crates/pneuma-cli" --locked ;;
esac

# cargo só recompila se necessário; silencioso quando já está atualizado.
cargo build --release -q --manifest-path "$ROOT/Cargo.toml" -p pneuma-cli

exec "$BIN" "$@"
