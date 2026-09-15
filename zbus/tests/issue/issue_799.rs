use std::pin::pin;

use event_listener::Event;
use futures_util::future::{Either, select};
use test_log::test;
use tracing::instrument;
use zbus::block_on;

use zbus::Result;

#[instrument]
#[test]
fn concurrent_interface_methods() {
    // This is  test case for ensuring the regression of #799 doesn't come back.
    block_on(async {
        struct Iface(Event);

        #[zbus::interface(name = "org.zbus.test.issue799")]
        impl Iface {
            async fn method1(&self) {
                self.0.notify(1);
                // Never return
                std::future::pending::<()>().await;
            }

            async fn method2(&self) {}
        }

        let event = Event::new();
        let listener = event.listen();
        let iface = Iface(event);
        let conn = zbus::connection::Builder::session()
            .name("org.zbus.test.issue799")
            .serve_at("/org/zbus/test/issue799", iface)
            .build()
            .await
            .unwrap();

        #[zbus::proxy(
            default_service = "org.zbus.test.issue799",
            default_path = "/org/zbus/test/issue799",
            interface = "org.zbus.test.issue799"
        )]
        trait Iface {
            async fn method1(&self) -> Result<()>;
            async fn method2(&self) -> Result<()>;
        }

        let proxy = IfaceProxy::new(&conn).await.unwrap();
        let proxy_clone = proxy.clone();
        // `method1` never returns, so it is raced against the rest of the test rather than
        // awaited.
        let method1 = pin!(async move {
            proxy_clone.method1().await.unwrap();
        });
        let rest = pin!(async {
            // Wait till the `method1`` is called.
            listener.await;

            // Now while the `method1` is in progress, a call to `method2` should just work.
            proxy.method2().await.unwrap();
        });
        let done = select(method1, rest).await;
        assert!(matches!(done, Either::Right(..)), "`method1` returned");
    })
}
