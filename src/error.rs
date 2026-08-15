//! Port of `src/core/error.ts`.
//!
//! Upstream throws these; the graph then compares them **by identity**
//! (`sub._error !== error`, `node._error === error`). Rust has no exception
//! channel to reuse, so an error is a value that flows on the `Result` return
//! of a computation and is stored on the node. Identity is preserved by
//! keeping the payload behind an `Rc` and comparing with `Rc::ptr_eq` — the
//! propagation and silent-recovery sweeps depend on that identity, not on
//! structural equality.

use std::any::Any;
use std::fmt;
use std::rc::Rc;

use crate::node::NodeId;

/// The payload a user computation failed with. Type-erased so the graph can
/// carry errors from computations of differing value types.
pub type UserError = Rc<dyn Any>;

pub(crate) enum ErrorKind {
    /// Port of `NotReadyError`: a tracked read whose value is currently
    /// pending. In the synchronous core nothing constructs one (no async
    /// source exists to be unready), but every site that classifies or
    /// propagates it is ported, because the async phase reintroduces it and
    /// the surrounding branches must already be in the right shape.
    NotReady {
        source: Option<NodeId>,
        /// Tags a visibility-only notification on the `affects()` boundary
        /// channel: boundaries update display state from it, but the root
        /// queue never registers a reporter.
        mark_visual: bool,
    },
    /// Port of `StatusError`: wraps a user error so the graph can track which
    /// node produced it, without losing the original for the boundary/effect
    /// error arm.
    Status { source: NodeId, cause: UserError },
    /// A user error that has not (yet) been wrapped for source tracking.
    User(UserError),
}

/// An error travelling through the reactive graph.
///
/// Cheap to clone (one `Rc` bump) and compared by identity, matching the
/// upstream object-identity semantics.
#[derive(Clone)]
pub struct ReactiveError(pub(crate) Rc<ErrorKind>);

impl ReactiveError {
    pub(crate) fn not_ready(source: Option<NodeId>) -> Self {
        ReactiveError(Rc::new(ErrorKind::NotReady {
            source,
            mark_visual: false,
        }))
    }

    /// Wrap a user payload for source tracking. Port of `new StatusError(el, e)`.
    pub(crate) fn status(source: NodeId, cause: UserError) -> Self {
        ReactiveError(Rc::new(ErrorKind::Status { source, cause }))
    }

    /// Build an error from any user payload.
    pub fn user<E: Any + 'static>(value: E) -> Self {
        ReactiveError(Rc::new(ErrorKind::User(Rc::new(value))))
    }

    /// Convenience for the common "message string" case.
    pub fn msg(message: impl Into<String>) -> Self {
        ReactiveError::user(message.into())
    }

    pub(crate) fn is_not_ready(&self) -> bool {
        matches!(*self.0, ErrorKind::NotReady { .. })
    }

    pub(crate) fn not_ready_source(&self) -> Option<NodeId> {
        match *self.0 {
            ErrorKind::NotReady { source, .. } => source,
            _ => None,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn is_mark_visual(&self) -> bool {
        matches!(*self.0, ErrorKind::NotReady {
            mark_visual: true,
            ..
        })
    }

    /// Port of `unwrapStatusError`: return the user's error from an internal
    /// status wrapper, so boundaries and effect error arms see what user code
    /// produced rather than the engine's tracking wrapper.
    pub fn unwrap_status(&self) -> Option<UserError> {
        match &*self.0 {
            ErrorKind::Status { cause, .. } => Some(cause.clone()),
            ErrorKind::User(cause) => Some(cause.clone()),
            ErrorKind::NotReady { .. } => None,
        }
    }

    /// Best-effort human-readable rendering, for tests and diagnostics.
    pub fn message(&self) -> String {
        match self.unwrap_status() {
            Some(cause) => downcast_message(&*cause),
            None => "NotReadyError".to_string(),
        }
    }

    /// Identity comparison. Upstream uses `===` on the thrown object; the
    /// propagation sweeps (`notifyStatus`, `settleErroredDependents`) are
    /// correct only under identity, never structural equality.
    pub(crate) fn same(a: &ReactiveError, b: &ReactiveError) -> bool {
        Rc::ptr_eq(&a.0, &b.0)
    }
}

fn downcast_message(value: &dyn Any) -> String {
    if let Some(s) = value.downcast_ref::<String>() {
        return s.clone();
    }
    if let Some(s) = value.downcast_ref::<&'static str>() {
        return (*s).to_string();
    }
    "<error>".to_string()
}

impl fmt::Debug for ReactiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &*self.0 {
            ErrorKind::NotReady { source, .. } => {
                f.debug_struct("NotReadyError").field("source", source).finish()
            }
            ErrorKind::Status { source, cause } => f
                .debug_struct("StatusError")
                .field("source", source)
                .field("cause", &downcast_message(&**cause))
                .finish(),
            ErrorKind::User(cause) => {
                f.debug_tuple("Error").field(&downcast_message(&**cause)).finish()
            }
        }
    }
}

/// Port of `NoOwnerError`.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct NoOwnerError;

impl fmt::Display for NoOwnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Context can only be accessed under a reactive root.")
    }
}

impl std::error::Error for NoOwnerError {}

/// Port of `ContextNotFoundError`.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct ContextNotFoundError;

impl fmt::Display for ContextNotFoundError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "Context must either be created with a default value or a value must be provided \
             before accessing it.",
        )
    }
}

impl std::error::Error for ContextNotFoundError {}

/// The error returned by the fallible context accessors.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ContextError {
    NoOwner(NoOwnerError),
    NotFound(ContextNotFoundError),
}

impl fmt::Display for ContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ContextError::NoOwner(e) => e.fmt(f),
            ContextError::NotFound(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for ContextError {}
