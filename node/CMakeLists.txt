cmake_minimum_required(VERSION 3.13)

set(PICO_BOARD pico2)

include($ENV{PICO_SDK_PATH}/external/pico_sdk_import.cmake)

project(sensor_system_node C CXX ASM)

set(CMAKE_C_STANDARD 11)
set(CMAKE_CXX_STANDARD 17)

pico_sdk_init()

find_package(Python3 REQUIRED COMPONENTS Interpreter)
find_program(PICOTOOL_EXECUTABLE picotool)

# Keep these values in sync with boot/boot_config.h.
set(ADXL355_FLASH_BASE        0x10000000)
set(ADXL355_BOOT_OFFSET       0x00000000)
set(ADXL355_BOOT_SIZE         0x00020000)
set(ADXL355_METADATA_OFFSET   0x00020000)
set(ADXL355_METADATA_SIZE     0x00020000)
set(ADXL355_SLOT_A_OFFSET     0x00040000)
set(ADXL355_SLOT_A_SIZE       0x001E0000)
set(ADXL355_SLOT_B_OFFSET     0x00220000)
set(ADXL355_SLOT_B_SIZE       0x001E0000)

set(SENSOR_SYSTEM_NODE_INCLUDE_DIRS
    ${CMAKE_CURRENT_LIST_DIR}
    ${CMAKE_CURRENT_LIST_DIR}/acquisition
    ${CMAKE_CURRENT_LIST_DIR}/adxl355
    ${CMAKE_CURRENT_LIST_DIR}/boot
    ${CMAKE_CURRENT_LIST_DIR}/boot_shared
    ${CMAKE_CURRENT_LIST_DIR}/common
    ${CMAKE_CURRENT_LIST_DIR}/config
    ${CMAKE_CURRENT_LIST_DIR}/controller
    ${CMAKE_CURRENT_LIST_DIR}/interfaces
    ${CMAKE_CURRENT_LIST_DIR}/storage
    ${CMAKE_CURRENT_LIST_DIR}/transport
)

set(SENSOR_SYSTEM_NODE_APP_SOURCES
    ${CMAKE_CURRENT_LIST_DIR}/main.cpp
    ${CMAKE_CURRENT_LIST_DIR}/adxl355/adxl355_driver.cpp
    ${CMAKE_CURRENT_LIST_DIR}/acquisition/acquisition_engine.cpp
    ${CMAKE_CURRENT_LIST_DIR}/controller/node_controller.cpp
    ${CMAKE_CURRENT_LIST_DIR}/config/config_crc.cpp
    ${CMAKE_CURRENT_LIST_DIR}/config/config_manager.cpp
    ${CMAKE_CURRENT_LIST_DIR}/config/config_store_flash.cpp
    ${CMAKE_CURRENT_LIST_DIR}/common/device_identity.cpp
    ${CMAKE_CURRENT_LIST_DIR}/transport/crc16.cpp
    ${CMAKE_CURRENT_LIST_DIR}/transport/frame_decoder.cpp
    ${CMAKE_CURRENT_LIST_DIR}/transport/frame_encoder.cpp
    ${CMAKE_CURRENT_LIST_DIR}/transport/rs485_port.cpp
    ${CMAKE_CURRENT_LIST_DIR}/boot/boot_metadata.cpp
    ${CMAKE_CURRENT_LIST_DIR}/boot_shared/boot_runtime_api.cpp
    ${CMAKE_CURRENT_LIST_DIR}/boot_shared/boot_shared_metadata_io.cpp
)

set(SENSOR_SYSTEM_NODE_BOOTLOADER_SOURCES
    ${CMAKE_CURRENT_LIST_DIR}/boot/boot_main.cpp
    ${CMAKE_CURRENT_LIST_DIR}/boot/boot_decision.cpp
    ${CMAKE_CURRENT_LIST_DIR}/boot/boot_flash.cpp
    ${CMAKE_CURRENT_LIST_DIR}/boot/boot_jump.cpp
    ${CMAKE_CURRENT_LIST_DIR}/boot/boot_maintenance.cpp
    ${CMAKE_CURRENT_LIST_DIR}/boot/boot_metadata.cpp
    ${CMAKE_CURRENT_LIST_DIR}/boot/boot_update_engine.cpp
    ${CMAKE_CURRENT_LIST_DIR}/boot/boot_update_protocol.cpp
    ${CMAKE_CURRENT_LIST_DIR}/boot/boot_update_server.cpp
)

function(sensor_system_node_configure_flash_region target flash_offset flash_size)
    math(EXPR flash_origin "${ADXL355_FLASH_BASE} + ${flash_offset}")
    math(EXPR flash_origin_hex "${flash_origin}" OUTPUT_FORMAT HEXADECIMAL)
    math(EXPR flash_size_hex "${flash_size}" OUTPUT_FORMAT HEXADECIMAL)

    set(linker_dir "${CMAKE_CURRENT_BINARY_DIR}/${target}_ld")
    file(MAKE_DIRECTORY "${linker_dir}")

    set(FLASH_ORIGIN_HEX "${flash_origin_hex}")
    set(FLASH_LENGTH_HEX "${flash_size_hex}")
    configure_file(
        ${PROJECT_SOURCE_DIR}/cmake/pico_flash_region.template.ld
        ${linker_dir}/pico_flash_region.ld
        @ONLY
    )

    file(READ "${PICO_LINKER_SCRIPT_PATH}/memmap_default.ld" memmap_default_contents)
    string(REPLACE
        "INCLUDE \"pico_flash_region.ld\""
        "INCLUDE \"${linker_dir}/pico_flash_region.ld\""
        memmap_target_contents
        "${memmap_default_contents}"
    )
    file(WRITE "${linker_dir}/memmap_default.ld" "${memmap_target_contents}")

    pico_set_linker_script(${target} "${linker_dir}/memmap_default.ld")
endfunction()

function(sensor_system_node_configure_common_target target)
    target_include_directories(${target} PRIVATE ${SENSOR_SYSTEM_NODE_INCLUDE_DIRS})
    target_link_libraries(${target}
        pico_stdlib
        pico_multicore
        hardware_spi
        hardware_uart
        hardware_dma
        hardware_watchdog
        hardware_flash
        pico_unique_id
    )
endfunction()

function(sensor_system_node_add_app_target target flash_offset flash_size output_name)
    add_executable(${target} ${SENSOR_SYSTEM_NODE_APP_SOURCES})
    sensor_system_node_configure_common_target(${target})
    sensor_system_node_configure_flash_region(${target} ${flash_offset} ${flash_size})

    set_target_properties(${target} PROPERTIES OUTPUT_NAME ${output_name})
    pico_enable_stdio_usb(${target} 1)
    pico_enable_stdio_uart(${target} 0)
    pico_add_extra_outputs(${target})
endfunction()

add_executable(sensor_system_node_bootloader ${SENSOR_SYSTEM_NODE_BOOTLOADER_SOURCES})
sensor_system_node_configure_common_target(sensor_system_node_bootloader)
sensor_system_node_configure_flash_region(sensor_system_node_bootloader ${ADXL355_BOOT_OFFSET} ${ADXL355_BOOT_SIZE})
set_target_properties(sensor_system_node_bootloader PROPERTIES OUTPUT_NAME sensor-system-node-bootloader)
pico_enable_stdio_usb(sensor_system_node_bootloader 0)
pico_enable_stdio_uart(sensor_system_node_bootloader 0)
pico_add_extra_outputs(sensor_system_node_bootloader)

sensor_system_node_add_app_target(sensor_system_node_slot_a ${ADXL355_SLOT_A_OFFSET} ${ADXL355_SLOT_A_SIZE} sensor-system-node-slot-a)
sensor_system_node_add_app_target(sensor_system_node_slot_b ${ADXL355_SLOT_B_OFFSET} ${ADXL355_SLOT_B_SIZE} sensor-system-node-slot-b)

# Standalone diagnostic image without the A/B boot chain.
# This keeps the old "single app" bring-up path available so we can
# isolate whether failures come from the bootloader handoff or from the
# application itself / external hardware.
sensor_system_node_add_app_target(sensor_system_node_direct ${ADXL355_BOOT_OFFSET} ${ADXL355_METADATA_OFFSET} sensor-system-node-direct)

add_custom_command(
    OUTPUT
        ${CMAKE_CURRENT_BINARY_DIR}/sensor-system-node-update-package.json
    COMMAND
        ${Python3_EXECUTABLE}
        ${PROJECT_SOURCE_DIR}/tools/build_boot_artifacts.py
        update-package
        --slot-a-bin $<TARGET_FILE_DIR:sensor_system_node_slot_a>/sensor-system-node-slot-a.bin
        --slot-b-bin $<TARGET_FILE_DIR:sensor_system_node_slot_b>/sensor-system-node-slot-b.bin
        --version-header ${PROJECT_SOURCE_DIR}/common/protocol_ids.h
        --output ${CMAKE_CURRENT_BINARY_DIR}/sensor-system-node-update-package.json
    DEPENDS
        sensor_system_node_slot_a
        sensor_system_node_slot_b
        ${PROJECT_SOURCE_DIR}/tools/build_boot_artifacts.py
        ${PROJECT_SOURCE_DIR}/common/protocol_ids.h
    VERBATIM
)

add_custom_target(sensor_system_node_update_package ALL
    DEPENDS ${CMAKE_CURRENT_BINARY_DIR}/sensor-system-node-update-package.json
)

add_custom_target(sensor_system_node_factory_image ALL
    COMMAND
        ${Python3_EXECUTABLE}
        ${PROJECT_SOURCE_DIR}/tools/build_boot_artifacts.py
        factory-image
        --bootloader-bin $<TARGET_FILE_DIR:sensor_system_node_bootloader>/sensor-system-node-bootloader.bin
        --slot-a-bin $<TARGET_FILE_DIR:sensor_system_node_slot_a>/sensor-system-node-slot-a.bin
        --slot-a-offset ${ADXL355_SLOT_A_OFFSET}
        --metadata-primary-offset ${ADXL355_METADATA_OFFSET}
        --metadata-secondary-offset 0x00021000
        --node-id 0
        --version-header ${PROJECT_SOURCE_DIR}/common/protocol_ids.h
        --output ${CMAKE_CURRENT_BINARY_DIR}/sensor-system-node-factory.bin
    COMMAND
        ${CMAKE_COMMAND} -E touch ${CMAKE_CURRENT_BINARY_DIR}/sensor-system-node-factory-image.stamp
    BYPRODUCTS
        ${CMAKE_CURRENT_BINARY_DIR}/sensor-system-node-factory.bin
        ${CMAKE_CURRENT_BINARY_DIR}/sensor-system-node-factory-image.stamp
    DEPENDS
        sensor_system_node_bootloader
        sensor_system_node_slot_a
        ${PROJECT_SOURCE_DIR}/tools/build_boot_artifacts.py
        ${PROJECT_SOURCE_DIR}/common/protocol_ids.h
    VERBATIM
)

add_custom_target(sensor_system_node_compat_bin ALL
    COMMAND
        ${CMAKE_COMMAND} -E copy_if_different
        ${CMAKE_CURRENT_BINARY_DIR}/sensor-system-node-factory.bin
        ${CMAKE_CURRENT_BINARY_DIR}/sensor-system-node.bin
    COMMAND
        ${CMAKE_COMMAND} -E touch ${CMAKE_CURRENT_BINARY_DIR}/sensor-system-node-compat-bin.stamp
    BYPRODUCTS
        ${CMAKE_CURRENT_BINARY_DIR}/sensor-system-node.bin
        ${CMAKE_CURRENT_BINARY_DIR}/sensor-system-node-compat-bin.stamp
    DEPENDS
        sensor_system_node_factory_image
    VERBATIM
)

if (PICOTOOL_EXECUTABLE)
    add_custom_target(sensor_system_node_factory_uf2 ALL
        COMMAND
            ${PICOTOOL_EXECUTABLE}
            uf2 convert
            ${CMAKE_CURRENT_BINARY_DIR}/sensor-system-node-factory.bin
            -t bin
            ${CMAKE_CURRENT_BINARY_DIR}/sensor-system-node-factory.uf2
            -t uf2
            -o ${ADXL355_FLASH_BASE}
        COMMAND
            ${CMAKE_COMMAND} -E touch ${CMAKE_CURRENT_BINARY_DIR}/sensor-system-node-factory-uf2.stamp
        BYPRODUCTS
            ${CMAKE_CURRENT_BINARY_DIR}/sensor-system-node-factory.uf2
            ${CMAKE_CURRENT_BINARY_DIR}/sensor-system-node-factory-uf2.stamp
        DEPENDS
            sensor_system_node_factory_image
        VERBATIM
    )

    add_custom_target(sensor_system_node_compat_uf2 ALL
        COMMAND
            ${CMAKE_COMMAND} -E copy_if_different
            ${CMAKE_CURRENT_BINARY_DIR}/sensor-system-node-factory.uf2
            ${CMAKE_CURRENT_BINARY_DIR}/sensor-system-node.uf2
        COMMAND
            ${CMAKE_COMMAND} -E touch ${CMAKE_CURRENT_BINARY_DIR}/sensor-system-node-compat-uf2.stamp
        BYPRODUCTS
            ${CMAKE_CURRENT_BINARY_DIR}/sensor-system-node.uf2
            ${CMAKE_CURRENT_BINARY_DIR}/sensor-system-node-compat-uf2.stamp
        DEPENDS
            sensor_system_node_factory_uf2
        VERBATIM
    )
endif()

add_custom_target(sensor_system_node ALL
    DEPENDS
        sensor_system_node_bootloader
        sensor_system_node_direct
        sensor_system_node_slot_a
        sensor_system_node_slot_b
        sensor_system_node_update_package
        sensor_system_node_factory_image
        sensor_system_node_compat_bin
)

if (TARGET sensor_system_node_compat_uf2)
    add_dependencies(sensor_system_node sensor_system_node_compat_uf2)
endif()

if (TARGET sensor_system_node_factory_uf2)
    add_custom_target(sensor_system_node_release
        COMMAND
            ${Python3_EXECUTABLE}
            ${PROJECT_SOURCE_DIR}/tools/package_node_release.py
            --node-dir ${PROJECT_SOURCE_DIR}
            --build-dir ${CMAKE_CURRENT_BINARY_DIR}
        DEPENDS
            sensor_system_node_factory_uf2
            sensor_system_node_direct
            sensor_system_node_slot_a
            sensor_system_node_slot_b
            sensor_system_node_update_package
            ${PROJECT_SOURCE_DIR}/tools/package_node_release.py
            ${PROJECT_SOURCE_DIR}/common/protocol_ids.h
        VERBATIM
    )
endif()
