use core::fmt;
use std::collections::HashSet;
use std::sync::mpsc::{self, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use crate::installation_topology::MAX_HOST_CHANNELS;

/// Execution boundary used by the host supervisor. The task owns all mutable
/// state of one physical channel; neither samples nor wire events cross this
/// boundary through a shared host queue.
pub trait SupervisedChannelTask: Send + 'static {
    type StepError: Send + 'static;
    type Shutdown: Send + 'static;

    /// Performs one bounded unit of channel work.
    fn step(&mut self) -> Result<(), Self::StepError>;

    /// Consumes the task and returns its shutdown evidence. Implementations
    /// decide how to drain persistence and stop their nodes.
    fn shutdown(self) -> Self::Shutdown;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelWorkerPhase {
    Starting,
    Running,
    StopRequested,
    Faulted,
    Stopped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelWorkerSnapshot {
    pub channel_id: u8,
    pub phase: ChannelWorkerPhase,
    pub completed_steps: u64,
}

pub struct ChannelTaskSpec<T> {
    channel_id: u8,
    channel_name: String,
    task: T,
}

impl<T> ChannelTaskSpec<T> {
    pub fn new(channel_id: u8, channel_name: impl Into<String>, task: T) -> Self {
        Self {
            channel_id,
            channel_name: channel_name.into(),
            task,
        }
    }
}

enum ChannelWorkerCommand {
    Stop,
}

pub struct SupervisedChannel<T>
where
    T: SupervisedChannelTask,
{
    channel_id: u8,
    channel_name: String,
    commands: Option<SyncSender<ChannelWorkerCommand>>,
    snapshot: Arc<Mutex<ChannelWorkerSnapshot>>,
    thread: Option<JoinHandle<ChannelWorkerExit<T::StepError, T::Shutdown>>>,
}

impl<T> SupervisedChannel<T>
where
    T: SupervisedChannelTask,
{
    pub const fn channel_id(&self) -> u8 {
        self.channel_id
    }

    pub fn channel_name(&self) -> &str {
        &self.channel_name
    }

    pub fn snapshot(&self) -> ChannelWorkerSnapshot {
        *lock_snapshot(&self.snapshot)
    }

    pub fn is_finished(&self) -> bool {
        match self.thread.as_ref() {
            Some(thread) => thread.is_finished(),
            None => true,
        }
    }

    /// Idempotent and non-blocking. A capacity-one control mailbox is enough:
    /// multiple stop requests have the same meaning as one.
    pub fn request_stop(&self) -> Result<(), ChannelSupervisorError> {
        let Some(commands) = &self.commands else {
            return Ok(());
        };
        match commands.try_send(ChannelWorkerCommand::Stop) {
            Ok(()) | Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => Ok(()),
        }
    }

    pub fn join(
        mut self,
    ) -> Result<ChannelWorkerExit<T::StepError, T::Shutdown>, ChannelSupervisorError> {
        self.commands.take();
        self.thread
            .take()
            .ok_or(ChannelSupervisorError::AlreadyJoined {
                channel_id: self.channel_id,
            })?
            .join()
            .map_err(|_| ChannelSupervisorError::WorkerPanicked {
                channel_id: self.channel_id,
            })
    }
}

impl<T> Drop for SupervisedChannel<T>
where
    T: SupervisedChannelTask,
{
    fn drop(&mut self) {
        self.commands.take();
        // Detaching is intentional here: dropping the command sender asks the
        // worker to stop at its next bounded step. Explicit shutdown uses join.
        self.thread.take();
    }
}

pub enum ChannelWorkerExit<E, S> {
    Stopped {
        completed_steps: u64,
        shutdown: S,
    },
    Faulted {
        completed_steps: u64,
        error: E,
        shutdown: S,
    },
}

pub type ChannelJoinResult<E, S> = Result<(u8, ChannelWorkerExit<E, S>), ChannelSupervisorError>;

pub struct ChannelSupervisorSet<T>
where
    T: SupervisedChannelTask,
{
    channels: Vec<SupervisedChannel<T>>,
}

impl<T> ChannelSupervisorSet<T>
where
    T: SupervisedChannelTask,
{
    pub fn empty() -> Self {
        Self {
            channels: Vec::new(),
        }
    }

    /// Starts one fully composed channel immediately. Installation startup
    /// uses this boundary so a node is never left producing into its finite
    /// journal while unrelated channels are still being prepared.
    pub fn add(&mut self, spec: ChannelTaskSpec<T>) -> Result<(), ChannelSupervisorError> {
        validate_add(&self.channels, &spec)?;
        match spawn_channel(spec) {
            Ok(channel) => {
                self.channels.push(channel);
                Ok(())
            }
            Err((error, task)) => {
                let _ = task.shutdown();
                Err(error)
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }

    /// Starts all prebuilt channel tasks. Validation happens before the first
    /// thread is spawned. If spawning a later worker fails, already-started
    /// workers receive stop and are joined before the error is returned.
    pub fn start(specs: Vec<ChannelTaskSpec<T>>) -> Result<Self, ChannelSupervisorError> {
        validate_specs(&specs)?;
        let mut set = Self {
            channels: Vec::with_capacity(specs.len()),
        };
        let mut specs = specs.into_iter();
        while let Some(spec) = specs.next() {
            if let Err(error) = set.add(spec) {
                for channel in &set.channels {
                    let _ = channel.request_stop();
                }
                for spec in specs {
                    let _ = spec.task.shutdown();
                }
                for channel in set.channels {
                    let _ = channel.join();
                }
                return Err(error);
            }
        }
        Ok(set)
    }

    pub fn channels(&self) -> &[SupervisedChannel<T>] {
        &self.channels
    }

    pub fn request_stop_all(&self) -> Result<(), ChannelSupervisorError> {
        for channel in &self.channels {
            channel.request_stop()?;
        }
        Ok(())
    }

    pub fn join_all(self) -> Vec<ChannelJoinResult<T::StepError, T::Shutdown>> {
        self.channels
            .into_iter()
            .map(|channel| {
                let channel_id = channel.channel_id();
                channel.join().map(|exit| (channel_id, exit))
            })
            .collect()
    }
}

fn validate_add<T>(
    channels: &[SupervisedChannel<T>],
    spec: &ChannelTaskSpec<T>,
) -> Result<(), ChannelSupervisorError>
where
    T: SupervisedChannelTask,
{
    if channels.len() >= MAX_HOST_CHANNELS {
        return Err(ChannelSupervisorError::InvalidChannelCount {
            actual: channels.len().saturating_add(1),
            maximum: MAX_HOST_CHANNELS,
        });
    }
    if spec.channel_id == 0 || usize::from(spec.channel_id) > MAX_HOST_CHANNELS {
        return Err(ChannelSupervisorError::InvalidChannelId(spec.channel_id));
    }
    if spec.channel_name.trim().is_empty() {
        return Err(ChannelSupervisorError::EmptyChannelName(spec.channel_id));
    }
    if channels
        .iter()
        .any(|channel| channel.channel_id == spec.channel_id)
    {
        return Err(ChannelSupervisorError::DuplicateChannelId(spec.channel_id));
    }
    Ok(())
}

fn validate_specs<T>(specs: &[ChannelTaskSpec<T>]) -> Result<(), ChannelSupervisorError> {
    if specs.is_empty() || specs.len() > MAX_HOST_CHANNELS {
        return Err(ChannelSupervisorError::InvalidChannelCount {
            actual: specs.len(),
            maximum: MAX_HOST_CHANNELS,
        });
    }
    let mut ids = HashSet::with_capacity(specs.len());
    for spec in specs {
        if spec.channel_id == 0 || usize::from(spec.channel_id) > MAX_HOST_CHANNELS {
            return Err(ChannelSupervisorError::InvalidChannelId(spec.channel_id));
        }
        if spec.channel_name.trim().is_empty() {
            return Err(ChannelSupervisorError::EmptyChannelName(spec.channel_id));
        }
        if !ids.insert(spec.channel_id) {
            return Err(ChannelSupervisorError::DuplicateChannelId(spec.channel_id));
        }
    }
    Ok(())
}

fn spawn_channel<T>(
    spec: ChannelTaskSpec<T>,
) -> Result<SupervisedChannel<T>, (ChannelSupervisorError, T)>
where
    T: SupervisedChannelTask,
{
    let ChannelTaskSpec {
        channel_id,
        channel_name,
        task,
    } = spec;
    let snapshot = Arc::new(Mutex::new(ChannelWorkerSnapshot {
        channel_id,
        phase: ChannelWorkerPhase::Starting,
        completed_steps: 0,
    }));
    let worker_snapshot = Arc::clone(&snapshot);
    let (command_tx, command_rx) = mpsc::sync_channel(1);
    let thread_name = format!("channel-{channel_id:02}-{channel_name}");
    let task_slot = Arc::new(Mutex::new(Some(task)));
    let worker_task_slot = Arc::clone(&task_slot);
    let thread = match thread::Builder::new().name(thread_name).spawn(move || {
        let task = lock_snapshot(&worker_task_slot)
            .take()
            .expect("spawned channel task is available exactly once");
        run_channel_task(task, command_rx, worker_snapshot)
    }) {
        Ok(thread) => thread,
        Err(source) => {
            let task = lock_snapshot(&task_slot)
                .take()
                .expect("failed spawn returns channel task ownership");
            return Err((ChannelSupervisorError::Spawn { channel_id, source }, task));
        }
    };
    Ok(SupervisedChannel {
        channel_id,
        channel_name,
        commands: Some(command_tx),
        snapshot,
        thread: Some(thread),
    })
}

fn run_channel_task<T>(
    mut task: T,
    commands: mpsc::Receiver<ChannelWorkerCommand>,
    snapshot: Arc<Mutex<ChannelWorkerSnapshot>>,
) -> ChannelWorkerExit<T::StepError, T::Shutdown>
where
    T: SupervisedChannelTask,
{
    update_snapshot(&snapshot, ChannelWorkerPhase::Running, 0);
    let mut completed_steps = 0_u64;
    loop {
        match commands.try_recv() {
            Ok(ChannelWorkerCommand::Stop) | Err(TryRecvError::Disconnected) => {
                update_snapshot(
                    &snapshot,
                    ChannelWorkerPhase::StopRequested,
                    completed_steps,
                );
                let shutdown = task.shutdown();
                update_snapshot(&snapshot, ChannelWorkerPhase::Stopped, completed_steps);
                return ChannelWorkerExit::Stopped {
                    completed_steps,
                    shutdown,
                };
            }
            Err(TryRecvError::Empty) => {}
        }

        match task.step() {
            Ok(()) => {
                completed_steps = completed_steps.saturating_add(1);
                update_snapshot(&snapshot, ChannelWorkerPhase::Running, completed_steps);
            }
            Err(error) => {
                update_snapshot(&snapshot, ChannelWorkerPhase::Faulted, completed_steps);
                let shutdown = task.shutdown();
                return ChannelWorkerExit::Faulted {
                    completed_steps,
                    error,
                    shutdown,
                };
            }
        }
    }
}

fn update_snapshot(
    snapshot: &Mutex<ChannelWorkerSnapshot>,
    phase: ChannelWorkerPhase,
    completed_steps: u64,
) {
    let mut snapshot = lock_snapshot(snapshot);
    snapshot.phase = phase;
    snapshot.completed_steps = completed_steps;
}

fn lock_snapshot<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Debug)]
pub enum ChannelSupervisorError {
    InvalidChannelCount {
        actual: usize,
        maximum: usize,
    },
    InvalidChannelId(u8),
    EmptyChannelName(u8),
    DuplicateChannelId(u8),
    Spawn {
        channel_id: u8,
        source: std::io::Error,
    },
    AlreadyJoined {
        channel_id: u8,
    },
    WorkerPanicked {
        channel_id: u8,
    },
}

impl fmt::Display for ChannelSupervisorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "channel supervisor error: {self:?}")
    }
}

impl std::error::Error for ChannelSupervisorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    struct TestTask {
        steps: Arc<AtomicUsize>,
        fail_at: Option<usize>,
    }

    impl SupervisedChannelTask for TestTask {
        type StepError = &'static str;
        type Shutdown = usize;

        fn step(&mut self) -> Result<(), Self::StepError> {
            let steps = self.steps.fetch_add(1, Ordering::SeqCst) + 1;
            if self.fail_at == Some(steps) {
                Err("injected channel failure")
            } else {
                std::thread::yield_now();
                Ok(())
            }
        }

        fn shutdown(self) -> Self::Shutdown {
            self.steps.load(Ordering::SeqCst)
        }
    }

    fn spec(
        channel_id: u8,
        fail_at: Option<usize>,
    ) -> (ChannelTaskSpec<TestTask>, Arc<AtomicUsize>) {
        let steps = Arc::new(AtomicUsize::new(0));
        (
            ChannelTaskSpec::new(
                channel_id,
                format!("line-{channel_id}"),
                TestTask {
                    steps: Arc::clone(&steps),
                    fail_at,
                },
            ),
            steps,
        )
    }

    #[test]
    fn one_faulted_channel_does_not_stop_the_other_workers() {
        let mut specs = Vec::new();
        let mut counters = Vec::new();
        for channel_id in 1..=8 {
            let (channel, counter) = spec(channel_id, (channel_id == 3).then_some(3));
            specs.push(channel);
            counters.push(counter);
        }
        let workers = ChannelSupervisorSet::start(specs).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while (!workers.channels()[2].is_finished()
            || counters
                .iter()
                .enumerate()
                .any(|(index, value)| index != 2 && value.load(Ordering::SeqCst) < 20))
            && Instant::now() < deadline
        {
            std::thread::yield_now();
        }

        assert!(workers.channels()[2].is_finished());
        assert!(counters
            .iter()
            .enumerate()
            .all(|(index, value)| index == 2 || value.load(Ordering::SeqCst) >= 20));
        workers.request_stop_all().unwrap();
        let exits = workers.join_all();
        assert!(matches!(
            exits[2],
            Ok((3, ChannelWorkerExit::Faulted { .. }))
        ));
        assert!(exits.iter().enumerate().all(|(index, exit)| {
            index == 2 || matches!(exit, Ok((_, ChannelWorkerExit::Stopped { .. })))
        }));
    }

    #[test]
    fn stop_is_bounded_idempotent_and_returns_shutdown_evidence() {
        let (spec, counter) = spec(1, None);
        let workers = ChannelSupervisorSet::start(vec![spec]).unwrap();
        workers.request_stop_all().unwrap();
        workers.request_stop_all().unwrap();
        let exits = workers.join_all();
        match &exits[0] {
            Ok((1, ChannelWorkerExit::Stopped { shutdown, .. })) => {
                assert_eq!(*shutdown, counter.load(Ordering::SeqCst));
            }
            _ => panic!("expected an orderly worker stop"),
        }
    }

    #[test]
    fn invalid_worker_sets_are_rejected_before_threads_start() {
        assert!(matches!(
            ChannelSupervisorSet::<TestTask>::start(Vec::new()),
            Err(ChannelSupervisorError::InvalidChannelCount { actual: 0, .. })
        ));
        let (first, _) = spec(1, None);
        let (duplicate, _) = spec(1, None);
        assert!(matches!(
            ChannelSupervisorSet::start(vec![first, duplicate]),
            Err(ChannelSupervisorError::DuplicateChannelId(1))
        ));
    }
}
