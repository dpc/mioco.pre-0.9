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

/// State of `mioco` coroutine
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum State {
    BlockedOnWrite(Token),
    BlockedOnRead(Token),
    Running,
    Finished,
}

impl State {
    /// What is the `mio::Interest` for a given `token` at the current state
    fn to_interest_for(&self, token : mio::Token) -> mio::Interest {
        match *self {
            State::Running => panic!("wrong state"),
            State::BlockedOnRead(blocked_token) => if token == blocked_token {
                mio::Interest::readable()
            }
            else {
                mio::Interest::none()
            },
            State::BlockedOnWrite(blocked_token) => if token == blocked_token {
                mio::Interest::writable()
            } else {
                mio::Interest::none()
            },
            State::Finished => mio::Interest::hup(),
        }
    }
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
    pub state : State,
    coroutine : Option<coroutine::coroutine::Handle>,
}


/// Wrapped mio IO (Evented+TryRead+TryWrite)
///
/// `Handle` is just a cloneable reference to this struct
struct IO<T>
where T : ReadWrite {
    coroutine: Rc<RefCell<Coroutine>>,
    token: Token,
    io : T,
    interest: mio::Interest,
    peer_hup: bool,
}

impl<T> IO<T>
where T : ReadWrite {

    /// Handle `hup` condition
    fn hup<H>(&mut self, event_loop: &mut EventLoop<H>, token: Token)
        where H : Handler {
            if self.interest == mio::Interest::hup() {
                self.interest = mio::Interest::none();
                event_loop.deregister(&self.io).ok().expect("deregister() failed");
            } else {
                self.peer_hup = true;
                self.reregister(event_loop, token)
            }
        }

    /// Reregister oneshot handler for the next event
    fn reregister<H>(&mut self, event_loop: &mut EventLoop<H>, token : Token)
        where H : Handler {

            self.interest = self.coroutine.borrow().state.to_interest_for(token) ;

            event_loop.reregister(
                &self.io, token,
                self.interest, mio::PollOpt::edge() | mio::PollOpt::oneshot()
                ).ok().expect("reregister failed")
        }
}

/* BUG: We can guarantee that only one coroutine is running at the time so no concurrent accesses
 * are possible, but user can clone Handle and give to different threads. Boo. */
unsafe impl<T> Send for Handle<T>
where T : ReadWrite { }

// Same as above?
unsafe impl Send for RefCoroutine { }

/// `mioco` wrapper over io associated with a given coroutine.
///
/// Create using `Builder`.
///
/// It implements standard library `Read` and `Write` traits that will
/// take care of blocking and unblocking coroutine when needed. Use this
/// from within `mioco` coroutine only, otherwise misbehaviour is ensured.
///
/// It implements `readable` and `writable`, modeled like original `mio::Handler`
/// methods. Call these from respective `mio::Handler`.
pub struct Handle<T>
where T : ReadWrite {
    inn : Rc<RefCell<IO<T>>>,
}


impl<T> Clone for Handle<T>
where T : ReadWrite {
    fn clone(&self) -> Handle<T> {
        Handle {
            inn: self.inn.clone()
        }
    }
}

impl<T> Handle<T>
where T : ReadWrite {

    /// Is this IO finished and free to be removed
    /// as no more events will be reported for it
    pub fn is_finished(&self) -> bool {
        let co = &self.inn.borrow().coroutine;
        let co_b = co.borrow();
        co_b.state == State::Finished && self.inn.borrow().interest == mio::Interest::none()
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


        let state = {
            let co = &self.inn.borrow().coroutine;
            let co_b = co.borrow();
            co_b.state
        };

        if let State::BlockedOnRead(blocked_token) = state {
            if token == blocked_token {
                let handle = {
                    let inn = self.inn.borrow();
                    let coroutine_handle = inn.coroutine.borrow().coroutine.as_ref().map(|c| c.clone()).unwrap();
                    inn.coroutine.borrow_mut().state = State::Running;
                    coroutine_handle
                };
                handle.resume().ok().expect("resume() failed");
            }

            let mut inn = self.inn.borrow_mut();
            inn.reregister(event_loop, token)
        } else if let State::BlockedOnWrite(blocked_token) = state {
            if token == blocked_token {
                let mut inn = self.inn.borrow_mut();
                inn.reregister(event_loop, token)
            }
        }

    }

    /// Readable event handler
    ///
    /// This corresponds to `mio::Hnalder::writable()`.
    pub fn writable<H>(&mut self, event_loop: &mut EventLoop<H>, token: Token)
    where H : Handler {

        let state = {
            let co = &self.inn.borrow().coroutine;
            let co_b = co.borrow();
            co_b.state
        };

        if let State::BlockedOnWrite(blocked_token) = state {
            if token == blocked_token {
                let handle = {
                    let inn = self.inn.borrow();
                    let coroutine_handle = inn.coroutine.borrow().coroutine.as_ref().map(|c| c.clone()).unwrap();
                    inn.coroutine.borrow_mut().state = State::Running;
                    coroutine_handle
                };
                handle.resume().ok().expect("resume() failed");

                let mut inn = self.inn.borrow_mut();
                inn.reregister(event_loop, token)
            }

        } else if let State::BlockedOnRead(blocked_token) = state {
            if token == blocked_token {
                let mut inn = self.inn.borrow_mut();
                inn.reregister(event_loop, token)
            }
        }
    }
}

impl<T> std::io::Read for Handle<T>
where T : ReadWrite {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let res = self.inn.borrow_mut().io.try_read(buf);
            match res {
                Ok(None) => {
                    {
                        let inn = self.inn.borrow();
                        inn.coroutine.borrow_mut().state = State::BlockedOnRead(inn.token);
                    }
                    coroutine::Coroutine::block();
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

impl<T> std::io::Write for Handle<T>
where T: ReadWrite {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        loop {
            let res = self.inn.borrow_mut().io.try_write(buf) ;
            match res {
                Ok(None) => {
                    {
                        let inn = self.inn.borrow();
                        inn.coroutine.borrow_mut().state = State::BlockedOnWrite(inn.token);
                    }
                    coroutine::Coroutine::block();
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
    coroutine: Rc<RefCell<Coroutine>>,
}

struct RefCoroutine {
    coroutine: Rc<RefCell<Coroutine>>,
}

impl Builder {

    /// Create new Coroutine builder
    pub fn new() -> Builder {
        Builder {
            coroutine: Rc::new(RefCell::new(Coroutine {
                state: State::Running,
                coroutine: None,
            }))
        }
    }

    /// Register `mio`'s io to be used within `mioco` coroutine
    ///
    /// Consumes the `io`, returns a `Handle` to a mio wrapper over it.
    pub fn wrap_io<H, T>(&self, event_loop: &mut mio::EventLoop<H>, io : T, token : Token) -> Handle<T>
    where H : Handler,
    T : ReadWrite {

        event_loop.register_opt(
            &io, token,
            mio::Interest::readable() | mio::Interest::writable(), mio::PollOpt::edge() | mio::PollOpt::oneshot()
            ).expect("register_opt failed");

        Handle {
            inn: Rc::new(RefCell::new(
                     IO {
                         coroutine: self.coroutine.clone(),
                         io: io,
                         token: token,
                         peer_hup: false,
                         interest: mio::Interest::none(),
                     }
                 )),
        }
    }

    /// Create a `mioco` coroutine handler
    ///
    /// `f` is routine handling connection. It should not use any blocking operations,
    /// and use it's argument for all IO with it's peer
    pub fn start<F>(self, f : F)
        where F : FnOnce() + Send + 'static {

            let ioref = RefCoroutine {
                coroutine: self.coroutine.clone(),
            };

            let coroutine_handle = coroutine::coroutine::Coroutine::spawn(move || {
                ioref.coroutine.borrow_mut().coroutine = Some(coroutine::Coroutine::current().clone());
                f();
                ioref.coroutine.borrow_mut().state = State::Finished;
            });

            coroutine_handle.resume().ok().expect("resume() failed");
        }
}
