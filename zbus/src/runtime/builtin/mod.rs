//! The runtime zbus brings along, for a connection that is handed no runtime of its own.
//!
//! It is made of three parts: a scheduler, which holds the connection's tasks and hands them out
//! to be polled; a reactor, which watches its sockets and keeps its timers; and a worker thread,
//! which runs the two.

mod scheduler;
