use core::fmt;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use nix::sys::signal::{pthread_sigmask, SigSet, SigmaskHow, Signal};

/// Receives process termination requests without an asynchronous signal
/// handler. Signals are blocked before channel workers are spawned, inherited
/// by those workers, and synchronously consumed by one dedicated thread.
pub struct TerminationSignalReceiver {
    receiver: Receiver<Signal>,
}

impl TerminationSignalReceiver {
    pub fn install() -> Result<Self, TerminationSignalError> {
        let mut signals = SigSet::empty();
        signals.add(Signal::SIGINT);
        signals.add(Signal::SIGTERM);
        pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&signals), None)
            .map_err(TerminationSignalError::Mask)?;

        let (sender, receiver) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("termination-signal".to_owned())
            .spawn(move || loop {
                match signals.wait() {
                    Ok(signal) => {
                        // One pending notification is sufficient. Continue
                        // consuming signals so a repeated SIGTERM cannot be
                        // left pending while controlled shutdown is running.
                        let _ = sender.try_send(signal);
                    }
                    Err(_) => return,
                }
            })
            .map_err(TerminationSignalError::Spawn)?;
        Ok(Self { receiver })
    }

    pub fn try_receive(&self) -> Result<Option<Signal>, TerminationSignalError> {
        match self.receiver.try_recv() {
            Ok(signal) => Ok(Some(signal)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(TerminationSignalError::Disconnected),
        }
    }
}

#[derive(Debug)]
pub enum TerminationSignalError {
    Mask(nix::Error),
    Spawn(std::io::Error),
    Disconnected,
}

impl fmt::Display for TerminationSignalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "termination signal error: {self:?}")
    }
}

impl std::error::Error for TerminationSignalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Mask(source) => Some(source),
            Self::Spawn(source) => Some(source),
            Self::Disconnected => None,
        }
    }
}
