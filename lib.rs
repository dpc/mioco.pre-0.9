// Copyright 2015-2016 Dawid Ciężarkiewicz <dpc@dpc.pw>
// See LICENSE-MPL2 file for more information.

//! # Mioco
//!
//! Scalable, coroutine-based, asynchronous IO handling library for Rust
//! programming language.
//!
//! Mioco uses asynchronous event loop, to cooperatively switch between
//! coroutines (aka. green threads), depending on data availability. You
//! can think of `mioco` as of *Node.js for Rust* or Rust *[green
//! threads][green threads] on top of [`mio`][mio]*.
//!
//! Mioco coroutines should not use any native blocking-IO operations.
//! Instead mioco provides it's own IO. Any long-running operations, or
//! blocking IO should be executed in `mioco::sync()` blocks.
//!
//! # <a name="features"></a> Features:
//!
//! ```norust
//! * multithreading support; (see `Config::set_thread_num()`)
//! * user-provided scheduling; (see `Config::set_scheduler()`);
//! * timers (see `MiocoHandle::timer()`);
//! * channels (see `sync::mpsc::channel()`);
//! * coroutine exit notification (see `CoroutineHandle::exit_notificator()`).
//! * synchronous operations support (see `MiocoHandle::sync()`).
//! * synchronization primitives (see `RwLock`).
//! ```
//!
//! # <a name="example"/></a> Example:
//!
//! See `examples/echo.rs` for an example TCP echo server:
//!


//! [green threads]: https://en.wikipedia.org/wiki/Green_threads
//! [mio]: https://github.com/carllerche/mio
//! [mio-api]: ../mioco/mio/index.html

#![feature(recover)]
#![feature(std_panic)]
#![feature(panic_propagate)]
#![feature(fnbox)]
#![feature(as_unsafe_cell)]
#![feature(reflect_marker)]
#![warn(missing_docs)]
#![allow(private_in_public)]

#![cfg_attr(feature="clippy", feature(plugin))]
#![cfg_attr(feature="clippy", plugin(clippy))]

#[cfg(test)]
extern crate env_logger;
#[cfg(test)]
extern crate net2;

extern crate thread_scoped;
extern crate libc;
extern crate spin;
extern crate mio as mio_orig;
extern crate context;
extern crate nix;
#[macro_use]
extern crate log;
extern crate time;
extern crate num_cpus;
extern crate slab;

#[macro_use]
mod src;

/// Some mio types, re-exported
pub mod mio {
    pub use super::mio_orig::{EventLoop, Handler, Ipv4Addr};
}

/// Custom scheduling
pub mod sched {
    pub use super::src::{Scheduler, SchedulerThread};
    pub use super::src::CoroutineControl as Coroutine;
}

pub use src::{timer, tcp, udp, sync, unix};

/*
pub mod sync {
    pub use super::src::sync;
}*/

pub use src::{Config, Event, EventSourceId, Handler, JoinHandle, MioAdapter, Mioco, RW, Evented};
pub use src::{get_userdata, set_userdata, set_children_userdata};
pub use src::{in_coroutine, select_wait, sleep, sleep_ms, spawn, start, shutdown};
pub use src::{start_threads, thread_num, yield_now};

#[cfg(test)]
mod tests;
