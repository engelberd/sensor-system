#pragma once

#include <cstddef>
#include <cstdint>
#include <type_traits>

#include "runtime/spsc_queue.h"

namespace runtime {

// Transport between one command producer/consumer and one result
// producer/consumer. The mailbox only owns bounded storage; operation
// semantics, deadlines and retries belong to the calling service.
template <typename Command,
          typename Result,
          size_t CommandCapacity,
          size_t ResultCapacity>
class OperationMailbox {
    static_assert(std::is_trivially_copyable<Command>::value,
                  "mailbox commands must be fixed value types");
    static_assert(std::is_trivially_copyable<Result>::value,
                  "mailbox results must be fixed value types");

public:
    static constexpr size_t command_capacity() { return CommandCapacity; }
    static constexpr size_t result_capacity() { return ResultCapacity; }

    // core 1 producer
    bool try_send_command(const Command& command) {
        return commands_.try_push(command);
    }

    // core 0 consumer
    bool try_receive_command(Command& command) {
        return commands_.try_pop(command);
    }

    // core 0 producer
    bool try_send_result(const Result& result) {
        return results_.try_push(result);
    }

    // core 1 consumer
    bool try_receive_result(Result& result) {
        return results_.try_pop(result);
    }

    size_t commands_waiting_approx() const {
        return commands_.size_approx();
    }

    size_t results_waiting_approx() const {
        return results_.size_approx();
    }

private:
    SpscQueue<Command, CommandCapacity> commands_{};
    SpscQueue<Result, ResultCapacity> results_{};
};

enum class OperationExecutorStatus : uint8_t {
    Idle = 0,
    PendingResultPublished,
    PendingResultBlocked,
    CommandExecutedAndPublished,
    CommandExecutedResultPending,
};

// Owned by the command consumer (core 0). Handler must return a Result with an
// operation_id field. The executor writes the ID from the consumed command so
// a handler cannot accidentally break correlation.
template <typename Mailbox, typename Command, typename Result>
class OperationExecutor {
public:
    OperationExecutor() = default;
    OperationExecutor(const OperationExecutor&) = delete;
    OperationExecutor& operator=(const OperationExecutor&) = delete;

    template <typename Handler>
    OperationExecutorStatus poll(Mailbox& mailbox, Handler&& handler) {
        if (has_pending_result_) {
            if (!mailbox.try_send_result(pending_result_)) {
                saturating_increment(result_backpressure_count_);
                return OperationExecutorStatus::PendingResultBlocked;
            }
            has_pending_result_ = false;
            return OperationExecutorStatus::PendingResultPublished;
        }

        Command command{};
        if (!mailbox.try_receive_command(command)) {
            return OperationExecutorStatus::Idle;
        }

        Result result = handler(command);
        result.operation_id = command.operation_id;
        saturating_increment(executed_count_);

        if (mailbox.try_send_result(result)) {
            return OperationExecutorStatus::CommandExecutedAndPublished;
        }

        pending_result_ = result;
        has_pending_result_ = true;
        saturating_increment(result_backpressure_count_);
        return OperationExecutorStatus::CommandExecutedResultPending;
    }

    bool has_pending_result() const { return has_pending_result_; }
    uint32_t executed_count() const { return executed_count_; }
    uint32_t result_backpressure_count() const {
        return result_backpressure_count_;
    }

private:
    static void saturating_increment(uint32_t& value) {
        if (value != UINT32_MAX) ++value;
    }

    Result pending_result_{};
    bool has_pending_result_ = false;
    uint32_t executed_count_ = 0;
    uint32_t result_backpressure_count_ = 0;
};

}  // namespace runtime
