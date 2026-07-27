#pragma once

#include <cstdint>

#define SENSOR_BOARD_PROFILE_CUSTOM_V2 1
#define SENSOR_BOARD_PROFILE_LEGACY_EVAL 2

#ifndef SENSOR_SYSTEM_BOARD_PROFILE
#define SENSOR_SYSTEM_BOARD_PROFILE SENSOR_BOARD_PROFILE_CUSTOM_V2
#endif

#ifndef SENSOR_TIMESTAMPING_V2_ENABLED
#define SENSOR_TIMESTAMPING_V2_ENABLED 0
#endif

namespace BoardProfile {

#if SENSOR_SYSTEM_BOARD_PROFILE == SENSOR_BOARD_PROFILE_CUSTOM_V2
static constexpr const char* kName = "custom_v2";
static constexpr uint8_t kSpiMiso = 12;
static constexpr uint8_t kSpiCs = 13;
static constexpr uint8_t kSpiSck = 10;
static constexpr uint8_t kSpiMosi = 11;
static constexpr int8_t kDrdy = 14;
static constexpr int8_t kInt1 = 15;
static constexpr int8_t kRs485De = 2;
#elif SENSOR_SYSTEM_BOARD_PROFILE == SENSOR_BOARD_PROFILE_LEGACY_EVAL
static constexpr const char* kName = "legacy_eval";
static constexpr uint8_t kSpiMiso = 12;
static constexpr uint8_t kSpiCs = 13;
static constexpr uint8_t kSpiSck = 14;
static constexpr uint8_t kSpiMosi = 15;
static constexpr int8_t kDrdy = 11;
static constexpr int8_t kInt1 = 10;
static constexpr int8_t kRs485De = -1;
#else
#error "Unknown SENSOR_SYSTEM_BOARD_PROFILE"
#endif

static constexpr uint8_t kRs485Tx = 0;
static constexpr uint8_t kRs485Rx = 1;
static constexpr bool kTimestampingV2Enabled =
    SENSOR_TIMESTAMPING_V2_ENABLED != 0;

static_assert(kSpiMiso != kSpiCs, "SPI MISO and CS pins conflict");
static_assert(kSpiMiso != kSpiSck, "SPI MISO and SCK pins conflict");
static_assert(kSpiMiso != kSpiMosi, "SPI MISO and MOSI pins conflict");
static_assert(kSpiCs != kSpiSck, "SPI CS and SCK pins conflict");
static_assert(kSpiCs != kSpiMosi, "SPI CS and MOSI pins conflict");
static_assert(kSpiSck != kSpiMosi, "SPI SCK and MOSI pins conflict");
static_assert(kDrdy < 0 || (
    kDrdy != static_cast<int8_t>(kSpiMiso) &&
    kDrdy != static_cast<int8_t>(kSpiCs) &&
    kDrdy != static_cast<int8_t>(kSpiSck) &&
    kDrdy != static_cast<int8_t>(kSpiMosi)
), "DRDY pin conflicts with SPI");
static_assert(kInt1 < 0 || (
    kInt1 != static_cast<int8_t>(kSpiMiso) &&
    kInt1 != static_cast<int8_t>(kSpiCs) &&
    kInt1 != static_cast<int8_t>(kSpiSck) &&
    kInt1 != static_cast<int8_t>(kSpiMosi)
), "INT1 pin conflicts with SPI");
static_assert(kDrdy < 0 || kInt1 < 0 || kDrdy != kInt1,
              "DRDY and INT1 pins conflict");

}  // namespace BoardProfile
