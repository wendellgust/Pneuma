# pico-freertos

Raspberry Pi Pico + FreeRTOS built by Pneuma through CMake + Ninja (`framework = "cmake"`).

Needs `cmake`, `ninja`, `arm-none-eabi-gcc`, the [Pico SDK](https://github.com/raspberrypi/pico-sdk)
(`PICO_SDK_PATH`) and [FreeRTOS-Kernel](https://github.com/FreeRTOS/FreeRTOS-Kernel)
(`FREERTOS_KERNEL_PATH`), or set `pico_sdk` / `freertos` in `pneuma.lua`.

    pneuma build            # cmake -G Ninja + build into .pneuma/build/pico
    pneuma upload           # picotool load (or copies the .uf2 to a mounted RPI-RP2)
    pneuma run              # build + upload + serial monitor
