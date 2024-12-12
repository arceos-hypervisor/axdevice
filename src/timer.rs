extern crate alloc;

use alloc::boxed::Box;
use timer_list::{TimeValue, TimerEvent, TimerList};

use core::sync::atomic::AtomicUsize;
use core::sync::atomic::Ordering;

static TOKEN: AtomicUsize = AtomicUsize::new(0);

struct VmmTimerEvent {
    token: usize,
    timer_callback: Box<dyn FnOnce(TimeValue) + Send + 'static>,
}

impl VmmTimerEvent {
    fn new<F>(token: usize, f: F) -> Self
    where
        F: FnOnce(TimeValue) + Send + 'static,
    {
        Self {
            token: token,
            timer_callback: Box::new(f),
        }
    }
}

impl TimerEvent for VmmTimerEvent {
    fn callback(self, now: TimeValue) {
        (self.timer_callback)(now)
    }
}

pub struct AxVmTimer {
    timer_list: TimerList<VmmTimerEvent>,
}

impl AxVmTimer {
    /// Constructs a new instance of `VmmTimer` with an empty timer list.
    pub fn new() -> Self {
        Self {
            timer_list: TimerList::new(),
        }
    }

    /// Registers a new timer that will execute at the specified deadline
    ///
    /// # Arguments
    /// - `deadline`: The absolute time in nanoseconds when the timer should trigger
    /// - `handler`: The callback function to execute when the timer expires
    ///
    /// # Returns
    /// A unique token that can be used to cancel this timer later
    pub fn register_timer<F>(&mut self, deadline: u64, handler: F) -> usize
    where
        F: FnOnce(TimeValue) + Send + 'static,
    {
        let token = TOKEN.fetch_add(1, Ordering::Release);
        let event = VmmTimerEvent::new(token, handler);
        self.timer_list
            .set(TimeValue::from_nanos(deadline as u64), event);
        token
    }

    /// Cancels a timer with the specified token.
    ///
    /// # Parameters
    /// - `token`: The unique token of the timer to cancel.
    pub fn cancel_timer(&mut self, token: usize) {
        self.timer_list.cancel(|event| event.token == token);
    }

    /// Expires one timer based on the current time.
    ///
    /// # Parameters
    /// - `now`: The current time as a `TimeValue` used to determine which timer should expire.
    ///
    /// # Returns
    /// An `Option` containing a tuple of `(TimeValue, VmmTimerEvent)` if a timer expired,
    /// or `None` if no timers are expired.
    pub fn expire_one(&mut self, now: TimeValue) -> Option<(TimeValue, VmmTimerEvent)> {
        self.timer_list.expire_one(now)
    }

    /// Checks for any expired timers and executes their callbacks if they have expired.
    ///
    /// # Parameters
    /// - `now`: The current time as a `TimeValue` used to determine which timers should be checked.
    ///
    /// # Returns
    /// `true` if an event was handled (i.e., a timer expired and its callback was executed),
    /// or `false` if no timers were expired.
    pub fn check_event(&mut self, now: TimeValue) -> bool {
        let event = self.timer_list.expire_one(now);
        if let Some((_deadline, event)) = event {
            trace!("pick one {:#?} to handler!!!", _deadline);
            event.callback(now);
            true
        } else {
            false
        }
    }
}
