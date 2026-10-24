-- Projeto com CMakeLists.txt próprio (Pico SDK + FreeRTOS). pneuma roda cmake -G Ninja por você.
-- Requisitos: cmake, ninja, arm-none-eabi-gcc, Pico SDK e FreeRTOS-Kernel (clone com --recurse-submodules).
return {
  default_env = "pico",
  monitor = { speed = 115200, timestamps = true },
  env = {
    pico = {
      board = "pico", framework = "cmake",
      -- pico_sdk = "/caminho/pico-sdk",            -- ou exporte PICO_SDK_PATH
      -- freertos = "/caminho/FreeRTOS-Kernel",     -- ou exporte FREERTOS_KERNEL_PATH
      -- cmake_args = { "-DCMAKE_BUILD_TYPE=Release" },
      -- cmake_target = "app",
      -- upload_cmd = { "picotool", "load", "-f", "-x", "{uf2}" },  -- padrão do RP2040
    },
    picow = { board = "rpipicow", framework = "cmake" },
  },
}
