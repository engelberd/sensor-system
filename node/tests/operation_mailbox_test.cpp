#include <cassert>
#include <cstdint>

#include "runtime/operation_mailbox.h"

namespace {

struct TestCommand {
    uint32_t operation_id = 0;
    uint32_t value = 0;
};

struct TestResult {
    uint32_t operation_id = 0;
    uint32_t value = 0;
};

using Mailbox = runtime::OperationMailbox<TestCommand, TestResult, 2, 1>;
using Executor = runtime::OperationExecutor<Mailbox, TestCommand, TestResult>;

void test_command_queue_is_bounded_without_overwrite() {
    Mailbox mailbox;
    assert(mailbox.try_send_command({1, 10}));
    assert(mailbox.try_send_command({2, 20}));
    assert(!mailbox.try_send_command({3, 30}));

    TestCommand command{};
    assert(mailbox.try_receive_command(command));
    assert(command.operation_id == 1 && command.value == 10);
    assert(mailbox.try_receive_command(command));
    assert(command.operation_id == 2 && command.value == 20);
    assert(!mailbox.try_receive_command(command));
}

void test_executor_correlates_and_limits_work_to_one_command() {
    Mailbox mailbox;
    Executor executor;
    assert(mailbox.try_send_command({41, 5}));
    assert(mailbox.try_send_command({42, 7}));
    uint32_t calls = 0;
    const auto handler = [&](const TestCommand& command) {
        ++calls;
        // A wrong handler-provided ID must never escape the executor.
        return TestResult{999, command.value * 2};
    };

    assert(executor.poll(mailbox, handler) ==
           runtime::OperationExecutorStatus::CommandExecutedAndPublished);
    assert(calls == 1);
    assert(mailbox.commands_waiting_approx() == 1);

    TestResult result{};
    assert(mailbox.try_receive_result(result));
    assert(result.operation_id == 41 && result.value == 10);

    assert(executor.poll(mailbox, handler) ==
           runtime::OperationExecutorStatus::CommandExecutedAndPublished);
    assert(calls == 2);
    assert(mailbox.try_receive_result(result));
    assert(result.operation_id == 42 && result.value == 14);
}

void test_result_backpressure_never_reexecutes_command() {
    Mailbox mailbox;
    Executor executor;
    assert(mailbox.try_send_result({90, 1}));
    assert(mailbox.try_send_command({7, 11}));
    assert(mailbox.try_send_command({8, 12}));
    uint32_t calls = 0;
    const auto handler = [&](const TestCommand& command) {
        ++calls;
        return TestResult{0, command.value + 100};
    };

    assert(executor.poll(mailbox, handler) ==
           runtime::OperationExecutorStatus::CommandExecutedResultPending);
    assert(calls == 1);
    assert(executor.has_pending_result());
    assert(mailbox.commands_waiting_approx() == 1);

    assert(executor.poll(mailbox, handler) ==
           runtime::OperationExecutorStatus::PendingResultBlocked);
    assert(calls == 1);
    assert(mailbox.commands_waiting_approx() == 1);

    TestResult result{};
    assert(mailbox.try_receive_result(result));
    assert(result.operation_id == 90);
    assert(executor.poll(mailbox, handler) ==
           runtime::OperationExecutorStatus::PendingResultPublished);
    assert(calls == 1);
    assert(mailbox.try_receive_result(result));
    assert(result.operation_id == 7 && result.value == 111);

    assert(executor.poll(mailbox, handler) ==
           runtime::OperationExecutorStatus::CommandExecutedAndPublished);
    assert(calls == 2);
    assert(mailbox.try_receive_result(result));
    assert(result.operation_id == 8 && result.value == 112);
    assert(executor.executed_count() == 2);
    assert(executor.result_backpressure_count() == 2);
}

}  // namespace

int main() {
    test_command_queue_is_bounded_without_overwrite();
    test_executor_correlates_and_limits_work_to_one_command();
    test_result_backpressure_never_reexecutes_command();
    return 0;
}
