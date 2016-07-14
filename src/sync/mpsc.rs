use super::super::RW;
use super::super::thread::{Handler, Message};
use super::super::evented::{EventSourceTrait, RcEventSource, Evented, EventedImpl};
use mio_orig::{EventLoop, Token, EventSet};
use std::sync::Arc;
use spin::Mutex;
use super::super::thread::MioSender;
use super::super::{sender_retry, in_coroutine};

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
type ArcChannelShared = Arc<Mutex<ChannelShared>>;
type ArcCounter = Arc<AtomicUsize>;

/// State shared between senders and receiver.
struct ChannelShared {
    token: Option<Token>,
    sender: Option<MioSender>,
}

/// Channel receiving end
///
/// Create with `channel()`
pub struct Receiver<T>(RcEventSource<ReceiverCore<T>>);

struct ReceiverCore<T> {
    receiver: mpsc::Receiver<T>,
    shared: ArcChannelShared,
    counter: ArcCounter,
}

impl<T> EventedImpl for Receiver<T>
    where T: 'static
{
    type Raw = ReceiverCore<T>;

    fn shared(&self) -> &RcEventSource<ReceiverCore<T>> {
        &self.0
    }
}

impl<T> EventSourceTrait for ReceiverCore<T> {
    fn register(&mut self, event_loop: &mut EventLoop<Handler>, token: Token, interest: EventSet) -> bool {
        debug_assert!(interest.is_readable());
        trace!("Receiver({}): register", token.as_usize());
        let mut lock = self.shared.lock();

        lock.token = Some(token);
        lock.sender = Some(event_loop.channel());

        if self.counter.load(Ordering::SeqCst) != 0 {
            trace!("Receiver({}): not empty; self notify", token.as_usize());
            lock.token = None;
            true
        } else {
            false
        }
    }

    fn reregister(&mut self,
                  _event_loop: &mut EventLoop<Handler>,
                  token: Token,
                  interest: EventSet) -> bool {
        debug_assert!(interest.is_readable());
        trace!("Receiver({}): reregister", token.as_usize());
        let mut lock = self.shared.lock();

        debug_assert!(lock.token.is_some());
        debug_assert!(lock.sender.is_some());

        if self.counter.load(Ordering::SeqCst) != 0 {
            lock.token = None;
            true
        } else {
            false
        }
    }

    fn deregister(&mut self, _event_loop: &mut EventLoop<Handler>, token: Token) {
        trace!("Receiver({}): dereregister", token.as_usize());
        let mut lock = self.shared.lock();
        lock.token = None;
        lock.sender = None;
    }
}

impl<T> Receiver<T> {
    fn new(shared: ArcChannelShared, counter: ArcCounter, receiver: mpsc::Receiver<T>) -> Self {
        let core = ReceiverCore {
            shared: shared,
            counter: counter,
            receiver: receiver,
        };

        Receiver(RcEventSource::new(core))
    }
}

impl<T> Receiver<T>
    where T: 'static
{
    /// Receive `T` sent using corresponding `Sender::send()`.
    ///
    /// Will block coroutine if no elements are available.
    pub fn recv(&self) -> Result<T, mpsc::RecvError> {
        if in_coroutine() {
            loop {
                let recv = self.try_recv();

                match recv {
                    Err(mpsc::TryRecvError::Empty) => self.block_on(RW::read()),
                    Err(mpsc::TryRecvError::Disconnected) => return Err(mpsc::RecvError),
                    Ok(t) => return Ok(t),
                }
                debug_assert!(self.empty_shared_token());
            }
        } else {
            self.shared().io_ref().receiver.recv()
        }
    }

    /// Try reading data from the queue.
    ///
    /// This will not block.
    pub fn try_recv(&self) -> Result<T, mpsc::TryRecvError> {
        let shared = self.shared();
        let io_ref = shared.io_ref();
        let res = io_ref.receiver.try_recv();
        if res.is_ok() {
            let _prev_counter = self.shared().io_ref().counter.fetch_sub(1, Ordering::SeqCst);
            // since send() first sends, then increases the counter, it is
            // possible for _prev_counter to be zero
        }
        res
    }

    fn empty_shared_token(&self) -> bool {
        let shared = self.shared();
        let io_ref = shared.io_ref();
        let lock = io_ref.shared.lock();
        lock.token.is_none()
    }
}

struct SenderShared {
    shared: ArcChannelShared,
    counter: ArcCounter,
}

impl Drop for SenderShared {
    fn drop(&mut self) {
        let prev_counter = self.counter.fetch_add(1, Ordering::SeqCst);
        if prev_counter == 0 {
            maybe_notify_receiver(&self.shared);
        }
    }
}

/// Channel sending end
///
/// Use this inside mioco coroutines or outside of mioco itself to send data
/// asynchronously to the receiving end.
///
/// Create with `channel()`
pub struct Sender<T> {
    shared: Arc<SenderShared>,
    sender: mpsc::Sender<T>,
}

pub struct SyncSender<T> {
    shared: Arc<SenderShared>,
    sender: mpsc::SyncSender<T>,
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Sender {
            shared: self.shared.clone(),
            sender: self.sender.clone(),
        }
    }
}

impl<T> Sender<T> {
    fn new(shared: ArcChannelShared, counter: ArcCounter, sender: mpsc::Sender<T>) -> Self {
        Sender {
            shared: Arc::new(SenderShared {
                shared: shared,
                counter: counter,
            }),
            sender: sender,
        }
    }
}

impl<T> SyncSender<T> {
    fn new(shared: ArcChannelShared, counter: ArcCounter, sender: mpsc::SyncSender<T>) -> Self {
        SyncSender {
            shared: Arc::new(SenderShared {
                shared: shared,
                counter: counter,
            }),
            sender: sender,
        }
    }

    /// Deliver `T` to the other end of the channel.
    ///
    /// Channel behaves like a queue.
    ///
    /// This is non-blocking operation.
    pub fn send(&self, t: T) -> Result<(), mpsc::SendError<T>> {
        try!(self.sender.send(t));
        let prev_counter = self.shared.counter.fetch_add(1, Ordering::SeqCst);

        if prev_counter == 0 {
            maybe_notify_receiver(&self.shared.shared);
        }
        Ok(())
    }
}

fn maybe_notify_receiver(shared: &ArcChannelShared) {
    let mut lock = shared.lock();
    let ChannelShared { ref mut sender, ref mut token } = *lock;

    if let Some(token) = *token {
        trace!("Sender: notifying {:?}", token);
        let sender = sender.as_ref().unwrap();
        sender_retry(&sender, Message::ChannelMsg(token))
    }
}

impl<T> Sender<T> {
    /// Deliver `T` to the other end of the channel.
    ///
    /// Channel behaves like a queue.
    ///
    /// This is non-blocking operation.
    pub fn send(&self, t: T) -> Result<(), mpsc::SendError<T>> {
        try!(self.sender.send(t));
        let prev_counter = self.shared.counter.fetch_add(1, Ordering::SeqCst);

        if prev_counter == 0 {
            maybe_notify_receiver(&self.shared.shared);
        }
        Ok(())
    }
}

/// Create a channel
///
/// Channel can be used to deliver data via MPSC queue.
///
/// Channel is modeled after `std::sync::mpsc::channel()`, only
/// supporting mioco-aware sending and receiving.
///
/// When receiving end is outside of coroutine, channel will behave just
/// like `std::sync::mpsc::channel()`.
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let shared = ChannelShared {
        token: None,
        sender: None,
    };

    let shared = Arc::new(Mutex::new(shared));

    let counter = Arc::new(AtomicUsize::new(0));
    let (sender, receiver) = mpsc::channel();
    (Sender::new(shared.clone(), counter.clone(), sender), Receiver::new(shared, counter, receiver))
}


/// Create a channel
///
/// Channel can be used to deliver data via MPSC queue.
///
/// Channel is modeled after `std::sync::mpsc::channel()`, only
/// supporting mioco-aware sending and receiving.
///
/// When receiving end is outside of coroutine, channel will behave just
/// like `std::sync::mpsc::channel()`.
pub fn sync_channel<T>(bound: usize) -> (SyncSender<T>, Receiver<T>) {
    let shared = ChannelShared {
        token: None,
        sender: None,
    };

    let shared = Arc::new(Mutex::new(shared));

    let counter = Arc::new(AtomicUsize::new(0));
    let (sender, receiver) = mpsc::sync_channel(bound);
    (SyncSender::new(shared.clone(), counter.clone(), sender), Receiver::new(shared, counter, receiver))
}


unsafe impl<T : Send> Send for Receiver<T> {}
unsafe impl<T : Send> Send for Sender<T> {}
unsafe impl<T : Send> Send for SyncSender<T> {}
