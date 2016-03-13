use std::sync as ssync;
use std::fmt;

use std;
use std::sync::Arc;
use std::sync::atomic;
use std::sync::atomic::Ordering;

mod mioco {
    pub use super::super::*;
}

use std::collections::VecDeque;

use mio_orig::{Token, EventLoop, EventSet};
use super::thread::{MioSender, Message, Handler};
use super::evented::{RcEventSource, EventSourceTrait, EventedImpl, Evented};
use super::{RW, sender_retry};

/// MPSC channel modeled after `std::sync::mpsc`.
pub mod mpsc;

/// A reader-writer lock
///
/// Based on `std::sync::RwLock`. Calls `mioco::yield_now()` on contention.
///
/// Works both inside and outside of mioco.
#[derive(Debug)]
pub struct RwLock<T: ?Sized> {
    lock: ssync::RwLock<T>,
}

impl<T> RwLock<T> {
    /// Creates a new instance of an RwLock<T> which is unlocked.
    pub fn new(t: T) -> Self {
        RwLock { lock: ssync::RwLock::new(t) }
    }
}

impl<T: ?Sized> RwLock<T> {
    /// Get a reference to raw `std::sync::RwLock`.
    ///
    /// Use it to perform operations outside of mioco coroutines.
    pub fn native_lock(&self) -> &ssync::RwLock<T> {
        &self.lock
    }

    /// Locks this rwlock with shared read access, blocking the current
    /// coroutine until it can be acquired.
    pub fn read(&self) -> ssync::LockResult<ssync::RwLockReadGuard<T>> {
        if mioco::in_coroutine() {
            self.read_in_mioco()
        } else {
            self.lock.read()
        }
    }

    fn read_in_mioco(&self) -> ssync::LockResult<ssync::RwLockReadGuard<T>> {
        loop {
            match self.lock.try_read() {
                Ok(guard) => return Ok(guard),
                Err(try_error) => {
                    match try_error {
                        ssync::TryLockError::Poisoned(p_err) => {
                            return Err(p_err);
                        }
                        ssync::TryLockError::WouldBlock => mioco::yield_now(),
                    }
                }
            }
        }
    }

    /// Attempts to acquire this rwlock with shared read access.
    pub fn try_read(&self) -> ssync::TryLockResult<ssync::RwLockReadGuard<T>> {
        self.lock.try_read()
    }

    /// Locks this rwlock with exclusive write access, blocking the current
    /// coroutine until it can be acquired.
    pub fn write(&self) -> ssync::LockResult<ssync::RwLockWriteGuard<T>> {
        if mioco::in_coroutine() {
            self.write_in_mioco()
        } else {
            self.lock.write()
        }
    }

    fn write_in_mioco(&self) -> ssync::LockResult<ssync::RwLockWriteGuard<T>> {
        loop {
            match self.lock.try_write() {
                Ok(guard) => return Ok(guard),
                Err(try_error) => {
                    match try_error {
                        ssync::TryLockError::Poisoned(p_err) => {
                            return Err(p_err);
                        }
                        ssync::TryLockError::WouldBlock => mioco::yield_now(),
                    }
                }
            }
        }
    }

    /// Attempts to lock this rwlock with exclusive write access.
    pub fn try_write(&self) -> ssync::TryLockResult<ssync::RwLockWriteGuard<T>> {
        self.lock.try_write()
    }

    /// Determines whether the lock is poisoned.
    pub fn is_poisoned(&self) -> bool {
        self.lock.is_poisoned()
    }
}

/// A Mutex
///
/// Based on `std::sync::Mutex`.
///
/// Calls `mioco::yield_now()` on contention. (NOTE: Subject to potential change).
///
/// Works both inside and outside of mioco.
pub struct Mutex<T: ?Sized> {
    lock: ssync::Mutex<T>,
}

impl<T: ?Sized + fmt::Debug + 'static> fmt::Debug for Mutex<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.lock.fmt(f)
    }
}

impl<T> Mutex<T> {
    /// Creates a new instance of an Mutex<T> which is unlocked.
    pub fn new(t: T) -> Self {
        Mutex { lock: ssync::Mutex::new(t) }
    }
}

impl<T: ?Sized> Mutex<T> {
    /// Get a reference to raw `std::sync::Mutex`.
    ///
    /// Use it to perform operations outside of mioco coroutines.
    pub fn native_lock(&self) -> &ssync::Mutex<T> {
        &self.lock
    }

    /// Acquire a mutex, blocking the current coroutine until it is able to do so.
    pub fn lock(&self) -> ssync::LockResult<ssync::MutexGuard<T>> {
        if mioco::in_coroutine() {
            self.lock_in_mioco()
        } else {
            self.lock.lock()
        }
    }

    fn lock_in_mioco(&self) -> ssync::LockResult<ssync::MutexGuard<T>> {
        loop {
            match self.try_lock() {
                Ok(guard) => return Ok(guard),
                Err(try_error) => {
                    match try_error {
                        ssync::TryLockError::Poisoned(p_err) => {
                            return Err(p_err);
                        }
                        ssync::TryLockError::WouldBlock => mioco::yield_now(),
                    }
                }
            }
        }
    }

    /// Attempt to acquire this lock.
    pub fn try_lock(&self) -> ssync::TryLockResult<ssync::MutexGuard<T>> {
        self.lock.try_lock()
    }
}

type CoroutineWakeup = (Token, MioSender);

/// Shared between all clones on Condvar
struct CondvarShared {
    native : std::sync::Condvar,
    sleeping_coroutines: VecDeque<CoroutineWakeup>,
}

pub struct Condvar(RcEventSource<CondvarCore>);

impl EventedImpl for Condvar
{
    type Raw = CondvarCore;

    fn shared(&self) -> &RcEventSource<Self::Raw> {
        &self.0
    }
}

struct CondvarCore {
    shared : Arc<Mutex<CondvarShared>>,
    shared_counter : Arc<atomic::AtomicUsize>,
    last_counter : usize,
}

impl CondvarCore {
    fn wake_one(&self) {
        let mut lock = self.shared.lock().unwrap();
        if let Some((token, sender)) = lock.sleeping_coroutines.pop_front() {
            sender_retry(&sender, Message::ChannelMsg(token));
        } else {
            lock.native.notify_one();
        }
    }
}

struct MutexGuard<'a, T: ?Sized + 'a> {
    __lock: &'a mut Mutex<T>,
    __guard: std::sync::MutexGuard<'a, T>,
}

impl Condvar {
    pub fn wait<'a, T>(&self, guard: MutexGuard<'a, T>)
        -> std::sync::LockResult<MutexGuard<'a, T>> {
        {
            let core = self.0.io_ref();
            let counter = core.shared_counter.load(Ordering::SeqCst);
            core.last_counter = counter;
        }
        let lock = Mutex::from_native(unsafe { guard.__lock });
        drop(guard);

        self.block_on(RW::read());

        lock.lock().map(|g| MutexGuard(g))
    }
}

impl EventSourceTrait for CondvarCore {
    fn register(&self, event_loop: &mut EventLoop<Handler>, token: Token, interest: EventSet) {
        debug_assert!(interest.is_readable());
        trace!("Condvar({}): register", token.as_usize());
        {
            let mut lock = self.shared.lock();

            lock.sleeping_coroutines.push_back(
                (token, event_loop.channel())
                );
        }

        if self.shared_counter.fetch_add(1, Ordering::SeqCst) != self.last_counter {
            trace!("Condvar({}): changed; self notify", token.as_usize());
            self.wake_one();
        }
    }

    fn reregister(&self, event_loop: &mut EventLoop<Handler>, token: Token, interest: EventSet) {
        self.register(event_loop, token, interest);
    }

    fn deregister(&self, _event_loop: &mut EventLoop<Handler>, token: Token) {
        trace!("Condvar({}): dereregister", token.as_usize());
        // TODO: remove from sleepers?
    }
}


