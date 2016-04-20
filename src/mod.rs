use slab;
use num_cpus;
use thread_scoped;

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;
use std::marker::Reflect;
use std::mem;
use std;

use mio_orig::{self, Token, EventLoop, EventLoopConfig};
use mio_orig::Handler as MioHandler;

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use self::sync::mpsc;

use std::ptr;

use self::timer::Timer;

macro_rules! thread_trace_fmt_prefix {
    () => ("T{}: ")
}

macro_rules! co_trace_fmt_prefix {
    () => ("T{}: C{}: ")
}

macro_rules! thread_debug {
    (target: $target:expr, $thread:expr, $fmt:tt, $($arg:tt)*) => (
        debug!(target: $target,
               concat!(thread_trace_fmt_prefix!(), $fmt),
               $thread.thread_id(),
               $($arg)*
              );
        );
    ($thread:expr, $fmt:tt, $($arg:tt)*) => (
        debug!(
               concat!(thread_trace_fmt_prefix!(), $fmt),
               $thread.thread_id(),
               $($arg)*
              );
    );
    ($co:expr, $fmt:tt) => (
        thread_debug!($co, $fmt,)
    );
}

macro_rules! co_debug {
    (target: $target:expr, $co:expr, $fmt:tt, $($arg:tt)*) => (
        debug!(target: $target,
               concat!(co_trace_fmt_prefix!(), $fmt),
               $co.handler_shared().thread_id(),
               $co.id.as_usize(),
               $($arg)*
              );
        );
    ($co:expr, $fmt:tt, $($arg:tt)*) => (
        debug!(
               concat!(co_trace_fmt_prefix!(), $fmt),
               $co.handler_shared().thread_id(),
               $co.id.as_usize(),
               $($arg)*
              );
    );
    ($co:expr, $fmt:tt) => (
        co_debug!($co, $fmt,)
    );
}


/// Useful synchronization primitives
pub mod sync;
/// Timers
pub mod timer;
/// Unix sockets IO
#[cfg(not(windows))]
pub mod unix;
/// TCP
pub mod tcp;
/// UDP
pub mod udp;

pub use self::evented::{Evented, MioAdapter};
mod evented;

use self::coroutine::{Coroutine, RcCoroutine};
mod coroutine;

pub use self::thread::Handler;
use self::thread::Message;
use self::thread::{tl_current_coroutine, tl_current_coroutine_ptr};
mod thread;


/// Read/Write/Both/None
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
// TODO: Make private again
pub struct RW {
    read: bool,
    write: bool,
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
    id: EventSourceId,
    rw: RW,
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

/// Retry `mio_orig::Sender::send()`.
///
/// As channels can fail randomly (eg. when Full), take care
/// of retrying on recoverable errors.
fn sender_retry<M: Send>(sender: &mio_orig::Sender<M>, msg: M) {
    let mut msg = Some(msg);
    let mut warning_printed = false;
    let mut counter = 0;
    loop {
        match sender.send(msg.take().expect("sender_retry")) {
            Ok(()) => break,
            Err(mio_orig::NotifyError::Closed(_)) => panic!("Closed channel on sender.send()."),
            Err(mio_orig::NotifyError::Io(_)) => panic!("IO error on sender.send()."),
            Err(mio_orig::NotifyError::Full(retry_msg)) => {
                counter += 1;
                msg = Some(retry_msg);
            }
        }

        if counter > 20000 {
            panic!("Mio Queue Full, process hangs. consider increasing \
                    `EventLoopConfig::notify_capacity");
        }

        if !warning_printed {
            warning_printed = true;
            warn!("send_retry: retry; consider increasing `EventLoopConfig::notify_capacity`");
        }
        std::thread::yield_now();
    }
}

/// Mioco Handler keeps only Slab of Coroutines, and uses a scheme in which
/// Token bits encode both Coroutine and EventSource within it
const EVENT_SOURCE_TOKEN_SHIFT: usize = 10;
const EVENT_SOURCE_TOKEN_MASK: usize = (1 << EVENT_SOURCE_TOKEN_SHIFT) - 1;

/// Convert token to ids.
fn token_to_ids(token: Token) -> (coroutine::Id, EventSourceId) {
    let val = token.as_usize();
    (coroutine::Id::new(val >> EVENT_SOURCE_TOKEN_SHIFT),
     EventSourceId(val & EVENT_SOURCE_TOKEN_MASK))
}

/// Convert ids to Token.
fn token_from_ids(co_id: coroutine::Id, io_id: EventSourceId) -> Token {
    // TODO: Add checks on wrap()
    debug_assert!(io_id.as_usize() <= EVENT_SOURCE_TOKEN_MASK);
    Token((co_id.as_usize() << EVENT_SOURCE_TOKEN_SHIFT) | io_id.as_usize())
}

/// Id of an event source used to enumerate them.
///
/// This is a temporary ID, used to distinguish event source
/// returned by `select!` operation.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct EventSourceId(usize);

impl EventSourceId {
    fn new(id: usize) -> Self {
        EventSourceId(id)
    }

    fn as_usize(&self) -> usize {
        self.0
    }
}

impl slab::Index for EventSourceId {
    fn as_usize(&self) -> usize {
        self.0
    }
    fn from_usize(i: usize) -> Self {
        EventSourceId(i)
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


/// Scheduler thread
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
    fn spawned(&mut self,
               event_loop: &mut mio_orig::EventLoop<thread::Handler>,
               coroutine_ctrl: CoroutineControl);

    /// A Coroutine became ready.
    ///
    /// `coroutine_ctrl` is a control reference to the Coroutine that became
    /// ready (to be resumed). It can be resumed immediately, or stored
    /// somewhere to be resumed later.
    ///
    /// Dropping `coroutine_ctrl` means the corresponding coroutine will be
    /// killed.
    fn ready(&mut self,
             event_loop: &mut mio_orig::EventLoop<thread::Handler>,
             coroutine_ctrl: CoroutineControl);

    /// Mio's tick have completed.
    ///
    /// Mio signals events in batches, after which a `tick` is signaled.
    ///
    /// All events events have been processed and all unblocked coroutines
    /// signaled with `SchedulerThread::ready()`.
    ///
    /// After returning from this function, `mioco` will let mio process a
    /// new batch of events.
    fn tick(&mut self, _event_loop: &mut mio_orig::EventLoop<thread::Handler>) {}

    /// Set the maximum time till the next tick.
    ///
    /// `timeout` will be called after each `tick` when all `ready` and
    /// `spawned` coroutines have been delivered to the scheduler. It can
    /// be used to force maximum time before next `tick` in case of no earlier
    /// events.
    ///
    /// Returning `None` means no timeout. `Some(time)` is a time in ms.
    fn timeout(&mut self) -> Option<u64> {
        None
    }
}

/// Default, simple first-in-first-out Scheduler.
///
/// Newly spawned coroutines will be spread in round-robbin fashion
/// between threads.
struct FifoScheduler {
    thread_num: Arc<AtomicUsize>,
}

impl FifoScheduler {
    pub fn new() -> Self {
        FifoScheduler { thread_num: Arc::new(AtomicUsize::new(0)) }
    }
}

struct FifoSchedulerThread {
    thread_id: usize,
    thread_i: usize,
    thread_num: Arc<AtomicUsize>,
    delayed: VecDeque<CoroutineControl>,
}

impl Scheduler for FifoScheduler {
    fn spawn_thread(&self) -> Box<SchedulerThread> {
        let prev = self.thread_num.fetch_add(1, Ordering::Relaxed);
        Box::new(FifoSchedulerThread {
            thread_id: prev,
            thread_i: prev,
            thread_num: self.thread_num.clone(),
            delayed: VecDeque::new(),
        })
    }
}

impl FifoSchedulerThread {
    fn thread_next_i(&mut self) -> usize {
        let ret = self.thread_i;
        self.thread_i += 1;
        if self.thread_i >= self.thread_num() {
            self.thread_i = 0;
        }
        ret
    }

    fn thread_num(&self) -> usize {
        self.thread_num.load(Ordering::Relaxed)
    }
}

impl SchedulerThread for FifoSchedulerThread {
    fn spawned(&mut self,
               event_loop: &mut mio_orig::EventLoop<thread::Handler>,
               coroutine_ctrl: CoroutineControl) {
        let thread_i = self.thread_next_i();
        if thread_i == self.thread_id {
            coroutine_ctrl.resume(event_loop);
        } else {
            coroutine_ctrl.migrate(event_loop, thread_i);
        }
    }

    fn ready(&mut self,
             event_loop: &mut mio_orig::EventLoop<thread::Handler>,
             coroutine_ctrl: CoroutineControl) {
        if coroutine_ctrl.is_yielding() {
            self.delayed.push_back(coroutine_ctrl);
        } else {
            coroutine_ctrl.resume(event_loop);
        }
    }

    fn tick(&mut self, event_loop: &mut mio_orig::EventLoop<thread::Handler>) {
        let len = self.delayed.len();
        for _ in 0..len {
            let coroutine_ctrl = self.delayed.pop_front().unwrap();
            coroutine_ctrl.resume(event_loop);
        }
    }

    fn timeout(&mut self) -> Option<u64> {
        if self.delayed.is_empty() {
            None
        } else {
            Some(1000)
        }
    }
}

/// Coroutine scheduling handle.
///
/// Through this interface schedulers can operate on coroutines.
pub struct CoroutineControl {
    /// In case `CoroutineControl` gets dropped in `SchedulerThread` Drop
    /// trait will kill the Coroutine
    was_handled: bool,
    is_yielding: bool,
    rc: RcCoroutine,
}

impl Drop for CoroutineControl {
    fn drop(&mut self) {
        if !self.was_handled {
            self.kill();
        }
    }
}

impl CoroutineControl {
    fn new(rc: RcCoroutine) -> Self {
        CoroutineControl {
            is_yielding: false,
            was_handled: false,
            rc: rc,
        }
    }

    /// Resume Coroutine.
    pub fn resume(mut self, event_loop: &mut EventLoop<thread::Handler>) {
        self.was_handled = true;
        let co_rc = self.rc.clone();
        debug_assert!(co_rc.borrow().state().is_ready());

        coroutine::jump_in(&co_rc);
        self.after_resume(event_loop);
    }

    /// After `resume()` (or ignored event()) we need to perform the following maintenance.
    fn after_resume(&self, event_loop: &mut EventLoop<thread::Handler>) {

        self.rc.borrow_mut().register_all(event_loop);
        self.rc.borrow_mut().start_children();

        let state = self.rc.borrow().state().clone();
        if state.is_yielding() {
            debug_assert!(self.rc.borrow().blocked_on.is_empty());
            let mut coroutine_ctrl = CoroutineControl::new(self.rc.clone());
            coroutine_ctrl.set_is_yielding();
            self.rc.borrow_mut().unblock_after_yield();
            let rc_coroutine = self.rc.borrow();
            let mut handler_shared = rc_coroutine.handler_shared_mut();

            handler_shared.add_ready(coroutine_ctrl);
        }
    }

    /// Migrate to a different thread.
    ///
    /// Move this Coroutine to be executed on a `SchedulerThread` for a
    /// given `thread_id`.
    ///
    /// Will panic if `thread_id` is not valid.
    pub fn migrate(mut self, event_loop: &mut EventLoop<thread::Handler>, thread_id: usize) {
        self.was_handled = true;
        let sender = {
            let mut co = self.rc.borrow_mut();

            let handler_shared = co.detach_from(event_loop, thread_id);
            let mut handler_shared = handler_shared.borrow_mut();
            handler_shared.coroutines.remove(co.id).unwrap();
            handler_shared.get_sender_to_thread(thread_id)
        };

        let rc = self.rc.clone();

        drop(self);

        // TODO: Spin on failure
        sender_retry(&sender, Message::Migration(CoroutineControl::new(rc)));
    }

    /// Finish migrating Coroutine by attaching it to a new thread.
    pub fn reattach_to(&mut self,
                       event_loop: &mut EventLoop<thread::Handler>,
                       handler: &mut thread::Handler) {
        let handler_shared = handler.shared().clone();

        let id = handler_shared.borrow_mut().attach(self.rc.clone());
        self.rc.borrow_mut().attach_to(event_loop, handler_shared, id);
    }

    fn set_is_yielding(&mut self) {
        self.is_yielding = true
    }


    /// Is this Coroutine ready after `yield_now()`?
    pub fn is_yielding(&self) -> bool {
        self.is_yielding
    }

    /// Get coroutine user-provided data.
    pub fn get_userdata<T: Any>(&self) -> Option<&T> {
        let coroutine_ref = unsafe { &mut *self.rc.as_unsafe_cell().get() as &mut Coroutine };

        match coroutine_ref.user_data {
            Some(ref arc) => {
                let boxed_any: &Box<Any + Send + Sync> = arc.as_ref();
                let any: &Any = boxed_any.as_ref();
                any.downcast_ref::<T>()
            }
            None => None,
        }
    }
}

/// Mioco instance.
pub struct Mioco {
    join_handles: Vec<std::thread::JoinHandle<()>>,
    config: Config,
}

impl Mioco {
    /// Create new `Mioco` instance.
    pub fn new() -> Self {
        Mioco::new_configured(Config::new())
    }

    /// Create new `Mioco` instance with custom configuration.
    pub fn new_configured(config: Config) -> Self {
        Mioco {
            join_handles: Vec::new(),
            config: config,
        }
    }

    /// Start mioco instance.
    ///
    /// Takes a starting handler function that will be executed in `mioco` environment.
    ///
    /// Will block until `mioco` is finished - there are no more coroutines to run.
    ///
    /// Returns the result of the `f`.
    ///
    /// See `MiocoHandle::start()`.
    pub fn start<F, T>(&mut self, f: F) -> std::thread::Result<T>
        where F: FnOnce() -> T,
              F: Send + 'static,
              T: Send + 'static
    {
        let (sender, receiver) = sync::mpsc::channel();

        self.run(f, sender);

        let join = JoinHandle { receiver: receiver };
        join.join()
    }

    fn run<F, T>(&mut self, f: F, co_exit_sender: coroutine::ExitSender<T>)
        where F: FnOnce() -> T,
              F: Send + 'static,
              T: Send + 'static
    {
        info!("starting instance with {} threads", self.config.thread_num);
        let thread_shared = Arc::new(thread::HandlerThreadShared::new(self.config.thread_num));

        let mut event_loops = VecDeque::new();
        let mut senders = Vec::new();
        for _ in 0..self.config.thread_num {
            let event_loop = EventLoop::configured(self.config.event_loop_config.clone())
                                 .expect("new EventLoop");
            senders.push(event_loop.channel());
            event_loops.push_back(event_loop);
        }

        let sched = self.config.scheduler.spawn_thread();
        let first_event_loop = event_loops.pop_front().unwrap();

        for i in 1..self.config.thread_num {

            let scheduler = self.config.scheduler.clone();
            let coroutine_config = self.config.coroutine_config;
            let event_loop = event_loops.pop_front().unwrap();
            let senders = senders.clone();
            let thread_shared = thread_shared.clone();
            let join = std::thread::Builder::new()
                           .name(format!("mioco_thread_{}", i))
                           .spawn(move || {
                               let sched = scheduler.spawn_thread();
                               Mioco::thread_loop::<F, T>(None,
                                                          sched,
                                                          event_loop,
                                                          i,
                                                          senders,
                                                          thread_shared,
                                                          None,
                                                          coroutine_config);
                           });

            match join {
                Ok(join) => self.join_handles.push(join),
                Err(err) => panic!("Couldn't spawn thread: {}", err),
            }
        }

        let mut user_data = None;
        mem::swap(&mut user_data, &mut self.config.user_data);
        Mioco::thread_loop(Some((f, co_exit_sender)),
                           sched,
                           first_event_loop,
                           0,
                           senders,
                           thread_shared,
                           user_data,
                           self.config.coroutine_config);

        for join in self.join_handles.drain(..) {
            let _ = join.join(); // TODO: Do something with it
        }
    }

    fn thread_loop<F, T>(f_and_sender: Option<(F, coroutine::ExitSender<T>)>,
                         scheduler: Box<SchedulerThread + 'static>,
                         mut event_loop: EventLoop<thread::Handler>,
                         thread_id: usize,
                         senders: Vec<thread::MioSender>,
                         thread_shared: thread::ArcHandlerThreadShared,
                         userdata: Option<Arc<Box<Any + Send + Sync>>>,
                         coroutine_config: coroutine::Config)
        where F: FnOnce() -> T,
              F: Send + 'static,
              T: Send + 'static
    {
        let handler_shared = thread::HandlerShared::new(senders,
                                                        thread_shared,
                                                        coroutine_config,
                                                        thread_id);
        let shared = Rc::new(RefCell::new(handler_shared));
        if let Some((f, exit_sender)) = f_and_sender {
            let coroutine_rc = Coroutine::spawn(shared.clone(), userdata, f, exit_sender);
            // Mark started only after first coroutine is spawned so that
            // threads don't start, detect no coroutines, and exit prematurely
            shared.borrow().signal_start_all();
            shared.borrow_mut().add_spawned(CoroutineControl::new(coroutine_rc));
        }
        let mut handler = thread::Handler::new(shared, scheduler);

        handler.shared().borrow().wait_for_start_all();
        {
            let sh = handler.shared().borrow();
            thread_debug!(sh, "event loop: starting");
        }
        handler.tick(&mut event_loop);
        event_loop.run(&mut handler).unwrap();
        {
            let sh = handler.shared().borrow();
            thread_debug!(sh, "event loop: done");
        }
    }
}

/// Mioco instance builder.
pub struct Config {
    thread_num: usize,
    scheduler: Arc<Box<Scheduler>>,
    event_loop_config: EventLoopConfig,
    user_data: Option<Arc<Box<Any + Send + Sync>>>,
    coroutine_config: coroutine::Config,
}

impl Config {
    /// Create mioco `Config`.
    ///
    /// Use it to configure mioco instance.
    ///
    /// See `start()` and `start_threads()` for convenience wrappers.
    pub fn new() -> Self {
        Config {
            thread_num: num_cpus::get(),
            scheduler: Arc::new(Box::new(FifoScheduler::new())),
            event_loop_config: Default::default(),
            user_data: None,
            coroutine_config: Default::default(),
        }
    }

    /// Set numer of threads to run mioco with
    ///
    /// Default is equal to a numer of CPUs in the system.
    pub fn set_thread_num(&mut self, thread_num: usize) -> &mut Self {
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
    pub fn set_scheduler(&mut self, scheduler: Box<Scheduler + 'static>) -> &mut Self {
        self.scheduler = Arc::new(scheduler);
        self
    }

    /// Set stack size in bytes.
    ///
    /// Default is 2MiB.
    ///
    /// Should be a power of 2.
    ///
    /// Stack size includes a protection page. Setting too small stack will
    /// lead to SEGFAULTs. See [context-rs stack.rs](https://github.com/zonyitoo/context-rs/blob/master/src/stack.rs)
    /// for implementation details. The sane minimum seems to be 128KiB,
    /// which is two 64KB pages.
    pub unsafe fn set_stack_size(&mut self, stack_size: usize) -> &mut Self {
        self.coroutine_config.stack_size = stack_size;
        self
    }

    /// Set user-provided data for the first coroutine
    ///
    /// See `set_userdata`.
    pub fn set_userdata<T: Reflect + Send + Sync + 'static>(&mut self, data: T) -> &mut Self {
        self.user_data = Some(Arc::new(Box::new(data)));
        self
    }

    /// Configure `mio::EvenLoop` for all the threads
    pub fn event_loop(&mut self) -> &mut EventLoopConfig {
        &mut self.event_loop_config
    }

    /// Set if this instance will be catching panics, that occur within the coroutines
    ///
    /// * If true mioco will gracefully handle panics inside coroutines.
    /// (default)
    /// * If false any panic inside coroutine will panic the whole mioco
    /// instance.
    pub fn set_catch_panics(&mut self, catch_panics: bool) -> &mut Self {
        self.coroutine_config.catch_panics = catch_panics;
        self
    }

    /// Set if this instance should use protected stacks (default).
    ///
    /// Unprotected stacks can be used to skip creation of stack guard page.
    /// This is useful when hitting OS limits regarding process mappings.
    /// It's better idea to fix it at the OS level, but eg. for automated
    /// testing it might be useful.
    pub unsafe fn set_stack_protection(&mut self, stack_protection: bool) -> &mut Self {
        self.coroutine_config.stack_protection = stack_protection;
        self
    }
}


/// Start a new mioco instance.
///
/// Creates a mioco instance with default configuration and calls
/// `Mioco::start(f)` on it.
///
/// This will block until mioco instance exits, which can be caused by:
/// * no more runnable coroutines,
/// * `shutdown()`.
pub fn start<F, T>(f: F) -> std::thread::Result<T>
    where F: FnOnce() -> T,
          F: Send + 'static,
          T: Send + 'static
{
    Mioco::new().start(f)
}

/// Start a new mioco instance with a given number of threads.
///
/// Returns after mioco instance exits.
///
/// Shorthand for `mioco::start()` running given number of threads.
pub fn start_threads<F, T>(thread_num: usize, f: F) -> std::thread::Result<T>
    where F: FnOnce() -> T,
          F: Send + 'static,
          T: Send + 'static
{
    let mut config = Config::new();
    config.set_thread_num(thread_num);
    Mioco::new_configured(config).start(f)
}

/// Allows to join on mioco Coroutine
///
/// Can be used both inside and outside of mioco instance.
pub struct JoinHandle<T> {
    receiver: sync::mpsc::Receiver<coroutine::ExitStatus<T>>,
}

impl<T> JoinHandle<T> where T: Send + 'static
{
    /// Block waiting for coroutine completion
    ///
    /// Return value returned by coroutine or `Err` if coroutine panicked or
    /// was killed.
    pub fn join(self) -> std::thread::Result<T> {
        match self.receiver.recv() {
            Ok(t) => t,
            // TODO: More informative errors
            Err(err) => Err(Box::new(err)),
        }
    }
}

/// Spawn a mioco coroutine.
///
/// If called inside an existing mioco instance - spawn and run a new coroutine
/// in it.
///
/// If called outside of existing mioco instance - spawn a new mioco instance
/// in a separate thread or use existing mioco instance to run new mioco
/// coroutine. The API intention is to guarantee:
///
/// * this call will not block
/// * coroutine will be executing in some mioco instance
///
/// the details on reusing existing mioco instances might change.
pub fn spawn<F, T>(f: F) -> JoinHandle<T>
    where F: FnOnce() -> T,
          F: Send + 'static,
          T: Send + 'static
{
    let coroutine = tl_current_coroutine_ptr();
    let (sender, receiver) = sync::mpsc::channel();

    if coroutine == ptr::null_mut() {
        std::thread::spawn(|| {
            Mioco::new().run(f, sender);
        });
    } else {
        let coroutine = unsafe { tl_current_coroutine() };
        coroutine.spawn_child(f, sender)
    }

    JoinHandle { receiver: receiver }
}

/// Shutdown current mioco instance.
///
/// Call from inside of a mioco instance to shut it down.
/// All existing coroutines will be forced in to panic, and
/// their stack unwind.
///
/// To shutdown mioco instance from the outside, spawn a coroutine
/// that shuts it down from the inside after receiving a chanel message.
pub fn shutdown() -> ! {
    let coroutine = unsafe { tl_current_coroutine() };
    {
        let shared = coroutine.handler_shared();
        shared.broadcast_shutdown();
    }
    loop {
        yield_now();
    }
}

/// Check if running inside a mioco coroutine.
///
/// Returns true when executing inside a mioco coroutine, false otherwise.
pub fn in_coroutine() -> bool {
    let coroutine = tl_current_coroutine_ptr();
    coroutine != ptr::null_mut()
}

/// Execute a block of synchronous operations.
///
/// This will execute a block of synchronous operations without blocking
/// cooperative coroutine scheduling. This is done by offloading the
/// synchronous operations to a separate thread, and blocking current
/// coroutine when operation is completed.
///
/// TODO: find some wise people to confirm if this is sound
/// TODO: use threadpool to prevent potential system starvation?
pub fn sync<'b, F, R>(f: F) -> R
    where F: FnOnce() -> R + 'b,
          F : Send,
          R : Send,
{
    let coroutine = unsafe { tl_current_coroutine() };

    if coroutine.sync_channel.is_none() {
        let (send, recv) = mpsc::channel();
        coroutine.sync_channel = Some((send, recv));
    }

    let &(ref tx, ref rx) = coroutine.sync_channel.as_ref().unwrap();
    let join = unsafe {
        thread_scoped::scoped({
            let sender = tx.clone();
            move || {
                let res = f();
                sender.send(()).unwrap();
                res
            }
        })
    };

    rx.recv().unwrap();

    let res = join.join();
    res
}

/// Get user-provided data of the current coroutine.
///
/// Gets a reference to the user data, that was set through `set_userdata`.
/// Returns `None` if `T` does not match or if no data was set.
///
/// See `set_userdata`.
pub fn get_userdata<'a, T: Any>() -> Option<&'a T> {
    let coroutine = unsafe { tl_current_coroutine() };
    match coroutine.user_data {
        Some(ref arc) => {
            let boxed_any: &Box<Any + Send + Sync> = arc.as_ref();
            let any: &Any = boxed_any.as_ref();
            any.downcast_ref::<T>()
        }
        None => None,
    }
}

/// Set user-provided data for the current coroutine.
///
/// Every coroutine can carry an additional piece of data.
pub fn set_userdata<T: Reflect + Send + Sync + 'static>(data: T) {
    let mut coroutine = unsafe { tl_current_coroutine() };
    coroutine.user_data = Some(Arc::new(Box::new(data)));
}

/// Set user-provided data for future child coroutines.
///
/// Set new user data that newly spawned coroutines will start with. Use
/// `None` to clear.
///
/// See `set_userdata`.
pub fn set_children_userdata<T: Reflect + Send + Sync + 'static>(data: Option<T>) {
    let mut coroutine = unsafe { tl_current_coroutine() };
    coroutine.inherited_user_data = match data {
        Some(data) => Some(Arc::new(Box::new(data))),
        None => None,
    }
}

/// Get number of threads of current mioco instance.
///
/// Get number of threads of the Mioco instance that the current coroutine
/// is running in.
///
/// This is useful eg. for load balancing: spawning as many coroutines as
/// there is handling threads that can run them.
pub fn thread_num() -> usize {
    let coroutine = unsafe { tl_current_coroutine() };

    coroutine.handler_shared().thread_num()
}

/// Block execution for a given time.
///
/// Inside mioco coroutine, this will yield execution and block coroutine
/// for a given period of time.
///
/// Out of mioco instance, this will act just like `std::thread::sleep`.
///
/// Warning: When issued inside of mioco, the precision of this call (and
/// other time based functionality) is limited by `mio` event loop
/// settings. Any small value of `time_ms` will effectively be rounded up to
/// `mio::EventLoop::timer_tick_ms()`. This can be configured with
/// custom `mioco::Config` argument passed to `mioco::new_configured()`
pub fn sleep(duration: std::time::Duration) {
    if in_coroutine() {
        let mut timer = Timer::new();
        let dur_ms : u64 = duration.as_secs() * 1000 + duration.subsec_nanos() as u64 / 1_000_000;
        timer.set_timeout(dur_ms as i64);
        let _ = timer.read();
    } else {
        std::thread::sleep(duration);
    }
}

/// Block execution for a given time.
///
/// See `sleep`.
#[allow(deprecated)]
pub fn sleep_ms(time_ms: u32) {
    if in_coroutine() {
        let mut timer = Timer::new();
        timer.set_timeout(time_ms as i64);
        let _ = timer.read();
    } else {
        std::thread::sleep_ms(time_ms);
    }
}

/// Yield execution of the current coroutine.
///
/// Coroutine can yield execution without blocking on anything
/// particular to allow scheduler to run other coroutines before
/// resuming execution of the current one.
pub fn yield_now() {
    let coroutine = unsafe { tl_current_coroutine() };
    coroutine.state = coroutine::State::Yielding;
    co_debug!(coroutine, "yield");
    coroutine::jump_out(&coroutine.self_rc.as_ref().unwrap());
}

/// Block the current coroutine waiting for an event.
///
/// Use `select!` macro instead.
///
/// **Warning**: Mioco can't guarantee that the returned `EventSource` will
/// not block when actually attempting to `read` or `write`. You must
/// use `try_read` and `try_write` instead to handle spurious wakeups.
///
/// The returned value contains event type and the id of the `EventSource`.
/// See `EventSourceId`.
pub fn select_wait() -> Event {
    let coroutine = unsafe { tl_current_coroutine() };
    coroutine.state = coroutine::State::Blocked;

    co_debug!(coroutine, "blocked on select");
    coroutine::jump_out(&coroutine.self_rc.as_ref().unwrap());

    co_debug!(coroutine, "select ret={:?}", coroutine.last_event);
    coroutine.last_event
}

/// Select operation on multiple IO.
///
/// **Warning**: Mioco can't guarantee that the returned `EventSource` will
/// not block when actually attempting to `read` or `write`. You must
/// use `try_read` and `try_write` instead to handle spurious wakeups.
///
#[macro_export]
macro_rules! select {
    (@wrap1 ) => {};
    (@wrap1 r:$rx:expr => $code:expr, $($tail:tt)*) => {
        unsafe {
            use $crate::Evented;
            $rx.select_add($crate::RW::read());
        }
        select!(@wrap1 $($tail)*)
    };
    (@wrap1 w:$rx:expr => $code:expr, $($tail:tt)*) => {
        unsafe {
            use $crate::Evented;
            $rx.select_add($crate::RW::write());
        }
        select!(@wrap1 $($tail)*)
    };
    (@wrap1 rw:$rx:expr => $code:expr, $($tail:tt)*) => {
        unsafe {
            use $crate::Evented;
            $rx.select_add($crate::RW::both());
        }
        select!(@wrap1 $($tail)*)
    };
    (@wrap2 $ret:ident) => {
        // end code
    };
    (@wrap2 $ret:ident r:$rx:expr => $code:expr, $($tail:tt)*) => {{
        use $crate::Evented;
        if $ret.id() == $rx.id() { $code }
        select!(@wrap2 $ret $($tail)*);
    }};
    (@wrap2 $ret:ident w:$rx:expr => $code:expr, $($tail:tt)*) => {{
        use $crate::Evented;
        if $ret.id() == $rx.id() { $code }
        select!(@wrap2 $ret $($tail)*);
    }};
    (@wrap2 $ret:ident rw:$rx:expr => $code:expr, $($tail:tt)*) => {{
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

