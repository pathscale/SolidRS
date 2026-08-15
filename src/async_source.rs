//! The `Promise` / `AsyncIterable` replacement.
//!
//! # The problem
//!
//! Upstream's async layer is built on two JS platform types and one JS platform
//! guarantee:
//!
//! * a `PromiseLike` whose `.then` may fire **synchronously** (upstream depends
//!   on this — see `tests/syncThenable.test.ts` and the `isSync` latches in
//!   `handleAsync`),
//! * an `AsyncIterable` yielding a sequence of values,
//! * the event loop draining microtasks between turns.
//!
//! Rust has none of these, and the brief rules out bringing an async runtime in.
//! It does not need one: nothing here *executes* anything. Upstream never polls
//! a future — it registers a continuation and lets the platform call it. The
//! only thing a runtime would provide is the call, and phase 1 already ported
//! the queue that makes calls: [`crate::host::drive_microtasks`].
//!
//! # The model
//!
//! A [`Flight<T>`] is a one-shot in-flight value; a [`Streaming<T>`] is a
//! sequence of them. Both are *resolved from outside* — by an IO completion
//! callback, a timer, a test. Resolving posts the continuation onto the
//! microtask queue, so the settle lands on the next checkpoint exactly as a
//! resolved promise's `.then` lands on the next microtask tick.
//!
//! That makes the JS-to-Rust test translation mechanical:
//!
//! | upstream | here |
//! |---|---|
//! | `new Promise(res => { resolve = res })` | `Flight::new()` → `(flight, resolver)` |
//! | `Promise.resolve(v)` | `Flight::ready(v)` (a sync-resolved thenable) |
//! | `resolve(v)` | `resolver.resolve(v)` |
//! | `await Promise.resolve()` | `host::drive_microtasks()` |
//! | `async function*` | `Streaming::new()` → `(stream, producer)` |
//!
//! # Single-threaded, by construction
//!
//! Nothing here is `Send`. A resolver belongs to the thread that created it;
//! resolving is a plain call that posts to that thread's microtask queue. An
//! async computation suspends the *graph*, it does not move work anywhere.

use std::cell::RefCell;
use std::rc::Rc;

use crate::error::ReactiveError;
use crate::runtime::with_rt;

/// What an in-flight value has settled to.
enum FlightState<T> {
    Pending,
    Resolved(T),
    Rejected(ReactiveError),
    /// Delivered — the continuation already ran, or the flight was superseded.
    Consumed,
}

type Continuation<T> = Box<dyn FnOnce(Result<T, ReactiveError>)>;

struct FlightInner<T> {
    state: RefCell<FlightState<T>>,
    /// Registered by `handle_async` when the flight is still pending at the
    /// time the computation returns it.
    on_settle: RefCell<Option<Continuation<T>>>,
}

/// A one-shot in-flight value — the port of a `Promise`.
///
/// Return one from a computation (wrapped in [`Async::Flight`]) to suspend the
/// node. A read of a suspended node throws `NotReadyError` into the graph,
/// which is what a `<Loading>` boundary catches.
pub struct Flight<T: 'static> {
    inner: Rc<FlightInner<T>>,
}

impl<T: 'static> Clone for Flight<T> {
    fn clone(&self) -> Self {
        Flight {
            inner: self.inner.clone(),
        }
    }
}

impl<T: 'static> std::fmt::Debug for Flight<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match &*self.inner.state.borrow() {
            FlightState::Pending => "Pending",
            FlightState::Resolved(_) => "Resolved",
            FlightState::Rejected(_) => "Rejected",
            FlightState::Consumed => "Consumed",
        };
        f.debug_struct("Flight").field("state", &s).finish()
    }
}

/// Settles a [`Flight`]. Hold it in an IO callback, a timer, or a test.
pub struct Resolver<T: 'static> {
    inner: Rc<FlightInner<T>>,
}

impl<T: 'static> Clone for Resolver<T> {
    fn clone(&self) -> Self {
        Resolver {
            inner: self.inner.clone(),
        }
    }
}

impl<T: 'static> std::fmt::Debug for Resolver<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Resolver")
    }
}

impl<T: 'static> Flight<T> {
    /// A flight that has not settled yet, and the handle that settles it.
    pub fn new() -> (Flight<T>, Resolver<T>) {
        let inner = Rc::new(FlightInner {
            state: RefCell::new(FlightState::Pending),
            on_settle: RefCell::new(None),
        });
        (
            Flight {
                inner: inner.clone(),
            },
            Resolver { inner },
        )
    }

    /// A flight that has *already* settled — the port of `Promise.resolve(v)`,
    /// and of any thenable whose `.then` fires synchronously.
    ///
    /// The distinction matters: upstream serves such a value immediately
    /// without ever suspending, opening a transition, or notifying a boundary.
    pub fn ready(value: T) -> Flight<T> {
        Flight {
            inner: Rc::new(FlightInner {
                state: RefCell::new(FlightState::Resolved(value)),
                on_settle: RefCell::new(None),
            }),
        }
    }

    /// A flight that has already failed — `Promise.reject(e)`.
    pub fn failed(error: ReactiveError) -> Flight<T> {
        Flight {
            inner: Rc::new(FlightInner {
                state: RefCell::new(FlightState::Rejected(error)),
                on_settle: RefCell::new(None),
            }),
        }
    }

    /// Identity, so a settle can tell whether it is still the node's current
    /// flight. Port of upstream's `el._inFlight !== result` guard, which is how
    /// a superseded fetch's late arrival is dropped.
    pub(crate) fn id(&self) -> usize {
        Rc::as_ptr(&self.inner) as *const u8 as usize
    }

    /// Take the settled value if the flight is already resolved at the moment
    /// the computation returns it.
    pub(crate) fn take_settled(&self) -> Option<Result<T, ReactiveError>> {
        let mut state = self.inner.state.borrow_mut();
        match std::mem::replace(&mut *state, FlightState::Consumed) {
            FlightState::Resolved(v) => Some(Ok(v)),
            FlightState::Rejected(e) => Some(Err(e)),
            other => {
                *state = other;
                None
            }
        }
    }

    /// Register the continuation the settle will run.
    pub(crate) fn on_settle(&self, f: Continuation<T>) {
        *self.inner.on_settle.borrow_mut() = Some(f);
    }
}

impl<T: 'static> Resolver<T> {
    /// Settle the flight with a value.
    ///
    /// The continuation is posted to the microtask queue rather than run
    /// inline, matching a promise's `.then`: the settle lands on the next
    /// checkpoint, so a resolver called from inside a flush cannot re-enter it.
    pub fn resolve(&self, value: T) {
        self.settle(Ok(value));
    }

    /// Settle the flight with an error — `reject`.
    pub fn reject(&self, error: ReactiveError) {
        self.settle(Err(error));
    }

    fn settle(&self, outcome: Result<T, ReactiveError>) {
        {
            let mut state = self.inner.state.borrow_mut();
            if !matches!(*state, FlightState::Pending) {
                // Already settled: a promise settles once.
                return;
            }
            *state = FlightState::Consumed;
        }
        let cont = self.inner.on_settle.borrow_mut().take();
        match cont {
            Some(f) => with_rt(|rt| {
                rt.microtasks.borrow_mut().push_back(Box::new(move || {
                    f(outcome);
                }));
            }),
            None => {
                // Settled before the graph registered interest (or after the
                // node was superseded). Stash it so a later `take_settled`
                // still sees it, mirroring a resolved promise handed to `.then`
                // after the fact.
                let mut state = self.inner.state.borrow_mut();
                *state = match outcome {
                    Ok(v) => FlightState::Resolved(v),
                    Err(e) => FlightState::Rejected(e),
                };
            }
        }
    }

    /// True once this flight has settled.
    pub fn is_settled(&self) -> bool {
        !matches!(*self.inner.state.borrow(), FlightState::Pending)
    }
}

// ----------------------------------------------------------------- streaming

struct StreamInner<T> {
    /// Values produced but not yet pulled.
    buffered: RefCell<std::collections::VecDeque<Result<T, ReactiveError>>>,
    done: std::cell::Cell<bool>,
    /// The graph's pull continuation, registered when it awaits the next step.
    ///
    /// `Rc`, not `Box`: the callback can close the stream from inside itself
    /// (the autodispose release does exactly that), so it is cloned out and
    /// invoked with no borrow held.
    on_step: RefCell<Option<StepCallback<T>>>,
    /// Set once the consumer stops caring (node disposed / superseded), so the
    /// producer can stop. Port of the iterator's `return()` on cleanup.
    closed: std::cell::Cell<bool>,
}

/// The graph's registered pull continuation for a stream.
type StepCallback<T> = Rc<dyn Fn(StreamStep<T>)>;

/// One step of a stream: a yielded value, an error, or completion.
pub(crate) enum StreamStep<T> {
    Yield(T),
    Error(ReactiveError),
    Done,
}

/// A sequence of values — the port of an `AsyncIterable`.
///
/// Each pushed value is one `yield`. Unlike a [`Flight`], a stream keeps the
/// node alive across landings: the first yield closes the uninitialized window,
/// later yields are revealed snapshots.
pub struct Streaming<T: 'static> {
    inner: Rc<StreamInner<T>>,
}

impl<T: 'static> Clone for Streaming<T> {
    fn clone(&self) -> Self {
        Streaming {
            inner: self.inner.clone(),
        }
    }
}

impl<T: 'static> std::fmt::Debug for Streaming<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Streaming")
    }
}

/// Pushes values into a [`Streaming`].
pub struct Producer<T: 'static> {
    inner: Rc<StreamInner<T>>,
}

impl<T: 'static> Clone for Producer<T> {
    fn clone(&self) -> Self {
        Producer {
            inner: self.inner.clone(),
        }
    }
}

impl<T: 'static> std::fmt::Debug for Producer<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Producer")
    }
}

impl<T: 'static> Streaming<T> {
    pub fn new() -> (Streaming<T>, Producer<T>) {
        let inner = Rc::new(StreamInner {
            buffered: RefCell::new(Default::default()),
            done: std::cell::Cell::new(false),
            on_step: RefCell::new(None),
            closed: std::cell::Cell::new(false),
        });
        (
            Streaming {
                inner: inner.clone(),
            },
            Producer { inner },
        )
    }

    pub(crate) fn id(&self) -> usize {
        Rc::as_ptr(&self.inner) as *const u8 as usize
    }

    /// Take an already-buffered step, if the producer ran ahead of the graph.
    ///
    /// Upstream calls this the "bare `IteratorResult`" fast path: a real
    /// producer with a value already buffered returns it without a promise, and
    /// `for await` assimilates it as an already-settled step.
    pub(crate) fn take_buffered(&self) -> Option<StreamStep<T>> {
        if let Some(step) = self.inner.buffered.borrow_mut().pop_front() {
            return Some(match step {
                Ok(v) => StreamStep::Yield(v),
                Err(e) => StreamStep::Error(e),
            });
        }
        if self.inner.done.get() {
            return Some(StreamStep::Done);
        }
        None
    }

    pub(crate) fn on_step(&self, f: Box<dyn Fn(StreamStep<T>)>) {
        *self.inner.on_step.borrow_mut() = Some(Rc::from(f));
    }

    /// Stop pulling. Port of the iterator's `return()` in the cleanup hook.
    pub(crate) fn close(&self) {
        self.inner.closed.set(true);
        self.inner.on_step.borrow_mut().take();
    }
}

impl<T: 'static> Producer<T> {
    /// Emit one value — one `yield`.
    pub fn push(&self, value: T) {
        self.deliver(Ok(value));
    }

    /// Fail the stream.
    pub fn fail(&self, error: ReactiveError) {
        self.deliver(Err(error));
    }

    /// Complete the stream.
    pub fn finish(&self) {
        if self.inner.closed.get() || self.inner.done.get() {
            return;
        }
        self.inner.done.set(true);
        let has_consumer = self.inner.on_step.borrow().is_some();
        if has_consumer {
            let inner = self.inner.clone();
            with_rt(|rt| {
                rt.microtasks.borrow_mut().push_back(Box::new(move || {
                    let cb = inner.on_step.borrow().clone();
                    if let Some(cb) = cb {
                        cb(StreamStep::Done);
                    }
                }));
            });
        }
    }

    /// True once the consumer has stopped pulling — the node was disposed or
    /// its computation superseded. A producer should stop here.
    pub fn is_closed(&self) -> bool {
        self.inner.closed.get()
    }

    fn deliver(&self, step: Result<T, ReactiveError>) {
        if self.inner.closed.get() || self.inner.done.get() {
            return;
        }
        let has_consumer = self.inner.on_step.borrow().is_some();
        if !has_consumer {
            self.inner.buffered.borrow_mut().push_back(step);
            return;
        }
        let inner = self.inner.clone();
        with_rt(|rt| {
            rt.microtasks.borrow_mut().push_back(Box::new(move || {
                if inner.closed.get() {
                    return;
                }
                // Cloned out: the callback may close the stream, which takes
                // `on_step` mutably.
                let cb = inner.on_step.borrow().clone();
                if let Some(cb) = cb {
                    cb(match step {
                        Ok(v) => StreamStep::Yield(v),
                        Err(e) => StreamStep::Error(e),
                    });
                }
            }));
        });
    }
}

impl<T: 'static> crate::node::ErasedFlight for Flight<T> {
    fn flight_id(&self) -> usize {
        self.id()
    }
    fn take_settled(&self) -> Option<Result<crate::node::AnyValue, ReactiveError>> {
        self.take_settled()
            .map(|r| r.map(|v| Rc::new(v) as crate::node::AnyValue))
    }
    fn on_settle(&self, f: Box<dyn FnOnce(Result<crate::node::AnyValue, ReactiveError>)>) {
        Flight::on_settle(
            self,
            Box::new(move |r| f(r.map(|v| Rc::new(v) as crate::node::AnyValue))),
        );
    }
}

impl<T: 'static> crate::node::ErasedStream for Streaming<T> {
    fn stream_id(&self) -> usize {
        self.id()
    }
    fn take_buffered(&self) -> Option<crate::node::ErasedStep> {
        self.take_buffered().map(|s| match s {
            StreamStep::Yield(v) => crate::node::ErasedStep::Yield(Rc::new(v)),
            StreamStep::Error(e) => crate::node::ErasedStep::Error(e),
            StreamStep::Done => crate::node::ErasedStep::Done,
        })
    }
    fn on_step(&self, f: Box<dyn Fn(crate::node::ErasedStep)>) {
        Streaming::on_step(
            self,
            Box::new(move |s| {
                f(match s {
                    StreamStep::Yield(v) => crate::node::ErasedStep::Yield(Rc::new(v)),
                    StreamStep::Error(e) => crate::node::ErasedStep::Error(e),
                    StreamStep::Done => crate::node::ErasedStep::Done,
                })
            }),
        );
    }
    fn close(&self) {
        Streaming::close(self);
    }
}

/// What an async-capable computation returns.
///
/// The three arms are upstream's three runtime shapes, made explicit. Upstream
/// discovers them by probing the returned object (`Symbol.asyncIterator`, then
/// `.then`); Rust names them, which also means a `sync: true` node returning an
/// async shape is a type error rather than upstream's
/// `SYNC_NODE_RECEIVED_ASYNC` runtime diagnostic.
pub enum Async<T: 'static> {
    /// Already available. Port of a non-thenable return, and of a
    /// synchronously-resolved thenable: no suspension, no transition.
    Ready(T),
    /// In flight.
    Flight(Flight<T>),
    /// A sequence of landings.
    Stream(Streaming<T>),
}

impl<T: 'static> std::fmt::Debug for Async<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Async::Ready(_) => f.write_str("Async::Ready(..)"),
            Async::Flight(_) => f.write_str("Async::Flight(..)"),
            Async::Stream(_) => f.write_str("Async::Stream(..)"),
        }
    }
}

impl<T: 'static> From<Flight<T>> for Async<T> {
    fn from(f: Flight<T>) -> Self {
        Async::Flight(f)
    }
}

impl<T: 'static> From<Streaming<T>> for Async<T> {
    fn from(s: Streaming<T>) -> Self {
        Async::Stream(s)
    }
}
