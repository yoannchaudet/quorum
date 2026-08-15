//! Cooperative cancellation for long-running Coordinator work.
//!
//! A [`CancelToken`] is a cheap, cloneable handle over a shared flag. A frontend
//! (CLI or future UX) holds one clone, hands another to the [`Coordinator`], and
//! flips it to request a graceful stop. The Coordinator checks the flag between
//! steps and threads it into agent runs, so an in-flight `copilot` process is
//! terminated promptly rather than only on timeout.
//!
//! Cancellation is cooperative and state-preserving: the Coordinator stops at the
//! last persisted step, so the work item can be resumed later.
//!
//! [`Coordinator`]: crate::coordinator::Coordinator

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A shared, cloneable cancellation flag. Cloning shares the same underlying
/// flag, so cancelling any clone cancels them all.
#[derive(Clone, Debug, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    /// A fresh token in the not-cancelled state.
    pub fn new() -> CancelToken {
        CancelToken {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Request cancellation. Idempotent.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clones_share_the_same_flag() {
        let a = CancelToken::new();
        let b = a.clone();
        assert!(!a.is_cancelled());
        assert!(!b.is_cancelled());
        b.cancel();
        assert!(a.is_cancelled());
        assert!(b.is_cancelled());
    }

    #[test]
    fn default_is_not_cancelled() {
        assert!(!CancelToken::default().is_cancelled());
    }
}
