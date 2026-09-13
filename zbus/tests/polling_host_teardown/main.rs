//! What is left of a host once the connections it ran are gone.
//!
//! The runtime is the same single-threaded one the `polling_host` test uses, reached as the file
//! it lives in. A connection hands its runtime tasks, timers and sources, and each of those holds
//! the host's state; releasing the host has to give all of it back, or a process that outlives one
//! connection carries the whole of it for good. A peer-to-peer pair covers both ways a connection
//! ends: one side says goodbye, the other simply stops existing.
//!
//! What a task produced is the host's to release as well, which is what the other test here is
//! about: such a value is free to reach back into the host as it goes.
#![cfg(all(unix, feature = "p2p"))]

#[path = "../polling_host/host.rs"]
mod host;

use std::{
    os::unix::net::UnixStream,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use ntest::timeout;
use zbus::{Guid, connection::Builder, runtime::traits};

use host::{Handle, Host};

/// Nothing of two finished connections outlives the host that ran them.
#[test]
#[timeout(15000)]
fn a_released_host_gives_back_everything_its_connections_took() {
    let host = Host::new().unwrap();
    let probe = host.probe();
    let (server_end, client_end) = UnixStream::pair().unwrap();

    host.run(async {
        // Both ends of the handshake run on this one thread, so neither can be awaited first.
        let (server, client) = futures_util::try_join!(
            Builder::unix_stream(server_end)
                .server(Guid::generate())
                .p2p()
                .runtime(host.handle())
                .build(),
            Builder::unix_stream(client_end)
                .p2p()
                .runtime(host.handle())
                .build(),
        )
        .unwrap();

        // The client leaves without a word, so its tasks are cancelled rather than run out.
        drop(client);
        server.graceful_shutdown().await;
    });

    assert_eq!(host.pending_timers(), 0, "a connection left a timer behind");

    drop(host);
    assert!(
        probe.is_released(),
        "the host's state outlived it: something it handed out still holds it",
    );
}

/// A detached task that has ended is released outside the list the host keeps it in.
///
/// Releasing it releases what it produced, and such a value's destructor is free to do what any
/// other one may: [`traits::TaskHandle::detach`] on a task spawned through the host's own handle
/// is one thing a connection's values do. That reaches for the same list, so a host that dropped
/// a finished task while holding it would wait on itself and never come back out of `run`.
#[test]
#[timeout(15000)]
fn a_finished_detached_task_is_released_outside_the_hosts_list() {
    let host = Host::new().unwrap();
    let probe = host.probe();
    let spawned_from_drop = Arc::new(AtomicBool::new(false));

    host.run(async {
        let output = SpawnsOnDrop {
            handle: host.handle(),
            spawned: Arc::clone(&spawned_from_drop),
        };
        // Detached, so nothing but the host's run loop can notice that this task has ended and
        // let the value it produced go.
        traits::TaskHandle::detach(traits::Runtime::spawn(
            &host.handle(),
            "a task with an output",
            async move { output },
        ));

        // One turn of the loop ends the task above and releases its output, and the task that
        // output spawned runs on another; both are turns this wait leaves room for.
        while !spawned_from_drop.load(Ordering::Acquire) {
            futures_lite::future::yield_now().await;
        }
    });

    drop(host);
    assert!(
        probe.is_released(),
        "the host's state outlived it: something it handed out still holds it",
    );
}

/// A value that spawns a task on the host as it goes, standing in for the connection values
/// whose destructors do the same.
struct SpawnsOnDrop {
    handle: Handle,
    // Set by the spawned task, so the test can wait for the whole chain rather than for a turn
    // count of the loop.
    spawned: Arc<AtomicBool>,
}

impl Drop for SpawnsOnDrop {
    fn drop(&mut self) {
        let spawned = Arc::clone(&self.spawned);

        traits::TaskHandle::detach(traits::Runtime::spawn(
            &self.handle,
            "a task spawned from a drop",
            async move { spawned.store(true, Ordering::Release) },
        ));
    }
}
