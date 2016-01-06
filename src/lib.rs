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
//! * mailboxes (see `mailbox()`);
//! * coroutine exit notification (see `CoroutineHandle::exit_notificator()`).
//! * synchronous operations support (see `MiocoHandle::sync()`).
//! * synchronization primitives (see `RwLock`).
//! ```
//!
//! # <a name="example"/></a> Example:
//!
//! See `examples/echo.rs` for an example TCP echo server:
//!
/*!
```
// MAKE_DOC_REPLACEME
```
*/
//! [green threads]: https://en.wikipedia.org/wiki/Green_threads
//! [mio]: https://github.com/carllerche/mio
//! [mio-api]: ../mioco/mio/index.html

#![cfg_attr(test, feature(convert))]
#![feature(recover)]
#![feature(fnbox)]
#![feature(cell_extras)]
#![feature(as_unsafe_cell)]
#![feature(reflect_marker)]
#![warn(missing_docs)]
#![allow(private_in_public)]

#[cfg(test)]
extern crate env_logger;

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

/// Re-export of some `mio` symbols.
pub mod mio {
    pub use super::mio_orig::{EventLoop, Handler, Ipv4Addr};
}

use std::any::Any;
use std::cell::{RefCell, Ref};
use std::rc::Rc;
use std::io;
use std::thread;
use std::marker::Reflect;
use std::mem::{self, transmute};

use std::boxed::FnBox;

use mio_orig::{TryRead, TryWrite, TryAccept, Token, EventLoop, EventSet, EventLoopConfig};

use std::collections::VecDeque;
use std::sync::{Arc};
use std::sync::atomic::{AtomicUsize, Ordering};

use slab::{Slab};

use context::{Context, Stack};


use std::ptr;

use timer::Timer;

/// Useful synchronization primitives
pub mod sync;
/// Timers
pub mod timer;
/// Unix sockets IO
pub mod unix;
/// TCP IO
pub mod tcp;
/// UDP IO
pub mod udp;
/// Mailboxes
pub mod mail;

use Message::*;

type RcCoroutine = Rc<RefCell<Coroutine>>;
type RcHandlerShared = Rc<RefCell<HandlerShared>>;
type ArcHandlerThreadShared = Arc<HandlerThreadShared>;

/// Read/Write/Both
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
// TODO: Make private again
pub struct RW {
    read : bool,
    write : bool,
}

impl RW {
    /// Read.
    pub fn read() -> Self {
        RW {
            read: true,
            write: false,
        }
    }

    /// Write
    pub fn write() -> Self {
        RW {
            read: false,
            write: true,
        }
    }

    /// Read + Write
    pub fn both() -> Self {
        RW {
            read: true,
            write: true,
        }
    }

    /// None.
    fn none() -> Self {
        RW {
            read: false,
            write: false,
        }
    }

    fn as_tuple(&self) -> (bool, bool) {
        (self.read, self.write)
    }

    fn has_read(&self) -> bool {
        self.read
    }

    fn has_write(&self) -> bool {
        self.write
    }
}


/// Event delivered to the coroutine
///
/// Read and/or Write + event source ID
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Event {
    id : EventSourceId,
    rw : RW,
}

impl Event {
    /// Index of the EventedShared handle
    pub fn id(&self) -> EventSourceId {
        self.id
    }

    /// Was the event a read
    pub fn has_read(&self) -> bool {
        self.rw.has_read()
    }

    /// Was the event a write
    pub fn has_write(&self) -> bool {
        self.rw.has_write()
    }
}

/// Coroutine exit status (value returned or panic)
#[derive(Clone, Debug)]
pub enum ExitStatus {
    /// Coroutine panicked
    Panic,
    /// Killed externally
    Killed,
    /// Coroutine returned some value
    Exit(Arc<io::Result<()>>)
}

impl ExitStatus {
    /// Is the `ExitStatus` a `Panic`?
    pub fn is_panic(&self) -> bool {
        match *self {
            ExitStatus::Panic => true,
            _ => false,
        }
    }
}

/// State of `mioco` coroutine
#[derive(Clone, Debug)]
enum State {
    /// Blocked on EventSource(s)
    Blocked,
    /// Yielding
    Yielding,
    /// Currently running
    Running,
    /// Ready to be started
    Ready,
    /// Finished
    Finished(ExitStatus),
}

impl State {
    /// Is the `State` `Ready`?
    fn is_ready(&self) -> bool {
        match *self {
            State::Ready => true,
            _ => false,
        }
    }

    /// Is the `State` `Running`?
    fn is_running(&self) -> bool {
        match *self {
            State::Running => true,
            _ => false,
        }
    }

    /// Is the `State` `Blocked`?
    fn is_blocked(&self) -> bool {
        match *self {
            State::Blocked => true,
            _ => false,
        }
    }

    /// Is the `State` `Yielding`?
    fn is_yielding(&self) -> bool {
        match *self {
            State::Yielding => true,
            _ => false,
        }
    }
}

/// Sends notify `Message` to the mioco Event Loop.
type MioSender = mio_orig::Sender<<Handler as mio_orig::Handler>::Message>;

mod prv {
    use super::{EventedInner, RcEvented, RW, tl_coroutine_current, State};
    use super::{coroutine_jump_out, entry_point, EventSourceId};

    pub trait EventedPrv {
        type Raw : EventedInner+'static;

        fn shared(&self) -> &RcEvented<Self::Raw>;

        /// Mark the `EventSourceRef` blocked and block until `Handler` does
        /// not wake us up again.
        fn block_on(&self, rw : RW) {
            let coroutine = tl_coroutine_current();
            {
                trace!("Coroutine({}): blocked on {:?}", coroutine.id.as_usize(), rw);
                coroutine.state = State::Blocked;
                coroutine.io.insert_with(|id| {
                    self.shared().0.borrow_mut().common.id = Some(id);
                    self.shared().0.borrow_mut().common.blocked_on = rw;
                    self.shared().to_trait()
                });
            };
            coroutine_jump_out(&coroutine.self_rc.as_ref().unwrap());
            {
                entry_point(&coroutine.self_rc.as_ref().unwrap());
                trace!("Coroutine({}): resumed due to event {:?}", coroutine.id.as_usize(), coroutine.last_event);
                debug_assert!(rw.has_read() || coroutine.last_event.has_write());
                debug_assert!(rw.has_write() || coroutine.last_event.has_read());
                debug_assert!(coroutine.last_event.id().as_usize() == self.shared().0.borrow().common.id.unwrap().as_usize());
            }
        }

        /// Index id of a `EventSource`
        fn id(&self) -> EventSourceId {
            self.shared().0.borrow().common.id.unwrap()
        }
    }
}


/// Mioco event source.
///
/// All types used as asynchronous event sources implement this trait.
pub trait Evented : prv::EventedPrv {
    /// Temporary ID used in select operation.
    fn id(&self) -> EventSourceId {
        prv::EventedPrv::id(self)
    }

    /// Add event source to next select operation.
    ///
    /// Use `select!` macro instead.
    ///
    /// This is unsafe as the `Select::wait()` has to be called afterwards,
    /// and registered EventSource must not be send to different
    /// thread/coroutine before `Select::wait()`.
    ///
    /// Use `select!` macro instead.
    unsafe fn select_add(&self, rw : RW) {
        let coroutine = tl_coroutine_current();

        if !coroutine.io.has_remaining() {
            let count = coroutine.io.count();
            coroutine.io.grow(count);
        }

        coroutine.io.insert_with(|id| {
            self.shared().0.borrow_mut().common.id = Some(id);
            self.shared().0.borrow_mut().common.blocked_on = rw;
            self.shared().to_trait()
        }).unwrap();
    }
}

/// Inner functions for Evented types
trait EventedInner {
    /// Register
    fn register(&self, event_loop : &mut EventLoop<Handler>, token : Token, interest : EventSet);

    /// Reregister
    fn reregister(&self, event_loop : &mut EventLoop<Handler>, token : Token, interest : EventSet);

    /// Deregister
    fn deregister(&self, event_loop : &mut EventLoop<Handler>, token : Token);

    /// Should the coroutine be resumed on event for this `EventSource<Self>`
    fn should_resume(&self) -> bool;
}

impl<T> EventedInner for T
where T : mio_orig::Evented {
    fn register(&self, event_loop : &mut EventLoop<Handler>, token : Token, interest : EventSet) {
        event_loop.register(
            self, token,
            interest,
            mio_orig::PollOpt::edge(),
            ).expect("register failed");
    }

    /// Reregister
    fn reregister(&self, event_loop : &mut EventLoop<Handler>, token : Token, interest : EventSet) {
        event_loop.reregister(
            self, token,
            interest,
            mio_orig::PollOpt::edge(),
            ).expect("reregister failed");
    }

    /// Deregister
    fn deregister(&self, event_loop : &mut EventLoop<Handler>, _token : Token) {
        event_loop.deregister(
            self
            ).expect("deregister failed");
    }

    fn should_resume(&self) -> bool {
        true
    }
}

trait RcEventedTrait {
    /// Reregister oneshot handler for the next event
    fn register(&mut self, event_loop: &mut EventLoop<Handler>, co_id : CoroutineId);

    /// Reregister oneshot handler for the next event
    fn reregister(&mut self, event_loop: &mut EventLoop<Handler>, co_id : CoroutineId);

    /// Un-reregister event we're not interested in anymore
    fn deregister(&mut self, event_loop: &mut EventLoop<Handler>, co_id : CoroutineId);

    fn hup(&mut self, _event_loop: &mut EventLoop<Handler>, token: Token);

    fn as_eventedsharedcommon(&self) -> Ref<EventedSharedCommon>;

    fn should_resume(&self) -> bool;
}

impl<T> RcEventedTrait for RcEvented<T>
where T : EventedInner {
    fn as_eventedsharedcommon(&self) -> Ref<EventedSharedCommon> {
        Ref::map(self.0.borrow(), |r| &r.common)
    }

    /// Handle `hup` condition
    fn hup(&mut self, _event_loop: &mut EventLoop<Handler>, _token: Token) {
        trace!("hup");
        self.0.borrow_mut().common.peer_hup = true;
    }

    fn should_resume(&self) -> bool {
        self.0.borrow().io.should_resume()
    }

    /// Reregister oneshot handler for the next event
    fn register(&mut self, event_loop: &mut EventLoop<Handler>, co_id : CoroutineId) {
        let mut interest = mio_orig::EventSet::none();

        if !self.0.borrow().common.peer_hup {
            interest = interest | mio_orig::EventSet::hup();

            if self.0.borrow().common.blocked_on.has_read() {
                interest = interest | mio_orig::EventSet::readable();
            }
        }

        if self.0.borrow().common.blocked_on.has_write() {
            interest = interest | mio_orig::EventSet::writable();
        }

        let token = token_from_ids(co_id, self.0.borrow().common.id.unwrap());

        EventedInner::register(&self.0.borrow().io, event_loop, token, interest);
    }

    /// Reregister oneshot handler for the next event
    fn reregister(&mut self, event_loop: &mut EventLoop<Handler>, co_id : CoroutineId) {
        let mut interest = mio_orig::EventSet::none();

        if !self.0.borrow().common.peer_hup {
            interest = interest | mio_orig::EventSet::hup();

            if self.0.borrow().common.blocked_on.has_read() {
                interest = interest | mio_orig::EventSet::readable();
            }
        }

        if self.0.borrow().common.blocked_on.has_write() {
            interest = interest | mio_orig::EventSet::writable();
        }

        let token = token_from_ids(co_id, self.0.borrow().common.id.unwrap());

        EventedInner::reregister(&self.0.borrow().io, event_loop, token, interest);
    }

    /// Un-reregister event we're not interested in anymore
    fn deregister(&mut self, event_loop: &mut EventLoop<Handler>, co_id : CoroutineId) {
        let token = token_from_ids(co_id, self.0.borrow().common.id.unwrap());
        EventedInner::deregister(&self.0.borrow().io, event_loop, token);
    }
}

struct EventedSharedCommon {
    id : Option<EventSourceId>,
    blocked_on : RW,
    peer_hup: bool,
}

/// Wrapped mio IO (mio_orig::Evented+TryRead+TryWrite)
///
/// `Handle` is just a cloneable reference to this struct
struct EventedShared<T> {
    common : EventedSharedCommon,
    io : T,
}

impl<T> EventedShared<T> {
    fn new(t : T) -> Self {
        EventedShared {
            common : EventedSharedCommon {
                id : None,
                blocked_on: RW::none(),
                peer_hup : false,
            },
            io : t,
        }
    }
}


struct RcEvented<T>(Rc<RefCell<EventedShared<T>>>);

impl<T> RcEvented<T> {
    fn new(t : T) -> Self {
        RcEvented(
            Rc::new(
                RefCell::new(EventedShared::new(t))
                )
            )
    }
}

impl<T> RcEvented<T>
where T : mio_orig::TryRead {
    fn try_read(&self, buf: &mut [u8]) -> io::Result<Option<usize>> {
        self.0.borrow_mut().io.try_read(buf)
    }
}

impl<T> RcEvented<T>
where T : mio_orig::TryWrite {
    fn try_write(&self, buf: &[u8]) -> std::io::Result<Option<usize>> {
        self.0.borrow_mut().io.try_write(buf)
    }
}

impl<T> RcEvented<T>
where T : mio_orig::TryAccept {
    fn try_accept(&self) -> io::Result<Option<<T as mio_orig::io::TryAccept>::Output>> {
        self.0.borrow_mut().io.accept()
    }
}

impl<T> RcEvented<T>
where T: EventedInner+'static {
    fn to_trait(&self) -> Box<RcEventedTrait+'static> {
        Box::new(RcEvented(self.0.clone()))
    }
}

impl<T> EventedInner for RcEvented<T>
where T : EventedInner {
    /// Register
    fn register(&self, event_loop : &mut EventLoop<Handler>, token : Token, interest : EventSet) {
        self.0.borrow().io.register(event_loop, token, interest);
    }

    /// Reregister
    fn reregister(&self, event_loop : &mut EventLoop<Handler>, token : Token, interest : EventSet) {
        self.0.borrow().io.reregister(event_loop, token, interest);
    }

    /// Deregister
    fn deregister(&self, event_loop : &mut EventLoop<Handler>, token : Token) {
        self.0.borrow().io.deregister(event_loop, token);
    }

    fn should_resume(&self) -> bool {
        self.0.borrow().io.should_resume()
    }
}

/// Retry `mio_orig::Sender::send()`.
///
/// As channels can fail randomly (eg. when Full), take care
/// of retrying on recoverable errors.
fn sender_retry<M : Send>(sender : &mio_orig::Sender<M>, msg : M) {
    let mut msg = Some(msg);
    let mut warning_printed = false;
    loop {
        match sender.send(msg.take().expect("sender_retry")) {
            Ok(()) => break,
            Err(mio_orig::NotifyError::Closed(_)) => panic!("Closed channel on sender.send()."),
            Err(mio_orig::NotifyError::Io(_)) => panic!("IO error on sender.send()."),
            Err(mio_orig::NotifyError::Full(retry_msg)) => {
                msg = Some(retry_msg);
            },
        }
        if !warning_printed {
            warning_printed = true;
            warn!("send_retry: retry; consider increasing `EventLoopConfig::notify_capacity`");
        }
        thread::yield_now();
    }
}

/// Mioco coroutine (a.k.a. *mioco handler*)
pub struct Coroutine {
    /// Context with a state of coroutine
    context: Context,

    /// Current state
    state : State,

    /// Last event that resumed the coroutine
    last_event: Event,

    /// `Handler` shared data that this `Coroutine` is running in
    handler_shared : Option<RcHandlerShared>,

    /// `Coroutine` will send exit status on it's finish
    /// through this list of Mailboxes
    exit_notificators : Vec<mail::MailboxOuterEnd<ExitStatus>>,

    /// Current coroutine Id
    id : CoroutineId,

    /// Coroutine stack
    stack: Stack,

    /// All event sources
    io : Slab<Box<RcEventedTrait+'static>, EventSourceId>,

    /// Newly spawned `Coroutine`-es
    children_to_start : Vec<RcCoroutine>,

    /// Function to be run inside Coroutine
    coroutine_func : Option<Box<FnBox() -> io::Result<()> + Send + 'static>>,

    /// In case Rc to self is needed
    self_rc : Option<RcCoroutine>,

    sync_mailbox: Option<(mail::MailboxOuterEnd<()>, mail::MailboxInnerEnd<()>)>,

    /// Userdata of the coroutine
    user_data: Option<Arc<Box<Any+Send+Sync>>>,

    /// Userdata meant for inheritance
    inherited_user_data: Option<Arc<Box<Any+Send+Sync>>>,
}

/// Mioco Handler keeps only Slab of Coroutines, and uses a scheme in which
/// Token bits encode both Coroutine and EventSource within it
const EVENT_SOURCE_TOKEN_SHIFT : usize = 10;
const EVENT_SOURCE_TOKEN_MASK : usize = (1 << EVENT_SOURCE_TOKEN_SHIFT) - 1;

/// Convert token to ids
fn token_to_ids(token : Token) -> (CoroutineId, EventSourceId) {
    let val = token.as_usize();
    (
        CoroutineId(val >> EVENT_SOURCE_TOKEN_SHIFT),
        EventSourceId(val & EVENT_SOURCE_TOKEN_MASK),
    )
}

/// Convert ids to Token
fn token_from_ids(co_id : CoroutineId, io_id : EventSourceId) -> Token {
    // TODO: Add checks on wrap()
    debug_assert!(io_id.as_usize() <= EVENT_SOURCE_TOKEN_MASK);
    Token((co_id.as_usize() << EVENT_SOURCE_TOKEN_SHIFT) | io_id.as_usize())
}

/// Event delivery point, kept in Handler slab.
#[derive(Clone)]
struct CoroutineSlabHandle {
    rc : RcCoroutine,
}

impl CoroutineSlabHandle {
    fn new(rc : RcCoroutine) -> Self {
        CoroutineSlabHandle {
            rc: rc,
        }
    }

    fn to_coroutine_control(self) -> CoroutineControl {
        CoroutineControl::new(self.rc)
    }

    /// Deliver an event to a Coroutine
    fn event(
        &self,
        event_loop : &mut EventLoop<Handler>,
        token : Token,
        events : EventSet,
        ) -> bool {

        let (co_id, io_id) = token_to_ids(token);

        trace!("Coroutine({}): event", self.id().as_usize());

        if !self.rc.borrow().state.is_blocked() {
            // subsequent event to coroutine that is either already
            // Finished, or Ready
            return false
        }

        let ready = {
            let Coroutine {
                ref mut io,
                ..
            } = *self.rc.borrow_mut();


            if let Some(mut io) = io.get_mut(io_id) {

                if events.is_hup() {
                    io.hup(event_loop, token);
                }

                let io_rw = io.as_eventedsharedcommon().blocked_on.as_tuple();
                match io_rw {
                    (false, false) => {
                        debug!("spurious event for event source blocked on nothing");
                        false
                    },
                    (true, false) if !events.is_readable() && !events.is_hup() => {
                        debug!("spurious not read event for event source blocked on read");
                        false
                    },
                    (false, true) if !events.is_writable() => {
                        debug!("spurious not write event for event source blocked on write");
                        false
                    },
                    (true, true) if !events.is_readable() && !events.is_hup() && !events.is_writable() => {
                        debug!("spurious unknown type event for event source blocked on read/write");
                        false
                    },
                    _ => {
                        if io.should_resume() {
                            true
                        } else {
                            // TODO: Actually, we can just reregister the Timer,
                            // not all sources, and in just this one case
                            io.reregister(event_loop, co_id);
                            false
                        }
                    }
                }
            } else {
                // spurious event, probably after select in which
                // more than one event sources were reported ready
                // in one group of events, and first event source
                // deregistered the later ones
                debug!("spurious event for event source coroutine is not blocked on");
                false
            }
        };

        if ready {
            trace!("Coroutine({}): set to ready", co_id.as_usize());
            // Wake coroutine on HUP, as it was read, to potentially let it fail the read and move on
            let event = match (events.is_readable() | events.is_hup(), events.is_writable()) {
                (true, true) => RW::both(),
                (true, false) => RW::read(),
                (false, true) => RW::write(),
                (false, false) => panic!(),
            };

            let mut co = self.rc.borrow_mut();
            co.state = State::Ready;
            co.last_event = Event {
                rw: event,
                id: io_id,
            };
            co.deregister_all(event_loop);
        }

        ready
    }

    fn id(&self) -> CoroutineId {
        let coroutine = self.rc.borrow();
        coroutine.id
    }

    /// After `resume()` (or ignored event()) we need to perform the following maintenance
    fn after_resume(
        &self,
        event_loop: &mut EventLoop<Handler>,
        ) {
        // Take care of newly spawned child-coroutines: start them now
        debug_assert!(!self.rc.borrow().state().is_running());

        self.rc.borrow_mut().register_all(event_loop);

        {

            let Coroutine {
                ref mut children_to_start,
                ref handler_shared,
                ref id,
                ..
            } = *self.rc.borrow_mut();

            trace!("Coroutine({}): {} children spawned", id.as_usize(), children_to_start.len());

            let mut handler_shared = handler_shared.as_ref().unwrap().borrow_mut();

            for coroutine in children_to_start.drain(..) {
                let coroutine_ctrl = CoroutineControl::new(coroutine);
                handler_shared.spawned.push(coroutine_ctrl);
            }
        }

        let state = self.rc.borrow().state();
        if state.is_yielding() {
            debug_assert!(self.rc.borrow().io.is_empty());
            let mut coroutine_ctrl = CoroutineControl::new(self.rc.clone());
            coroutine_ctrl.set_is_yielding();
            self.rc.borrow_mut().state = State::Ready;
            let handler_shared = &self.rc.borrow().handler_shared;
            let mut handler_shared = handler_shared.as_ref().unwrap().borrow_mut();

            handler_shared.ready.push(coroutine_ctrl);
        }
    }
}



/// Coroutine control block
///
/// Through this interface Coroutine can be resumed and migrated.
pub struct CoroutineControl {
    /// In case `CoroutineControl` gets dropped in `SchedulerThread` Drop
    /// trait will kill the Coroutine
    was_handled : bool,
    is_yielding : bool,
    rc : RcCoroutine,
}

impl Drop for CoroutineControl {
    fn drop(&mut self) {
        if !self.was_handled {
            trace!("Coroutine({}): kill", self.id().as_usize());
            self.rc.borrow_mut().state = State::Finished(ExitStatus::Killed);
            coroutine_jump_in(&self.rc);
        }
    }
}

impl CoroutineControl {
    fn new(rc : RcCoroutine) -> Self {
        CoroutineControl {
            is_yielding: false,
            was_handled: false,
            rc: rc,
        }
    }

    // TODO: Eliminate this needles clone()
    fn to_slab_handle(&self) -> CoroutineSlabHandle {
        CoroutineSlabHandle::new(self.rc.clone())
    }

    /// Resume Coroutine
    ///
    /// Panics if Coroutine is not in Ready state.
    pub fn resume(
        mut self,
        event_loop : &mut EventLoop<Handler>,
        ) {
        self.was_handled = true;
        trace!("Coroutine({}): resume", self.id().as_usize());
        let co_rc = self.rc.clone();
        let is_ready = co_rc.borrow().state.is_ready();
        if is_ready {
            coroutine_jump_in(&co_rc);
            self.to_slab_handle().after_resume(event_loop);
        } else {
            panic!("Tried to resume Coroutine that is not ready");
        }
    }

    fn id(&self) -> CoroutineId {
       self.rc.borrow().id
    }

    /// Migrate to a different thread
    ///
    /// Move this Coroutine to be executed on a `SchedulerThread` for a
    /// given `thread_id`.
    ///
    /// Will panic if `thread_id` is not valid.
    pub fn migrate(
        mut self,
        event_loop : &mut EventLoop<Handler>,
        thread_id : usize,
        ) {
        self.was_handled = true;
        let sender = {
            trace!("Coroutine({}): migrate to thread {}", self.id().as_usize(), thread_id);
            let mut co = self.rc.borrow_mut();
            co.deregister_all(event_loop);

            let id = co.id;
            let handler_shared = co.handler_shared.take();
            debug_assert!(co.handler_shared.is_none());
            let mut handler_shared = handler_shared.as_ref().unwrap().borrow_mut();
            handler_shared.coroutines.remove(id).unwrap();
            handler_shared.senders[thread_id].clone()
        };

        let rc = self.rc.clone();

        drop(self);

        // TODO: Spin on failure
        sender_retry(&sender, Migration(rc));
    }


    /// Finish migrating Coroutine by attaching it to a new thread
    fn reattach_to(
        &mut self,
        event_loop : &mut EventLoop<Handler>,
        handler : &mut Handler,
        ) {
        let handler_shared = handler.shared.clone();

        {
            let mut co = self.rc.borrow_mut();
            co.register_all(event_loop);
        }
        trace!("Coroutine({}): reattach in a new thread", self.id().as_usize());
        let coroutines = &mut handler.shared.borrow_mut().coroutines;

        if !coroutines.has_remaining() {
            let count = coroutines.count();
            coroutines.grow(count);
        }

        let _id = coroutines.insert_with(|id| {
            let mut co = self.rc.borrow_mut();

            co.id = id;
            co.handler_shared = Some(handler_shared);

            CoroutineSlabHandle::new(self.rc.clone())
        }).expect("Run out of slab for coroutines");
    }

    fn set_is_yielding(&mut self) {
        self.is_yielding = true
    }


    /// Is this Coroutine ready after `yield_now()`?
    pub fn is_yielding(&self) -> bool {
        self.is_yielding
    }

    /// Gets a reference to the user data set through `set_userdata`. Returns `None` if `T` does not match or if no data was set
    pub fn get_userdata<'a, T: Any>(&'a self) -> Option<&'a T> {
        let coroutine_ref = unsafe {
            &mut *self.rc.as_unsafe_cell().get() as &mut Coroutine
        };

        match coroutine_ref.user_data {
            Some(ref arc) => {
                let boxed_any: &Box<Any+Send+Sync> = arc.as_ref();
                let any: &Any = boxed_any.as_ref();
                any.downcast_ref::<T>()
            },
            None => None,
        }
    }
}

impl Coroutine {
    /// Spawn a new Coroutine
    fn spawn<F>(handler_shared : RcHandlerShared, inherited_user_data: Option<Arc<Box<Any+Send+Sync>>>, f : F) -> RcCoroutine
    where F : FnOnce() -> io::Result<()> + Send + 'static {
        trace!("Coroutine: spawning");
        let stack_size = handler_shared.borrow().stack_size;

        let id = {
            let coroutines = &mut handler_shared.borrow_mut().coroutines;

            if !coroutines.has_remaining() {
                let count = coroutines.count();
                coroutines.grow(count);
            }

            coroutines.insert_with(|id| {
                let coroutine = Coroutine {
                    state: State::Ready,
                    id: id,
                    last_event: Event{ rw: RW::read(), id: EventSourceId(0)},
                    context: Context::empty(),
                    handler_shared: Some(handler_shared.clone()),
                    exit_notificators: Vec::new(),
                    io: Slab::new(4),
                    children_to_start: Vec::new(),
                    stack: Stack::new(stack_size),
                    coroutine_func: Some(Box::new(f)),
                    self_rc: None,
                    sync_mailbox: None,
                    user_data: inherited_user_data.clone(),
                    inherited_user_data: inherited_user_data,
                };

                CoroutineSlabHandle::new(Rc::new(RefCell::new(coroutine)))
            }).expect("Run out of slab for coroutines")
        };
        handler_shared.borrow_mut().coroutines_inc();

        let coroutine_rc = handler_shared.borrow().coroutines[id].rc.clone();

        coroutine_rc.borrow_mut().self_rc = Some(coroutine_rc.clone());

        let coroutine_ptr = {
            // The things we do for borrowck...
            let coroutine_ptr = {
                &*coroutine_rc.borrow() as *const Coroutine
            };
            coroutine_ptr
        };

        extern "C" fn init_fn(arg: usize, _: *mut libc::types::common::c95::c_void) -> ! {
            let ctx : &Context = {

                let res = std::panic::recover(
                    move|| {
                        let coroutine : &mut Coroutine = unsafe { transmute(arg) };
                        trace!("Coroutine({}): started", {
                            coroutine.id.as_usize()
                        });

                        entry_point(coroutine.self_rc.as_ref().unwrap());
                        let f = coroutine.coroutine_func.take().unwrap();

                        f.call_box(())
                    }
                    );

                let coroutine : &mut Coroutine = unsafe { transmute(arg) };
                coroutine.io.clear();
                coroutine.self_rc = None;

                let id = coroutine.id;
                {
                    let mut handler_shared = coroutine.handler_shared.as_ref().unwrap().borrow_mut();
                    handler_shared.coroutines.remove(id).unwrap();
                    handler_shared.coroutines_dec();
                }

                match res {
                    Ok(res) => {
                        trace!("Coroutine({}): finished returning {:?}", id.as_usize(), res);
                        let arc_res = Arc::new(res);
                        coroutine.exit_notificators.iter().map(
                            |end| end.send(ExitStatus::Exit(arc_res.clone()))
                            ).count();
                        coroutine.state = State::Finished(ExitStatus::Exit(arc_res));

                    },
                    Err(cause) => {
                        trace!("Coroutine({}): panicked: {:?}", id.as_usize(), cause.downcast::<&str>());
                        if let State::Finished(ExitStatus::Killed) = coroutine.state {
                            coroutine.exit_notificators.iter().map(
                                |end| end.send(ExitStatus::Killed)
                                ).count();
                        } else {
                            coroutine.state = State::Finished(ExitStatus::Panic);
                            coroutine.exit_notificators.iter().map(
                                |end| end.send(ExitStatus::Panic)
                                ).count();
                        }
                    }
                }

                unsafe {
                    let handler = coroutine.handler_shared.as_ref().unwrap().borrow();
                    transmute(&handler.context as *const Context)
                }
            };

            Context::load(ctx);
        }

        {
            let Coroutine {
                ref mut stack,
                ref mut context,
                ..
            } = *coroutine_rc.borrow_mut();

            context.init_with(
                init_fn,
                coroutine_ptr as usize,
                std::ptr::null_mut(),
                stack,
                );
        }

        coroutine_rc
    }

    fn state(&self) -> State {
        self.state.clone()
    }

    fn deregister_all(&mut self, event_loop: &mut EventLoop<Handler>) {
        for io in self.io.iter_mut() {
            io.deregister(event_loop, self.id);
        }
        self.io.clear();
    }

    fn register_all(&mut self, event_loop: &mut EventLoop<Handler>) {
        for io in self.io.iter_mut() {
            io.register(event_loop, self.id);
        }
    }
}

/// Resume coroutine execution, jumping into it
fn coroutine_jump_in(coroutine : &RefCell<Coroutine>) {
    let prev = TL_CURRENT_COROUTINE.with( |co| {
        let mut co = co.borrow_mut();
        let prev = *co;
        *co = &mut *coroutine.borrow_mut() as *mut Coroutine;
        prev
    });

    {
        let ref mut state = coroutine.borrow_mut().state;
        match *state {
            State::Ready => {
                *state = State::Running;
            },
            State::Finished(ExitStatus::Killed) => {} ,
            ref state => panic!("coroutine_jump_in: wrong state {:?}", state),
        }
    }

    // We know that we're holding at least one Rc to the Coroutine,
    // and noone else is holding a reference as we can do `.borrow_mut()`
    // so we cheat with unsafe just to context-switch from coroutine
    // without having RefCells still borrowed.
    let (context_in, context_out) = {
        let Coroutine {
            ref context,
            ref handler_shared,
            ..
        } = *coroutine.borrow_mut();
        {
            let mut shared_context = &mut handler_shared.as_ref().unwrap().borrow_mut().context;
            (context as *const Context, shared_context as *mut Context)
        }
    };

    Context::swap(unsafe {&mut *context_out}, unsafe {&*context_in});
    TL_CURRENT_COROUTINE.with( |co| {
        *co.borrow_mut() = prev;
    });
}

/// Coroutine entry point checks
fn entry_point(coroutine : &RefCell<Coroutine>) {
    if let State::Finished(ExitStatus::Killed) = coroutine.borrow().state {
        panic!("Killed externally")
    }
}

/// Block coroutine execution, jumping out of it
fn coroutine_jump_out(coroutine : &RefCell<Coroutine>) {
    {
        let state = &coroutine.borrow().state;
        debug_assert!(state.is_blocked() || state.is_yielding());
    }

    // See `resume()` for unsafe comment
    let (context_in, context_out) = {
        let Coroutine {
            ref mut context,
            ref handler_shared,
            ..
        } = *coroutine.borrow_mut();
        {
            let shared_context = &mut handler_shared.as_ref().unwrap().borrow_mut().context;
            (context as *mut Context, shared_context as *const Context)
        }
    };

    Context::swap(unsafe {&mut *context_in}, unsafe {&*context_out});
}

/// Id of an event source used to enumerate them
///
/// It's unique within coroutine of an event source, but not globally.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct EventSourceId(usize);

impl EventSourceId {
    fn as_usize(&self) -> usize {
        self.0
    }
}

impl slab::Index for EventSourceId {
    fn as_usize(&self) -> usize {
        self.0
    }
    fn from_usize(i : usize) -> Self {
        EventSourceId(i)
    }
}

/// Id of a Coroutine used to enumerate them
///
/// It's unique within a thread
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CoroutineId(usize);

impl CoroutineId {
    fn as_usize(&self) -> usize {
        self.0
    }
}

impl slab::Index for CoroutineId {
    fn as_usize(&self) -> usize {
        self.0
    }
    fn from_usize(i : usize) -> Self {
        CoroutineId(i)
    }
}


/// Handle to spawned coroutine
pub struct CoroutineHandle {
     coroutine: RcCoroutine,
}

impl CoroutineHandle {
    /// Create an exit notificator
    pub fn exit_notificator(&self) -> mail::MailboxInnerEnd<ExitStatus> {
        let (outer, inner) = mail::mailbox();
        let mut co = self.coroutine.borrow_mut();
        let Coroutine {
            ref state,
            ref mut exit_notificators,
            ..
        } = *co;

        if let &State::Finished(ref exit) = state {
            outer.send(exit.clone())
        } else {
            exit_notificators.push(outer);
        }
        inner
    }
}

struct HandlerThreadShared {
    mioco_started: AtomicUsize,
    coroutines_num : AtomicUsize,
    #[allow(dead_code)] // Not used yet
    thread_num : AtomicUsize,
}

impl HandlerThreadShared {
    fn new(thread_num : usize) -> Self {
        HandlerThreadShared {
            mioco_started: AtomicUsize::new(0),
            coroutines_num: AtomicUsize::new(0),
            thread_num: AtomicUsize::new(thread_num),
        }
    }
}

/// Data belonging to `Handler`, but referenced and manipulated by coroutinees
/// belonging to it.
struct HandlerShared {
    /// Slab allocator
    coroutines : slab::Slab<CoroutineSlabHandle, CoroutineId>,

    /// Context saved when jumping into coroutine
    context : Context,

    /// Senders to other EventLoops
    senders : Vec<MioSender>,

    /// Shared between threads
    thread_shared : ArcHandlerThreadShared,

    /// Default stack size
    stack_size : usize,

    /// Newly spawned Coroutines
    spawned : Vec<CoroutineControl>,

    /// Coroutines that were made ready
    ready : Vec<CoroutineControl>,
}

impl HandlerShared {
    fn new(senders : Vec<MioSender>, thread_shared : ArcHandlerThreadShared, stack_size : usize) -> Self {
        HandlerShared {
            coroutines: Slab::new(512),
            thread_shared: thread_shared,
            context: Context::empty(),
            senders: senders,
            stack_size: stack_size,
            spawned: Vec::new(),
            ready: Vec::new(),
        }
    }

    fn wait_for_start_all(&self) {
        while self.thread_shared.mioco_started.load(Ordering::SeqCst) == 0 {
            thread::yield_now()
        }
    }

    fn signal_start_all(&self) {
        self.thread_shared.mioco_started.store(1, Ordering::SeqCst)
    }

    fn coroutines_num(&self) -> usize {
        // Relaxed is OK, since Threads will eventually notice if it goes to
        // zero and at the start `SeqCst` in `mioco_start` and
        // `mioco_started` will enforce that `coroutines_num > 0` is visible
        // on all threads at the start.
        self.thread_shared.coroutines_num.load(Ordering::Relaxed)
    }

    fn coroutines_inc(&self) {
        self.thread_shared.coroutines_num.fetch_add(1, Ordering::SeqCst);
    }

    fn coroutines_dec(&self) {
        let prev = self.thread_shared.coroutines_num.fetch_sub(1, Ordering::SeqCst);
        debug_assert!(prev > 0);
    }

    /// Get number of threads
    fn thread_num(&self) -> usize {
        self.thread_shared.thread_num.load(Ordering::Relaxed)
    }


}

/// Coroutine Scheduler
///
/// Custom implementations of this trait allow users to change the order in
/// which Coroutines are being scheduled.
pub trait Scheduler : Sync+Send {
    /// Spawn per-thread Scheduler
    fn spawn_thread(&self) -> Box<SchedulerThread + 'static>;
}


/// Per-thread Scheduler
pub trait SchedulerThread {
    /// New coroutine was spawned.
    ///
    /// This can be used to run it immediately (see
    /// `CoroutineControl::resume()`), save it to be started later, or
    /// migrate it to different thread immediately (see
    /// `CoroutineControl::migrate()`).
    ///
    /// Dropping `coroutine_ctrl` means the corresponding coroutine will be
    /// killed.
    fn spawned(&mut self, event_loop: &mut mio_orig::EventLoop<Handler>, coroutine_ctrl: CoroutineControl);

    /// A Coroutine became ready.
    ///
    /// `coroutine_ctrl` is a control reference to the Coroutine that became
    /// ready (to be resumed). It can be resumed immediately, or stored
    /// somewhere to be resumed later.
    ///
    /// Dropping `coroutine_ctrl` means the corresponding coroutine will be
    /// killed.
    fn ready(&mut self, event_loop: &mut mio_orig::EventLoop<Handler>, coroutine_ctrl: CoroutineControl);

    /// Mio's tick have completed.
    ///
    /// Mio signals events in batches, after which a `tick` is signaled.
    ///
    /// All events events have been processed and all unblocked coroutines
    /// signaled with `SchedulerThread::ready()`.
    ///
    /// After returning from this function, `mioco` will let mio process a
    /// new batch of events.
    fn tick(&mut self, _event_loop: &mut mio_orig::EventLoop<Handler>) {}
}

/// Default, simple first-in-first-out Scheduler.
///
/// Newly spawned coroutines will be spread in round-robbin fashion
/// between threads.
struct FifoScheduler {
    thread_num : Arc<AtomicUsize>,
}

impl FifoScheduler {
    pub fn new() -> Self {
        FifoScheduler {
            thread_num : Arc::new(AtomicUsize::new(0)),
        }
    }
}

struct FifoSchedulerThread {
    thread_i : usize,
    thread_num : Arc<AtomicUsize>,
    delayed : VecDeque<CoroutineControl>,
}

impl Scheduler for FifoScheduler {
    fn spawn_thread(&self) -> Box<SchedulerThread> {
        self.thread_num.fetch_add(1, Ordering::Relaxed);
        Box::new(FifoSchedulerThread{
            thread_i: 0,
            thread_num: self.thread_num.clone(),
            delayed: VecDeque::new(),
        })
    }
}

impl FifoSchedulerThread {
    fn thread_next_i(&mut self) -> usize {
        self.thread_i += 1;
        if self.thread_i >= self.thread_num() {
            self.thread_i = 0;
        }
        self.thread_i
    }

    fn thread_num(&self) -> usize {
        self.thread_num.load(Ordering::Relaxed)
    }
}

impl SchedulerThread for FifoSchedulerThread {
    fn spawned(&mut self, event_loop: &mut mio_orig::EventLoop<Handler>, coroutine_ctrl: CoroutineControl) {
        let thread_i = self.thread_next_i();
        trace!("Migrating newly spawn Coroutine to thread {}", thread_i);
        coroutine_ctrl.migrate(event_loop, thread_i);
    }

    fn ready(
        &mut self,
        event_loop: &mut mio_orig::EventLoop<Handler>,
        coroutine_ctrl: CoroutineControl
             ) {
        if coroutine_ctrl.is_yielding() {
            self.delayed.push_back(coroutine_ctrl);
        } else {
            coroutine_ctrl.resume(event_loop);
        }
    }

    fn tick(&mut self, event_loop : &mut mio_orig::EventLoop<Handler>) {
        let len = self.delayed.len();
        for _ in 0..len {
            let coroutine_ctrl = self.delayed.pop_front().unwrap();
            coroutine_ctrl.resume(event_loop);
        }
    }
}

/// Mioco event loop `Handler`
///
/// Registered in `mio_orig::EventLoop` and implementing `mio_orig::Handler`.  This `struct` is quite
/// internal so you should not have to worry about it.
pub struct Handler {
    shared : RcHandlerShared,
    scheduler : Box<SchedulerThread+'static>,
}

impl Handler {
    fn new(shared : RcHandlerShared, scheduler : Box<SchedulerThread>) -> Self {
        Handler {
            shared: shared,
            scheduler: scheduler,
        }
    }

    /// To prevent recursion, all the newly spawned or newly made
    /// ready Coroutines are delivered to scheduler here.
    fn deliver_to_scheduler(&mut self, event_loop : &mut EventLoop<Self>) {
        let Handler {
            ref shared,
            ref mut scheduler,
        } = *self;

        loop {
            let mut spawned = Vec::new();
            let mut ready = Vec::new();
            {
                let mut shared = shared.borrow_mut();

                if shared.spawned.len() == 0 && shared.ready.len() == 0 {
                    break;
                }
                std::mem::swap(&mut spawned, &mut shared.spawned);
                std::mem::swap(&mut ready, &mut shared.ready);
            }

            for spawned in spawned.drain(..) {
                scheduler.spawned(event_loop, spawned);
            }

            for ready in ready.drain(..) {
                scheduler.ready(event_loop, ready);
            }
        }
    }
}

/// EventLoop message type
pub enum Message {
    /// Mailbox notification
    MailboxMsg(Token),
    /// Coroutine migration
    Migration(Rc<RefCell<Coroutine>>),
}

unsafe impl Send for Message { }

impl mio_orig::Handler for Handler {
    type Timeout = Token;
    type Message = Message;

    fn tick(&mut self, event_loop: &mut mio_orig::EventLoop<Self>) {
        let coroutines_num = self.shared.borrow().coroutines_num();
        trace!("Handler::tick(): coroutines_num = {}", coroutines_num);
        if coroutines_num == 0 {
            trace!("Shutting down EventLoop");
            event_loop.shutdown();
        }
        self.scheduler.tick(event_loop);
    }

    fn ready(&mut self, event_loop: &mut mio_orig::EventLoop<Handler>, token: mio_orig::Token, events: mio_orig::EventSet) {
        trace!("Handler::ready({:?}): started", token);
        let (co_id, _) = token_to_ids(token);
        let co = {
            let shared = self.shared.borrow();
            match shared.coroutines.get(co_id).as_ref() {
                Some(&co) => co.clone(),
                None => {
                    trace!("Handler::ready() ignored");
                    return
                },
            }
        };
        if co.event(event_loop, token, events) {
            self.scheduler.ready(event_loop, co.to_coroutine_control());
        }

        self.deliver_to_scheduler(event_loop);

        trace!("Handler::ready({:?}): finished", token);
    }

    fn notify(&mut self, event_loop: &mut EventLoop<Handler>, msg: Self::Message) {
        match msg {
            MailboxMsg(token) => self.ready(event_loop, token, EventSet::readable()),
            Migration(rc_coroutine) => {
                let mut co = CoroutineControl::new(rc_coroutine);
                co.reattach_to(event_loop, self);
                self.scheduler.ready(event_loop, co);
                self.deliver_to_scheduler(event_loop);
            },
        }
    }

    fn timeout(&mut self, event_loop: &mut EventLoop<Self>, msg: Self::Timeout) {
        self.ready(event_loop, msg, EventSet::readable());
    }
}

/// Mioco instance
///
/// Main mioco structure.
pub struct Mioco {
    join_handles : Vec<thread::JoinHandle<()>>,
    config : Config,
}

impl Mioco {
    /// Create new `Mioco` instance
    pub fn new() -> Self {
        Mioco::new_configured(Config::new())
    }

    /// Create new `Mioco` instance
    pub fn new_configured(config : Config) -> Self
    {
        Mioco {
            join_handles: Vec::new(),
            config: config,
         }
    }

    /// Start mioco handling
    ///
    /// Takes a starting handler function that will be executed in `mioco` environment.
    ///
    /// Will block until `mioco` is finished - there are no more handlers to run.
    ///
    /// See `MiocoHandle::spawn()`.
    pub fn start<F>(&mut self, f : F)
        where
        F : FnOnce() -> io::Result<()> + Send + 'static,
        F : Send
        {
            info!("Starting mioco instance with {} handler threads", self.config.thread_num);
            let thread_shared = Arc::new(HandlerThreadShared::new(self.config.thread_num));

            let mut event_loops = VecDeque::new();
            let mut senders = Vec::new();
            for _ in 0..self.config.thread_num {
                let event_loop = EventLoop::configured(self.config.event_loop_config.clone()).expect("new EventLoop");
                senders.push(event_loop.channel());
                event_loops.push_back(event_loop);
            }

            let sched = self.config.scheduler.spawn_thread();
            let first_event_loop = event_loops.pop_front().unwrap();

            for i in 1..self.config.thread_num {

                let scheduler = self.config.scheduler.clone();
                let stack_size = self.config.stack_size;
                let event_loop = event_loops.pop_front().unwrap();
                let senders = senders.clone();
                let thread_shared = thread_shared.clone();
                let join = std::thread::Builder::new().name(format!("mioco_thread_{}", i)).spawn(move || {
                    let sched = scheduler.spawn_thread();
                    Mioco::thread_loop::<F>(None, sched, event_loop, senders, thread_shared, stack_size, None);
                });

                match join {
                    Ok(join) => self.join_handles.push(join),
                    Err(err) => panic!("Couldn't spawn thread: {}", err),
                }
            }

            let mut user_data = None;
            mem::swap(&mut user_data, &mut self.config.user_data);
            Mioco::thread_loop(Some(f), sched, first_event_loop, senders, thread_shared, self.config.stack_size, user_data);

            for join in self.join_handles.drain(..) {
                let _ = join.join(); // TODO: Do something with it
            }
        }

    fn thread_loop<F>(
        f : Option<F>,
        mut scheduler : Box<SchedulerThread+'static>,
        mut event_loop : EventLoop<Handler>,
        senders : Vec<MioSender>,
        thread_shared : ArcHandlerThreadShared,
        stack_size: usize,
        userdata: Option<Arc<Box<Any + Send + Sync>>>,
        )
        where F : FnOnce() -> io::Result<()> + Send + 'static,
              F : Send
    {
            let handler_shared = HandlerShared::new(senders, thread_shared, stack_size);
            let shared = Rc::new(RefCell::new(handler_shared));
            if let Some(f) = f {
                let coroutine_rc = Coroutine::spawn(shared.clone(), userdata, f);
                let coroutine_ctrl = CoroutineControl::new(coroutine_rc);
                scheduler.spawned(&mut event_loop, coroutine_ctrl);
                // Mark started only after first coroutine is spawned so that
                // threads don't start, detect no coroutines, and exit prematurely
                shared.borrow().signal_start_all();
            }
            let mut handler = Handler::new(shared, scheduler);

            handler.shared.borrow().wait_for_start_all();
            handler.deliver_to_scheduler(&mut event_loop);
            event_loop.run(&mut handler).unwrap();
    }
}


/// Mioco instance builder.
pub struct Config {
    thread_num : usize,
    scheduler : Arc<Box<Scheduler>>,
    event_loop_config : EventLoopConfig,
    stack_size : usize,
    user_data : Option<Arc<Box<Any + Send + Sync>>>,
}

impl Config {
    /// Create mioco `Config`
    ///
    /// Use it to configure mioco instance
    ///
    /// See `start` and `start_threads` for convenience wrappers.
    pub fn new() -> Self {
        let mut config = Config {
            thread_num: num_cpus::get(),
            scheduler: Arc::new(Box::new(FifoScheduler::new())),
            event_loop_config: Default::default(),
            stack_size: 2 * 1024 * 1024,
            user_data: None,
        };
        config.event_loop_config.tick_ms(Some(1000));
        config
    }

    /// Set numer of threads to run mioco with
    ///
    /// Default is equal to a numer of CPUs in the system.
    pub fn set_thread_num(&mut self, thread_num : usize) -> &mut Self {
        self.thread_num = thread_num;
        self
    }

    /// Set custom scheduler.
    ///
    /// See `Scheduler` trait.
    ///
    /// Default is a simple FIFO-scheduler that spreads all the new
    /// coroutines between all threads in round-robin fashion, and runs them
    /// in FIFO manner.
    ///
    /// See private `FifoSchedule` source for details.
    pub fn set_scheduler(&mut self, scheduler : Box<Scheduler + 'static>) -> &mut Self {
        self.scheduler = Arc::new(scheduler);
        self
    }

    /// Set stack size in bytes.
    ///
    /// Default is 2MB.
    ///
    /// Should be a power of 2.
    ///
    /// Stack size includes a protection page. Setting too small stack will
    /// lead to SEGFAULTs. See [context-rs stack.rs](https://github.com/zonyitoo/context-rs/blob/master/src/stack.rs)
    /// for implementation details. The sane minimum seems to be 128KiB,
    /// which is two 64KB pages.
    pub unsafe fn set_stack_size(&mut self, stack_size : usize) -> &mut Self {
        self.stack_size = stack_size;
        self
    }

    /// Set the user data of the first spawned coroutine
    ///
    /// Default is no Userdata
    pub fn set_userdata<T: Reflect + Send + Sync + 'static>(&mut self, data: T) -> &mut Self
    {
        self.user_data = Some(Arc::new(Box::new(data)));
        self
    }

    /// Configure `mio::EvenLoop` for all the threads
    pub fn even_loop(&mut self) -> &mut EventLoopConfig {
        &mut self.event_loop_config
    }
}

/// Current coroutine thread-local reference
///
/// This reference is used to store a reference to a currently executing
/// mioco coroutine.
///
/// Should not be used directly, use `tl_coroutine_current()` instead.
thread_local!(static TL_CURRENT_COROUTINE: RefCell<*mut Coroutine> = RefCell::new(ptr::null_mut()));

// TODO: Technically this leaks unsafe, but only within
// internals of the module. Any function calling `tl_coroutine_current()`
// must not pass the reference anywhere outside!
//
// It might be possible to use a type system to enforce this. Eg. maybe this
// should return `Ref` or `RefCell`.
fn tl_coroutine_current() -> &'static mut Coroutine {
    let coroutine = TL_CURRENT_COROUTINE.with(|coroutine| *coroutine.borrow());
    if coroutine == ptr::null_mut() {
        panic!("mioco API function called outside of coroutine, use `RUST_BACKTRACE=1` to pinpoint");
    }
    unsafe { &mut *coroutine }
}

/// Start mioco instance.
///
/// Will start new mioco instance and return only after it's done.
///
/// Shorthand for creating new `Mioco` instance with default settings and
/// starting it right away.
pub fn start<F>(f : F)
    where F : FnOnce() -> io::Result<()> + Send + 'static,
          F : Send
{
    Mioco::new().start(f);
}

/// Start mioco instance using a given number of threads.
///
/// Returns after mioco instance exits.
///
/// Shorthand for `mioco::start()` running given number of threads.
pub fn start_threads<F>(thread_num : usize, f : F)
    where F : FnOnce() -> io::Result<()> + Send + 'static,
          F : Send
{
    let mut config = Config::new();
    config.set_thread_num(thread_num);
    Mioco::new_configured(config).start(f);
}

/// Spawn a mioco coroutine.
///
/// If called inside an existing mioco instance - spawn and run a new coroutine
/// in it.
///
/// If called outside of existing mioco instance - spawn a new mioco instance
/// in a separate thread or use existing mioco instance to run new mioco
/// coroutine. The API intention is to guarantee:
/// * this call will not block
/// * coroutine will be executing in some mioco instance
/// the exact details might change.
pub fn spawn<F>(f : F)
where F : FnOnce() -> io::Result<()> + Send + 'static {
    let coroutine = TL_CURRENT_COROUTINE.with(|coroutine| *coroutine.borrow());
    if coroutine == ptr::null_mut() {
        thread::spawn(|| {
            start(f);
        });
    } else {
        spawn_ext(f);
    }
}

/// Spawn a `mioco` coroutine
///
/// Can't be used outside of existing coroutine.
///
/// Returns a `CoroutineHandle` that can be used to perform
/// additional operations.
// TODO: Could this be unified with `spawn()` so the return type
// can be simply ignored?
pub fn spawn_ext<F>(f : F) -> CoroutineHandle
where F : FnOnce() -> io::Result<()> + Send + 'static {
    let coroutine = tl_coroutine_current();

    let coroutine_ref = Coroutine::spawn(
        coroutine.handler_shared.as_ref().unwrap().clone(),
        coroutine.inherited_user_data.clone(),
        f
        );
    let ret = CoroutineHandle {
        coroutine: coroutine_ref.clone(),
    };
    coroutine.children_to_start.push(coroutine_ref);

    ret
}

/// Test whether currently executing inside a mioco coroutine.
pub fn in_coroutine() -> bool {
    let coroutine = TL_CURRENT_COROUTINE.with(|coroutine| *coroutine.borrow());
    coroutine != ptr::null_mut()
}

/// Execute a block of synchronous operations
///
/// This will execute a block of synchronous operations without blocking
/// cooperative coroutine scheduling. This is done by offloading the
/// synchronous operations to a separate thread, a notifying the
/// coroutine when the result is available.
///
/// TODO: find some wise people to confirm if this is sound
/// TODO: use threadpool to prevent potential system starvation?
pub fn sync<'b, F, R>(f : F) -> R
where F : FnOnce() -> R + 'b {

    struct FakeSend<F>(F);

    unsafe impl<F> Send for FakeSend<F> { };

    let f = FakeSend(f);

    let coroutine = tl_coroutine_current();

    if coroutine.sync_mailbox.is_none() {
        let (send, recv) = mail::mailbox();
        coroutine.sync_mailbox = Some((send, recv));
    }

    let &(ref mail_send, ref mail_recv) = coroutine.sync_mailbox.as_ref().unwrap();
    let join = unsafe {thread_scoped::scoped(move || {
        let FakeSend(f) = f;
        let res = f();
        mail_send.send(());
        FakeSend(res)
    })};

    mail_recv.read();

    let FakeSend(res) = join.join();
    res
}

/// Gets a reference to the user data set through `set_userdata`. Returns `None` if `T` does not match or if no data was set
pub fn get_userdata<'a, T: Any>() -> Option<&'a T>
{
    let coroutine = tl_coroutine_current();
    match coroutine.user_data {
        Some(ref arc) => {
            let boxed_any: &Box<Any+Send+Sync> = arc.as_ref();
            let any: &Any = boxed_any.as_ref();
            any.downcast_ref::<T>()
        },
        None => None,
    }
}

/// Sets new user data for the current coroutine
pub fn set_userdata<T: Reflect + Send + Sync + 'static>(data: T)
{
    let mut coroutine = tl_coroutine_current();
    coroutine.user_data = Some(Arc::new(Box::new(data)));
}

/// Sets new user data that will inherit to newly spawned coroutines. Use `None` to clear.
pub fn set_children_userdata<T: Reflect + Send + Sync + 'static>(data: Option<T>)
{
    let mut coroutine = tl_coroutine_current();
    coroutine.inherited_user_data = match data {
        Some(data) => Some(Arc::new(Box::new(data))),
        None => None,
    }
}

/// Get number of threads of the Mioco instance that coroutine is
/// running in.
///
/// This is useful for load balancing: spawning as many coroutines as
/// there is handling threads that can run them.
pub fn thread_num() -> usize {
    let coroutine = tl_coroutine_current();

    let handler_shared = coroutine.handler_shared.as_ref().unwrap().borrow();
    handler_shared.thread_num()
}

/// Block coroutine for a given time
///
/// Warning: The precision of this call (and other `timer()` like
/// functionality) is limited by `mio` event loop settings. Any small
/// value of `time_ms` will effectively be rounded up to
/// `mio_orig::EventLoop::timer_tick_ms()`.
pub fn sleep(time_ms : i64) {
    let mut timer = Timer::new();
    timer.set_timeout(time_ms);
    let _ = timer.read();
}

/// Yield coroutine execution
///
/// Coroutine can yield execution without blocking on anything
/// particular to allow scheduler to run other coroutines before
/// resuming execution of the current one.
///
/// For this to be effective, custom scheduler must be implemented.
/// See `trait Scheduler`.
///
/// Note: named `yield_now` as `yield` is reserved word.
pub fn yield_now() {
    let coroutine = tl_coroutine_current();
    coroutine.state = State::Yielding;
    trace!("Coroutine({}): yield", coroutine.id.as_usize());
    coroutine_jump_out(&coroutine.self_rc.as_ref().unwrap());
    entry_point(&coroutine.self_rc.as_ref().unwrap());
    trace!("Coroutine({}): resumed after yield ", coroutine.id.as_usize());
    debug_assert!(coroutine.state.is_running());
}

/// Wait till an event is ready
///
/// Use `select!` macro instead.
///
/// **Warning**: Mioco can't guarantee that the returned `EventSource` will
/// not block when actually attempting to `read` or `write`. You must
/// use `try_read` and `try_write` instead.
///
/// The returned value contains event type and the id of the `EventSource`.
/// See `EventSource::id()`.
pub fn select_wait() -> Event {
    let coroutine = tl_coroutine_current();
    coroutine.state = State::Blocked;

    trace!("Coroutine({}): blocked on select", coroutine.id.as_usize());
    coroutine_jump_out(&coroutine.self_rc.as_ref().unwrap());

    entry_point(&coroutine.self_rc.as_ref().unwrap());
    trace!("Coroutine({}): resumed due to event {:?}", coroutine.id.as_usize(), coroutine.last_event);
    debug_assert!(coroutine.state.is_running());
    let e = coroutine.last_event;
    e
}

/// **Warning**: Mioco can't guarantee that the returned `EventSource` will
/// not block when actually attempting to `read` or `write`. You must
/// use `try_read` and `try_write` instead.
///
#[macro_export]
macro_rules! select {
    (@wrap1 ) => {};
    (@wrap1 $rx:ident:r => $code:expr, $($tail:tt)*) => {
        unsafe {
            use $crate::Evented;
            $rx.select_add($crate::RW::read());
        }
        select!(@wrap1 $($tail)*)
    };
    (@wrap1 $rx:ident:w => $code:expr, $($tail:tt)*) => {
        unsafe {
            use $crate::Evented;
            $rx.select_add($crate::RW::write());
        }
        select!(@wrap1 $($tail)*)
    };
    (@wrap1 $rx:ident:rw => $code:expr, $($tail:tt)*) => {
        unsafe {
            use $crate::Evented;
            $rx.select_add($crate::RW::both());
        }
        select!(@wrap1 $($tail)*)
    };
    (@wrap2 $ret:ident) => {
        // end code
    };
    (@wrap2 $ret:ident $rx:ident:r => $code:expr, $($tail:tt)*) => {{
        use $crate::Evented;
        if $ret.id() == $rx.id() { $code }
        select!(@wrap2 $ret $($tail)*);
    }};
    (@wrap2 $ret:ident $rx:ident:w => $code:expr, $($tail:tt)*) => {{
        use $crate::Evented;
        if $ret.id() == $rx.id() { $code }
        select!(@wrap2 $ret $($tail)*);
    }};
    (@wrap2 $ret:ident $rx:ident:rw => $code:expr, $($tail:tt)*) => {{
        use $crate::Evented;
        if ret.id() == $rx.id() { $code }
        select!(@wrap2 $ret $($tail)*);
    }};
    ($($tail:tt)*) => {{
        select!(@wrap1 $($tail)*);
        let ret = mioco::select_wait();
        select!(@wrap2 ret $($tail)*);
    }};
}

#[cfg(test)]
mod tests;
