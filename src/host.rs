//! The microtask seam.
//!
//! Upstream schedules a flush with `queueMicrotask(flush)` and lets the JS
//! event loop drive it. There is no event loop here, so the microtask queue is
//! ours to provide — and **this module is the one place a host drives it**.
//!
//! Two ways to get work done, and they compose:
//!
//! * [`crate::flush`] is the explicit synchronous drain. It does not depend on
//!   anything calling into this module: tests, and any caller that needs to
//!   *observe* the effect of a write, use it directly.
//! * [`drive_microtasks`] is what an integration calls once per turn of its own
//!   loop — after dispatching an event, at the end of a frame, or wherever the
//!   host considers the current task finished. That is the analogue of the JS
//!   microtask checkpoint, and it is the only reason writes made outside a
//!   `flush` ever settle on their own.
//!
//! A host that never calls [`drive_microtasks`] is not broken — it simply has
//! to call `flush()` itself. A host that calls it at a sensible checkpoint gets
//! upstream's batching behaviour: several writes in a turn collapse into one
//! update pass.
//!
//! ```
//! use solid_rs::{create_signal, create_effect, create_root, host};
//! use std::{cell::RefCell, rc::Rc};
//!
//! let seen = Rc::new(RefCell::new(Vec::new()));
//! let sink = seen.clone();
//! let (n, set_n) = create_signal(0);
//! create_root(move |_| {
//!     create_effect(move |_| n.get(), move |v: &i32, _| sink.borrow_mut().push(*v));
//! });
//! host::drive_microtasks();
//!
//! // Two writes in one host turn, one update pass.
//! set_n.set(1);
//! set_n.set(2);
//! host::drive_microtasks();
//! assert_eq!(*seen.borrow(), vec![0, 2]);
//! ```

use crate::runtime::with_rt;

/// Drain the pending microtask queue.
///
/// Call this at the end of a host task — after an event handler returns, at a
/// frame boundary, wherever the host's notion of "the current task is done"
/// falls. Tasks queued *by* a microtask run in the same drain, matching JS.
///
/// Returns the number of microtasks run, which is `0` when nothing was
/// scheduled.
pub fn drive_microtasks() -> usize {
    with_rt(|rt| {
        let mut ran = 0usize;
        loop {
            let next = rt.microtasks.borrow_mut().pop_front();
            match next {
                Some(task) => {
                    task();
                    ran += 1;
                }
                None => return ran,
            }
        }
    })
}

/// True when a flush is pending on the microtask queue.
pub fn has_pending_microtasks() -> bool {
    with_rt(|rt| !rt.microtasks.borrow().is_empty())
}
