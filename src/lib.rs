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
#![feature(result_expect)]
#![feature(reflect_marker)]
#![feature(rc_weak)]
#![feature(rt)]
#![warn(missing_docs)]

extern crate spin;
extern crate mio;
extern crate context;
extern crate nix;
#[macro_use]
extern crate log;
extern crate bit_vec;
extern crate time;

use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::io;
use std::mem::{transmute, size_of_val};

use mio::{TryRead, TryWrite, Token, EventLoop, EventSet};
use std::any::Any;
use std::marker::{PhantomData, Reflect};
use mio::util::Slab;

use std::collections::VecDeque;
use spin::Mutex;
use std::sync::{Arc};

use bit_vec::BitVec;

use time::{SteadyTime, Duration};

use context::{Context, Stack};
use context::thunk::Thunk;
use std::rt::unwind::try;

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
    /// Index of the EventSourceRefShared handle
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
        match self {
            &ExitStatus::Panic => true,
            _ => false,
        }
    }
}

/// State of `mioco` coroutine
#[derive(Clone, Debug)]
enum State {
    BlockedOn(RW),
    Running,
    Finished(ExitStatus),
}

impl State {
    /// Is the `State` a `Finished(_)`?
    fn is_finished(&self) -> bool {
        match self {
            &State::Finished(_) => true,
            _ => false,
        }
    }

    /// Is the `State` `Finished(_)`?
    fn is_running(&self) -> bool {
        match self {
            &State::Running => true,
            _ => false,
        }
    }
}

/// Sends notify `Message` to the mioco Event Loop.
pub type MioSender = mio::Sender<<Handler as mio::Handler>::Message>;

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
        event_loop.register_opt(
            self, token,
            interest,
            mio::PollOpt::edge(),
            ).expect("register_opt failed");
    }

    fn reregister(&self, event_loop : &mut EventLoop<Handler>, token : Token, interest : EventSet) {
        event_loop.reregister(
            self, token,
            interest,
            mio::PollOpt::edge(),
            ).expect("reregister failed");
    }

    fn deregister(&self, event_loop : &mut EventLoop<Handler>, _token : Token) {
        event_loop.deregister(self).expect("deregister failed");
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

    /// Register
    fn register(&self, event_loop : &mut EventLoop<Handler>, token : Token, interest : EventSet) {
        let mut lock = self.shared.lock();

        lock.token = Some(token);
        lock.sender = Some(event_loop.channel());
        lock.interest = interest;

        if interest.is_readable() && !lock.inn.is_empty() {
            lock.interest = EventSet::none();
            lock.sender.as_ref().unwrap().send(token).unwrap()
        }
    }

    /// Reregister
    fn reregister(&self, _event_loop : &mut EventLoop<Handler>, token : Token, interest : EventSet) {
        let mut lock = self.shared.lock();

        lock.interest = interest;

        if interest.is_readable() && !lock.inn.is_empty() {
            lock.interest = EventSet::none();
            lock.sender.as_ref().unwrap().send(token).unwrap()
        }
    }

    /// Deregister
    fn deregister(&self, _event_loop : &mut EventLoop<Handler>, _token : Token) {
        let mut lock = self.shared.lock();
        lock.interest = EventSet::none();
    }

    fn should_resume(&self) -> bool {
        let lock = self.shared.lock();
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
        trace!("register timer: {:?}", token);
        let timeout = self.timeout;
        let now = SteadyTime::now();
        let delay = if timeout <= now {
            0
        } else {
            (timeout - now).num_milliseconds()
        };

        match event_loop.timeout_ms(token, delay as u64) {
            Ok(_) => {},
            Err(reason)=> {
                error!("Could not create mio::Timeout: {:?}", reason);
            }
        }
    }

    fn reregister(&self, event_loop : &mut EventLoop<Handler>, token : Token, interest : EventSet) {
        self.register(event_loop, token, interest)
    }

    fn deregister(&self, _event_loop : &mut EventLoop<Handler>, token : Token) {
        trace!("deregister timer: {:?}", token);
    }

    fn should_resume(&self) -> bool {
        self.timeout <= SteadyTime::now()
    }
}

type RefCoroutine = Rc<RefCell<Coroutine>>;

/// *`mioco` coroutine* (a.k.a. *mioco handler*)
///
/// Mioco
///
/// Referenced by EventSourceRefShared running within it.
struct Coroutine {
    /// Context with a state of coroutine
    context: Context,

    /// Coroutine stack
    stack: Stack,

    /// Current state
    state : State,

    /// Last event that resumed the coroutine
    last_event: Event,

    /// Last register tick
    last_tick : u32,

    /// All handles, weak to avoid `Rc`-cycle
    io : Vec<Weak<RefCell<EventSourceRefShared>>>,

    /// Mask of handle ids that we're blocked on
    blocked_on : BitVec<usize>,

    /// Mask of handle ids that are registered in Handler
    registered : BitVec<usize>,

    /// `Handler` shared data that this `Coroutine` is running in
    server_shared : RefHandlerShared,

    /// Newly spawned `Coroutine`-es
    children_to_start : Vec<RefCoroutine>,

    /// `Coroutine` will send exit status on it's finish
    exit_notificators : Vec<MailboxOuterEnd<ExitStatus>>,
}

impl Coroutine {
    fn spawn<F>(server : RefHandlerShared, f : F) -> RefCoroutine
    where F : FnOnce(&mut MiocoHandle) -> io::Result<()> + 'static {

        trace!("Coroutine: spawning");
        server.borrow_mut().coroutines_no += 1;

        let coroutine = Coroutine {
            state: State::Running,
            last_event: Event{ rw: RW::Read, id: EventSourceId(0)},
            io: Vec::with_capacity(4),
            blocked_on: Default::default(),
            registered: Default::default(),
            server_shared: server,
            children_to_start: Vec::new(),
            last_tick: !0,
            exit_notificators: Vec::new(),
            context: Context::empty(),
            stack: Stack::new(1024 * 1024),
        };

        let coroutine_ref = Rc::new(RefCell::new(coroutine));

        struct SendFnOnce<F>
        {
            f : F
        }

        // We fake the `Send` because `mioco` guarantees serialized
        // execution between coroutines, switching between them
        // only in predefined points.
        unsafe impl<F> Send for SendFnOnce<F>
            where F : FnOnce(&mut MiocoHandle) -> io::Result<()> + 'static
            {

            }

        struct SendRefCoroutine {
            coroutine: RefCoroutine,
        }

        // Same logic as in `SendFnOnce` applies here.
        unsafe impl Send for SendRefCoroutine { }

        let sendref = SendRefCoroutine {
            coroutine: coroutine_ref.clone(),
        };

        let send_f = SendFnOnce {
            f: f,
        };

        extern "C" fn init_fn(arg: usize, f: *mut ()) -> ! {
            let func: Box<Thunk<(), _>> = unsafe { transmute(f) };
            if let Err(cause) = unsafe { try(move|| func.invoke(())) } {
                error!("Panicked inside: {:?}", cause.downcast::<&str>());
            }

            let ctx: &Context = unsafe { transmute(arg) };

            let mut dummy = Context::empty();
            Context::swap(&mut dummy, ctx);

            unreachable!();
        }

        {
            let Coroutine {
                ref mut stack,
                ref mut context,
                ref server_shared,
                ..
            } = *coroutine_ref.borrow_mut();

            context.init_with(
                init_fn,
                unsafe { transmute(&server_shared.borrow_mut().context as *const Context) },
                move || {
                    trace!("Coroutine: started");
                    let mut mioco_handle = MiocoHandle {
                        coroutine: sendref.coroutine,
                        timer: None,
                    };

                    // Guard to perform cleanup in case of panic
                    let mut guard = CoroutineGuard::new(mioco_handle.coroutine.clone());

                    let SendFnOnce { f } = send_f;

                    let res = f(&mut mioco_handle);

                    guard.finish(res);
                    trace!("Coroutine: finished");
                },
                stack,
                );
        }

        coroutine_ref
    }

    /// After `resume()` on the `Coroutine.handle` finished,
    /// the `Coroutine` have blocked or finished and we need to
    /// perform the following maintenance
    fn after_resume(&mut self, event_loop: &mut EventLoop<Handler>) {
        // If there were any newly spawned child-coroutines: start them now
        for coroutine in &self.children_to_start {
            trace!("Resume new child coroutine");
            resume(coroutine);
            if !coroutine.borrow().state.is_finished() {
                coroutine.borrow_mut().reregister(event_loop);
            }
        }
        self.children_to_start.clear();

        trace!("Reregister coroutine");
        self.reregister(event_loop);
    }

    fn reregister(&mut self, event_loop: &mut EventLoop<Handler>) {
        if self.state.is_finished() {
            trace!("Coroutine: deregistering");
            self.deregister_all(event_loop);
        } else {
            self.reregister_blocked_on(event_loop)
        }
    }

    fn deregister_all(&mut self, event_loop: &mut EventLoop<Handler>) {
        let mut shared = self.server_shared.borrow_mut();

        for i in 0..self.io.len() {
            let io = self.io[i].upgrade().unwrap();
            let mut io = io.borrow_mut();
            io.deregister(event_loop);
            trace!("Removing source token={:?}", io.token);
            shared.sources.remove(io.token).expect("cleared empty slot");
        }
    }

    fn reregister_blocked_on(&mut self, event_loop: &mut EventLoop<Handler>) {

        let rw = match self.state {
            State::BlockedOn(rw) => rw,
            _ => panic!("This should not happen"),
        };

        let Coroutine {
            ref mut registered,
            ref mut blocked_on,
            ref mut io,
            ..
        } = *self;

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
                    let io = io[i + bit].upgrade().unwrap();
                    if registered_block & (1 << bit) != 0 {
                        debug_assert!(blocked_on_block & (1 << bit) == 0);
                        let io = io.borrow();
                        io.unreregister(event_loop);
                    } else {
                        debug_assert!(blocked_on_block & (1 << bit) != 0);
                        let mut io = io.borrow_mut();
                        io.reregister(event_loop, rw);
                    }
                }
            }
            i += bit_size;
        }

        // effectively: self.registered = self.blocked_on;
        for (mut target, src) in unsafe { registered.storage_mut().iter_mut().zip(blocked_on.storage().iter()) } {
            *target = *src
        }
    }
}

/// Resume coroutine execution
fn resume(coroutine_ref: &RefCoroutine) {
    if coroutine_ref.borrow().state.is_finished() {
        return
    }

    // We know that we're holding at least one Rc to the Coroutine,
    // and noone else is holding a reference as we can do `.borrow_mut()`
    // so we cheat with unsafe just to context-switch from coroutine
    // without having RefCells still borrowed.
    let (context_in, context_out) = {
        let Coroutine {
            ref context,
            ref server_shared,
            ..
        } = *coroutine_ref.borrow_mut();
        {
            let mut shared_context = &mut server_shared.borrow_mut().context;
            (context as *const Context, shared_context as *mut Context)
        }
    };

    Context::swap(unsafe {&mut *context_out}, unsafe {&*context_in});
}

/// Block coroutine execution
fn block(coroutine_ref: &RefCoroutine) {
    debug_assert!(!coroutine_ref.borrow().state.is_finished());

    // See `resume()` for unsafe comment
    let (context_in, context_out) = {
        let Coroutine {
            ref mut context,
            ref server_shared,
            ..
        } = *coroutine_ref.borrow_mut();
        {
            let shared_context = &mut server_shared.borrow_mut().context;
            (context as *mut Context, shared_context as *const Context)
        }
    };

    Context::swap(unsafe {&mut *context_in}, unsafe {&*context_out});
}

/// Coroutine guard used for cleanup and exit notification
struct CoroutineGuard {
    finished : bool,
    coroutine_ref : RefCoroutine,
}

impl CoroutineGuard {
    fn new(coroutine : RefCoroutine) -> CoroutineGuard {
        CoroutineGuard {
            finished: false,
            coroutine_ref: coroutine,
        }
    }

    fn finish(&mut self, res : io::Result<()>) {
        self.finished = true;

        let arc_res = Arc::new(res);
        let mut co = self.coroutine_ref.borrow_mut();
        co.exit_notificators.iter().map(|end| end.send(ExitStatus::Exit(arc_res.clone()))).count();
        co.state = State::Finished(ExitStatus::Exit(arc_res));
    }
}

impl Drop for CoroutineGuard {
    fn drop(&mut self) {
        let mut co = self.coroutine_ref.borrow_mut();

        co.server_shared.borrow_mut().coroutines_no -= 1;
        // TODO: https://github.com/contain-rs/bit-vec/pulls
        co.blocked_on.clear();

        if !self.finished {
            co.state = State::Finished(ExitStatus::Panic);
            co.exit_notificators.iter().map(|end| end.send(ExitStatus::Panic)).count();
        }
    }
}


type RefEventSourceRefShared = Rc<RefCell<EventSourceRefShared>>;

/// Wrapped mio IO (mio::Evented+TryRead+TryWrite)
///
/// `Handle` is just a cloneable reference to this struct
struct EventSourceRefShared {
    coroutine: RefCoroutine,
    token: Token,
    id : usize, /// Index in MiocoHandle::handles
    io : Box<Evented+'static>,
    peer_hup: bool,
    registered: bool,
}

impl EventSourceRefShared {
    /// Handle `hup` condition
    fn hup(&mut self, _event_loop: &mut EventLoop<Handler>, _token: Token) {
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

            if !self.registered {
                self.registered = true;
                Evented::register(&*self.io, event_loop, self.token, interest);
            } else {
                Evented::reregister(&*self.io, event_loop, self.token, interest);
             }
        }

    /// Un-reregister events we're not interested in anymore
    fn unreregister(&self, event_loop: &mut EventLoop<Handler>) {
            debug_assert!(self.registered);
            let interest = mio::EventSet::none();
            Evented::reregister(&*self.io, event_loop, self.token, interest);
        }

    /// Un-reregister events we're not interested in anymore
    fn deregister(&mut self, event_loop: &mut EventLoop<Handler>) {
            if self.registered {
                Evented::deregister(&*self.io, event_loop, self.token);
                self.registered = false;
            }
        }
}

/// `mioco` wrapper over raw structure implementing `mio::Evented` trait
#[derive(Clone)]
struct EventSourceRef {
    inn : RefEventSourceRefShared,
}

/// Event source inside a coroutine
///
/// Event sources are a core of Mioco. Mioco coroutines use them to handle
/// IO in a blocking fashion.
///
/// They come in different flavours and can be created from native `mio` types by wrapping withing
/// a coroutine with `MiocoHandle::wrap()` or type-specific constructors like `mailbox()` or
/// `MiocoHandle::timeout()`.
#[derive(Clone)]
pub struct EventSource<T> {
    inn : RefEventSourceRefShared,
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

impl<T> EventSource<T>
where T : Reflect+'static {
    /// Mark the `EventSourceRef` blocked and block until `Handler` does
    /// not wake us up again.
    fn block_on(&self, rw : RW) {
        {
            let inn = self.inn.borrow();
            inn.coroutine.borrow_mut().state = State::BlockedOn(rw);
            // TODO: https://github.com/contain-rs/bit-vec/pulls
            inn.coroutine.borrow_mut().blocked_on.clear();
            inn.coroutine.borrow_mut().blocked_on.set(inn.id, true);
        };
        trace!("coroutine blocked on {:?}", rw);
        let coroutine_ref = self.inn.borrow().coroutine.clone();
        block(&coroutine_ref);
        {
            let inn = self.inn.borrow_mut();
            debug_assert!(rw.has_read() || inn.coroutine.borrow().last_event.has_write());
            debug_assert!(rw.has_write() || inn.coroutine.borrow().last_event.has_read());
            debug_assert!(inn.coroutine.borrow().last_event.id().as_usize() == inn.id);
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
        EventSourceId(self.inn.borrow().id)
    }
}

impl EventSourceRef {
    /// Readable event handler
    ///
    /// This corresponds to `mio::Handler::readable()`.
    pub fn ready(&mut self,
                 event_loop : &mut EventLoop<Handler>,
                 token : Token,
                 events : EventSet,
                 tick : u32) {
        if events.is_hup() {
            let mut inn = self.inn.borrow_mut();
            inn.hup(event_loop, token);
        }

        let my_id = {
            let inn = self.inn.borrow();
            let id = inn.id;
            let mut co = inn.coroutine.borrow_mut();
            let prev_last_tick = co.last_tick;
            co.last_tick = tick;

            co.registered.set(id, false);

            if prev_last_tick == tick {
                None
            } else if !co.blocked_on.get(id).unwrap() {
                // spurious event, probably after select in which
                // more than one event sources were reported ready
                // in one group of events, and first event source
                // deregistered the later ones
                debug!("spurious event for event source couroutine is not blocked on");
                None
            } else if let State::BlockedOn(rw) = co.state {
                match rw {
                    RW::Read if !events.is_readable() && !events.is_hup() => {
                        debug!("spurious not read event for coroutine blocked on read");
                        None
                    },
                    RW::Write if !events.is_writable() => {
                        debug!("spurious not write event for coroutine blocked on write");
                        None
                    },
                    RW::Both if !events.is_readable() && !events.is_hup() && !events.is_writable() => {
                        debug!("spurious unknown type event for coroutine blocked on read/write");
                        None
                    },
                    _ => {
                        if inn.io.should_resume() {
                            Some(id)
                        } else {
                            None
                        }
                    }
                }
            } else {
                debug_assert!(co.state.is_finished());
                None
            }
        };

        if let Some(my_id) = my_id {
            // Wake coroutine on HUP, as it was read, to potentially let it fail the read and move on
            let event = match (events.is_readable() | events.is_hup(), events.is_writable()) {
                (true, true) => RW::Both,
                (true, false) => RW::Read,
                (false, true) => RW::Write,
                (false, false) => panic!(),
            };
            {
                let inn = self.inn.borrow();
                inn.coroutine.borrow_mut().state = State::Running;
                inn.coroutine.borrow_mut().last_event = Event {
                    rw: event,
                    id: EventSourceId(my_id),
                };
            };

            let coroutine_ref = self.inn.borrow().coroutine.clone();
            resume(&coroutine_ref);
        }

        let coroutine = {
            let inn = &self.inn.borrow();
            inn.coroutine.clone()
        };

        let mut co = coroutine.borrow_mut();

        co.after_resume(event_loop);

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
pub struct MiocoHandle {
    coroutine : Rc<RefCell<Coroutine>>,
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

fn select_impl_set_mask_rc_handles(handles : &[Weak<RefCell<EventSourceRefShared>>], blocked_on: &mut BitVec<usize>) {
    {
        // TODO: https://github.com/contain-rs/bit-vec/pulls
        blocked_on.clear();
        for handle in handles {
            let io = handle.upgrade().unwrap();
            blocked_on.set(io.borrow().id, true);
        }
    }
}


/// Handle to spawned coroutine
pub struct CoroutineHandle {
    coroutine_ref: RefCoroutine,
}

impl CoroutineHandle {
    /// Create an exit notificator
    pub fn exit_notificator(&self) -> MailboxInnerEnd<ExitStatus> {
        let (outer, inner) = mailbox();
        let mut co = self.coroutine_ref.borrow_mut();
        let Coroutine {
            ref state,
            ref mut exit_notificators,
            ..
        } = *co;

        if let &State::Finished(ref exit) = state {
            outer.send(exit.clone());
        } else {
            exit_notificators.push(outer);
        }
        inner
    }
}

impl MiocoHandle {

    /// Create a `mioco` coroutine handler
    ///
    /// `f` is routine handling connection. It must not use any real blocking-IO operations, only
    /// `mioco` provided types (`EventSource`) and `MiocoHandle` functions. Otherwise `mioco`
    /// cooperative scheduling can block on real blocking-IO which defeats using mioco.
    pub fn spawn<F>(&self, f : F) -> CoroutineHandle
        where F : FnOnce(&mut MiocoHandle) -> io::Result<()> + 'static {
            let coroutine_ref = Coroutine::spawn(self.coroutine.borrow().server_shared.clone(), f);
            let ret = CoroutineHandle {
                coroutine_ref: coroutine_ref.clone(),
            };
            self.coroutine.borrow_mut().children_to_start.push(coroutine_ref);

            ret
        }


    /// Register `mio`'s native io type to be used within `mioco` coroutine
    ///
    /// Consumes the `io`, returns a mioco wrapper over it. Use this wrapped IO
    /// to perform IO.
    pub fn wrap<T : 'static>(&mut self, io : T) -> EventSource<T>
    where T : Evented {
        let token = {
            let co = self.coroutine.borrow();
            let mut shared = co.server_shared.borrow_mut();
            shared.sources.insert_with(|token| {
                EventSourceRef {
                    inn: Rc::new(RefCell::new(
                                 EventSourceRefShared {
                                     coroutine: self.coroutine.clone(),
                                     io: Box::new(io),
                                     token: token,
                                     peer_hup: false,
                                     id: self.coroutine.borrow().io.len(),
                                     registered: false,
                                 }
                                 )),
                }
            })
        }.expect("run out of tokens");
        trace!("Added source token={:?}", token);

        let io_new = {
            let co = self.coroutine.borrow();
            let shared = co.server_shared.borrow_mut();
            shared.sources[token].inn.clone()
        };

        let handle = EventSource {
            inn: io_new.clone(),
            _t: PhantomData,
        };

        let Coroutine {
            ref mut registered,
            ref mut blocked_on,
            ref mut io,
            ..
        } = *self.coroutine.borrow_mut();


        io.push(Rc::downgrade(&io_new.clone()));
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
        self.coroutine.borrow_mut().state = State::BlockedOn(rw);
        trace!("coroutine blocked on {:?}", rw);
        block(&self.coroutine);
        debug_assert!(self.coroutine.borrow().state.is_running());

        self.coroutine.borrow().last_event
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
                ref mut blocked_on,
                ..
            } = *self.coroutine.borrow_mut();

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
                ref mut blocked_on,
                ..
            } = *self.coroutine.borrow_mut();

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
                ref mut blocked_on,
                ..
            } = *self.coroutine.borrow_mut();

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
                ref mut blocked_on,
                ..
            } = *self.coroutine.borrow_mut();

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
                ref mut blocked_on,
                ..
            } = *self.coroutine.borrow_mut();

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
                ref mut blocked_on,
                ..
            } = *self.coroutine.borrow_mut();

            select_impl_set_mask_from_ids(ids, blocked_on);
        }

        self.select_impl(RW::Read)
    }
}

type RefHandlerShared = Rc<RefCell<HandlerShared>>;
/// Data belonging to `Handler`, but referenced and manipulated by `Coroutine`-es
/// belonging to it.
struct HandlerShared {
    /// Slab allocator
    /// TODO: dynamically growing slab would be better; or a fast hashmap?
    /// FIXME: See https://github.com/carllerche/mio/issues/219 . Using an allocator
    /// in which just-deleted entries are not potentially reused right away might prevent
    /// potentical sporious wakeups on newly allocated entries.
    sources : Slab<EventSourceRef>,

    /// Number of `Coroutine`-s running in the `Handler`.
    coroutines_no : u32,

    /// Context saved when jumping into coroutine
    context: Context,
}

impl HandlerShared {
    fn new() -> Self {
        HandlerShared {
            sources: Slab::new(32 * 1024),
            coroutines_no: 0,
            context: Context::empty(),
        }
    }
}

/// Mioco event loop `Handler`
///
/// Registered in `mio::EventLoop` and implementing `mio::Handler`.  This `struct` is quite
/// internal so you should not have to worry about it.
pub struct Handler {
    shared : RefHandlerShared,
    tick : u32,
}

impl Handler {
    fn new(shared : RefHandlerShared) -> Self {
        Handler {
            shared: shared,
            tick : 0,
        }
    }
}

impl mio::Handler for Handler {
    type Timeout = Token;
    type Message = Token;

    fn tick(&mut self, event_loop: &mut mio::EventLoop<Self>) {
        self.tick += 1;

        if self.shared.borrow().coroutines_no == 0 {
            event_loop.shutdown();
        }
    }

    fn ready(&mut self, event_loop: &mut mio::EventLoop<Handler>, token: mio::Token, events: mio::EventSet) {
        // It's possible we got an event for a Source that was deregistered
        // by finished coroutine. In case the token is already occupied by
        // different source, we will wake it up needlessly. If it's empty, we just
        // ignore the event.
        trace!("Handler::ready(token={:?})", token);
        let mut source = match self.shared.borrow().sources.get(token) {
            Some(source) => source.clone(),
            None => {
                trace!("Handler::ready() ignored");
                return
            },
        };
        source.ready(event_loop, token, events, self.tick);
        trace!("Handler::ready finished");
    }

    fn notify(&mut self, event_loop: &mut EventLoop<Handler>, msg: Self::Message) {
        self.ready(event_loop, msg, EventSet::readable());
    }

    fn timeout(&mut self, event_loop: &mut EventLoop<Self>, msg: Self::Timeout) {
        self.ready(event_loop, msg, EventSet::readable());
    }
}

/// Mioco instance
///
/// Main mioco structure.
pub struct Mioco {
    event_loop : EventLoop<Handler>,
    server : Handler,
}

impl Mioco {
    /// Create new `Mioco` instance
    pub fn new() -> Self {
        let shared = Rc::new(RefCell::new(HandlerShared::new()));
        Mioco {
            event_loop: EventLoop::new().expect("new EventLoop"),
            server: Handler::new(shared.clone()),
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
        where F : FnOnce(&mut MiocoHandle) -> io::Result<()> + 'static,
        {
            let Mioco {
                ref mut server,
                ref mut event_loop,
            } = *self;

            let shared = server.shared.clone();

            let coroutine_ref = Coroutine::spawn(shared, f);

            trace!("Initial resume");
            resume(&coroutine_ref);
            coroutine_ref.borrow_mut().after_resume(event_loop);

            let coroutines_no = server.shared.borrow().coroutines_no;
            if  coroutines_no > 0 {
                trace!("Start event loop");
                event_loop.run(server).unwrap();
            } else {
                trace!("No coroutines to start event loop with");
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

type RefMailboxShared<T> = Arc<Mutex<MailboxShared<T>>>;
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
    shared : RefMailboxShared<T>,
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
    shared : RefMailboxShared<T>,
}

impl<T> MailboxOuterEnd<T> {
    fn new(shared : RefMailboxShared<T>) -> Self {
        MailboxOuterEnd {
            shared: shared
        }
    }
}

impl<T> MailboxInnerEnd<T> {
    fn new(shared : RefMailboxShared<T>) -> Self {
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
    /// See `EventSource<MailboxInnerEnd<T>>::recv()`.
    pub fn send(&self, t : T) {
        let mut lock = self.shared.lock();
        let MailboxShared {
            ref mut sender,
            ref mut token,
            ref mut inn,
            ref mut interest,
        } = *lock;

        inn.push_back(t);

        if interest.is_readable() {
            if let &mut Some(token) = token {
                let sender = sender.as_ref().unwrap();
                sender.send(token).unwrap()
            }
        }
    }
}

impl<T> EventSource<MailboxInnerEnd<T>>
where T : Reflect+'static {
    /// Receive `T` sent using corresponding `MailboxOuterEnd::send()`.
    ///
    /// Will block coroutine if no elements are available.
    pub fn read(&mut self) -> T {
        loop {
            if let Some(t) = self.try_read() {
                return t
            }

            self.block_on(RW::Read)
        }
    }

    /// Try reading current time (if the timer is done)
    pub fn try_read(&mut self) -> Option<T> {
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
    where F : FnOnce(&mut MiocoHandle) -> io::Result<()> + 'static,
{
    let mut mioco = Mioco::new();
    mioco.start(f);
}

#[cfg(test)]
mod tests;
