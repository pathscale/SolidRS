//! Port of `src/core/context.ts`.
//!
//! Context is owner-scoped dependency injection. The record is copied on write
//! and shared by reference on node creation, so a child's write is invisible
//! to its parent — without that, every context would be a singleton.
//!
//! Upstream keys contexts with `Symbol()`. Rust has no symbols, so a process-
//! wide counter supplies the identity and the type parameter carries the value
//! type; `Context<T>` is `Copy` and cheap to pass around.

use std::any::Any;
use std::cell::Cell;
use std::marker::PhantomData;
use std::rc::Rc;

use crate::error::{ContextError, ContextNotFoundError, NoOwnerError};
use crate::node::ContextId;
use crate::runtime::with_rt;
use crate::Owner;

thread_local! {
    static NEXT_CONTEXT_ID: Cell<ContextId> = const { Cell::new(0) };
}

/// A context channel.
pub struct Context<T: 'static> {
    id: ContextId,
    /// `None` means the context has no default and must be provided before it
    /// is read.
    ///
    /// A function pointer rather than a value: it keeps `Context<T>` `Copy`
    /// (so it drops into closures without ceremony) without requiring the
    /// default to be `&'static`.
    default_value: Option<fn() -> T>,
    _t: PhantomData<fn() -> T>,
}

impl<T: 'static> std::fmt::Debug for Context<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context")
            .field("id", &self.id)
            .field("has_default", &self.default_value.is_some())
            .finish()
    }
}

impl<T: 'static> Clone for Context<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T: 'static> Copy for Context<T> {}

/// Create a context with no default value.
///
/// Reading it before a provider sets it yields [`ContextNotFoundError`].
pub fn create_context<T: 'static>() -> Context<T> {
    Context {
        id: NEXT_CONTEXT_ID.with(|c| {
            let v = c.get();
            c.set(v + 1);
            v
        }),
        default_value: None,
        _t: PhantomData,
    }
}

/// Create a context with a default value used when nothing has provided one.
///
/// ```
/// use solid_rs::{create_context_with_default, create_root, get_context};
/// let theme = create_context_with_default(|| "light".to_string());
/// create_root(move |_| {
///     assert_eq!(get_context(theme, None).unwrap(), "light");
/// });
/// ```
pub fn create_context_with_default<T: 'static>(default_value: fn() -> T) -> Context<T> {
    Context {
        default_value: Some(default_value),
        ..create_context::<T>()
    }
}

impl<T: 'static> Context<T> {
    /// The context's default value, if it was created with one.
    pub fn default_value(&self) -> Option<T> {
        self.default_value.map(|f| f())
    }

    /// Stable identity of the channel. Port of the upstream `Symbol`.
    pub fn id(&self) -> ContextId {
        self.id
    }
}

/// Read a context value from `owner` (or the current owner).
///
/// Port of `getContext`.
///
/// # Errors
///
/// [`NoOwnerError`] when called outside any owner, [`ContextNotFoundError`]
/// when neither a provider nor a default supplied a value.
pub fn get_context<T: Clone + 'static>(
    ctx: Context<T>,
    owner: Option<Owner>,
) -> Result<T, ContextError> {
    let owner = owner.or_else(crate::get_owner);
    let Some(owner) = owner else {
        return Err(ContextError::NoOwner(NoOwnerError));
    };

    let found: Option<T> = with_rt(|rt| {
        let nodes = rt.nodes();
        let n = nodes.get(owner.id())?;
        let record = n.context.borrow();
        let v: &Rc<dyn Any> = record.get(&ctx.id)?;
        v.downcast_ref::<T>().cloned()
    });

    match found.or_else(|| ctx.default_value.map(|f| f())) {
        Some(v) => Ok(v),
        None => Err(ContextError::NotFound(ContextNotFoundError)),
    }
}

/// Provide a context value on `owner` (or the current owner).
///
/// Port of `setContext`. The record is copied rather than mutated in place, so
/// the value is visible to this owner's descendants and to nobody else.
///
/// # Errors
///
/// [`NoOwnerError`] when called outside any owner.
pub fn set_context<T: 'static>(
    ctx: Context<T>,
    value: Option<T>,
    owner: Option<Owner>,
) -> Result<(), NoOwnerError> {
    let owner = owner.or_else(crate::get_owner);
    let Some(owner) = owner else {
        return Err(NoOwnerError);
    };

    // `None` falls back to the default, matching upstream's
    // `isUndefined(value) ? context.defaultValue : value`.
    let boxed: Option<Rc<dyn Any>> = match value {
        Some(v) => Some(Rc::new(v)),
        None => ctx.default_value.map(|f| Rc::new(f()) as Rc<dyn Any>),
    };

    with_rt(|rt| {
        let nodes = rt.nodes();
        let Some(n) = nodes.get(owner.id()) else { return };
        let mut record = n.context.borrow_mut();
        let mut next = (**record).clone();
        match boxed {
            Some(v) => {
                next.insert(ctx.id, v);
            }
            None => {
                next.remove(&ctx.id);
            }
        }
        *record = Rc::new(next);
    });
    Ok(())
}
