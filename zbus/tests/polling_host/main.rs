//! A whole connection lifecycle on a runtime that is nothing but one thread.
//!
//! The `host` module is a `traits::Runtime` over the `polling` crate: readiness, timers and
//! tasks, all driven by the thread that calls `Host::run`. A connection built on it does
//! everything it does on that one thread, which is what the thread count here checks.
//!
//! This binary holds one test on purpose. The count is of the whole process, so a second test
//! sharing the process could start or finish a thread of the harness's between the two readings.
#![cfg(all(unix, feature = "proxy", feature = "service"))]

mod host;

use std::time::Duration;

use ntest::timeout;
use zbus::{connection::Builder, proxy::CacheProperties};

use host::Host;

/// A connection built, served on, called through and shut down without a thread being started.
#[test]
#[timeout(15000)]
fn a_single_threaded_host_runs_a_connection_without_zbus_threads() {
    let host = Host::new().unwrap();
    let handle = host.handle();
    let probe = host.probe();
    let before = threads();

    host.run(async {
        let conn = Builder::session()
            .runtime(handle)
            .method_timeout(Duration::from_secs(1))
            .serve_at("/org/zbus/PollingHost", Greeter)
            .build()
            .await
            .unwrap();

        let name = conn
            .unique_name()
            .expect("a bus connection has a unique name")
            .to_string();
        let proxy = GreeterProxy::builder(&conn)
            .destination(name)
            .cache_properties(CacheProperties::No)
            .build()
            .await
            .unwrap();
        assert_eq!(proxy.greet("host").await.unwrap(), "Hello host!");

        // A shutdown waits for every clone of the connection, the proxy's included.
        drop(proxy);
        conn.graceful_shutdown().await;
    });

    assert_eq!(
        threads(),
        before,
        "the connection's lifecycle started a thread",
    );
    assert_eq!(
        host.pending_timers(),
        0,
        "the lifecycle left a timer behind"
    );

    drop(host);
    assert!(
        probe.is_released(),
        "the host's state outlived it: something it handed out still holds it",
    );
}

/// How many threads the process has.
///
/// Only Linux is asked for it. Elsewhere the lifecycle still runs, with nothing to compare.
#[cfg(target_os = "linux")]
fn threads() -> usize {
    std::fs::read_dir("/proc/self/task")
        .expect("the thread directory of a running process")
        .count()
}

#[cfg(not(target_os = "linux"))]
fn threads() -> usize {
    0
}

/// An interface that answers.
struct Greeter;

#[zbus::interface(
    interface = "org.zbus.PollingHost.Greeter",
    proxy(default_path = "/org/zbus/PollingHost")
)]
impl Greeter {
    async fn greet(&self, name: &str) -> String {
        format!("Hello {name}!")
    }
}
