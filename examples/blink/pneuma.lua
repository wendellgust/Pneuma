-- pneuma.lua — configuração do projeto. É Lua de verdade: pode usar if, funções, variáveis.
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
