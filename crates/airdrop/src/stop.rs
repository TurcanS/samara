//! Ctrl-C / SIGTERM as an explicit value. The first signal asks the running command to
//! stop; it then runs its own cleanup (mDNS goodbye, helper teardown, Wi-Fi reconnect,
//! partial files removed) to the end. A later signal only says that cleanup is running.

use std::time::{Duration, Instant};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::watch;

/// The command was stopped by the user (exit code 130).
#[derive(Debug)]
pub struct Interrupted;

#[derive(Clone)]
pub struct Stop(watch::Receiver<bool>, std::sync::Arc<watch::Sender<bool>>);

impl Stop {
    /// Install the SIGINT/SIGTERM handlers.
    pub fn install() -> Self {
        let (tx, rx) = watch::channel(false);
        let tx = std::sync::Arc::new(tx);
        let own = tx.clone();
        tokio::spawn(async move {
            let (Ok(mut int), Ok(mut term)) = (signal(SignalKind::interrupt()), signal(SignalKind::terminate())) else {
                return;
            };
            let mut first: Option<Instant> = None;
            loop {
                tokio::select! {
                    _ = int.recv() => {}
                    _ = term.recv() => {}
                }
                match first {
                    None => {
                        first = Some(Instant::now());
                        let _ = tx.send(true);
                    }
                    // not for a double delivery of one Ctrl-C
                    Some(t) if t.elapsed() > Duration::from_secs(1) => {
                        eprintln!("Still stopping (restoring Wi-Fi), please wait...");
                    }
                    Some(_) => {}
                }
            }
        });
        Self(rx, own)
    }

    /// Stop as a signal would (`--json`: the driving program said `done` or went away).
    pub fn request(&self) {
        let _ = self.1.send(true);
    }

    pub fn requested(&self) -> bool {
        *self.0.borrow()
    }

    /// Resolves once a stop was asked for.
    pub async fn wait(&self) {
        let mut rx = self.0.clone();
        let _ = rx.wait_for(|&s| s).await;
    }

    /// Run `fut` unless a stop comes first.
    pub async fn guard<T>(&self, fut: impl std::future::Future<Output = T>) -> Result<T, Interrupted> {
        tokio::select! {
            v = fut => Ok(v),
            () = self.wait() => Err(Interrupted),
        }
    }
}
