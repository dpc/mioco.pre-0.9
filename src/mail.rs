use super::{RW, Handler};
use super::evented::{EventSourceTrait, RcEventSource, Evented, EventedImpl};
use super::mio_orig::{EventLoop, Token, EventSet};
use std::sync::Arc;
use spin::Mutex;
use super::MioSender;
use super::Message;
use std::collections::VecDeque;
use super::sender_retry;

type MailboxQueue<T> = Option<T>;
type ArcMailboxShared<T> = Arc<Mutex<MailboxShared<T>>>;

struct MailboxShared<T> {
    token: Option<Token>,
    sender: Option<MioSender>,
    inn: VecDeque<T>,
    interest: EventSet,
}

/// Mailbox receiving end
///
/// Use this only inside mioco coroutines, as an asynchronous event source.
///
/// Create with `mailbox()`
pub struct MailboxInnerEnd<T>(RcEventSource<MailboxInnerCore<T>>);

struct MailboxInnerCore<T>(ArcMailboxShared<T>);

impl<T> EventedImpl for MailboxInnerEnd<T> where T: 'static
{
    type Raw = MailboxInnerCore<T>;

    fn shared(&self) -> &RcEventSource<MailboxInnerCore<T>> {
        &self.0
    }
}

impl<T> EventSourceTrait for MailboxInnerCore<T> {
    fn register(&self, event_loop: &mut EventLoop<Handler>, token: Token, interest: EventSet) {
        trace!("MailboxInnerEnd({}): register", token.as_usize());
        let mut lock = self.0.lock();

        lock.token = Some(token);
        lock.sender = Some(event_loop.channel());
        lock.interest = interest;

        if interest.is_readable() && !lock.inn.is_empty() {
            trace!("MailboxInnerEnd({}): not empty; self notify",
                   token.as_usize());
            lock.interest = EventSet::none();
            sender_retry(lock.sender.as_ref().unwrap(), Message::MailboxMsg(token));
        }
    }

    fn reregister(&self, _event_loop: &mut EventLoop<Handler>, token: Token, interest: EventSet) {
        trace!("MailboxInnerEnd({}): reregister", token.as_usize());
        let mut lock = self.0.lock();

        lock.interest = interest;

        if interest.is_readable() && !lock.inn.is_empty() {
            lock.interest = EventSet::none();
            sender_retry(lock.sender.as_ref().unwrap(), Message::MailboxMsg(token));
        }
    }

    fn deregister(&self, _event_loop: &mut EventLoop<Handler>, token: Token) {
        trace!("MailboxInnerEnd({}): dereregister", token.as_usize());
        let mut lock = self.0.lock();
        lock.token = None;
        lock.sender = None;
        lock.interest = EventSet::none();
    }

    fn should_resume(&self) -> bool {
        let lock = self.0.lock();
        trace!("MailboxInnerEnd: should_resume? {}", !lock.inn.is_empty());
        !lock.inn.is_empty()
    }
}


/// Mailbox sending end
///
/// Use this inside mioco coroutines or outside of mioco itself to send data
/// asynchronously to the receiving end.
///
/// Create with `mailbox()`
pub struct MailboxOuterEnd<T> {
    shared: ArcMailboxShared<T>,
}

impl<T> Clone for MailboxOuterEnd<T> {
    fn clone(&self) -> Self {
        MailboxOuterEnd { shared: self.shared.clone() }
    }
}

impl<T> MailboxOuterEnd<T> {
    fn new(shared: ArcMailboxShared<T>) -> Self {
        MailboxOuterEnd { shared: shared }
    }
}

impl<T> MailboxInnerEnd<T> {
    fn new(shared: ArcMailboxShared<T>) -> Self {
        MailboxInnerEnd(RcEventSource::new(MailboxInnerCore(shared)))
    }
}

impl<T> MailboxOuterEnd<T> {
    /// Deliver `T` to the other end of the mailbox.
    ///
    /// Mailbox behaves like a queue.
    ///
    /// This is non-blocking operation.
    pub fn send(&self, t: T) {
        let mut lock = self.shared.lock();
        let MailboxShared {
            ref mut sender,
            ref mut token,
            ref mut inn,
            ref mut interest,
        } = *lock;

        inn.push_back(t);
        debug_assert!(!inn.is_empty());
        trace!("MailboxOuterEnd: putting message in a queue; new len: {}",
               inn.len());

        if interest.is_readable() {
            let token = token.unwrap();
            trace!("MailboxOuterEnd: notifying {:?}", token);
            let sender = sender.as_ref().unwrap();
            sender_retry(&sender, Message::MailboxMsg(token))
        }
    }
}

impl<T> MailboxInnerEnd<T> where T: 'static
{
    /// Receive `T` sent using corresponding `MailboxOuterEnd::send()`.
    ///
    /// Will block coroutine if no elements are available.
    pub fn read(&self) -> T {
        loop {
            if let Some(t) = self.try_read() {
                return t;
            }

            self.block_on(RW::read())
        }
    }

    /// Try reading data from the queue.
    ///
    /// This will not block.
    pub fn try_read(&self) -> Option<T> {
        let shared = self.shared();
        let io_ref = shared.io_ref();
        let shared = &io_ref.0;
        let mut lock = shared.lock();

        lock.inn.pop_front()
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

    (MailboxOuterEnd::new(shared.clone()),
     MailboxInnerEnd::new(shared))
}


unsafe impl<T> Send for MailboxInnerEnd<T> {}
