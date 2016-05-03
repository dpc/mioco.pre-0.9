use super::RW;
use super::thread::Handler;
use super::evented::{EventSourceTrait, RcEventSource, Evented, EventedImpl};
use mio_orig::{self, EventLoop, Token, EventSet};
use time::{SteadyTime, Duration};

/// A Timer generating event after a given time
///
/// Use `MiocoHandle::select()` to wait for an event, or `read()` to block until
/// done.
///
/// Note the timer effective resolution is limited by underlying mio timer.
/// See `mioco::sleep()` for details.
pub struct Timer {
    rc: RcEventSource<TimerCore>,
}

struct TimerCore {
    // TODO: Rename these two?
    timeout: SteadyTime,
    mio_timeout: Option<mio_orig::Timeout>,
}

impl Timer {
    /// Create a new timer
    pub fn new() -> Timer {
        let timer_core = TimerCore {
            timeout: SteadyTime::now(),
            mio_timeout: None,
        };
        Timer { rc: RcEventSource::new(timer_core) }
    }

    fn is_done(&self) -> bool {
        self.rc.io_ref().should_resume()
    }
}


impl Default for Timer {
    fn default() -> Self { Timer::new() }
}

impl EventedImpl for Timer {
    type Raw = TimerCore;

    fn shared(&self) -> &RcEventSource<TimerCore> {
        &self.rc
    }
}

impl Timer {
    /// Read a timer to block on it until it is done.
    ///
    /// Returns current time
    ///
    /// TODO: Return wakeup time instead
    pub fn read(&mut self) -> SteadyTime {
        loop {
            if let Some(t) = self.try_read() {
                return t;
            }

            self.block_on(RW::read());
        }
    }

    /// Try reading current time (if the timer is done)
    ///
    /// TODO: Return wakeup time instead
    pub fn try_read(&mut self) -> Option<SteadyTime> {
        let done = self.is_done();

        if done {
            Some(SteadyTime::now())
        } else {
            None
        }
    }

    /// Set timeout for the timer
    ///
    /// The timeout counts from the time `set_timeout` is called.
    ///
    /// Note the timer effective resolution is limited by underlying mio
    /// timer. See `mioco::sleep()` for details.
    pub fn set_timeout(&mut self, delay_ms: i64) {
        self.rc.io_mut().timeout = SteadyTime::now() + Duration::milliseconds(delay_ms);
    }

    /// Set timeout for the timer using absolute time.
    pub fn set_timeout_absolute(&mut self, timeout: SteadyTime) {
        self.rc.io_mut().timeout = timeout;
    }


    /// Get absolute value of the timer timeout.
    pub fn get_timeout_absolute(&mut self) -> SteadyTime {
        self.rc.io_ref().timeout
    }
}

impl TimerCore {
    fn should_resume(&self) -> bool {
        trace!("Timer: should_resume? {}",
               self.timeout <= SteadyTime::now());
        self.timeout <= SteadyTime::now()
    }
}

impl EventSourceTrait for TimerCore {
    fn register(&mut self, event_loop: &mut EventLoop<Handler>, token: Token, _interest: EventSet) {
        let timeout = self.timeout;
        let now = SteadyTime::now();
        let delay = if timeout <= now {
            0
        } else {
            (timeout - now).num_milliseconds()
        };

        trace!("Timer({}): set timeout in {}ms", token.as_usize(), delay);
        match event_loop.timeout_ms(token, delay as u64) {
            Ok(timeout) => {
                self.mio_timeout = Some(timeout);
            }
            Err(reason) => {
                panic!("Could not create mio::Timeout: {:?}", reason);
            }
        }
    }

    fn reregister(&mut self, event_loop: &mut EventLoop<Handler>, token: Token, interest: EventSet) {
        event_loop.clear_timeout(self.mio_timeout.unwrap());
        self.register(event_loop, token, interest)
    }

    fn deregister(&mut self, event_loop: &mut EventLoop<Handler>, _token: Token) {
        event_loop.clear_timeout(self.mio_timeout.unwrap());
    }
}

unsafe impl Send for Timer {}
