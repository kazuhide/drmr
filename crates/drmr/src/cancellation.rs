//! Register termination events before media work and join the signal monitor on exit.
use drmr_core::concurrency::Stop;
use std::{io, sync::mpsc, thread};
use tokio::sync::oneshot;

pub struct Cancellation {
    pub stop: Stop,
    shutdown: Option<oneshot::Sender<()>>,
    monitor: Option<thread::JoinHandle<()>>,
}

impl Cancellation {
    pub fn install() -> io::Result<Self> {
        let stop = Stop::new();
        let observed = stop.clone();
        let (shutdown, end) = oneshot::channel();
        let (ready, registered) = mpsc::sync_channel(1);
        let monitor = thread::Builder::new()
            .name("conversion-signals".into())
            .spawn(move || {
                let result = (|| -> io::Result<()> {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    runtime.block_on(async {
                        #[cfg(unix)]
                        let (mut interrupt, mut terminate) = {
                            use tokio::signal::unix::{SignalKind, signal};
                            (
                                signal(SignalKind::interrupt())?,
                                signal(SignalKind::terminate())?,
                            )
                        };
                        #[cfg(windows)]
                        let (mut interrupt, mut terminate) = (
                            tokio::signal::windows::ctrl_c()?,
                            tokio::signal::windows::ctrl_break()?,
                        );
                        // Registration is complete before the caller may spawn a tool.
                        ready
                            .send(Ok(()))
                            .map_err(|_| io::Error::other("signal monitor unavailable"))?;
                        tokio::select! {
                            _ = interrupt.recv() => observed.set(),
                            _ = terminate.recv() => observed.set(),
                            _ = end => {},
                        }
                        Ok(())
                    })
                })();
                if let Err(error) = result {
                    let _ = ready.send(Err(error));
                }
            })?;
        match registered.recv() {
            Ok(Ok(())) => Ok(Self {
                stop,
                shutdown: Some(shutdown),
                monitor: Some(monitor),
            }),
            result => {
                let _ = shutdown.send(());
                let _ = monitor.join();
                Err(match result {
                    Ok(Err(error)) => error,
                    _ => io::Error::other("signal monitor unavailable"),
                })
            }
        }
    }
}

impl Drop for Cancellation {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(monitor) = self.monitor.take() {
            let _ = monitor.join();
        }
    }
}
