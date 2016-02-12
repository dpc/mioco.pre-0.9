use super::{RW, EventSourceId, State, Handler, CoroutineId};
use super::{tl_coroutine_current, coroutine_jump_out, entry_point};
use super::{token_from_ids};
use super::mio_orig;

use super::mio_orig::{EventLoop, Token, EventSet};

use std::io;
use std::rc::Rc;
use std::cell::{RefCell, Ref, RefMut};
use std::os::unix::io::{RawFd, FromRawFd, AsRawFd};

/// Mioco event source.
///
/// All types used as asynchronous event sources implement this trait.
pub trait Evented {
    #[doc(hidden)]
    /// Add event source to next select operation.
    ///
    /// Use `select!` macro instead.
    ///
    /// This is unsafe as the `Select::wait()` has to be called afterwards,
    /// and registered EventSource must not be send to different
    /// thread/coroutine before `Select::wait()`.
    ///
    /// Use `select!` macro instead.
    unsafe fn select_add(&self, rw: RW);

    /// Mark the `EventSourceRef` blocked and block until `Handler` does
    /// not wake us up again.
    #[doc(hidden)]
    fn block_on(&self, rw: RW);

    /// Temporary ID used in select operation.
    #[doc(hidden)]
    fn id(&self) -> EventSourceId;
}

/// Private trait for all mioco provided IO
pub trait EventedImpl {
    type Raw : EventedInner+'static;

    fn shared(&self) -> &RcEvented<Self::Raw>;

    fn block_on_prv(&self, rw: RW) {
            let coroutine = tl_coroutine_current();
            {
                trace!("Coroutine({}): blocked on {:?}",
                       coroutine.id.as_usize(),
                       rw);
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
                trace!("Coroutine({}): resumed due to event {:?}",
                       coroutine.id.as_usize(),
                       coroutine.last_event);
                debug_assert!(rw.has_read() || coroutine.last_event.has_write());
                debug_assert!(rw.has_write() || coroutine.last_event.has_read());
                debug_assert!(coroutine.last_event.id().as_usize() ==
                              self.shared().0.borrow().common.id.unwrap().as_usize());
            }
        }

        /// Index id of a `EventSource`
        fn id_prv(&self) -> EventSourceId {
            self.shared().0.borrow().common.id.unwrap()
        }

        unsafe fn select_add_prv(&self, rw: RW) {
            let coroutine = tl_coroutine_current();

            if !coroutine.io.has_remaining() {
                let count = coroutine.io.count();
                coroutine.io.grow(count);
            }

            coroutine.io
                .insert_with(|id| {
                    self.shared().0.borrow_mut().common.id = Some(id);
                    self.shared().0.borrow_mut().common.blocked_on = rw;
                    self.shared().to_trait()
                })
            .unwrap();
        }
}

/// Raw `mio` types adapter to mioco IO
///
/// Use to adapt any `mio::Evented` implementing struct
/// to mioco. See source of `src/tcp.rs` for example of
/// usage.
pub struct MioAdapter<MT>(RcEvented<MT>);

// All mioco IO functions are guaranteed to not keep
// any references to `Evented` structs. `Rc` is used
// only during blocking, when coroutine is not running.
unsafe impl<MT> Send for MioAdapter<MT>
where MT : mio_orig::Evented+'static {}

impl<MT> MioAdapter<MT>
where MT : mio_orig::Evented+'static {
    /// Create `MioAdapter` from raw mio type.
    pub fn new(mio_type : MT) -> Self {
        MioAdapter(RcEvented::new(mio_type))
    }
}

impl<MT> EventedImpl for MioAdapter<MT>
where MT : mio_orig::Evented+'static {
    type Raw = MT;

    fn shared(&self) -> &RcEvented<Self::Raw> {
        &self.0
    }
}

impl<MT> MioAdapter<MT>
where MT : mio_orig::Evented+'static + mio_orig::TryRead {
    /// Try reading data into a buffer.
    ///
    /// This will not block.
    pub fn try_read(&mut self, buf: &mut [u8]) -> io::Result<Option<usize>> {
        self.shared().try_read(buf)
    }
}

impl<MT> io::Read for MioAdapter<MT>
where MT : mio_orig::Evented+'static + mio_orig::TryRead {
    /// Block on read.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let res = self.shared().try_read(buf);

            match res {
                Ok(None) => self.block_on(RW::read()),
                Ok(Some(r)) => {
                    return Ok(r);
                }
                Err(e) => return Err(e),
            }
        }
    }
}

impl<MT> MioAdapter<MT>
where MT : mio_orig::Evented+'static + mio_orig::TryWrite {
    /// Try writing a data from the buffer.
    ///
    /// This will not block.
    pub fn try_write(&self, buf: &[u8]) -> io::Result<Option<usize>> {
        self.shared().try_write(buf)
    }

}

impl<MT> io::Write for MioAdapter<MT>
where MT : mio_orig::Evented+'static + mio_orig::TryWrite {
    /// Block on write.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        loop {
            let res = self.shared().try_write(buf);

            match res {
                Ok(None) => self.block_on(RW::write()),
                Ok(Some(r)) => {
                    return Ok(r);
                }
                Err(e) => return Err(e),
            }
        }
    }

    // TODO: ?
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<MT> FromRawFd for MioAdapter<MT>
where MT : mio_orig::Evented+'static + FromRawFd {
    unsafe fn from_raw_fd(fd: RawFd) -> Self {
        MioAdapter(RcEvented::new(MT::from_raw_fd(fd)))
    }
}

impl<MT> AsRawFd for MioAdapter<MT>
where MT : mio_orig::Evented+'static + AsRawFd {
    fn as_raw_fd(&self) -> RawFd {
        self.shared().0.borrow_mut().io.as_raw_fd()
    }
}

impl<R, EP> Evented for EP
where
EP : EventedImpl<Raw=R>,
R : EventedInner+'static {

    unsafe fn select_add(&self, rw: RW) {
        EventedImpl::select_add_prv(self, rw)
    }

    fn block_on(&self, rw: RW) {
        EventedImpl::block_on_prv(self, rw)
    }

    fn id(&self) -> EventSourceId {
        EventedImpl::id_prv(self)
    }
}

/// Inner functions for Evented types
pub trait EventedInner {
    /// Register
    fn register(&self, event_loop: &mut EventLoop<Handler>, token: Token, interest: EventSet);

    /// Reregister
    fn reregister(&self, event_loop: &mut EventLoop<Handler>, token: Token, interest: EventSet);

    /// Deregister
    fn deregister(&self, event_loop: &mut EventLoop<Handler>, token: Token);

    /// Should the coroutine be resumed on event for this `EventSource<Self>`
    fn should_resume(&self) -> bool;
}

impl<T> EventedInner for T where T: mio_orig::Evented
{
    fn register(&self, event_loop: &mut EventLoop<Handler>, token: Token, interest: EventSet) {
        event_loop.register(self, token, interest, mio_orig::PollOpt::edge())
                  .expect("register failed");
    }

    /// Reregister
    fn reregister(&self, event_loop: &mut EventLoop<Handler>, token: Token, interest: EventSet) {
        event_loop.reregister(self, token, interest, mio_orig::PollOpt::edge())
                  .expect("reregister failed");
    }

    /// Deregister
    fn deregister(&self, event_loop: &mut EventLoop<Handler>, _token: Token) {
        event_loop.deregister(self)
                  .expect("deregister failed");
    }

    fn should_resume(&self) -> bool {
        true
    }
}

pub trait RcEventedTrait {
    /// Reregister oneshot handler for the next event
    fn register(&mut self, event_loop: &mut EventLoop<Handler>, co_id: CoroutineId);

    /// Reregister oneshot handler for the next event
    fn reregister(&mut self, event_loop: &mut EventLoop<Handler>, co_id: CoroutineId);

    /// Un-reregister event we're not interested in anymore
    fn deregister(&mut self, event_loop: &mut EventLoop<Handler>, co_id: CoroutineId);

    fn hup(&mut self, _event_loop: &mut EventLoop<Handler>, token: Token);

    fn blocked_on(&self) -> RW;

    fn should_resume(&self) -> bool;
}

impl<T> RcEventedTrait for RcEvented<T> where T: EventedInner
{
    fn blocked_on(&self) -> RW {
        self.0.borrow().common.blocked_on
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
    fn register(&mut self, event_loop: &mut EventLoop<Handler>, co_id: CoroutineId) {
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
    fn reregister(&mut self, event_loop: &mut EventLoop<Handler>, co_id: CoroutineId) {
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
    fn deregister(&mut self, event_loop: &mut EventLoop<Handler>, co_id: CoroutineId) {
        let token = token_from_ids(co_id, self.0.borrow().common.id.unwrap());
        EventedInner::deregister(&self.0.borrow().io, event_loop, token);
    }
}

struct EventedSharedCommon {
    id: Option<EventSourceId>,
    blocked_on: RW,
    peer_hup: bool,
}

/// Wrapped mio IO (mio_orig::Evented+TryRead+TryWrite)
///
/// `Handle` is just a cloneable reference to this struct
pub struct EventedShared<T> {
    common: EventedSharedCommon,
    io: T,
}

impl<T> EventedShared<T> {
    pub fn new(t: T) -> Self {
        EventedShared {
            common: EventedSharedCommon {
                id: None,
                blocked_on: RW::none(),
                peer_hup: false,
            },
            io: t,
        }
    }
}

pub struct RcEvented<T>(Rc<RefCell<EventedShared<T>>>);

impl<T> RcEvented<T> {
    pub fn new(t: T) -> Self {
        RcEvented(Rc::new(RefCell::new(EventedShared::new(t))))
    }

    pub fn io_ref(&self) -> Ref<T> {
        Ref::map(self.0.borrow(), |r| &r.io)
    }

    pub fn io_mut(&self) -> RefMut<T> {
        RefMut::map(self.0.borrow_mut(), |r| &mut r.io)
    }
}

impl<T> RcEvented<T> where T: mio_orig::TryRead
{
    fn try_read(&self, buf: &mut [u8]) -> io::Result<Option<usize>> {
        self.0.borrow_mut().io.try_read(buf)
    }
}

impl<T> RcEvented<T> where T: mio_orig::TryWrite
{
    fn try_write(&self, buf: &[u8]) -> io::Result<Option<usize>> {
        self.0.borrow_mut().io.try_write(buf)
    }
}

impl<T> RcEvented<T> where T: mio_orig::TryAccept
{
    pub fn try_accept(&self) -> io::Result<Option<<T as mio_orig::io::TryAccept>::Output>> {
        self.0.borrow_mut().io.accept()
    }
}

impl<T> RcEvented<T> where T: EventedInner + 'static
{
    fn to_trait(&self) -> Box<RcEventedTrait + 'static> {
        Box::new(RcEvented(self.0.clone()))
    }
}

impl<T> EventedInner for RcEvented<T> where T: EventedInner
{
    /// Register
    fn register(&self, event_loop: &mut EventLoop<Handler>, token: Token, interest: EventSet) {
        self.0.borrow().io.register(event_loop, token, interest);
    }

    /// Reregister
    fn reregister(&self, event_loop: &mut EventLoop<Handler>, token: Token, interest: EventSet) {
        self.0.borrow().io.reregister(event_loop, token, interest);
    }

    /// Deregister
    fn deregister(&self, event_loop: &mut EventLoop<Handler>, token: Token) {
        self.0.borrow().io.deregister(event_loop, token);
    }

    fn should_resume(&self) -> bool {
        self.0.borrow().io.should_resume()
    }
}

