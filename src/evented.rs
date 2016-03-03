use super::{RW, EventSourceId, coroutine};
use super::thread::Handler;
use super::thread::{tl_current_coroutine};
use super::{token_from_ids};
use super::mio_orig;

use super::mio_orig::{EventLoop, Token, EventSet};

use std::io;
use std::rc::Rc;
use std::cell::{RefCell, Ref, RefMut};
#[cfg(not(windows))]
use std::os::unix::io::{RawFd, FromRawFd, AsRawFd};

/// Mioco event source.
///
/// All types used as asynchronous event sources implement this trait.
///
/// A generic implementation: `MioAdapter` implements this trait, wrapping
/// native mio types (implementing `mio::Evented` trait).
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
    type Raw : EventSourceTrait+'static;

    fn shared(&self) -> &RcEventSource<Self::Raw>;

    fn block_on_prv(&self, rw: RW) {
            let coroutine = unsafe {tl_current_coroutine()};
            coroutine.block_on(self.shared(), rw);
            coroutine::jump_out(&coroutine.self_rc.as_ref().unwrap());
            {
                co_debug!(coroutine, "resumed due to event {:?}", coroutine.last_event);
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
            let coroutine = tl_current_coroutine();
            let id = coroutine.blocked_on.len();
            let shared = self.shared();
            {
                let mut common = &mut shared.0.borrow_mut().common;
                common.id = Some(EventSourceId::new(id));
                common.blocked_on = rw;
            }
            coroutine.blocked_on.push(shared.to_trait());
        }
}

// All mioco IO functions are guaranteed to not keep any references to
// `Evented` structs after return. `Rc` is used only when coroutine
// is blocked - meaning it is not using it.
unsafe impl<T> Send for MioAdapter<T>
where T: mio_orig::Evented + Send {}

/// Adapt raw `mio` type to mioco `Evented` requirements.
///
/// See source of `src/tcp.rs` for example of usage.
pub struct MioAdapter<MT>(RcEventSource<MT>);

impl<MT> MioAdapter<MT>
where MT : mio_orig::Evented+'static {
    /// Create `MioAdapter` from raw mio type.
    pub fn new(mio_type : MT) -> Self {
        MioAdapter(RcEventSource::new(mio_type))
    }
}

impl<MT> EventedImpl for MioAdapter<MT>
where MT : mio_orig::Evented+'static {
    type Raw = MT;

    fn shared(&self) -> &RcEventSource<Self::Raw> {
        &self.0
    }
}

impl<MT> MioAdapter<MT>
where MT : mio_orig::Evented+'static + mio_orig::TryRead {
    /// Try reading data into a buffer.
    ///
    /// This will not block.
    pub fn try_read(&mut self, buf: &mut [u8]) -> io::Result<Option<usize>> {
        self.shared().io_mut().try_read(buf)
    }
}

impl<MT> io::Read for MioAdapter<MT>
where MT : mio_orig::Evented+'static + mio_orig::TryRead {
    /// Block on read.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let res = self.try_read(buf);

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
        self.shared().io_mut().try_write(buf)
    }
}

impl<MT> io::Write for MioAdapter<MT>
where MT : mio_orig::Evented+'static + mio_orig::TryWrite {
    /// Block on write.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        loop {
            let res = self.try_write(buf);

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

impl<MT, O> MioAdapter<MT>
where MT : mio_orig::Evented+'static + mio_orig::TryAccept<Output=O>,
      O : mio_orig::Evented+'static
{
    /// Attempt to accept a pending connection.
    ///
    /// This will not block.
    pub fn try_accept(&self) -> io::Result<Option<MioAdapter<O>>> {
        self.shared()
            .io_ref()
            .accept() // This is `try_accept`, see https://github.com/carllerche/mio/issues/355
            .map(|t| t.map(|t| MioAdapter::new(t)))
    }
}

impl<MT, O> MioAdapter<MT>
where MT : mio_orig::Evented+'static + mio_orig::TryAccept<Output=O>,
      O : mio_orig::Evented+'static
{
    /// Block on accepting a connection.
    pub fn accept(&self) -> io::Result<MioAdapter<O>> {
        loop {
            let res = self.try_accept();

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

//    type Output = MioAdapter<MT::Output>;
#[cfg(not(windows))]
impl<MT> FromRawFd for MioAdapter<MT>
where MT : mio_orig::Evented+'static + FromRawFd {
    unsafe fn from_raw_fd(fd: RawFd) -> Self {
        MioAdapter(RcEventSource::new(MT::from_raw_fd(fd)))
    }
}

#[cfg(not(windows))]
impl<MT> AsRawFd for MioAdapter<MT>
where MT : mio_orig::Evented+'static + AsRawFd {
    fn as_raw_fd(&self) -> RawFd {
        self.shared().0.borrow_mut().io.as_raw_fd()
    }
}

impl<R, EP> Evented for EP
where
EP : EventedImpl<Raw=R>,
R : EventSourceTrait+'static {

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

/// Trait for coroutine event source
///
/// From the perspective of the mioco event loop any event source
/// can be (re-/de-)registered and can conditionally wake up coroutine
/// it belongs to.
///
/// As the list of blocked event sources for each coroutine can have elements
/// of different types, the trait object is being used.
pub trait EventSourceTrait {
    /// Register
    fn register(&self, event_loop: &mut EventLoop<Handler>, token: Token, interest: EventSet);

    /// Reregister
    fn reregister(&self, event_loop: &mut EventLoop<Handler>, token: Token, interest: EventSet);

    /// Deregister
    fn deregister(&self, event_loop: &mut EventLoop<Handler>, token: Token);
}

impl<T> EventSourceTrait for T where T: mio_orig::Evented
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
}

pub trait RcEventSourceTrait {
    /// Reregister oneshot handler for the next event
    fn register(&mut self, event_loop: &mut EventLoop<Handler>, co_id: coroutine::Id);

    /// Reregister oneshot handler for the next event
    fn reregister(&mut self, event_loop: &mut EventLoop<Handler>, co_id: coroutine::Id);

    /// Un-reregister event we're not interested in anymore
    fn deregister(&mut self, event_loop: &mut EventLoop<Handler>, co_id: coroutine::Id);

    fn hup(&mut self, _event_loop: &mut EventLoop<Handler>, token: Token);

    fn blocked_on(&self) -> RW;
}

/// Common control data for all event sources.
pub struct EventSourceCommon {
    pub id: Option<EventSourceId>,
    pub blocked_on: RW,
    pub peer_hup: bool,
}

/// Wrapped mio IO (mio_orig::Evented+TryRead+TryWrite)
///
/// `Handle` is just a cloneable reference to this struct
pub struct RcEventSourceShared<T> {
    common: EventSourceCommon,
    io: T,
}

impl<T> RcEventSourceShared<T> {
    pub fn new(t: T) -> Self {
        RcEventSourceShared {
            common: EventSourceCommon {
                id: None,
                blocked_on: RW::none(),
                peer_hup: false,
            },
            io: t,
        }
    }
}

/// To share the data between coroutine itself and the coroutine internals
/// referring to the blocked event sources, `RcEventSource` is being used.
///
/// Mioco event loop and coroutine itself never use the data in the same
/// time: either event loop or coroutine logic can be executing at the same
/// time. Technically this could use a pointer, but RefCell is useful for
/// making sure no references are being kept when switching between these
/// two execution streams.
///
/// RcEventSource is parametrized over `T`, but also implements
/// `EventSourceTrait` to allow trait-object (dynamic-dispatch) access.
pub struct RcEventSource<T>(Rc<RefCell<RcEventSourceShared<T>>>);

impl<T> RcEventSource<T> {
    pub fn new(t: T) -> Self {
        RcEventSource(Rc::new(RefCell::new(RcEventSourceShared::new(t))))
    }

    pub fn io_ref(&self) -> Ref<T> {
        Ref::map(self.0.borrow(), |r| &r.io)
    }

    pub fn io_mut(&self) -> RefMut<T> {
        RefMut::map(self.0.borrow_mut(), |r| &mut r.io)
    }

    #[allow(unused)]
    pub fn common_ref(&self) -> Ref<EventSourceCommon> {
        Ref::map(self.0.borrow(), |r| &r.common)
    }

    pub fn common_mut(&self) -> RefMut<EventSourceCommon> {
        RefMut::map(self.0.borrow_mut(), |r| &mut r.common)
    }
}

impl<T> RcEventSourceTrait for RcEventSource<T> where T: EventSourceTrait
{
    fn blocked_on(&self) -> RW {
        self.0.borrow().common.blocked_on
    }

    /// Handle `hup` condition
    fn hup(&mut self, _event_loop: &mut EventLoop<Handler>, _token: Token) {
        trace!("hup");
        self.0.borrow_mut().common.peer_hup = true;
    }

    /// Reregister oneshot handler for the next event
    fn register(&mut self, event_loop: &mut EventLoop<Handler>, co_id: coroutine::Id) {
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

        EventSourceTrait::register(&self.0.borrow().io, event_loop, token, interest);
    }

    /// Reregister oneshot handler for the next event
    fn reregister(&mut self, event_loop: &mut EventLoop<Handler>, co_id: coroutine::Id) {
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

        EventSourceTrait::reregister(&self.0.borrow().io, event_loop, token, interest);
    }

    /// Un-reregister event we're not interested in anymore
    fn deregister(&mut self, event_loop: &mut EventLoop<Handler>, co_id: coroutine::Id) {
        let token = token_from_ids(co_id, self.0.borrow().common.id.unwrap());
        EventSourceTrait::deregister(&self.0.borrow().io, event_loop, token);
    }
}

impl<T> RcEventSource<T> where T: EventSourceTrait + 'static
{
    pub fn to_trait(&self) -> Box<RcEventSourceTrait + 'static> {
        Box::new(RcEventSource(self.0.clone()))
    }
}

impl<T> EventSourceTrait for RcEventSource<T> where T: EventSourceTrait
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
}
