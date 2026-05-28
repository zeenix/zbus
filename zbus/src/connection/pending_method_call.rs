use std::{
    io::{self, ErrorKind},
    num::NonZeroU32,
    pin::Pin,
    task::{Context, Poll},
};

use futures_core::Future;
use ordered_stream::{OrderedFuture, OrderedStream, PollResult};

use crate::{Message, MessageStream, Result, message::Type};

/// A method call whose completion can be awaited or joined with other streams.
///
/// This is useful for cache population method calls, where joining the call with an update signal
/// stream can be used to ensure that cache updates are not overwritten by a cache population whose
/// task is scheduled later.
#[derive(Debug)]
pub(crate) struct PendingMethodCall {
    stream: Option<MessageStream>,
    serial: NonZeroU32,
}

impl PendingMethodCall {
    pub(super) fn new(stream: Option<MessageStream>, serial: NonZeroU32) -> Self {
        Self { stream, serial }
    }
}

impl Future for PendingMethodCall {
    type Output = Result<Message>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.poll_before(cx, None).map(|ret| {
            ret.map(|(_, r)| r).unwrap_or_else(|| {
                Err(crate::Error::InputOutput(
                    io::Error::new(ErrorKind::BrokenPipe, "socket closed").into(),
                ))
            })
        })
    }
}

impl OrderedFuture for PendingMethodCall {
    type Output = Result<Message>;
    type Ordering = zbus::message::Sequence;

    fn poll_before(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        before: Option<&Self::Ordering>,
    ) -> Poll<Option<(Self::Ordering, Self::Output)>> {
        let this = self.get_mut();
        if let Some(stream) = &mut this.stream {
            loop {
                match Pin::new(&mut *stream).poll_next_before(cx, before) {
                    Poll::Ready(PollResult::Item {
                        data: Ok(msg),
                        ordering,
                    }) => {
                        if msg.header().reply_serial() != Some(this.serial) {
                            continue;
                        }
                        let res = match msg.message_type() {
                            Type::Error => Err(msg.into()),
                            Type::MethodReturn => Ok(msg),
                            _ => continue,
                        };
                        this.stream = None;
                        return Poll::Ready(Some((ordering, res)));
                    }
                    Poll::Ready(PollResult::Item {
                        data: Err(e),
                        ordering,
                    }) => {
                        return Poll::Ready(Some((ordering, Err(e))));
                    }

                    Poll::Ready(PollResult::NoneBefore) => {
                        return Poll::Ready(None);
                    }
                    Poll::Ready(PollResult::Terminated) => {
                        return Poll::Ready(None);
                    }
                    Poll::Pending => return Poll::Pending,
                }
            }
        }
        Poll::Ready(None)
    }
}
