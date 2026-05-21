#pragma once

#include <cstdint>

#include "common/protocol_ids.h"

struct DeviceConfig {
    uint8_t node_id = UNASSIGNED_NODE_ID;
    uint32_t baudrate = 115200;

    uint16_t odr_hz = 250;
    uint8_t range_g = 2;
    uint8_t high_pass_corner = 0;

    int32_t offset_x = 0;
    int32_t offset_y = 0;
    int32_t offset_z = 0;

    uint16_t act_threshold = 0;
    uint8_t act_count = 1;

    uint8_t fifo_watermark = 30;
};
