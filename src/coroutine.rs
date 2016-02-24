use super::{Event, EventSourceId, RW, coroutine, token_to_ids, sender_retry};
use super::{CoroutineControl};
use super::thread::{TL_CURRENT_COROUTINE};
use super::thread::{HandlerShared, Message};
use super::thread::Handler;
use super::evented::{RcEventSourceTrait, RcEventSource, EventSourceTrait};
use super::thread::RcHandlerShared;
use super::sync::mpsc;
use super::mio::EventLoop;
use super::mio_orig::{Token, EventSet};

use context::{Context, Stack};
use slab;
use libc;

use std::any::Any;
use std::io;
use std::cell;
use std::sync::Arc;
use std::cell::RefCell;
use std::rc::Rc;
use std::boxed::FnBox;
use std::mem;
use std::panic;
use std::ptr;

/// Special Token used to signal scheduler timeout
/// See `SchedulerThread::timeout`
pub static SPECIAL_ID : coroutine::Id = coroutine::Id(0);
pub static SPECIAL_ID_SCHED_TIMEOUT : EventSourceId = EventSourceId(0);

/// Coroutines are number from this upwards
pub static STARTING_ID : coroutine::Id = coroutine::Id(1);


/// Id of a Coroutine used to enumerate them
///
/// It's unique within a thread
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Id(usize);

impl Id {
    pub fn new(id : usize) -> Self {
        Id(id)
    }

    pub fn as_usize(&self) -> usize {
        self.0
    }
}

impl slab::Index for Id {
    fn as_usize(&self) -> usize {
        self.0
    }
    fn from_usize(i: usize) -> Self {
        Id(i)
    }
}


/// Coroutine exit status (value returned or panic)
#[derive(Clone, Debug)]
pub enum ExitStatus {
    /// Coroutine panicked
    Panic,
    /// Killed externally
    Killed,
    /// Coroutine returned some value
    Exit(Arc<io::Result<()>>),
}

impl ExitStatus {
    /// Is the `ExitStatus` a `Panic`?
    #[allow(unused)]
    pub fn is_panic(&self) -> bool {
        match *self {
            ExitStatus::Panic => true,
            _ => false,
        }
    }
}

/// State of `mioco` coroutine
#[derive(Clone, Debug)]
pub enum State {
    /// Blocked on EventSource(s)
    Blocked,
    /// Yielding
    Yielding,
    /// Currently running
    Running,
    /// Ready to be started
    Ready,
    /// Finished
    Finished(ExitStatus),
}

impl State {
    /// Is the `State` `Ready`?
    pub fn is_ready(&self) -> bool {
        match *self {
            State::Ready => true,
            _ => false,
        }
    }

    /// Is the `State` `Running`?
    pub fn is_running(&self) -> bool {
        match *self {
            State::Running => true,
            _ => false,
        }
    }

    /// Is the `State` `Blocked`?
    pub fn is_blocked(&self) -> bool {
        match *self {
            State::Blocked => true,
            _ => false,
        }
    }

    /// Is the `State` `Yielding`?
    pub fn is_yielding(&self) -> bool {
        match *self {
            State::Yielding => true,
            _ => false,
        }
    }
}

pub type RcCoroutine = Rc<RefCell<Coroutine>>;


/// Mioco coroutine (a.k.a. *mioco handler*)
// TODO: Make everything private
pub struct Coroutine {
    /// Context with a state of coroutine
    context: Context,

    /// Current state
    pub state: State,

    /// Last event that resumed the coroutine
    pub last_event: Event,

    /// `Handler` shared data that this `Coroutine` is running in
    handler_shared: Option<RcHandlerShared>,

    /// `Coroutine` will send exit status on it's finish
    /// through this list of Mailboxes
    pub exit_notificators: Vec<mpsc::Sender<ExitStatus>>,

    /// Current coroutine Id
    pub id: Id,

    /// Coroutine stack
    stack: Stack,

    /// All event sources the coroutine is blocked on
    pub blocked_on: slab::Slab<Box<RcEventSourceTrait + 'static>, EventSourceId>,

    /// Newly spawned `Coroutine`-es
    pub children_to_start: Vec<RcCoroutine>,

    /// Function to be run inside Coroutine
    coroutine_func: Option<Box<FnBox() -> io::Result<()> + Send + 'static>>,

    /// In case Rc to self is needed
    pub self_rc: Option<RcCoroutine>,

    pub sync_channel: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,

    /// Userdata of the coroutine
    pub user_data: Option<Arc<Box<Any + Send + Sync>>>,

    /// Userdata meant for inheritance
    pub inherited_user_data: Option<Arc<Box<Any + Send + Sync>>>,

    /// if this coroutine should catch panics
    catch_panics: bool,
}

impl Coroutine {
    /// Spawn a new Coroutine
    pub fn spawn<F>(handler_shared: RcHandlerShared,
                inherited_user_data: Option<Arc<Box<Any + Send + Sync>>>,
                f: F,
                catch_panics: bool)
                -> RcCoroutine
        where F: FnOnce() -> io::Result<()> + Send + 'static
    {
        trace!("Coroutine: spawning");
        let stack_size = handler_shared.borrow().stack_size;

        let id = {
            let coroutines = &mut handler_shared.borrow_mut().coroutines;

            if !coroutines.has_remaining() {
                let count = coroutines.count();
                coroutines.grow(count);
            }

            coroutines.insert_with(|id| {
                          let coroutine = Coroutine {
                              state: State::Ready,
                              id: id,
                              last_event: Event {
                                  rw: RW::read(),
                                  id: EventSourceId(0),
                              },
                              context: Context::empty(),
                              handler_shared: Some(handler_shared.clone()),
                              exit_notificators: Vec::new(),
                              blocked_on: slab::Slab::new(4),
                              children_to_start: Vec::new(),
                              stack: Stack::new(stack_size).unwrap(),
                              coroutine_func: Some(Box::new(f)),
                              self_rc: None,
                              sync_channel: None,
                              user_data: inherited_user_data.clone(),
                              inherited_user_data: inherited_user_data,
                              catch_panics: catch_panics,
                          };

                          CoroutineSlabHandle::new(Rc::new(RefCell::new(coroutine)))
                      })
                      .expect("Run out of slab for coroutines")
        };
        handler_shared.borrow_mut().coroutines_inc();

        let coroutine_rc = handler_shared.borrow().coroutines[id].rc.clone();

        coroutine_rc.borrow_mut().self_rc = Some(coroutine_rc.clone());

        let coroutine_ptr = {
            // The things we do for borrowck...
            let coroutine_ptr = {
                &*coroutine_rc.borrow() as *const Coroutine
            };
            coroutine_ptr
        };

        extern "C" fn init_fn(arg: usize, _: *mut libc::types::common::c95::c_void) -> ! {
            let ctx: &Context = {

                //never panic inside init_fn, that causes a SIGILL
                let res = panic::recover(move || {
                    let coroutine: &mut Coroutine = unsafe { mem::transmute(arg) };
                    trace!("Coroutine({}): started", {
                        coroutine.id.as_usize()
                    });

                    entry_point(coroutine.self_rc.as_ref().unwrap());
                    let f = coroutine.coroutine_func.take().unwrap();

                    f.call_box(())
                });

                let coroutine: &mut Coroutine = unsafe { mem::transmute(arg) };
                coroutine.blocked_on.clear();
                coroutine.self_rc = None;

                let id = coroutine.id;
                {
                    let mut handler_shared = coroutine.handler_shared
                                                      .as_ref()
                                                      .unwrap()
                                                      .borrow_mut();
                    handler_shared.coroutines.remove(id).unwrap();
                    handler_shared.coroutines_dec();
                }

                match res {
                    Ok(res) => {
                        trace!("Coroutine({}): finished returning {:?}", id.as_usize(), res);
                        let arc_res = Arc::new(res);
                        coroutine.exit_notificators
                                 .iter()
                                 .map(|end| end.send(ExitStatus::Exit(arc_res.clone())))
                                 .count();
                        coroutine.state = State::Finished(ExitStatus::Exit(arc_res));

                    }
                    Err(cause) => {
                        if coroutine.catch_panics {
                            trace!("Coroutine({}): panicked: {:?}",
                                   id.as_usize(),
                                   cause.downcast::<&str>());
                            if let State::Finished(ExitStatus::Killed) = coroutine.state {
                                coroutine.exit_notificators
                                         .iter()
                                         .map(|end| end.send(ExitStatus::Killed))
                                         .count();
                            } else {
                                coroutine.state = State::Finished(ExitStatus::Panic);
                                coroutine.exit_notificators
                                         .iter()
                                         .map(|end| end.send(ExitStatus::Panic))
                                         .count();
                            }
                        } else {
                            //send fail here instead with the internal reason, so the user may get a nice backtrace
                            let handler = coroutine.handler_shared.as_ref().unwrap().borrow();
                            sender_retry(&handler.get_sender_to_own_thread(), Message::PropagatePanic(cause));
                        }
                    }
                }

                unsafe {
                    let handler = coroutine.handler_shared.as_ref().unwrap().borrow();
                    mem::transmute(&handler.context as *const Context)
                }
            };

            Context::load(ctx);
        }

        {
            let Coroutine {
                ref mut stack,
                ref mut context,
                ..
            } = *coroutine_rc.borrow_mut();

            context.init_with(init_fn, coroutine_ptr as usize, ptr::null_mut(), stack);
        }

        coroutine_rc
    }

    pub fn spawn_child<F>(&mut self, f: F) -> RcCoroutine
        where F: FnOnce() -> io::Result<()> + Send + 'static {
            let child = Coroutine::spawn(
                self.handler_shared.as_ref().unwrap().clone(),
                self.inherited_user_data.clone(),
                f,
                self.catch_panics);
            self.children_to_start.push(child.clone());
            child
        }

    pub fn handler_shared(&self) -> cell::Ref<HandlerShared> {
        self.handler_shared.as_ref().unwrap().borrow()
    }

    pub fn handler_shared_mut(&self) -> cell::RefMut<HandlerShared> {
        self.handler_shared.as_ref().unwrap().borrow_mut()
    }

    pub fn block_on<T>(&mut self, event_source : &RcEventSource<T>, rw: RW)
        where T: EventSourceTrait + 'static
    {
        trace!("Coroutine({}): blocked on {:?}", self.id.as_usize(), rw);
        self.state = coroutine::State::Blocked;
        self.blocked_on.insert_with(|id| {
            event_source.common_mut().id = Some(id);
            event_source.common_mut().blocked_on = rw;
            event_source.to_trait()
        });
    }

    pub fn unblock(&mut self, event_loop: &mut EventLoop<Handler>, event : Event) {
        self.state = coroutine::State::Ready;
        self.last_event = event;

        self.deregister_all(event_loop);
    }
    pub fn finish(&mut self) {
        self.state = coroutine::State::Finished(coroutine::ExitStatus::Killed)
    }

    pub fn unblock_after_yield(&mut self) {
        self.state = coroutine::State::Ready;
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    // TODO: Make priv.
    pub fn deregister_all(&mut self, event_loop: &mut EventLoop<Handler>) {
        for io in self.blocked_on.iter_mut() {
            io.deregister(event_loop, self.id);
        }
        self.blocked_on.clear();
    }

    // TODO: Make priv.
    pub fn register_all(&mut self, event_loop: &mut EventLoop<Handler>) {
        for io in self.blocked_on.iter_mut() {
            io.register(event_loop, self.id);
        }
    }

    pub fn start_children(&mut self) {
        let Coroutine {
            ref mut children_to_start,
            ref handler_shared,
            ref id,
            ..
        } = *self;

        trace!("Coroutine({}): {} children spawned",
        id.as_usize(),
        children_to_start.len());

        let mut handler_shared = handler_shared.as_ref().unwrap().borrow_mut();

        for child in children_to_start.drain(..) {
            handler_shared.add_spawned(CoroutineControl::new(child));
        }
    }

    pub fn detach_from(&mut self, event_loop : &mut EventLoop<Handler>) -> RcHandlerShared {
        trace!("Coroutine({}): dettached from thread", self.id.as_usize());
        self.deregister_all(event_loop);

        let handler_shared = self.handler_shared.take();
        debug_assert!(self.handler_shared.is_none());

        handler_shared.unwrap()
    }

    pub fn attach_to(&mut self, event_loop : &mut EventLoop<Handler>, handler_shared : RcHandlerShared, id : Id) {
        trace!("Coroutine({}): attached to thread", self.id.as_usize());
        self.id = id;
        self.handler_shared = Some(handler_shared);

        self.register_all(event_loop);
    }
}

/// Event delivery point, kept in Handler slab.
#[derive(Clone)]
pub struct CoroutineSlabHandle {
    rc: RcCoroutine,
}

impl CoroutineSlabHandle {
    pub fn new(rc: RcCoroutine) -> Self {
        CoroutineSlabHandle { rc: rc }
    }

    pub fn to_coroutine_control(self) -> CoroutineControl {
        CoroutineControl::new(self.rc)
    }

    /// Deliver an event to a Coroutine
    pub fn event(&self, event_loop: &mut EventLoop<Handler>, token: Token, events: EventSet) -> bool {

        let (co_id, io_id) = token_to_ids(token);

        trace!("Coroutine({}): event", self.id().as_usize());

        if !self.rc.borrow().state().is_blocked() {
            // subsequent event to coroutine that is either already
            // Finished, or Ready
            return false;
        }

        trace!("Coroutine({}): set to ready", co_id.as_usize());
        // Wake coroutine on HUP, as it was read, to potentially let it fail the read and move on
        let event = match (events.is_readable() | events.is_hup(), events.is_writable()) {
            (true, true) => RW::both(),
            (true, false) => RW::read(),
            (false, true) => RW::write(),
            (false, false) => panic!(),
        };

        let mut co = self.rc.borrow_mut();
        co.unblock(event_loop,
                   Event {
                       rw: event,
                       id: io_id,
                   });
        return true
    }

    pub fn id(&self) -> coroutine::Id {
        let coroutine = self.rc.borrow();
        coroutine.id
    }
}

/// Coroutine entry point checks
// TODO: Make part of the Coroutine, using rc_self
pub fn entry_point(coroutine: &RefCell<Coroutine>) {
    if let State::Finished(ExitStatus::Killed) = coroutine.borrow().state {
        panic!("Killed externally")
    }
}

/// Resume coroutine execution, jumping into it
// TODO: Make part of the Coroutine, using rc_self
pub fn jump_in(coroutine: &RefCell<Coroutine>) {
    let prev = TL_CURRENT_COROUTINE.with(|co| {
        let mut co = co.borrow_mut();
        let prev = *co;
        *co = &mut *coroutine.borrow_mut() as *mut Coroutine;
        prev
    });

    {
        let ref mut state = coroutine.borrow_mut().state;
        match *state {
            State::Ready => {
                *state = State::Running;
            }
            State::Finished(ExitStatus::Killed) => {}
            ref state => panic!("coroutine_jump_in: wrong state {:?}", state),
        }
    }

    // We know that we're holding at least one Rc to the Coroutine,
    // and noone else is holding a reference as we can do `.borrow_mut()`
    // so we cheat with unsafe just to context-switch from coroutine
    // without having RefCells still borrowed.
    let (context_in, context_out) = {
        let Coroutine {
            ref context,
            ref handler_shared,
            ..
        } = *coroutine.borrow_mut();
        {
            let mut shared_context = &mut handler_shared.as_ref().unwrap().borrow_mut().context;
            (context as *const Context, shared_context as *mut Context)
        }
    };

    Context::swap(unsafe { &mut *context_out }, unsafe { &*context_in });
    TL_CURRENT_COROUTINE.with(|co| {
        *co.borrow_mut() = prev;
    });
}

/// Block coroutine execution, jumping out of it
// TODO: Make part of the Coroutine, using rc_self
pub fn jump_out(coroutine: &RefCell<Coroutine>) {
    {
        let state = &coroutine.borrow().state;
        debug_assert!(state.is_blocked() || state.is_yielding());
    }

    // See `resume()` for unsafe comment
    let (context_in, context_out) = {
        let Coroutine {
            ref mut context,
            ref handler_shared,
            ..
        } = *coroutine.borrow_mut();
        {
            let shared_context = &mut handler_shared.as_ref().unwrap().borrow_mut().context;
            (context as *mut Context, shared_context as *const Context)
        }
    };

    Context::swap(unsafe { &mut *context_in }, unsafe { &*context_out });
}
