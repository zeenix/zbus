//! Compile-time checks that enabling another runtime feature does not replace a stream builder.

use zbus::connection::Builder;

#[cfg(all(unix, feature = "async-io"))]
type AsyncIoUnixStream = std::os::unix::net::UnixStream;
#[cfg(all(windows, feature = "async-io"))]
type AsyncIoUnixStream = uds_windows::UnixStream;

#[test]
fn unix_stream_builder_signatures_are_additive() {
    #[cfg(all(any(unix, windows), feature = "async-io"))]
    {
        let _: fn(AsyncIoUnixStream) -> Builder<'static> = Builder::async_io_unix_stream;
        #[cfg(feature = "blocking-api")]
        let _: fn(AsyncIoUnixStream) -> zbus::blocking::connection::Builder<'static> =
            zbus::blocking::connection::Builder::async_io_unix_stream;
    }

    #[cfg(all(unix, feature = "tokio"))]
    {
        let _: fn(tokio::net::UnixStream) -> Builder<'static> = Builder::tokio_unix_stream;
        #[cfg(feature = "blocking-api")]
        let _: fn(tokio::net::UnixStream) -> zbus::blocking::connection::Builder<'static> =
            zbus::blocking::connection::Builder::tokio_unix_stream;
    }
}
