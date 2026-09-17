// The `unixexec` transport runs a command, which needs one of the backends.
#![cfg(all(
    feature = "unixexec",
    not(target_os = "windows"),
    any(feature = "async-io", feature = "tokio")
))]

use ntest::timeout;
use test_log::test;

use zbus::{Result, block_on, conn::Builder};

#[test]
#[timeout(15000)]
fn unixexec_connection_async() {
    block_on(test_unixexec_connection()).unwrap();
}

/// A bus connection over the standard I/O of `systemd-stdio-bridge`.
///
/// A machine without that program has nothing to say about this, so the test passes there.
async fn test_unixexec_connection() -> Result<()> {
    let connection = match Builder::address("unixexec:path=systemd-stdio-bridge")
        .build()
        .await
    {
        Ok(connection) => connection,
        Err(zbus::Error::Connection(e, _)) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(());
        }
        Err(e) => return Err(e),
    };

    match connection
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "Hello",
            &(),
        )
        .await
    {
        Err(zbus::Error::MethodError(_, _, _)) => (),
        Err(e) => panic!("{}", e),

        _ => panic!(),
    };

    Ok(())
}
