use std::{
    fs::{File, OpenOptions},
    io::Write as _,
    path::Path,
};

use harness_runtime::{RuntimeEvent, RuntimeEventBus};
use tokio::sync::{broadcast, oneshot};

use crate::error::CliError;

pub struct RuntimeEventLog {
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<Result<(), CliError>>,
}

impl RuntimeEventLog {
    pub fn start(path: &Path, events: &RuntimeEventBus) -> Result<Self, CliError> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|source| CliError::Io {
                context: "creating runtime event log directory",
                source: Box::new(source),
            })?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|source| CliError::Io {
                context: "opening runtime event log",
                source: Box::new(source),
            })?;
        let path = path.to_path_buf();
        let mut receiver = events.subscribe();
        let (stop, mut stop_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut file = file;
            loop {
                tokio::select! {
                    _ = &mut stop_rx => {
                        drain_ready(&mut receiver, &mut file, &path)?;
                        flush(&mut file)?;
                        return Ok(());
                    }
                    received = receiver.recv() => {
                        match received {
                            Ok(event) => write_event(&mut file, &event, &path)?,
                            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                                return Err(CliError::ObservabilityLagged { skipped });
                            }
                            Err(broadcast::error::RecvError::Closed) => {
                                flush(&mut file)?;
                                return Ok(());
                            }
                        }
                    }
                }
            }
        });
        Ok(Self {
            stop: Some(stop),
            task,
        })
    }

    pub async fn stop(mut self) -> Result<(), CliError> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.task
            .await
            .map_err(|source| CliError::ObservabilityTask(Box::new(source)))?
    }
}

fn drain_ready(
    receiver: &mut broadcast::Receiver<RuntimeEvent>,
    file: &mut File,
    path: &Path,
) -> Result<(), CliError> {
    loop {
        match receiver.try_recv() {
            Ok(event) => write_event(file, &event, path)?,
            Err(broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed) => {
                return Ok(());
            }
            Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                return Err(CliError::ObservabilityLagged { skipped });
            }
        }
    }
}

fn write_event(file: &mut File, event: &RuntimeEvent, path: &Path) -> Result<(), CliError> {
    let mut line = serde_json::to_vec(event).map_err(|source| CliError::RuntimeEventSerialize {
        path: path.to_path_buf(),
        source: Box::new(source),
    })?;
    line.push(b'\n');
    file.write_all(&line).map_err(|source| CliError::Io {
        context: "writing runtime event log",
        source: Box::new(source),
    })?;
    Ok(())
}

fn flush(file: &mut File) -> Result<(), CliError> {
    file.flush().map_err(|source| CliError::Io {
        context: "flushing runtime event log",
        source: Box::new(source),
    })
}
