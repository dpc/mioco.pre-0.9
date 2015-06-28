// Copyright 2015 Dawid Ciężarkiewicz <dpc@dpc.pw>
// See LICENSE-MPL2 file for more information.

//! Coroutine-based handler library for mio
//!
//! Using coroutines, an event-based mio model can be simplified to a set of routines, seamlessly
//! scheduled on demand in userspace.
//!
//! Using `mioco` a single input consuming and single output producing routine can be used to
//! handle each connection.

extern crate mio;
extern crate coroutine;
extern crate nix;

use std::cell::RefCell;
use std::sync::Arc;
use mio::{TryRead, TryWrite, Evented, Token, Handler, EventLoop};

#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq)]
enum State {
    BlockedOnWrite(Token),
    BlockedOnRead(Token),
    Running,
    Finished,
}

impl State {
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

pub trait ReadWrite : TryRead+TryWrite+std::io::Read+std::io::Write+Evented { }

impl<T> ReadWrite for T where T: TryRead+TryWrite+std::io::Read+std::io::Write+Evented {}

/// Coroutine of Coroutine IO
struct Coroutine {
    /// Coroutine of Coroutine itself. Stored here so it's available
    /// through every handle and `Coroutine` itself without referencing
    /// back
    pub state : State,
    coroutine : Option<coroutine::coroutine::Handle>,
}

pub struct Handle<T>
where T : ReadWrite {
    io : Arc<RefCell<Coroutine>>, // Can we implement using Rc on the same premisse as Send for Handle ?
    token: Token,
    stream : T, // TODO: Convert to &ReadWrite or even ReadWrite as generic type if possible
    interest: mio::Interest,
    peer_hup: bool,
}

impl<T> Handle<T>
where T : ReadWrite {

    fn hup<H>(&mut self, event_loop: &mut EventLoop<H>, token: Token)
        where H : Handler {
            if self.interest == mio::Interest::hup() {
                self.interest = mio::Interest::none();
                event_loop.deregister(&self.stream).ok().expect("deregister() failed");
            } else {
                self.peer_hup = true;
                self.reregister(event_loop, token)
            }
        }

    fn reregister<H>(&mut self, event_loop: &mut EventLoop<H>, token : Token)
        where H : Handler {

            self.interest = self.io.borrow().state.to_interest_for(token) ;

            event_loop.reregister(
                &self.stream, token,
                self.interest, mio::PollOpt::edge() | mio::PollOpt::oneshot()
                ).ok().expect("reregister failed")
        }
}

/* XXX: TODO: Is this OK? It seems we can guarantee that only 
 * one coroutine is running at the time so no concurent
 * accesses are possible. But is it enough? 
 *
 * Not OK. User can clone HandleRef and give to different threads. Boo. */
unsafe impl<T> Send for HandleRef<T>
where T : ReadWrite { }


/// IO Handler reference passed to routine running inside `mioco` `Coroutine`.
///
/// It implements standard library `Read` and `Write` traits that will
/// take care of blocking and unblocking coroutine when needed.
///
/// Create using `Builder`.
pub struct HandleRef<T>
where T : ReadWrite {
    inn : Arc<RefCell<Handle<T>>>,
}


impl<T> Clone for HandleRef<T>
where T : ReadWrite {
    fn clone(&self) -> HandleRef<T> {
        HandleRef {
            inn: self.inn.clone()
        }
    }
}

impl<T> HandleRef<T>
where T : ReadWrite {

    pub fn is_finished(&self) -> bool {
        let io = &self.inn.borrow().io;
        let io_b = io.borrow();
        io_b.state == State::Finished && self.inn.borrow().interest == mio::Interest::none()
    }

    /// Readable event handler
    ///
    /// This is based on `mio`'s `readable` method in `Handler` trait.
    pub fn readable<H>(&mut self, event_loop: &mut EventLoop<H>, token: Token, hint: mio::ReadHint)
    where H : Handler {

        if hint.is_hup() {
            let mut inn = self.inn.borrow_mut();
            inn.hup(event_loop, token);
            return;
        }


        let state = {
            let io = &self.inn.borrow().io;
            let io_b = io.borrow();
            io_b.state
        };

        if let State::BlockedOnRead(blocked_token) = state {
            if token == blocked_token {
                let handle = {
                    let inn = self.inn.borrow();
                    let coroutine_handle = inn.io.borrow().coroutine.as_ref().map(|c| c.clone()).unwrap();
                    inn.io.borrow_mut().state = State::Running;
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
    /// This is based on `mio`'s `writable` method in `Handler` trait.
    pub fn writable<H>(&mut self, event_loop: &mut EventLoop<H>, token: Token)
    where H : Handler {

        let state = {
            let io = &self.inn.borrow().io;
            let io_b = io.borrow();
            io_b.state
        };

        if let State::BlockedOnWrite(blocked_token) = state {
            if token == blocked_token {
                let handle = {
                    let inn = self.inn.borrow();
                    let coroutine_handle = inn.io.borrow().coroutine.as_ref().map(|c| c.clone()).unwrap();
                    inn.io.borrow_mut().state = State::Running;
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

impl<T> std::io::Read for HandleRef<T>
where T : ReadWrite {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let res = self.inn.borrow_mut().stream.try_read(buf);
            match res {
                Ok(None) => {
                    {
                        let inn = self.inn.borrow();
                        inn.io.borrow_mut().state = State::BlockedOnRead(inn.token);
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

impl<T> std::io::Write for HandleRef<T>
where T: ReadWrite {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        loop {
            let res = self.inn.borrow_mut().stream.try_write(buf) ;
            match res {
                Ok(None) => {
                    {
                        let inn = self.inn.borrow();
                        inn.io.borrow_mut().state = State::BlockedOnWrite(inn.token);
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



// Mioco coroutine builder
//
// Create one with `new`, then use `wrap_io` that you going to use
// in the `Coroutine` that you spawn with `start`.
pub struct Builder {
    io: Arc<RefCell<Coroutine>>,
}

struct RefCoroutine {
    io: Arc<RefCell<Coroutine>>,
}

// Needs to figure this out
unsafe impl Send for RefCoroutine { }

impl Builder {

    /// Create a new Coroutine builder
    pub fn new() -> Builder {
        Builder {
            io: Arc::new(RefCell::new(Coroutine {
                state: State::Running,
                coroutine: None,
            }))
        }
    }

    pub fn wrap_io<H, T>(&self, event_loop: &mut mio::EventLoop<H>, stream : T, token : Token) -> HandleRef<T>
    where H : Handler,
    T : ReadWrite {

        event_loop.register_opt(
            &stream, token,
            mio::Interest::readable() | mio::Interest::writable(), mio::PollOpt::edge() | mio::PollOpt::oneshot()
            ).ok().expect("register_opt failed");

        HandleRef {
            inn: Arc::new(RefCell::new(
                     Handle {
                         io: self.io.clone(),
                         stream: stream,
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
                io: self.io.clone(),
            };

            let coroutine_handle = coroutine::coroutine::Coroutine::spawn(move || {
                ioref.io.borrow_mut().coroutine = Some(coroutine::Coroutine::current().clone());
                f();
                ioref.io.borrow_mut().state = State::Finished;
            });


            coroutine_handle.resume().ok().expect("resume() failed");
        }
}

