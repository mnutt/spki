//! Bound blocking disk work without blocking the single-threaded RPC executor.
use anyhow::{Context, Result};
static SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

pub(crate) async fn run<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    // Reject overload instead of building an unbounded queue of bodies/closures.
    let permit = SLOTS
        .try_acquire()
        .context("storage busy; retry the request")?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
    .context("disk worker failed")?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(flavor = "current_thread")]
    async fn blocking_work_leaves_the_executor_responsive() {
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let task = tokio::spawn(run(move || {
            let _ = started.send(());
            wait.recv_timeout(std::time::Duration::from_secs(2))?;
            Ok(())
        }));
        ready.await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        release.send(()).unwrap();
        task.await.unwrap().unwrap();
    }
}
