#pragma once

#include <cstdint>

#include "common/protocol_ids.h"

namespace runtime {

struct CommissioningCommand {
    uint32_t operation_id = 0;
    uint64_t round_nonce = 0;
    uint8_t node_address = 0;
};

enum class CommissioningResultCode : uint8_t {
    Assigned = 0,
    AlreadyAssigned = 1,
    InvalidAddress = 2,
    StorageFailed = 3,
    BootMetadataFailed = 4,
    RollbackFailed = 5,
};

struct CommissioningResult {
    uint32_t operation_id = 0;
    CommissioningResultCode code = CommissioningResultCode::Assigned;
    uint8_t node_address = 0;
};

class ICommissioningBootSettings {
public:
    virtual ~ICommissioningBootSettings() = default;
    virtual bool sync_node_address(uint8_t node_address) = 0;
};

template <typename ConfigManager>
class CommissioningRuntime {
public:
    CommissioningRuntime(ConfigManager& config_manager,
                         ICommissioningBootSettings& boot_settings)
        : config_manager_(config_manager), boot_settings_(boot_settings) {}

    CommissioningResult handle(const CommissioningCommand& command) {
        if (command.node_address < 1 || command.node_address > 253) {
            return {command.operation_id,
                    CommissioningResultCode::InvalidAddress, 0};
        }
        const auto previous = config_manager_.current();
        if (previous.node_id != UNASSIGNED_NODE_ID) {
            return {command.operation_id,
                    CommissioningResultCode::AlreadyAssigned,
                    previous.node_id};
        }

        auto next = previous;
        next.node_id = command.node_address;
        config_manager_.replace_device_config(next);
        if (!config_manager_.save()) {
            config_manager_.replace_device_config(previous);
            return {command.operation_id,
                    CommissioningResultCode::StorageFailed, 0};
        }
        if (!boot_settings_.sync_node_address(command.node_address)) {
            config_manager_.replace_device_config(previous);
            if (!config_manager_.save() ||
                !boot_settings_.sync_node_address(previous.node_id)) {
                return {command.operation_id,
                        CommissioningResultCode::RollbackFailed, 0};
            }
            return {command.operation_id,
                    CommissioningResultCode::BootMetadataFailed, 0};
        }
        return {command.operation_id,
                CommissioningResultCode::Assigned,
                command.node_address};
    }

private:
    ConfigManager& config_manager_;
    ICommissioningBootSettings& boot_settings_;
};

}  // namespace runtime
