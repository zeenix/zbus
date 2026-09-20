//! A burst of concurrent method calls between two connections, each inside a `zbus::block_on`
//! on a thread of its own: the server on a thread the benchmark starts, the client on the
//! benchmark's thread, the way a client and the service it talks to never share a thread. One id
//! runs over an in-process channel, the other over a unix socket pair; the transport is the only
//! difference between the two, so the second id also measures the runtime's readiness path.
//!
//! The bursts are timed inside a `zbus::block_on` on the benchmark's thread, through Criterion's
//! async bencher, because CodSpeed's instrumentation skips `iter_custom`. The server and the
//! client are built once per id beforehand, and only the bursts are timed.

use std::{future::Future, hint::black_box, thread, time::Duration};

use criterion::{
    BenchmarkGroup, Criterion, async_executor::AsyncExecutor, criterion_group, criterion_main,
    measurement::Measurement,
};
use futures_util::StreamExt;
use zbus::{Guid, Message, MessageStream, connection::Builder};

/// Runs a routine's future to completion inside `zbus::block_on`.
struct ZbusExecutor;

impl AsyncExecutor for ZbusExecutor {
    fn block_on<T>(&self, future: impl Future<Output = T>) -> T {
        zbus::block_on(future)
    }
}

fn concurrent_method_calls(c: &mut Criterion) {
    let mut group = c.benchmark_group("method-call");
    group.sample_size(10);
    group.throughput(criterion::Throughput::Elements(
        CONCURRENT_METHOD_CALLS as u64,
    ));

    run_burst_bench(&mut group, "1000-concurrent-p2p", channel_pair());
    #[cfg(unix)]
    run_burst_bench(&mut group, "1000-concurrent-p2p-socket", socket_pair());

    group.finish();
}

/// Builds the server on a thread of its own and the client on the caller's, benches one burst of
/// concurrent calls per iteration, then drops the client and joins the server thread.
///
/// The server's `block_on` runs until the client hangs up, which is when its message stream
/// ends.
fn run_burst_bench<M>(
    group: &mut BenchmarkGroup<'_, M>,
    id: &str,
    (server, client): (Builder<'static>, Builder<'static>),
) where
    M: Measurement,
{
    let server_thread = thread::spawn(move || {
        zbus::block_on(async move {
            let connection = server
                .serve_at(BENCHMARK_PATH, BenchmarkInterface)
                .build()
                .await
                .unwrap();
            let mut messages = MessageStream::from(&connection);
            while messages.next().await.is_some() {}
        })
    });
    let client = zbus::block_on(async move {
        client
            .method_timeout(Duration::from_secs(30))
            .build()
            .await
            .unwrap()
    });

    group.bench_function(id, |b| {
        b.to_async(ZbusExecutor)
            .iter(|| call_ping_concurrently(&client));
    });

    drop(client);
    server_thread.join().unwrap();
}

/// Both ends of an in-process channel, with the handshake skipped.
fn channel_pair() -> (Builder<'static>, Builder<'static>) {
    let (server_socket, client_socket) = zbus::connection::socket::Channel::pair();
    let guid = Guid::generate();
    (
        Builder::authenticated_socket(server_socket, guid.clone()).p2p(),
        Builder::authenticated_socket(client_socket, guid).p2p(),
    )
}

/// Both ends of a unix socket pair, handshake included.
#[cfg(unix)]
fn socket_pair() -> (Builder<'static>, Builder<'static>) {
    let (server_end, client_end) = std::os::unix::net::UnixStream::pair().unwrap();
    (
        Builder::unix_stream(server_end)
            .server(Guid::generate())
            .p2p(),
        Builder::unix_stream(client_end).p2p(),
    )
}

async fn call_ping_concurrently(client: &zbus::Connection) {
    let replies = futures_util::future::try_join_all(
        (0..CONCURRENT_METHOD_CALLS).map(|value| call_ping(client, value as u32)),
    )
    .await
    .unwrap();
    black_box(replies);
}

async fn call_ping(client: &zbus::Connection, value: u32) -> zbus::Result<Message> {
    client
        .call_method(
            None::<()>,
            BENCHMARK_PATH,
            Some(BENCHMARK_INTERFACE),
            "Ping",
            &value,
        )
        .await
}

struct BenchmarkInterface;

#[zbus::interface(name = "org.zbus.Benchmark")]
impl BenchmarkInterface {
    async fn ping(&self, value: u32) -> u32 {
        value
    }
}

const CONCURRENT_METHOD_CALLS: usize = 1000;
const BENCHMARK_PATH: &str = "/org/zbus/Benchmark";
const BENCHMARK_INTERFACE: &str = "org.zbus.Benchmark";

criterion_group!(benches, concurrent_method_calls);
criterion_main!(benches);
