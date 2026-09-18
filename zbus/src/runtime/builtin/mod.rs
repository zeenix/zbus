//! The runtime zbus brings along, for a connection that is handed no runtime of its own.
//!
//! It is made of three parts: a scheduler, which holds the connection's tasks and hands them out
//! to be polled; a reactor, which watches its sockets and keeps its timers; and a worker thread,
//! which runs the two.

mod poll;
mod reactor;
mod scheduler;

/// The thread a runtime runs its scheduler and its reactor on.
mod worker {
    /// Whether the calling thread is a runtime's worker.
    pub(super) fn on_worker_thread() -> bool {
        false
    }
}
