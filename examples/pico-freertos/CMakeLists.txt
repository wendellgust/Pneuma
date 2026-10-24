cmake_minimum_required(VERSION 3.13)

# Pico SDK and FreeRTOS-Kernel come from the environment (or pneuma.lua: pico_sdk / freertos).
include($ENV{PICO_SDK_PATH}/external/pico_sdk_import.cmake)
include($ENV{FREERTOS_KERNEL_PATH}/portable/ThirdParty/GCC/RP2040/FreeRTOS_Kernel_import.cmake)

project(pico_freertos C CXX ASM)
set(CMAKE_C_STANDARD 11)
set(CMAKE_CXX_STANDARD 17)
pico_sdk_init()

add_executable(app src/main.c)
target_include_directories(app PRIVATE src)
target_link_libraries(app pico_stdlib FreeRTOS-Kernel FreeRTOS-Kernel-Heap4)
pico_enable_stdio_usb(app 1)
pico_enable_stdio_uart(app 0)
pico_add_extra_outputs(app)   # .uf2 for upload
