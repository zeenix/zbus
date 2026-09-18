//! What a connection costs on the runtime it is built on: its setup and teardown, a method
//! call's round trip, a large body, a signal, the runtime's task spawn and its blocking hook.
//! Every benchmark runs over a unix socket pair with a p2p handshake, so nothing here depends on
//! a bus and everything goes through the runtime's readiness path, which an in-process channel
//! would bypass.
//!
//! Each id times its operations inside one `zbus::block_on`, the shape of a program that does all
//! of its work inside one call, on a pair built beforehand in a `block_on` of its own. The timing
//! goes through Criterion's async bencher, whose loop runs inside one `block_on`, because
//! CodSpeed's instrumentation skips `iter_custom`. `build-and-shutdown` and
//! `build-and-peer-credentials` need a fresh pair for each operation and, with no untimed async
//! setup to build it in, time its build as well: read them against `build-and-drop`.

#[cfg(unix)]
mod unix {
    use std::{future::Future, hint::black_box, os::unix::net::UnixStream, time::Duration};

    use criterion::{Criterion, Throughput, async_executor::AsyncExecutor, criterion_group};
    use futures_util::{StreamExt, lock::Mutex};
    use zbus::{Connection, Message, connection::Builder, message::Type};

    const PATH: &str = "/org/zbus/Benchmark";
    const INTERFACE: &str = "org.zbus.Benchmark";
    const BIG: usize = 1024 * 1024;

    /// Runs a routine's future to completion inside `zbus::block_on`.
    struct ZbusExecutor;

    impl AsyncExecutor for ZbusExecutor {
        fn block_on<T>(&self, future: impl Future<Output = T>) -> T {
            zbus::block_on(future)
        }
    }

    fn runtime(c: &mut Criterion) {
        let mut group = c.benchmark_group("connection");
        group.sample_size(20);
        group.bench_function("build-and-drop", |b| {
            b.to_async(ZbusExecutor)
                .iter(|| async { drop(black_box(pair().await)) });
        });
        group.bench_function("build-and-shutdown", |b| {
            b.to_async(ZbusExecutor).iter(|| async {
                let (server, client) = pair().await;
                futures_util::join!(server.graceful_shutdown(), client.graceful_shutdown());
            });
        });
        group.finish();

        let mut group = c.benchmark_group("method-call");
        {
            let (_server, client) = zbus::block_on(pair());
            group.bench_function("roundtrip", |b| {
                b.to_async(ZbusExecutor)
                    .iter(|| async { black_box(ping(&client, 1).await) });
            });
        }
        group.sample_size(10);
        group.throughput(Throughput::Bytes(BIG as u64));
        {
            let (_server, client) = zbus::block_on(pair());
            let body = vec![7u8; BIG];
            group.bench_function("1MiB-body", |b| {
                b.to_async(ZbusExecutor)
                    .iter(|| async { black_box(echo(&client, &body).await) });
            });
        }
        group.finish();

        let mut group = c.benchmark_group("signal");
        {
            let (server, client) = zbus::block_on(pair());
            let signals = zbus::block_on(async {
                zbus::MessageStream::for_match_rule(
                    zbus::MatchRule::builder()
                        .msg_type(Type::Signal)
                        .interface(INTERFACE)
                        .build()
                        .unwrap(),
                    &client,
                    None,
                )
                .await
                .unwrap()
            });
            // Shared behind an async lock: each routine's future needs the stream mutably across
            // an await, and cannot borrow it from the closure that makes it.
            let signals = Mutex::new(signals);
            group.bench_function("emit-receive", |b| {
                b.to_async(ZbusExecutor).iter(|| async {
                    server
                        .emit_signal(None::<()>, PATH, INTERFACE, "Tick", &())
                        .await
                        .unwrap();
                    black_box(signals.lock().await.next().await.unwrap().unwrap());
                });
            });
        }
        group.finish();

        let mut group = c.benchmark_group("spawn");
        group.throughput(Throughput::Elements(100));
        {
            let (_server, client) = zbus::block_on(pair());
            group.bench_function("100-tasks", |b| {
                b.to_async(ZbusExecutor).iter(|| async {
                    let tasks: Vec<_> = (0..100u32)
                        .map(|i| client.spawn("bench", async move { i }))
                        .collect();
                    for task in tasks {
                        black_box(task.await.unwrap());
                    }
                });
            });
        }
        group.finish();

        // The credentials are cached on the connection, so only a fresh pair pays the lookup
        // and the blocking hook behind it.
        let mut group = c.benchmark_group("blocking-hook");
        group.sample_size(20);
        group.bench_function("build-and-peer-credentials", |b| {
            b.to_async(ZbusExecutor).iter(|| async {
                let (_server, client) = pair().await;
                black_box(client.peer_creds().await.unwrap().clone());
            });
        });
        group.finish();
    }

    /// A server and a client over a fresh socket pair, handshake included.
    async fn pair() -> (Connection, Connection) {
        let (server_end, client_end) = UnixStream::pair().unwrap();
        let server = Builder::unix_stream(server_end)
            .server(zbus::Guid::generate())
            .p2p()
            .serve_at(PATH, Benchmark)
            .build();
        let client = Builder::unix_stream(client_end)
            .p2p()
            .method_timeout(Duration::from_secs(30))
            .build();
        futures_util::try_join!(server, client).unwrap()
    }

    async fn ping(client: &Connection, value: u32) -> Message {
        client
            .call_method(None::<()>, PATH, Some(INTERFACE), "Ping", &value)
            .await
            .unwrap()
    }

    async fn echo(client: &Connection, body: &[u8]) -> Message {
        client
            .call_method(None::<()>, PATH, Some(INTERFACE), "Echo", &body)
            .await
            .unwrap()
    }

    struct Benchmark;

    #[zbus::interface(name = "org.zbus.Benchmark")]
    impl Benchmark {
        fn ping(&self, value: u32) -> u32 {
            value
        }

        fn echo(&self, body: Vec<u8>) -> Vec<u8> {
            body
        }
    }

    criterion_group!(benches, runtime);
}

#[cfg(unix)]
criterion::criterion_main!(unix::benches);

#[cfg(not(unix))]
fn main() {}
