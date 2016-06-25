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
//! Mioco API mimics Rust standard library threading API, so it's easy
//! convert existing code to Mioco.
//!
//! Mioco coroutines should not use any native blocking-IO operations.  Any
//! long-running operations, or blocking IO should be executed in
//! `mioco::sync()` blocks.
//!
//! # <a name="features"></a> Features:
//!
//! ```norust
//! * multithreading support; (see `Config::set_thread_num()`)
//! * timers (see `timer` module);
//! * coroutine exit notification (see `JoinHandle`).
//! * synchronous operations support (see `mioco::offload()`).
//! * synchronization primitives (see `sync` module):
//!   * channels (see `sync::mpsc::channel()`);
//!   * support for synchronization with native environment (outside of Mioco instance)
//! * user-provided scheduling; (see `Config::set_scheduler()`);
//! ```
//!
//! # <a name="example"/></a> Example:
//!
//! See `examples/echo.rs` for an example TCP echo server
//!


//! [green threads]: https://en.wikipedia.org/wiki/Green_threads
//! [mio]: https://github.com/carllerche/mio
//! [mio-api]: ../mioco/mio/index.html

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
extern crate num_cpus;
extern crate slab;
extern crate owning_ref;

#[macro_use]
mod src;

/// Some mio types that are part of mioco-API, re-exported
pub mod mio {
    pub use super::mio_orig::{EventLoop, Handler, Ipv4Addr};
}

/// Custom scheduling
pub mod sched {
    pub use super::src::{Scheduler, SchedulerThread};
    pub use super::src::CoroutineControl as Coroutine;
}

#[cfg(not(windows))]
pub use src::unix;

pub use src::{timer, tcp, udp, sync};

pub use src::{Config, Event, EventSourceId, Handler, JoinHandle, MioAdapter, Mioco, RW, Evented};
pub use src::{get_userdata, set_userdata, set_children_userdata};
pub use src::{in_coroutine, select_wait, sleep, sleep_ms, spawn, start, shutdown, offload};
pub use src::{start_threads, thread_num, yield_now};

#[cfg(test)]
mod tests;
