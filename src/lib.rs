// Copyright 2015 Dawid Ciężarkiewicz <dpc@dpc.pw>
// See LICENSE-MPL2 file for more information.

//! Coroutine-based handler library for mio
//!
//! Using coroutines, an event-based mio model can be simplified to a set of routines, seamlessly
//! scheduled on demand in userspace.
//!
//! With `mioco` a coroutines can be used to simplify writing asynchronous io handilng in
//! synchronous fashion.

#![feature(result_expect)]
#![warn(missing_docs)]

extern crate mio;
extern crate coroutine;
extern crate nix;

use std::cell::RefCell;
use std::rc::Rc;
use mio::{TryRead, TryWrite, Evented, Token, Handler, EventLoop};

/// Select type
///
/// TBD.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SelectType {
    /// Read
    Read,
    /// Write
    Write,
    /// Any
    Both,
}

/// Last Event
///
/// Read or Write + index of handle
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LastEvent {
    /// Read for InternalHandle of a given index
    Read(usize),
    /// Write for InternalHandle of a given index
    Write(usize),
}

impl LastEvent {
    /// Index of the IO handle
    pub fn idx(&self) -> usize {
        match *self {
            LastEvent::Read(idx) | LastEvent::Write(idx) => idx,
        }
    }

    /// Was the event a read
    pub fn is_read(&self) -> bool {
        match *self {
            LastEvent::Read(_) => true,
            LastEvent::Write(_) => false,
        }
    }

    /// Was the event a write
    pub fn is_write(&self) -> bool {
        match *self {
            LastEvent::Read(_) => false,
            LastEvent::Write(_) => true,
        }
    }
}

/// State of `mioco` coroutine
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum State {
    BlockedOnWrite(Token),
    BlockedOnRead(Token),
    Select(SelectType),
    Running,
    Finished,
}

/// `mioco` can work on any type implementing this trait
pub trait ReadWrite : TryRead+TryWrite+std::io::Read+std::io::Write+Evented { }

impl<T> ReadWrite for T where T: TryRead+TryWrite+std::io::Read+std::io::Write+Evented {}

/// `mioco` coroutine
///
/// Referenced by IO running within it.
struct Coroutine {
    /// Coroutine of Coroutine itself. Stored here so it's available
    /// through every handle and `Coroutine` itself without referencing
    /// back
    coroutine : Option<coroutine::coroutine::Handle>,
    /// Current state
    state : State,
    /// Last event that resumed the coroutine
    last_event: LastEvent,
    /// All tokens associated with this cooroutine
    tokens : Vec<Token>,
}

/// Wrapped mio IO (Evented+TryRead+TryWrite)
///
/// `Handle` is just a cloneable reference to this struct
struct IO {
    coroutine: Rc<RefCell<Coroutine>>,
    token: Token,
    idx : usize, /// Index in CoroutineHandle::handles
    io : Box<ReadWrite+'static>,
    pending_read : bool,
    pending_write : bool,
    peer_hup: bool,
}


impl IO {
    /// Handle `hup` condition
    fn hup<H>(&mut self, event_loop: &mut EventLoop<H>, token: Token)
        where H : Handler {
            self.peer_hup = true;
            self.reregister(event_loop, token)
        }

    /// Reregister oneshot handler for the next event
    fn reregister<H>(&mut self, event_loop: &mut EventLoop<H>, token : Token)
        where H : Handler {

            if self.coroutine.borrow().state == State::Finished {
                return;
            }

            let mut interest =  mio::Interest::none();

            if !self.peer_hup {
                interest = interest | mio::Interest::hup()
            }

            if !self.pending_read {
                interest = interest | mio::Interest::readable()
            }
            if !self.pending_write {
                interest = interest | mio::Interest::writable()
            }

            event_loop.reregister(
                &*self.io, token,
                interest, mio::PollOpt::edge() | mio::PollOpt::oneshot()
                ).ok().expect("reregister failed")
        }
}

/// `mioco` wrapper over io associated with a given coroutine.
///
/// To be used to trigger events inside `mioco` coroutine. Create using
/// `Builder::io_wrap`.
///
/// It implements `readable` and `writable`, corresponding to original `mio::Handler`
/// methods. Call these from respective `mio::Handler`.
#[derive(Clone)]
pub struct ExternalHandle {
    inn : Rc<RefCell<IO>>,
}

/// `mioco` wrapper over io associated with a given coroutine.
///
/// Passed to closure function.
///
/// It implements standard library `Read` and `Write` traits that will
/// take care of blocking and unblocking coroutine when needed. 
#[derive(Clone)]
pub struct InternalHandle {
    inn : Rc<RefCell<IO>>,
}
impl ExternalHandle {

    /// Call for every token
    pub fn for_every_token<F>(&self, mut f : F)
        where F : FnMut(Token) {
            let co = &self.inn.borrow().coroutine;
            for &token in &co.borrow().tokens {
                f(token)
            }
        }


    /// Is this coroutine finishd
    pub fn is_finished(&self) -> bool {
        let co = &self.inn.borrow().coroutine;
        let co_b = co.borrow();
        co_b.state == State::Finished
    }

    /// Access the wrapped IO
    pub fn with_raw<F>(&self, f : F)
        where F : Fn(&ReadWrite) {
        let io = &self.inn.borrow().io;
        f(&**io)
    }

    /// Access the wrapped IO as mutable
    pub fn with_raw_mut<F>(&mut self, f : F)
        where F : Fn(&mut ReadWrite) {
        let mut io = &mut self.inn.borrow_mut().io;
        f(&mut **io)
    }

    /// Readable event handler
    ///
    /// This corresponds to `mio::Hnalder::readable()`.
    pub fn readable<H>(&mut self, event_loop: &mut EventLoop<H>, token: Token, hint: mio::ReadHint)
    where H : Handler {

        if hint.is_hup() {
            let mut inn = self.inn.borrow_mut();
            inn.hup(event_loop, token);
            return;
        }

        self.inn.borrow_mut().pending_read = true;

        let state = {
            let co = &self.inn.borrow().coroutine;
            let co_b = co.borrow();
            co_b.state
        };


        let resume = match state {
            State::Select(SelectType::Both) | State::Select(SelectType::Read) => true,
            State::BlockedOnRead(blocked_token) if token == blocked_token => true,
            _ => false,
        };

        if resume {
            let my_idx = self.inn.borrow().idx;
            let handle = {
                let inn = self.inn.borrow();
                let coroutine_handle = inn.coroutine.borrow().coroutine.as_ref().map(|c| c.clone()).unwrap();
                inn.coroutine.borrow_mut().state = State::Running;
                inn.coroutine.borrow_mut().last_event = LastEvent::Read(my_idx);
                coroutine_handle
            };
            handle.resume().ok().expect("resume() failed");
        }

        let mut inn = self.inn.borrow_mut();
        inn.reregister(event_loop, token);
    }

    /// Readable event handler
    ///
    /// This corresponds to `mio::Hnalder::writable()`.
    pub fn writable<H>(&mut self, event_loop: &mut EventLoop<H>, token: Token)
    where H : Handler {

        self.inn.borrow_mut().pending_write = true;

        let state = {
            let co = &self.inn.borrow().coroutine;
            let co_b = co.borrow();
            co_b.state
        };

        let resume = match state {
            State::BlockedOnWrite(blocked_token) if token == blocked_token => true,
            State::Select(SelectType::Both) | State::Select(SelectType::Write) => true,
            _ => false,
        };

        if resume {
            let my_idx = self.inn.borrow().idx;
            let handle = {
                let inn = self.inn.borrow();
                let coroutine_handle = inn.coroutine.borrow().coroutine.as_ref().map(|c| c.clone()).unwrap();
                inn.coroutine.borrow_mut().state = State::Running;
                inn.coroutine.borrow_mut().last_event = LastEvent::Write(my_idx);
                coroutine_handle
            };
            handle.resume().ok().expect("resume() failed");
        }

        let mut inn = self.inn.borrow_mut();
        inn.reregister(event_loop, token)
    }

    /// Deregister IO from event loop
    ///
    /// Must be called for every IO, after `is_finished` was detected
    pub fn deregister<H>(&mut self, event_loop: &mut EventLoop<H>)
    where H : Handler {
        event_loop.deregister(&*self.inn.borrow().io).expect("deregister failed")
    }
}

impl std::io::Read for InternalHandle {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let res = {
                let mut inn = self.inn.borrow_mut();

                if inn.peer_hup {
                    return Ok(0)
                }

                if inn.pending_read {
                    inn.pending_read = false;
                    inn.io.try_read(buf)
                } else {
                    Ok(None)
                }
            };

            match res {
                Ok(None) => {
                    {
                        let inn = self.inn.borrow();
                        inn.coroutine.borrow_mut().state = State::BlockedOnRead(inn.token);
                        debug_assert!(!inn.pending_read);
                    }
                    coroutine::Coroutine::block();
                    {
                        let inn = self.inn.borrow_mut();
                        debug_assert!(inn.pending_read);
                        debug_assert!(inn.coroutine.borrow().last_event.is_read());
                        debug_assert!(inn.coroutine.borrow().last_event.idx() == inn.idx);
                    }
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

impl std::io::Write for InternalHandle {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        loop {
            let res = {
                let mut inn = self.inn.borrow_mut();
                if inn.pending_write {
                    inn.pending_write = false;
                    inn.io.try_write(buf)
                } else {
                    Ok(None)
                }
            };

            match res {
                Ok(None) => {
                    {
                        let inn = self.inn.borrow();
                        inn.coroutine.borrow_mut().state = State::BlockedOnWrite(inn.token);
                        debug_assert!(!inn.pending_write);
                    }
                    coroutine::Coroutine::block();
                    {
                        let inn = self.inn.borrow_mut();
                        debug_assert!(inn.pending_write);
                        debug_assert!(inn.coroutine.borrow().last_event.is_write());
                        debug_assert!(inn.coroutine.borrow().last_event.idx() == inn.idx);
                    }
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

    /* TODO: Should we pass flush to TcpStream/ignore? */
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// `mioco` coroutine builder
///
/// Create one with `new`, then use `wrap_io` on io that you are going to use in the coroutine
/// that you spawn with `start`.
pub struct Builder {
    coroutine : Rc<RefCell<Coroutine>>,
    handles : Vec<InternalHandle>
}

struct RefCoroutine {
    coroutine: Rc<RefCell<Coroutine>>,
}
unsafe impl Send for RefCoroutine { }

struct HandleSender(Vec<InternalHandle>);

unsafe impl Send for HandleSender {}

impl Builder {

    /// Create new Coroutine builder
    pub fn new() -> Builder {
        Builder {
            coroutine: Rc::new(RefCell::new(Coroutine {
                state: State::Running,
                coroutine: None,
                last_event: LastEvent::Read(0),
                tokens: Vec::with_capacity(4),
            })),
            handles: Vec::with_capacity(4),
        }
    }

    /// Register `mio`'s io to be used within `mioco` coroutine
    ///
    /// Consumes the `io`, returns a `Handle` to a mio wrapper over it.
    pub fn wrap_io<H, T : 'static>(&mut self, event_loop: &mut mio::EventLoop<H>, io : T, token : Token) -> ExternalHandle
    where H : Handler,
    T : ReadWrite {

        event_loop.register_opt(
            &io, token,
            mio::Interest::readable() | mio::Interest::writable() | mio::Interest::hup(),
            mio::PollOpt::edge() | mio::PollOpt::oneshot(),
            ).expect("register_opt failed");

        self.coroutine.borrow_mut().tokens.push(token);

        let io = Rc::new(RefCell::new(
                     IO {
                         coroutine: self.coroutine.clone(),
                         io: Box::new(io),
                         token: token,
                         peer_hup: false,
                         idx: self.handles.len(),
                         pending_read: false,
                         pending_write: false,
                     }
                 ));

        let handle = ExternalHandle {
            inn: io.clone()
        };

        self.handles.push(InternalHandle {
            inn: io.clone()
        });

        handle
    }

    /// Create a `mioco` coroutine handler
    ///
    /// `f` is routine handling connection. It should not use any blocking operations,
    /// and use it's argument for all IO with it's peer
    pub fn start<F>(self, f : F)
        where F : FnOnce(&mut CoroutineHandle) + Send + 'static {

            let ioref = RefCoroutine {
                coroutine: self.coroutine.clone(),
            };

            let handles = HandleSender(self.handles);

            let coroutine_handle = coroutine::coroutine::Coroutine::spawn(move || {
                let HandleSender(handles) = handles;
                ioref.coroutine.borrow_mut().coroutine = Some(coroutine::Coroutine::current().clone());
                let mut co_handle = CoroutineHandle {
                    handles: handles,
                    coroutine: ioref.coroutine,
                };
                f(&mut co_handle);
                co_handle.coroutine.borrow_mut().state = State::Finished;
            });

            coroutine_handle.resume().ok().expect("resume() failed");
        }
}

/// Coroutine control
pub struct CoroutineHandle {
    handles : Vec<InternalHandle>,
    coroutine : Rc<RefCell<Coroutine>>,
}

impl CoroutineHandle {

    /// Wait till an event is ready
    pub fn select(&mut self) -> LastEvent {
        for ref handle in &self.handles {
            let inn = handle.inn.borrow();
            if inn.pending_read {
                return LastEvent::Read(inn.idx);
            }
            if inn.pending_write {
                return LastEvent::Write(inn.idx);
            }
        }

        self.coroutine.borrow_mut().state = State::Select(SelectType::Both);
        coroutine::Coroutine::block();
        debug_assert!(self.coroutine.borrow().state == State::Running);

        let idx = self.coroutine.borrow().last_event.idx();
        debug_assert!(self.handles[idx].inn.borrow().pending_read || self.handles[idx].inn.borrow().pending_write);
        self.coroutine.borrow().last_event
    }

    /// Wait till a read event is ready
    pub fn select_read(&mut self) -> LastEvent {
        for ref handle in &self.handles {
            let inn = handle.inn.borrow();
            if inn.pending_read {
                return LastEvent::Read(inn.idx);
            }
        }

        self.coroutine.borrow_mut().state = State::Select(SelectType::Read);
        coroutine::Coroutine::block();
        debug_assert!(self.coroutine.borrow().state == State::Running);

        let idx = self.coroutine.borrow().last_event.idx();
        debug_assert!(self.handles[idx].inn.borrow().pending_read);
        self.coroutine.borrow().last_event
    }

    /// Wait till a read event is ready
    pub fn select_write(&mut self) -> LastEvent {
        for ref handle in &self.handles {
            let inn = handle.inn.borrow();
            if inn.pending_write {
                return LastEvent::Write(inn.idx);
            }
        }

        self.coroutine.borrow_mut().state = State::Select(SelectType::Write);
        coroutine::Coroutine::block();
        debug_assert!(self.coroutine.borrow().state == State::Running);

        let idx = self.coroutine.borrow().last_event.idx();
        debug_assert!(self.handles[idx].inn.borrow().pending_write);
        self.coroutine.borrow().last_event
    }


    /// Reference to a handle of a given index
    pub fn handles(&mut self) -> &mut [InternalHandle] {
        &mut self.handles
    }
}
