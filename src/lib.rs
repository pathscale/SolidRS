//! # solid_rs
//!
//! A port of the **synchronous core** of [`@solidjs/signals`] 2.0.0-rc.0 to
//! Rust, pinned to upstream commit
//! `15975306e524f197e2231ba6bd2c259c0dc39362`.
//!
//! Semantics come from the TypeScript, not from Rust reactive libraries. Where
//! the TypeScript shape does not survive the translation, the *shape* changed
//! and the semantics did not — every such decision is recorded in
//! `porting-reply.md` (PORTING-QUESTIONS.md holds only the still-open items),
//! and the scope boundary is in `README.md`.
//!
//! ## The model in four sentences
//!
//! A **signal** holds a value. A **memo** derives one, recomputing only when a
//! tracked dependency actually changes. An **effect** splits into a tracked
//! compute phase and an untracked apply phase, so reads in the apply phase do
//! not feed back. Writes **stage** and commit at the next flush, which makes a
//! tick — not a statement — the unit of truth for values.
//!
//! ```
//! use solid_rs::{create_signal, create_memo, create_effect, create_root, flush};
//! use std::{cell::RefCell, rc::Rc};
//!
//! let log = Rc::new(RefCell::new(Vec::new()));
//! let sink = log.clone();
//!
//! let (count, set_count) = create_signal(0);
//! let doubled = create_memo(move |_| count.get() * 2);
//!
//! create_root(move |_dispose| {
//!     create_effect(move |_| doubled.get(), move |v: &i32, _| sink.borrow_mut().push(*v));
//! });
//! flush();
//!
//! // A write stages; the old value is still current until the tick commits.
//! set_count.set(5);
//! assert_eq!(count.get(), 0);
//! flush();
//! assert_eq!(count.get(), 5);
//! assert_eq!(*log.borrow(), vec![0, 10]);
//! ```
//!
//! ## Single-threaded by construction
//!
//! Nothing here is `Send` or `Sync`, and nothing locks. The runtime is
//! thread-local; a graph belongs to the thread that created it.
//!
//! ## Driving updates
//!
//! There is no JS event loop, so [`flush`] is the explicit drain, and
//! [`host::drive_microtasks`] is the single place an integration pumps the
//! microtask queue. See the [`host`] module.
//!
//! [`@solidjs/signals`]: https://github.com/solidjs/solid/tree/next/packages/solid-signals

#![deny(rust_2018_idioms)]
#![warn(missing_debug_implementations)]

mod async_source;
mod asyncrt;
mod constants;
mod context;
mod core;
mod effect;
mod error;
mod graph;
mod heap;
mod invariants;
mod map;
mod node;
mod owner;
mod runtime;
mod scheduler;
mod signals;
mod status;
mod verdict;

pub mod host;

pub use crate::async_source::{Async, Flight, Producer, Resolver, Streaming};
pub use crate::verdict::{is_pending, latest};
pub use crate::context::{
    create_context, create_context_with_default, get_context, set_context, Context,
};
pub use crate::core::{run_with_owner, untrack};
pub use crate::error::{
    ContextError, ContextNotFoundError, NoOwnerError, ReactiveError, UserError,
};
pub use crate::node::{Disposable, NodeId};
pub use crate::owner::{get_next_child_id, peek_next_child_id, IdError};
pub use crate::scheduler::{flush, flush_with, reset_error_halt};
pub use crate::map::{
    map_array, map_array_by_index, map_array_by_index_with, map_array_keyed,
    map_array_keyed_with, map_array_with, repeat, repeat_with, MapOptions, RepeatOptions,
};
pub use crate::signals::debug;
pub use crate::signals::{
    create_async_memo, create_async_memo_with, create_computed_signal, create_computed_signal_with, create_effect, create_effect_with,
    create_memo, create_memo_with, create_reaction, create_render_effect,
    create_render_effect_with, create_root, create_root_with, create_signal, create_signal_with,
    get_observer, get_owner, live_node_count, on_cleanup, on_dispose, try_create_effect,
    try_create_memo_with, Dispose, EffectOptions, Equals, IntoCleanup, Memo, Options, Owner,
    ReadSignal, WriteSignal,
};
