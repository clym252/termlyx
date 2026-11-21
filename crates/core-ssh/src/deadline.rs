//! A connect deadline that stops counting while a human is being asked
//! something (host key verifier, auth prompter).

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::Instant;

#[derive(Debug, Clone, Copy)]
struct State {
    /// When the budget runs out, if nothing pauses it again.
    deadline: Instant,
    /// Set while at least one human wait is outstanding.
    paused_since: Option<Instant>,
    /// Number of outstanding human waits.
    depth: u32,
}

/// Bounds network waits by a budget that is suspended while a [`pause`]
/// guard is alive.
///
/// [`pause`]: HumanDeadline::pause
#[derive(Debug)]
pub(crate) struct HumanDeadline {
    state: watch::Sender<State>,
}

/// The connect deadline ran out.
#[derive(Debug)]
pub(crate) struct Elapsed;

impl HumanDeadline {
    pub(crate) fn new(budget: Duration) -> Arc<Self> {
        Arc::new(Self {
            state: watch::Sender::new(State {
                deadline: Instant::now() + budget,
                paused_since: None,
                depth: 0,
            }),
        })
    }

    /// Suspends the deadline until the returned guard is dropped. Guards
    /// nest; the deadline resumes when the last one goes, pushed back by the
    /// time spent paused.
    pub(crate) fn pause(self: &Arc<Self>) -> PauseGuard {
        self.state.send_modify(|s| {
            if s.depth == 0 {
                s.paused_since = Some(Instant::now());
            }
            s.depth += 1;
        });
        PauseGuard(self.clone())
    }

    /// Runs `fut` to completion unless the (pausable) deadline passes first.
    pub(crate) async fn run<F: Future>(&self, fut: F) -> Result<F::Output, Elapsed> {
        let mut rx = self.state.subscribe();
        let mut fut = std::pin::pin!(fut);
        loop {
            let s = *rx.borrow_and_update();
            if s.paused_since.is_some() {
                // Wait for the human (or for `fut` to finish without them).
                tokio::select! {
                    out = &mut fut => return Ok(out),
                    _ = rx.changed() => continue,
                }
            }
            tokio::select! {
                out = &mut fut => return Ok(out),
                // The deadline may have moved; re-read it.
                _ = rx.changed() => continue,
                () = tokio::time::sleep_until(s.deadline) => {
                    if self.state.borrow().paused_since.is_none() {
                        return Err(Elapsed);
                    }
                }
            }
        }
    }
}

/// Keeps a [`HumanDeadline`] paused while alive.
#[must_use = "the deadline resumes as soon as the guard is dropped"]
pub(crate) struct PauseGuard(Arc<HumanDeadline>);

impl Drop for PauseGuard {
    fn drop(&mut self) {
        self.0.state.send_modify(|s| {
            s.depth -= 1;
            if s.depth == 0
                && let Some(since) = s.paused_since.take()
            {
                s.deadline += since.elapsed();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn times_out_after_budget() {
        let d = HumanDeadline::new(Duration::from_secs(2));
        let start = Instant::now();
        let r = d.run(std::future::pending::<()>()).await;
        assert!(r.is_err());
        assert_eq!(start.elapsed(), Duration::from_secs(2));
    }

    #[tokio::test(start_paused = true)]
    async fn human_time_is_not_counted() {
        let d = HumanDeadline::new(Duration::from_secs(2));
        let start = Instant::now();
        let work = {
            let d = d.clone();
            async move {
                tokio::time::sleep(Duration::from_secs(1)).await; // network
                {
                    let _g = d.pause();
                    let _nested = d.pause();
                    tokio::time::sleep(Duration::from_secs(60)).await; // human
                }
                tokio::time::sleep(Duration::from_millis(900)).await; // network
                "done"
            }
        };
        assert_eq!(d.run(work).await.unwrap(), "done");
        assert_eq!(start.elapsed(), Duration::from_millis(61_900));
    }

    #[tokio::test(start_paused = true)]
    async fn network_time_after_a_pause_still_counts() {
        let d = HumanDeadline::new(Duration::from_secs(2));
        let start = Instant::now();
        let work = {
            let d = d.clone();
            async move {
                tokio::time::sleep(Duration::from_secs(1)).await;
                drop(d.pause()); // an instant answer
                std::future::pending::<()>().await;
            }
        };
        assert!(d.run(work).await.is_err());
        assert_eq!(start.elapsed(), Duration::from_secs(2));
    }

    #[tokio::test(start_paused = true)]
    async fn budget_spent_before_the_pause_is_kept() {
        // 1.5 s of network, a 10 s human wait, then the network hangs: the
        // remaining 0.5 s of budget elapses after the pause.
        let d = HumanDeadline::new(Duration::from_secs(2));
        let start = Instant::now();
        let work = {
            let d = d.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(1500)).await;
                {
                    let _g = d.pause();
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
                std::future::pending::<()>().await;
            }
        };
        assert!(d.run(work).await.is_err());
        assert_eq!(start.elapsed(), Duration::from_secs(12));
    }
}
