// Copyright 2015 Dawid Ciężarkiewicz <dpc@dpc.pw>
// See LICENSE-MPL2 file for more information.

//! # Mioco
//!
//! Scalable, asynchronous IO coroutine-based handling (aka MIO COroutines).
//!
//! Using `mioco` you can handle scalable, asynchronous [`mio`][mio]-based IO, using set of synchronous-IO
//! handling functions. Based on asynchronous [`mio`][mio] events `mioco` will cooperatively schedule your
//! handlers.
//!
//! You can think of `mioco` as of *Node.js for Rust* or *[green threads][green threads] on top of [`mio`][mio]*.
//!
//! # <a name="features"></a> Features:
//!
//! ```norust
//! * all `mio` types can be used (see `MiocoHandle::wrap()`);
//! * timers (see `MiocoHandle::timer()`);
//! * mailboxes (see `mailbox()`);
//! * coroutine exit notification (see `CoroutineHandle`).
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

#![cfg_attr(test, feature(convert))]
#![feature(reflect_marker)]
#![feature(catch_panic)]
#![feature(drain)]
#![warn(missing_docs)]

extern crate spin;
extern crate mio;
extern crate context;
extern crate nix;
#[macro_use]
extern crate log;
extern crate bit_vec;
extern crate time;
extern crate num_cpus;

use std::cell::RefCell;
use std::rc::{Rc};
use std::io;
use std::thread;
use std::mem::{transmute, size_of_val};

use mio::{TryRead, TryWrite, Token, EventLoop, EventSet};
use std::any::Any;
use std::marker::{PhantomData, Reflect};
use mio::util::Slab;

use std::collections::VecDeque;
use spin::Mutex;
use std::sync::{Arc};
use std::sync::atomic::{AtomicUsize, Ordering};

use bit_vec::BitVec;

use time::{SteadyTime, Duration};

use context::{Context, Stack};
use context::thunk::Thunk;

use Message::*;

type RcCoroutine = Rc<RefCell<Coroutine>>;
type RcEventSourceShared = Rc<RefCell<EventSourceShared>>;
type ArcMailboxShared<T> = Arc<Mutex<MailboxShared<T>>>;
type RcHandlerShared = Rc<RefCell<HandlerShared>>;
type RcCoroutineShared = Rc<RefCell<CoroutineShared>>;
type ArcHandlerThreadShared = Arc<HandlerThreadShared>;

/// Read/Write/Both
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum RW {
    /// Read
    Read,
    /// Write
    Write,
    /// Any / Both (depends on context)
    Both,
}

impl RW {
    fn has_read(&self) -> bool {
        match *self {
            RW::Read | RW::Both => true,
            RW::Write => false,
        }
    }

    fn has_write(&self) -> bool {
        match *self {
            RW::Write | RW::Both => true,
            RW::Read => false,
        }
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
    /// Index of the EventSourceShared handle
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

/// Value returned by coroutine or panic
#[derive(Clone, Debug)]
pub enum ExitStatus {
    /// Coroutine panicked
    Panic,
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
    /// Blocked on RW
    BlockedOn(RW),
    /// Currently running
    Running,
    /// Ready to be started
    Ready,
    /// Finished
    Finished(ExitStatus),
}

impl State {
    /// Is the `State` a `Finished(_)`?
    fn is_finished(&self) -> bool {
        match *self {
            State::Finished(_) => true,
            _ => false,
        }
    }

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
            State::BlockedOn(_) => true,
            _ => false,
        }
    }
}

/// Sends notify `Message` to the mioco Event Loop.
type MioSender = mio::Sender<<Handler as mio::Handler>::Message>;

/// `mioco` can work on any type implementing this trait
pub trait Evented : Any {
    /// Convert to &Any
    fn as_any(&self) -> &Any;
    /// Convert to &mut Any
    fn as_any_mut(&mut self) -> &mut Any;

    /// Register
    fn register(&self, event_loop : &mut EventLoop<Handler>, token : Token, interest : EventSet);

    /// Reregister
    fn reregister(&self, event_loop : &mut EventLoop<Handler>, token : Token, interest : EventSet);

    /// Deregister
    fn deregister(&self, event_loop : &mut EventLoop<Handler>, token : Token);

    /// Should the coroutine be resumed on event for this `EventSource<Self>`
    fn should_resume(&self) -> bool {
        true
    }
}

impl<T> Evented for T
where T : mio::Evented+Reflect+'static {
    fn as_any(&self) -> &Any {
        self as &Any
    }

    fn as_any_mut(&mut self) -> &mut Any {
        self as &mut Any
    }

    fn register(&self, event_loop : &mut EventLoop<Handler>, token : Token, interest : EventSet) {
        trace!("Evented({}): register", token.as_usize());
        event_loop.register_opt(
            self, token,
            interest,
            mio::PollOpt::edge(),
            ).expect("register_opt failed");
    }

    fn reregister(&self, event_loop : &mut EventLoop<Handler>, token : Token, interest : EventSet) {
        trace!("Evented({}): reregister", token.as_usize());
        event_loop.reregister(
            self, token,
            interest,
            mio::PollOpt::edge(),
            ).expect("reregister failed");
    }

    fn deregister(&self, event_loop : &mut EventLoop<Handler>, token : Token) {
        trace!("Evented({}): deregister", token.as_usize());
        event_loop.deregister(self).expect("deregister failed");
    }
}

/// Retry `mio::Sender::send()`.
///
/// As channels can fail randomly (eg. when Full), take care
/// of retrying on recoverable errors.
fn sender_retry<M : Send>(sender : &mio::Sender<M>, msg : M) {
    let mut msg = Some(msg);
    loop {
        match sender.send(msg.take().expect("sender_retry")) {
            Ok(()) => break,
            Err(mio::NotifyError::Closed(_)) => panic!("Closed channel on sender.send()."),
            Err(mio::NotifyError::Io(_)) => panic!("IO error on sender.send()."),
            Err(mio::NotifyError::Full(retry_msg)) => {
                msg = Some(retry_msg);
            },
        }
        trace!("send_retry: retry");
        thread::yield_now();
    }
}

impl<T> Evented for MailboxInnerEnd<T>
where T:Reflect+'static {
    fn as_any(&self) -> &Any {
        self as &Any
    }

    fn as_any_mut(&mut self) -> &mut Any {
        self as &mut Any
    }

    fn register(&self, event_loop : &mut EventLoop<Handler>, token : Token, interest : EventSet) {
        trace!("MailboxInnerEnd({}): register", token.as_usize());
        let mut lock = self.shared.lock();

        lock.token = Some(token);
        lock.sender = Some(event_loop.channel());
        lock.interest = interest;

        if interest.is_readable() && !lock.inn.is_empty() {
            trace!("MailboxInnerEnd({}): not empty; self notify", token.as_usize());
            lock.interest = EventSet::none();
            sender_retry(lock.sender.as_ref().unwrap(), MailboxMsg(token));
        }
    }

    fn reregister(&self, _event_loop : &mut EventLoop<Handler>, token : Token, interest : EventSet) {
        trace!("MailboxInnerEnd({}): reregister", token.as_usize());
        let mut lock = self.shared.lock();

        lock.interest = interest;

        if interest.is_readable() && !lock.inn.is_empty() {
            lock.interest = EventSet::none();
            sender_retry(lock.sender.as_ref().unwrap(), MailboxMsg(token));
        }
    }

    fn deregister(&self, _event_loop : &mut EventLoop<Handler>, token : Token) {
        trace!("MailboxInnerEnd({}): dereregister", token.as_usize());
        let mut lock = self.shared.lock();
        lock.token = None;
        lock.sender = None;
        lock.interest = EventSet::none();
    }

    fn should_resume(&self) -> bool {
        let lock = self.shared.lock();
        trace!("MailboxInnerEnd: should_resume? {}", !lock.inn.is_empty());
        !lock.inn.is_empty()
    }
}

impl Evented for Timer {
    fn as_any(&self) -> &Any {
        self as &Any
    }

    fn as_any_mut(&mut self) -> &mut Any {
        self as &mut Any
    }

    fn register(&self, event_loop : &mut EventLoop<Handler>, token : Token, _interest : EventSet) {
        let timeout = self.timeout;
        let now = SteadyTime::now();
        let delay = if timeout <= now {
            0
        } else {
            (timeout - now).num_milliseconds()
        };

        trace!("Timer({}): set timeout in {}ms", token.as_usize(), delay);
        match event_loop.timeout_ms(token, delay as u64) {
            Ok(_) => {},
            Err(reason)=> {
                panic!("Could not create mio::Timeout: {:?}", reason);
            }
        }
    }

    fn reregister(&self, event_loop : &mut EventLoop<Handler>, token : Token, interest : EventSet) {
        self.register(event_loop, token, interest)
    }

    fn deregister(&self, _event_loop : &mut EventLoop<Handler>, _token : Token) {
    }

    fn should_resume(&self) -> bool {
        trace!("Timer: should_resume? {}", self.timeout <= SteadyTime::now());
        self.timeout <= SteadyTime::now()
    }
}

/// Shared Coroutine data
// TODO: Might be it can just be merged into
// `Coroutine` itself as primary reason it was created:
// preventing Rc cycles, could be taken care of by
// `catch_panic`
struct CoroutineShared {
    /// Context with a state of coroutine
    context: Context,

    /// Current state
    state : State,

    /// Last event that resumed the coroutine
    last_event: Event,

    /// `Handler` shared data that this `Coroutine` is running in
    handler_shared : Option<RcHandlerShared>,

    /// Mask of handle ids that we're blocked on
    blocked_on : BitVec<usize>,

    // TODO: Move to Coroutine
    /// Mask of handle ids that are registered in Handler
    registered : BitVec<usize>,

    /// `Coroutine` will send exit status on it's finish
    /// through this list of Mailboxes
    exit_notificators : Vec<MailboxOuterEnd<ExitStatus>>,

    /// Current coroutine Id
    id : CoroutineId,
}

/// *`mioco` coroutine* (a.k.a. *mioco handler*)
pub struct Coroutine {
    /// Shared data
    shared : RcCoroutineShared,

    /// Coroutine stack
    stack: Stack,

    /// All event sources
    io : Vec<RcEventSourceShared>,

    /// Newly spawned `Coroutine`-es
    children_to_start : Vec<RcCoroutine>,

    /// Function to be run inside Coroutine
    coroutine_func : Option<Box<FnOnce(&mut MiocoHandle) -> io::Result<()> + Send + 'static>>,
}

/// Mioco Handler keeps only Slab of Coroutines, and uses a scheme in which
/// Token bits encode both Coroutine and EventSource within it
const EVENT_SOURCE_TOKEN_BITS : usize = 10;
const EVENT_SOURCE_TOKEN_MASK : usize = (1 << EVENT_SOURCE_TOKEN_BITS) - 1;

/// Convert token to ids
fn token_to_ids(token : Token) -> (CoroutineId, EventSourceId) {
    let val = token.as_usize();
    (
        CoroutineId(val >> EVENT_SOURCE_TOKEN_BITS),
        EventSourceId(val & EVENT_SOURCE_TOKEN_MASK),
    )
}

/// Convert ids to Token
fn token_from_ids(co_id : CoroutineId, io_id : EventSourceId) -> Token {
    // TODO: Add checks on wrap()
    debug_assert!(io_id.as_usize() <= EVENT_SOURCE_TOKEN_MASK);
    Token((co_id.as_usize() << EVENT_SOURCE_TOKEN_BITS) | io_id.as_usize())
}

/// Coroutine control block
///
/// Through this interface Coroutine can be resumed and migrated.
pub struct CoroutineControl {
    rc : RcCoroutine,
}


impl CoroutineControl {
    fn new(rc : RcCoroutine) -> Self {
        CoroutineControl {
            rc: rc
        }
    }

    /// Private Clone to prevent any references after `migrate()`.
    fn clone_priv(&self) -> Self {
        CoroutineControl {
            rc: self.rc.clone(),
        }
    }
    /// Deliver an event to a Coroutine
    fn event(
        &self,
        event_loop : &mut EventLoop<Handler>,
        scheduler : &mut (SchedulerThread+'static),
        token : Token,
        events : EventSet,
        ) -> bool {
        let (_, io_id) = token_to_ids(token);

        trace!("Coroutine({}): event", self.id().as_usize());
        let (should_resume, should_reregister) = {
            let coroutine = self.rc.borrow();
            let io = coroutine.io[io_id.as_usize()].clone();

            if events.is_hup() {
                io.borrow_mut().hup(event_loop, token);
            }

            let mut co_shared = coroutine.shared.borrow_mut();

            co_shared.registered.set(io_id.as_usize(), false);

            if let State::BlockedOn(rw) = co_shared.state {

                if !co_shared.blocked_on.get(io_id.as_usize()).unwrap() {
                    // spurious event, probably after select in which
                    // more than one event sources were reported ready
                    // in one group of events, and first event source
                    // deregistered the later ones
                    debug!("spurious event for event source coroutine is not blocked on");
                    (false, false)
                } else {
                    match rw {
                        RW::Read if !events.is_readable() && !events.is_hup() => {
                            debug!("spurious not read event for coroutine blocked on read");
                            (false, false)
                        },
                        RW::Write if !events.is_writable() => {
                            debug!("spurious not write event for coroutine blocked on write");
                            (false, false)
                        },
                        RW::Both if !events.is_readable() && !events.is_hup() && !events.is_writable() => {
                            debug!("spurious unknown type event for coroutine blocked on read/write");
                            (false, false)
                        },
                        _ => {
                            if io.borrow().io.should_resume() {
                                (true, false)
                            } else {
                                // TODO: Actually, we can just reregister the Timer,
                                // not all sources, and in just this one case
                                (false, true)
                            }
                        }
                    }
                }
            } else {
                // subsequent event to coroutine that is either already
                // Finished, or Ready
                (false, false)
            }
        };

        if should_resume {
            trace!("Coroutine({}): set to ready", self.id().as_usize());
            // Wake coroutine on HUP, as it was read, to potentially let it fail the read and move on
            let event = match (events.is_readable() | events.is_hup(), events.is_writable()) {
                (true, true) => RW::Both,
                (true, false) => RW::Read,
                (false, true) => RW::Write,
                (false, false) => panic!(),
            };

            let shared = &self.rc.borrow().shared;
            let mut co_shared = shared.borrow_mut();
            co_shared.state = State::Ready;
            co_shared.last_event = Event {
                rw: event,
                id: io_id,
            };
            true
        } else if should_reregister {
            trace!("Coroutine({}): event ignored (reregister)", self.id().as_usize());
            // Wake coroutine on HUP, as it was read, to potentially let it fail the read and move on
            self.after_resume(event_loop, scheduler);
            false
        } else {
            trace!("Coroutine({}): event ignored (no reregister)", self.id().as_usize());
            false
        }
    }

    /// Resume Coroutine
    ///
    /// Panics if Coroutine is not in Ready state.
    pub fn resume(
        &self,
        event_loop : &mut EventLoop<Handler>,
        scheduler : &mut (SchedulerThread+'static),
        ) {
        trace!("Coroutine({}): resume", self.id().as_usize());
        let shared = self.rc.borrow().shared.clone();
        let is_ready = shared.borrow().state.is_ready();
        if is_ready {
            coroutine_jump_in(&shared);
            self.after_resume(event_loop, scheduler);
        } else {
            panic!("Tried to resume Coroutine that is not ready");
        }
    }

    fn id(&self) -> CoroutineId {
        let coroutine = self.rc.borrow();
        let shared = coroutine.shared.borrow();
        shared.id
    }

    /// After `resume()` (or ignored event()) we need to perform the following maintenance
    fn after_resume(
        &self,
        event_loop: &mut EventLoop<Handler>,
        scheduler : &mut (SchedulerThread+'static),
        ) {
        // Take care of newly spawned child-coroutines: start them now
        let mut children = Vec::new();

        debug_assert!(!self.rc.borrow().state().is_running());
        debug_assert!(!self.rc.borrow().state().is_ready());

        std::mem::swap(&mut children, &mut self.rc.borrow_mut().children_to_start);

        trace!("Coroutine({}): {} children spawned", self.id().as_usize(), children.len());
        for coroutine in children.drain(..) {
            let coroutine_ctrl = CoroutineControl { rc: coroutine };
            scheduler.new(event_loop, coroutine_ctrl);
        }

        self.rc.borrow_mut().reregister(event_loop);
    }

    /// Migrate to a different thread
    ///
    /// Move this Coroutine to be executed on a `SchedulerThread` for a
    /// given `thread_id`.
    ///
    /// Will panic if `thread_id` is not valid.
    pub fn migrate(
        self,
        event_loop : &mut EventLoop<Handler>,
        thread_id : usize,
        ) {
        let sender = {
            trace!("Coroutine({}): migrate to thread {}", self.id().as_usize(), thread_id);
            let mut co = self.rc.borrow_mut();
            co.deregister_all(event_loop);
            let mut co_shared = co.shared.borrow_mut();
            co_shared.registered.clear();

            let id = co_shared.id;
            // TODO: https://github.com/contain-rs/bit-vec/pulls
            co_shared.blocked_on.clear();
            let handler_shared = co_shared.handler_shared.take();
            debug_assert!(co_shared.handler_shared.is_none());
            let mut handler_shared = handler_shared.as_ref().unwrap().borrow_mut();
            handler_shared.coroutines.remove(Token(id.as_usize())).unwrap();
            handler_shared.senders[thread_id].clone()
        };

        // TODO: Spin on failure
        sender_retry(&sender, Migration(self.rc));
    }


    /// Finish migrating Coroutine by attaching it to a new thread
    fn reattach_to(
        &mut self,
        handler : &mut Handler,
        ) {
        let handler_shared = handler.shared.clone();

        trace!("Coroutine({}): reattach in a new thread", self.id().as_usize());
        let _id = handler.shared.borrow_mut().coroutines.insert_with(|id| {
            let id = CoroutineId(id.as_usize());

            let co = self.rc.borrow();
            let mut co_shared = co.shared.borrow_mut();

            co_shared.id = id;

            co_shared.handler_shared = Some(handler_shared);

            self.clone_priv()
        }).expect("Run out of slab for coroutines");
    }
}

impl Coroutine {

    /// Spawn a new Coroutine
    fn spawn<F>(handler_shared : RcHandlerShared, f : F) -> RcCoroutine
    where F : FnOnce(&mut MiocoHandle) -> io::Result<()> + Send + 'static {
        trace!("Coroutine: spawning");
        let id = handler_shared.borrow_mut().coroutines.insert_with(|id| {
            let id = CoroutineId(id.as_usize());

            let shared = CoroutineShared {
                state: State::Ready,
                id: id,
                last_event: Event{ rw: RW::Read, id: EventSourceId(0)},
                context: Context::empty(),
                handler_shared: Some(handler_shared.clone()),
                blocked_on: Default::default(),
                exit_notificators: Vec::new(),
                registered: Default::default(),
            };

            let coroutine = Coroutine {
                shared: Rc::new(RefCell::new(shared)),
                io: Vec::with_capacity(4),
                children_to_start: Vec::new(),
                stack: Stack::new(1024 * 1024),
                coroutine_func: Some(Box::new(f)),
            };

            CoroutineControl::new(Rc::new(RefCell::new(coroutine)))
        }).expect("Run out of slab for coroutines");
        handler_shared.borrow_mut().coroutines_inc();

        let coroutine_rc = handler_shared.borrow().coroutines[id].rc.clone();

        let coroutine_ptr = {
            // The things we do for borrowck...
            let coroutine_ptr = {
                &*coroutine_rc.borrow() as *const Coroutine
            };
            coroutine_ptr
        };

        #[derive(Copy, Clone)]
        #[allow(raw_pointer_derive)]
        struct SendCoroutinePtr {
            coroutine_ptr: *const Coroutine,
        }

        let send_coroutine_ptr = SendCoroutinePtr {
            coroutine_ptr: coroutine_ptr,
        };

        unsafe impl Send for SendCoroutinePtr { }

        extern "C" fn init_fn(arg: usize, f: *mut ()) -> ! {
            let func: Box<Thunk<(), ()>> = unsafe { transmute(f) };

            func.invoke(());

           let ctx : &Context = {
               let coroutine: &Coroutine= unsafe { transmute(arg) };
               let coroutine_shared = coroutine.shared.borrow();
               unsafe {
                   let handler = coroutine_shared.handler_shared.as_ref().unwrap().borrow();
                   transmute(&handler.context as *const Context)
               }
           };

            let mut dummy = Context::empty();
            Context::swap(&mut dummy, ctx);

            unreachable!();
        }

        {
            let Coroutine {
                ref mut stack,
                ref shared,
                ..
            } = *coroutine_rc.borrow_mut();

            let CoroutineShared {
                ref mut context,
                ..
            } = *shared.borrow_mut();

            context.init_with(
                init_fn,
                coroutine_ptr as usize,
                move || {
                    let res = thread::catch_panic(
                        move|| {
                            let coroutine : &mut Coroutine = unsafe { transmute(send_coroutine_ptr.coroutine_ptr) };
                            trace!("Coroutine({}): started", {
                                let shared = coroutine.shared.borrow();
                                shared.id.as_usize()
                            });

                            let f = coroutine.coroutine_func.take().unwrap();

                            let mut mioco_handle = MiocoHandle {
                                coroutine: coroutine,
                                timer: None,
                            };

                            f(&mut mioco_handle)
                        }
                        );

                    let coroutine : &mut Coroutine = unsafe { transmute(send_coroutine_ptr.coroutine_ptr) };
                    coroutine.io.clear();
                    let mut co_shared = coroutine.shared.borrow_mut();

                    let id = co_shared.id;
                    // TODO: https://github.com/contain-rs/bit-vec/pulls
                    co_shared.blocked_on.clear();
                    {
                        let mut handler_shared = co_shared.handler_shared.as_ref().unwrap().borrow_mut();
                        handler_shared.coroutines.remove(Token(id.as_usize())).unwrap();
                        handler_shared.coroutines_dec();
                    }

                    match res {
                        Ok(res) => {
                            trace!("Coroutine({}): finished returning {:?}", id.as_usize(), res);
                            let arc_res = Arc::new(res);
                            co_shared.exit_notificators.iter().map(
                                |end| end.send(ExitStatus::Exit(arc_res.clone()))
                                ).count();
                            co_shared.state = State::Finished(ExitStatus::Exit(arc_res));

                        },
                        Err(cause) => {
                            trace!("Coroutine({}): panicked: {:?}", id.as_usize(), cause.downcast::<&str>());
                            co_shared.state = State::Finished(ExitStatus::Panic);
                            co_shared.exit_notificators.iter().map(
                                |end| end.send(ExitStatus::Panic)
                                ).count();
                        }
                    }
                },
                stack,
                );
        }

        coroutine_rc
    }

    fn state(&self) -> State {
        self.shared.borrow().state.clone()
    }

    fn reregister(&mut self, event_loop: &mut EventLoop<Handler>) {
        if self.state().is_finished() {
            trace!("Coroutine: deregistering");
            self.deregister_all(event_loop);
        } else {
            self.reregister_blocked_on(event_loop)
        }
    }

    fn deregister_all(&mut self, event_loop: &mut EventLoop<Handler>) {
        for i in 0..self.io.len() {
            let mut io = self.io[i].borrow_mut();
            io.deregister(event_loop);
        }
    }

    fn reregister_blocked_on(&mut self, event_loop: &mut EventLoop<Handler>) {

        let rw = match self.shared.borrow().state {
            State::BlockedOn(rw) => rw,
            _ => panic!("This should not happen"),
        };

        let Coroutine {
            ref mut io,
            ref shared,
            ..
        } = *self;
        {
            let CoroutineShared {
                ref blocked_on,
                ref registered,
                ..
            } = *shared.borrow();

            let mut i = 0;
            for (registered_block, blocked_on_block) in registered.blocks().zip(blocked_on.blocks()) {
                debug_assert!(size_of_val(&registered_block) == size_of_val(&blocked_on_block));
                let bit_size = size_of_val(&registered_block) * 8;
                let mut block = registered_block ^ blocked_on_block;
                'for_each_set_bit: loop {
                    let lz = block.leading_zeros() as usize;
                    if lz == bit_size {
                        break 'for_each_set_bit
                    } else {
                        let bit = bit_size - 1 - lz;
                        debug_assert!(bit < bit_size);
                        block &= !(1 << bit);
                        let mut io = io[i + bit].borrow_mut();
                        if registered_block & (1 << bit) != 0 {
                            debug_assert!(blocked_on_block & (1 << bit) == 0);
                            io.unreregister(event_loop);
                        } else {
                            debug_assert!(blocked_on_block & (1 << bit) != 0);
                            io.reregister(event_loop, rw);
                        }
                    }
                }
                i += bit_size;
            }
        }

        let CoroutineShared {
            ref mut blocked_on,
            ref mut registered,
            ..
        } = *shared.borrow_mut();


        // effectively: self.registered = self.blocked_on;
        for (mut target, src) in unsafe { registered.storage_mut().iter_mut().zip(blocked_on.storage().iter()) } {
            *target = *src
        }
    }
}

/// Resume coroutine execution, jumping into it
fn coroutine_jump_in(coroutine_shared : &RefCell<CoroutineShared>) {
    if !coroutine_shared.borrow().state.is_ready() {
        return
    }
    coroutine_shared.borrow_mut().state = State::Running;

    // We know that we're holding at least one Rc to the Coroutine,
    // and noone else is holding a reference as we can do `.borrow_mut()`
    // so we cheat with unsafe just to context-switch from coroutine
    // without having RefCells still borrowed.
    let (context_in, context_out) = {
        let CoroutineShared {
            ref context,
            ref handler_shared,
            ..
        } = *coroutine_shared.borrow_mut();
        {
            let mut shared_context = &mut handler_shared.as_ref().unwrap().borrow_mut().context;
            (context as *const Context, shared_context as *mut Context)
        }
    };

    Context::swap(unsafe {&mut *context_out}, unsafe {&*context_in});
}

/// Block coroutine execution, jumping out of it
fn coroutine_jump_out(coroutine_shared : &RefCell<CoroutineShared>) {
    debug_assert!(coroutine_shared.borrow().state.is_blocked());

    // See `resume()` for unsafe comment
    let (context_in, context_out) = {
        let CoroutineShared {
            ref mut context,
            ref handler_shared,
            ..
        } = *coroutine_shared.borrow_mut();
        {
            let shared_context = &mut handler_shared.as_ref().unwrap().borrow_mut().context;
            (context as *mut Context, shared_context as *const Context)
        }
    };

    Context::swap(unsafe {&mut *context_in}, unsafe {&*context_out});
}

/// Wrapped mio IO (mio::Evented+TryRead+TryWrite)
///
/// `Handle` is just a cloneable reference to this struct
struct EventSourceShared {
    coroutine_shared: RcCoroutineShared,
    id : EventSourceId,
    io : Box<Evented+'static>,
    peer_hup: bool,
    registered: bool,
}

impl EventSourceShared {
    /// Handle `hup` condition
    fn hup(&mut self, _event_loop: &mut EventLoop<Handler>, _token: Token) {
        trace!("hup");
        self.peer_hup = true;
    }

    /// Reregister oneshot handler for the next event
    fn reregister(&mut self, event_loop: &mut EventLoop<Handler>, rw : RW) {
            let mut interest = mio::EventSet::none();

            if !self.peer_hup {
                interest = interest | mio::EventSet::hup();

                if rw.has_read() {
                    interest = interest | mio::EventSet::readable();
                }
            }

            if rw.has_write() {
                interest = interest | mio::EventSet::writable();
            }

            let token = token_from_ids(self.coroutine_shared.borrow().id, self.id);

            if !self.registered {
                self.registered = true;
                Evented::register(&*self.io, event_loop, token, interest);
            } else {
                Evented::reregister(&*self.io, event_loop, token, interest);
             }
        }

    /// Un-reregister events we're not interested in anymore
    fn unreregister(&self, event_loop: &mut EventLoop<Handler>) {
            debug_assert!(self.registered);
            let interest = mio::EventSet::none();
            let token = token_from_ids(self.coroutine_shared.borrow().id, self.id);
            Evented::reregister(&*self.io, event_loop, token, interest);
        }

    /// Un-reregister events we're not interested in anymore
    fn deregister(&mut self, event_loop: &mut EventLoop<Handler>) {
            if self.registered {
                let token = token_from_ids(self.coroutine_shared.borrow().id, self.id);
                Evented::deregister(&*self.io, event_loop, token);
                self.registered = false;
            }
        }
}

/// Event source inside a coroutine
///
/// Event sources are a core of Mioco. Mioco coroutines use them to handle
/// IO in a blocking fashion.
///
/// They come in different flavours and can be created from native `mio` types by wrapping within
/// a coroutine with `MiocoHandle::wrap()` or type-specific constructors like `mailbox()` or
/// `MiocoHandle::timer()`.
#[derive(Clone)]
pub struct EventSource<T> {
    inn : RcEventSourceShared,
    _t: PhantomData<T>,
}

/// Id of an event source used to enumerate them
///
/// It's unique within coroutine of an event source, but not globally.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct EventSourceId(usize);

impl EventSourceId {
    fn as_usize(&self) -> usize {
        self.0
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


impl<T> EventSource<T>
where T : Reflect+'static {
    /// Mark the `EventSourceRef` blocked and block until `Handler` does
    /// not wake us up again.
    fn block_on(&self, rw : RW) {
        {
            let inn = self.inn.borrow();
            let mut co_shared = inn.coroutine_shared.borrow_mut();
            trace!("Coroutine({}): blocked on {:?}", co_shared.id.as_usize(), rw);
            co_shared.state = State::BlockedOn(rw);
            // TODO: https://github.com/contain-rs/bit-vec/pulls
            co_shared.blocked_on.clear();
            co_shared.blocked_on.set(inn.id.as_usize(), true);
        };
        let co_shared_ref = self.inn.borrow().coroutine_shared.clone();
        coroutine_jump_out(&co_shared_ref);
        {
            let inn = self.inn.borrow_mut();
            let co_shared = inn.coroutine_shared.borrow_mut();
            trace!("Coroutine({}): resumed due to event {:?}", co_shared.id.as_usize(), co_shared.last_event);
            debug_assert!(rw.has_read() || co_shared.last_event.has_write());
            debug_assert!(rw.has_write() || co_shared.last_event.has_read());
            debug_assert!(co_shared.last_event.id().as_usize() == inn.id.as_usize());
        }
    }

    /// Access raw mio type
    pub fn with_raw<F, R>(&self, f : F) -> R
        where F : Fn(&T) -> R {
        let io = &self.inn.borrow().io;
        f(io.as_any().downcast_ref::<T>().unwrap())
    }

    /// Access mutable raw mio type
    pub fn with_raw_mut<F, R>(&mut self, f : F) -> R
        where F : Fn(&mut T) -> R {
        let mut io = &mut self.inn.borrow_mut().io;
        f(io.as_any_mut().downcast_mut::<T>().unwrap())
    }

    /// Index identificator of a `EventSource`
    pub fn id(&self) -> EventSourceId {
        EventSourceId(self.inn.borrow().id.as_usize())
    }
}

impl<T> EventSource<T>
where T : mio::TryAccept+Reflect+'static {
    /// Block on accept
    pub fn accept(&self) -> io::Result<T::Output> {
        loop {
            let res = {
                let mut inn = self.inn.borrow_mut();
                inn.io.as_any_mut().downcast_mut::<T>().unwrap().accept()
            };

            match res {
                Ok(None) => {
                    self.block_on(RW::Read)
                },
                Ok(Some(r))  => {
                    return Ok(r);
                },
                Err(e) => {
                    return Err(e)
                }
            }
        }
    }
}

impl<T> std::io::Read for EventSource<T>
where T : TryRead+Reflect+'static {
    /// Block on read
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let res = {
                let mut inn = self.inn.borrow_mut();
                inn.io.as_any_mut().downcast_mut::<T>().unwrap().try_read(buf)
            };

            match res {
                Ok(None) => {
                    self.block_on(RW::Read)
                },
                Ok(Some(r))  => {
                    return Ok(r);
                },
                Err(e) => {
                    return Err(e)
                }
            }
        }
    }
}

impl<T> EventSource<T>
where T : TryRead+Reflect+'static {
    /// Try to read without blocking
    pub fn try_read(&mut self, buf: &mut [u8]) -> std::io::Result<Option<usize>> {
        let mut inn = self.inn.borrow_mut();
        inn.io.as_any_mut().downcast_mut::<T>().unwrap().try_read(buf)
    }
}

impl<T> std::io::Write for EventSource<T>
where T : TryWrite+Reflect+'static {
    /// Block on write
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        loop {
            let res = {
                let mut inn = self.inn.borrow_mut();
                inn.io.as_any_mut().downcast_mut::<T>().unwrap().try_write(buf)
            };

            match res {
                Ok(None) => {
                    self.block_on(RW::Write)
                },
                Ok(Some(r)) => {
                    return Ok(r);
                },
                Err(e) => {
                    return Err(e)
                }
            }
        }
    }

    /// Flush. This currently does nothing
    ///
    /// TODO: Should we do something with the flush? --dpc */
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<T> EventSource<T>
where T : TryWrite+Reflect+'static {
    /// Try to write without blocking
    pub fn try_write(&mut self, buf: &[u8]) -> std::io::Result<Option<usize>> {
        let mut inn = self.inn.borrow_mut();
        inn.io.as_any_mut().downcast_mut::<T>().unwrap().try_write(buf)
    }
}


/// Mioco instance handle
///
/// Use this from withing coroutines to use Mioco-provided functionality.
pub struct MiocoHandle<'a> {
    coroutine : &'a mut Coroutine,
    timer : Option<EventSource<Timer>>,
}

fn select_impl_set_mask_from_ids(ids : &[EventSourceId], blocked_on : &mut BitVec<usize>) {
    {
        // TODO: https://github.com/contain-rs/bit-vec/pulls
        blocked_on.clear();
        for &id in ids {
            blocked_on.set(id.as_usize(), true);
        }
    }
}

fn select_impl_set_mask_rc_handles(handles : &[Rc<RefCell<EventSourceShared>>], blocked_on: &mut BitVec<usize>) {
    {
        // TODO: https://github.com/contain-rs/bit-vec/pulls
        blocked_on.clear();
        for io in handles {
            blocked_on.set(io.borrow().id.as_usize(), true);
        }
    }
}


/// Handle to spawned coroutine
pub struct CoroutineHandle {
     coroutine_shared: RcCoroutineShared,
}

impl CoroutineHandle {
    /// Create an exit notificator
    pub fn exit_notificator(&self) -> MailboxInnerEnd<ExitStatus> {
        let (outer, inner) = mailbox();
        let mut co_shared = self.coroutine_shared.borrow_mut();
        let CoroutineShared {
            ref state,
            ref mut exit_notificators,
            ..
        } = *co_shared;

        if let &State::Finished(ref exit) = state {
            outer.send(exit.clone())
        } else {
            exit_notificators.push(outer);
        }
        inner
    }
}

impl<'a> MiocoHandle<'a> {
    /// Create a `mioco` coroutine handler
    ///
    /// `f` is routine handling connection. It must not use any real blocking-IO operations, only
    /// `mioco` provided types (`EventSource`) and `MiocoHandle` functions. Otherwise `mioco`
    /// cooperative scheduling can block on real blocking-IO which defeats using mioco.
    pub fn spawn<F>(&mut self, f : F) -> CoroutineHandle
        where F : FnOnce(&mut MiocoHandle) -> io::Result<()> + Send + 'static {
            let coroutine_ref = Coroutine::spawn(
                self.coroutine.shared.borrow().handler_shared.as_ref().unwrap().clone(),
                f
                );
            let ret = CoroutineHandle {
                coroutine_shared: coroutine_ref.borrow().shared.clone(),
            };
            self.coroutine.children_to_start.push(coroutine_ref);

            ret
        }

    /// Register `mio`'s native io type to be used within `mioco` coroutine
    ///
    /// Consumes the `io`, returns a mioco wrapper over it. Use this wrapped IO
    /// to perform IO.
    pub fn wrap<T : 'static>(&mut self, io : T) -> EventSource<T>
    where T : Evented {
        let io_new = {
            let co = &self.coroutine;

            Rc::new(RefCell::new(
                    EventSourceShared {
                        coroutine_shared: co.shared.clone(),
                        io: Box::new(io),
                        peer_hup: false,
                        id: EventSourceId(co.io.len()),
                        registered: false,
                    }
                    ))
        };

        let handle = EventSource {
            inn: io_new.clone(),
            _t: PhantomData,
        };

        let &mut Coroutine {
            ref mut io,
            ref shared,
            ..
        } = self.coroutine;

        let CoroutineShared {
            ref mut registered,
            ref mut blocked_on,
            ..
        } = *shared.borrow_mut();

        io.push(io_new.clone());
        blocked_on.push(false);
        registered.push(false);
        debug_assert!(io.len() == blocked_on.len());
        debug_assert!(io.len() == registered.len());

        handle
    }

    /// Get mutable reference to a timer source io for this coroutine
    ///
    /// Each coroutine has one internal Timer source, that will become readable
    /// when it's timeout (see `set_timer()` ) expire.
    pub fn timer(&mut self) -> &mut EventSource<Timer> {
        match self.timer {
            Some(ref mut timer) => timer,
            None => {
                self.timer = Some(self.wrap(Timer::new()));
                self.timer.as_mut().unwrap()
            }
        }
    }

    /// Block coroutine for a given time
    pub fn sleep(&mut self, time_ms : i64) {
        let prev_timeout = self.timer().get_timeout_absolute();
        self.timer().set_timeout(time_ms);
        let _ = self.timer().read();
        self.timer().set_timeout_absolute(prev_timeout);
    }

    /// Wait till a read event is ready
    fn select_impl(&mut self, rw : RW) -> Event {
        let shared = &self.coroutine.shared;
        shared.borrow_mut().state = State::BlockedOn(rw);
        trace!("Coroutine({}): blocked on {:?}", shared.borrow().id.as_usize(), rw);
        coroutine_jump_out(&shared);
        trace!("Coroutine({}): resumed due to event {:?}", shared.borrow().id.as_usize(), shared.borrow().last_event);
        debug_assert!(shared.borrow().state.is_running());
        let e = shared.borrow().last_event;
        e
    }

    /// Wait till an event is ready
    ///
    /// **Warning**: Mioco can't guarantee that the returned `EventSource` will
    /// not block when actually attempting to `read` or `write`. You must
    /// use `try_read` and `try_write` instead.
    ///
    /// The returned value contains event type and the id of the `EventSource`.
    /// See `EventSource::id()`.
    pub fn select(&mut self) -> Event {
        {
            let Coroutine {
                ref io,
                ref shared,
                ..
            } = *self.coroutine;

            let CoroutineShared {
                ref mut blocked_on,
                ..
            } = *shared.borrow_mut();

            select_impl_set_mask_rc_handles(&**io, blocked_on);
        }
        self.select_impl(RW::Both)
    }

    /// Wait till a read event is ready
    ///
    /// See `MiocoHandle::select`.
    pub fn select_read(&mut self) -> Event {
        {
            let Coroutine {
                ref io,
                ref shared,
                ..
            } = *self.coroutine;

            let CoroutineShared {
                ref mut blocked_on,
                ..
            } = *shared.borrow_mut();

            select_impl_set_mask_rc_handles(&**io, blocked_on);
        }
        self.select_impl(RW::Read)
    }

    /// Wait till a read event is ready.
    ///
    /// See `MiocoHandle::select`.
    pub fn select_write(&mut self) -> Event {
        {
            let Coroutine {
                ref io,
                ref shared,
                ..
            } = *self.coroutine;

            let CoroutineShared {
                ref mut blocked_on,
                ..
            } = *shared.borrow_mut();

            select_impl_set_mask_rc_handles(&**io, blocked_on);
        }
        self.select_impl(RW::Write)
    }

    /// Wait till any event is ready on a set of Handles.
    ///
    /// See `EventSource::id()`.
    /// See `MiocoHandle::select()`.
    pub fn select_from(&mut self, ids : &[EventSourceId]) -> Event {
        {
            let Coroutine {
                ref shared,
                ..
            } = *self.coroutine;

            let CoroutineShared {
                ref mut blocked_on,
                ..
            } = *shared.borrow_mut();


            select_impl_set_mask_from_ids(ids, blocked_on);
        }

        self.select_impl(RW::Both)
    }

    /// Wait till write event is ready on a set of Handles.
    ///
    /// See `MiocoHandle::select_from`.
    pub fn select_write_from(&mut self, ids : &[EventSourceId]) -> Event {
        {
            let Coroutine {
                ref shared,
                ..
            } = *self.coroutine;

            let CoroutineShared {
                ref mut blocked_on,
                ..
            } = *shared.borrow_mut();

            select_impl_set_mask_from_ids(ids, blocked_on);
        }

        self.select_impl(RW::Write)
    }

    /// Wait till read event is ready on a set of Handles.
    ///
    /// See `MiocoHandle::select_from`.
    pub fn select_read_from(&mut self, ids : &[EventSourceId]) -> Event {
        {
            let Coroutine {
                ref shared,
                ..
            } = *self.coroutine;

            let CoroutineShared {
                ref mut blocked_on,
                ..
            } = *shared.borrow_mut();

            select_impl_set_mask_from_ids(ids, blocked_on);
        }

        self.select_impl(RW::Read)
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
    /// TODO: dynamically growing slab would be better; or a fast hashmap?
    coroutines : Slab<CoroutineControl>,

    /// Context saved when jumping into coroutine
    context : Context,

    /// Senders to other EventLoops
    senders : Vec<MioSender>,

    /// Shared between threads
    thread_shared : ArcHandlerThreadShared,
}

impl HandlerShared {
    fn new(senders : Vec<MioSender>, thread_shared : ArcHandlerThreadShared) -> Self {
        HandlerShared {
            coroutines: Slab::new(32 * 1024),
            thread_shared: thread_shared,
            context: Context::empty(),
            senders: senders,
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
        self.thread_shared.coroutines_num.fetch_add(1, Ordering::Relaxed);
    }

    fn coroutines_dec(&self) {
        let prev = self.thread_shared.coroutines_num.fetch_sub(1, Ordering::Relaxed);
        debug_assert!(prev > 0);
    }
}

/// Coroutine Scheduler
///
/// Custom implementations of this trait allow users to change the order in
/// which Coroutines are being scheduled.
pub trait Scheduler {
    /// Spawn per-thread Scheduler
    fn spawn_thread(&mut self) -> Box<SchedulerThread + 'static>;
}


/// Per-thread Scheduler
pub trait SchedulerThread : Send {
    /// A Coroutine was spawned.
    ///
    /// New Coroutine was spawned. This can be used to run it immediately
    /// (or not), or migrate it to different thread immediately.
    fn new(&mut self, event_loop: &mut mio::EventLoop<Handler>, coroutine_ctrl: CoroutineControl);

    /// A Coroutine became ready.
    ///
    /// `coroutines_num` is a control reference to the Coroutine that became
    /// ready (to be resumed). It can be resumed immediately, or stored
    /// somewhere to be resumed later.
    fn ready(&mut self, event_loop: &mut mio::EventLoop<Handler>, coroutine_ctrl: CoroutineControl);

    /// A mio's tick have complete.
    ///
    /// This means all pending events have been processed and all Coroutines blocked on them
    /// already signaled with `Scheduler::ready()`.
    ///
    /// After returning from this function, `mioco` will let mio process a
    /// new batch of events.
    fn tick(&mut self, _event_loop: &mut mio::EventLoop<Handler>) {}
}

/// Default, simple first-in-first-out Scheduler.
///
/// Newly spawned coroutines will be spread in round-robbin fashion
/// between threads.
struct FifoScheduler {
    thread_i : Arc<AtomicUsize>,
    thread_num : Arc<AtomicUsize>,
}

impl FifoScheduler {
    pub fn new() -> Self {
        FifoScheduler {
            thread_i : Arc::new(AtomicUsize::new(0)),
            thread_num : Arc::new(AtomicUsize::new(0)),
        }
    }
}

struct FifoSchedulerThread {
    thread_i : Arc<AtomicUsize>,
    thread_num : Arc<AtomicUsize>,
}

impl Scheduler for FifoScheduler {
    fn spawn_thread(&mut self) -> Box<SchedulerThread> {
        self.thread_num.fetch_add(1, Ordering::Relaxed);
        Box::new(FifoSchedulerThread{
            thread_i: self.thread_i.clone(),
            thread_num: self.thread_num.clone(),
        })
    }
}

impl FifoSchedulerThread {
    fn thread_next_i(&self) -> usize {
        self.thread_i.fetch_add(1, Ordering::Relaxed)
    }

    fn thread_num(&self) -> usize {
        self.thread_num.load(Ordering::Relaxed)
    }
}

impl SchedulerThread for FifoSchedulerThread {
    fn new(&mut self, event_loop: &mut mio::EventLoop<Handler>, coroutine_ctrl: CoroutineControl) {
        let thread_i = self.thread_next_i() % self.thread_num();
        trace!("Migrating newly spawn Coroutine to thread {}", thread_i);
        coroutine_ctrl.migrate(event_loop, thread_i);
    }

    fn ready(&mut self, event_loop: &mut mio::EventLoop<Handler>, coroutine_ctrl: CoroutineControl) {
        coroutine_ctrl.resume(event_loop, self);
    }

    fn tick(&mut self, _: &mut mio::EventLoop<Handler>) {}
}

/// Mioco event loop `Handler`
///
/// Registered in `mio::EventLoop` and implementing `mio::Handler`.  This `struct` is quite
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
}

/// EventLoop message type
pub enum Message {
    /// Mailbox notification
    MailboxMsg(Token),
    /// Coroutine migration
    Migration(Rc<RefCell<Coroutine>>),
}

unsafe impl Send for Message { }

impl mio::Handler for Handler {
    type Timeout = Token;
    type Message = Message;

    fn tick(&mut self, event_loop: &mut mio::EventLoop<Self>) {
        let coroutines_num = self.shared.borrow().coroutines_num();
        trace!("Handler::tick(): coroutines_num = {}", coroutines_num);
        if coroutines_num == 0 {
            trace!("Shutting down EventLoop");
            event_loop.shutdown();
        }
    }

    fn ready(&mut self, event_loop: &mut mio::EventLoop<Handler>, token: mio::Token, events: mio::EventSet) {
        trace!("Handler::ready({:?}): started", token);
        let (co_id, _) = token_to_ids(token);
        let co = {
            let shared = self.shared.borrow();
            match shared.coroutines.get(Token(co_id.as_usize())).as_ref() {
                Some(&co) => co.clone_priv(),
                None => {
                    trace!("Handler::ready() ignored");
                    return
                },
            }
        };
        if co.event(event_loop, &mut *self.scheduler, token, events) {
            self.scheduler.ready(event_loop, co);
        }
        trace!("Handler::ready({:?}): finished", token);
    }

    fn notify(&mut self, event_loop: &mut EventLoop<Handler>, msg: Self::Message) {
        match msg {
            MailboxMsg(token) => self.ready(event_loop, token, EventSet::readable()),
            Migration(coroutine) => {
                let mut co = CoroutineControl { rc: coroutine };
                co.reattach_to(self);
                self.scheduler.ready(event_loop, co);
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
    scheduler : Box<Scheduler + 'static>,
    join_handles : Vec<thread::JoinHandle<()>>,
}

impl Mioco {
    /// Create new `Mioco` instance
    pub fn new() -> Self {
        Self::new_with_scheduler(FifoScheduler::new())
    }

    /// Create new `Mioco` instance with custom scheduler
    pub fn new_with_scheduler<S>(scheduler : S) -> Self
        where
        S : Scheduler + 'static,
    {
         Mioco {
            scheduler: Box::new(scheduler),
            join_handles: Vec::new(),
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
        where F : FnOnce(&mut MiocoHandle) -> io::Result<()> + Send + 'static,
              F : Send
        {
            self.start_threads::<F, FifoScheduler>(1, f);
        }

    /// Start mioco handler that will spawn a `thread` number of threads
    pub fn start_threads<F, S>(&mut self, thread_num : usize, f : F)
        where
        S : Scheduler + 'static,
        F : FnOnce(&mut MiocoHandle) -> io::Result<()> + Send + 'static,
        F : Send
        {
            let thread_shared = Arc::new(HandlerThreadShared::new(thread_num));

            let mut event_loops = VecDeque::new();
            let mut senders = Vec::new();
            for _ in 0..thread_num {
                let event_loop = EventLoop::new().expect("new EventLoop");
                senders.push(event_loop.channel());
                event_loops.push_back(event_loop);
            }

            let sched = self.scheduler.spawn_thread();
            self.spawn_thread(0, Some(f), sched, event_loops.pop_front().unwrap(), senders.clone(), thread_shared.clone());
            for i in 1..thread_num {
                let sched = self.scheduler.spawn_thread();
                self.spawn_thread::<F>(i, None, sched, event_loops.pop_front().unwrap(), senders.clone(), thread_shared.clone());
            }

            for join in self.join_handles.drain(..) {
                let _ = join.join(); // TODO: Do something with it
            }
        }

    fn spawn_thread<F>(
        &mut self,
        thread_i : usize,
        f : Option<F>,
        mut scheduler : Box<SchedulerThread+'static>,
        mut event_loop : EventLoop<Handler>,
        senders : Vec<MioSender>,
        thread_shared : ArcHandlerThreadShared,
        )
        where F : FnOnce(&mut MiocoHandle) -> io::Result<()> + Send + 'static,
              F : Send
    {

        let join = thread::Builder::new().name(format!("mioco_thread_{}", thread_i)).spawn(move || {
            let shared = Rc::new(RefCell::new(HandlerShared::new(senders, thread_shared)));
            if let Some(f) = f {
                let coroutine_rc = Coroutine::spawn(shared.clone(), f);
                let coroutine_ctrl = CoroutineControl{ rc: coroutine_rc };
                scheduler.new(&mut event_loop, coroutine_ctrl);
                // Mark started only after first coroutine is spawned so that
                // threads don't start, detect no coroutines, and exit prematurely
                shared.borrow().signal_start_all();
            }
            let mut handler = Handler::new(shared, scheduler);

            handler.shared.borrow().wait_for_start_all();
            event_loop.run(&mut handler).unwrap();
        });

        match join {
            Ok(join) => self.join_handles.push(join),
            Err(err) => panic!("Couldn't spawn thread: {}", err),
        }
    }
}

/// Create a Mailbox
///
/// Mailbox can be used to deliver notifications to handlers from anywhere:
///
/// * other coroutines,
/// * outside of Mioco, even a different thread.
///
pub fn mailbox<T>() -> (MailboxOuterEnd<T>, MailboxInnerEnd<T>) {
    let shared = MailboxShared {
        token: None,
        sender: None,
        inn: VecDeque::new(),
        interest: EventSet::none(),
    };

    let shared = Arc::new(Mutex::new(shared));

    (MailboxOuterEnd::new(shared.clone()), MailboxInnerEnd::new(shared))
}

type MailboxQueue<T> = Option<T>;

struct MailboxShared<T> {
    token : Option<Token>,
    sender : Option<MioSender>,
    inn : VecDeque<T>,
    interest : EventSet,
}

/// Outside Mailbox End
///
/// Use from outside the coroutine handler.
///
/// Create with `mailbox()`
pub struct MailboxOuterEnd<T> {
    shared : ArcMailboxShared<T>,
}

impl<T> Clone for MailboxOuterEnd<T> {
    fn clone(&self) -> Self {
        MailboxOuterEnd {
            shared: self.shared.clone()
        }
    }
}

/// Inner Mailbox End
///
/// Use from within coroutine handler.
///
/// Create with `mailbox()`.
pub struct MailboxInnerEnd<T> {
    shared : ArcMailboxShared<T>,
}

impl<T> MailboxOuterEnd<T> {
    fn new(shared : ArcMailboxShared<T>) -> Self {
        MailboxOuterEnd {
            shared: shared
        }
    }
}

impl<T> MailboxInnerEnd<T> {
    fn new(shared : ArcMailboxShared<T>) -> Self {
        MailboxInnerEnd {
            shared: shared
        }
    }
}

impl<T> MailboxOuterEnd<T> {
    /// Deliver `T` to the other end of the mailbox.
    ///
    /// Mailbox behaves like a queue.
    ///
    /// This is non-blocking operation.
    ///
    /// See `EventSource<MailboxInnerEnd<T>>::read()`.
    pub fn send(&self, t : T) {
        let mut lock = self.shared.lock();
        let MailboxShared {
            ref mut sender,
            ref mut token,
            ref mut inn,
            ref mut interest,
        } = *lock;

        inn.push_back(t);
        debug_assert!(!inn.is_empty());
        trace!("MailboxOuterEnd: putting message in a queue; new len: {}", inn.len());

        if interest.is_readable() {
            let token = token.unwrap();
            trace!("MailboxOuterEnd: notifying {:?}", token);
            let sender = sender.as_ref().unwrap();
            sender_retry(&sender, MailboxMsg(token))
        }
    }
}

impl<T> EventSource<MailboxInnerEnd<T>>
where T : Reflect+'static {
    /// Receive `T` sent using corresponding `MailboxOuterEnd::send()`.
    ///
    /// Will block coroutine if no elements are available.
    pub fn read(&self) -> T {
        loop {
            if let Some(t) = self.try_read() {
                return t
            }

            self.block_on(RW::Read)
        }
    }

    /// Try reading current time (if the timer is done)
    pub fn try_read(&self) -> Option<T> {
        let mut inn = self.inn.borrow_mut();
        let handle = inn.io.as_any_mut().downcast_mut::<MailboxInnerEnd<T>>().unwrap();
        let mut lock = handle.shared.lock();

        lock.inn.pop_front()
    }
}

/// A Timer generating event after a given time
///
/// Can be used to block coroutine or to implement timeout for other `EventSource`.
///
/// Create using `MiocoHandle::timeout()`.
///
/// Use `MiocoHandle::select()` to wait for an event, or `read()` to block until
/// done.
pub struct Timer {
    timeout: SteadyTime,
}

impl Timer {
    fn new() -> Timer {
        Timer { timeout: SteadyTime::now() }
    }

    fn is_done(&self) -> bool {
        self.timeout <= SteadyTime::now()
    }
}

impl EventSource<Timer> {
    /// Read a timer to block on it until it is done.
    ///
    /// Returns current time
    ///
    /// TODO: Return wakeup time instead
    pub fn read(&mut self) -> SteadyTime {
        loop {
            if let Some(t) = self.try_read() {
                return t;
            }

            self.block_on(RW::Read);
        }
    }

    /// Try reading current time (if the timer is done)
    ///
    /// TODO: Return wakeup time instead
    pub fn try_read(&mut self) -> Option<SteadyTime> {
        let done = self.with_raw(|timer| { timer.is_done() });

        if done {
            Some(SteadyTime::now())
        } else {
            None
        }
    }

    /// Set timeout for the timer
    ///
    /// The timeout counts from the time `set_timer` is called.
    pub fn set_timeout(&mut self, delay_ms : i64) {
        self.with_raw_mut(
            |timer|
            timer.timeout = SteadyTime::now() + Duration::milliseconds(delay_ms)
            );
    }

    fn set_timeout_absolute(&mut self, timeout : SteadyTime) {
        self.with_raw_mut(
            |timer| timer.timeout = timeout
            );
    }


    fn get_timeout_absolute(&mut self) -> SteadyTime {
        self.with_raw_mut(
            |timer| timer.timeout
            )
    }
}


/// Shorthand for creating new `Mioco` instance and starting it right away.
pub fn start<F>(f : F)
    where F : FnOnce(&mut MiocoHandle) -> io::Result<()> + Send + 'static,
          F : Send
{
    let mut mioco = Mioco::new();
    mioco.start_threads::<F, FifoScheduler>(num_cpus::get(), f);
}

/// Shorthand for creating new `Mioco` instance and starting it right away.
pub fn start_threads<F>(thread_num : usize, f : F)
    where F : FnOnce(&mut MiocoHandle) -> io::Result<()> + Send + 'static,
          F : Send
{
    info!("Starting mioco instance with {} handler threads", thread_num);
    let mut mioco = Mioco::new();
    mioco.start_threads::<F, FifoScheduler>(thread_num, f);
}


#[cfg(test)]
mod tests;
