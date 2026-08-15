//! Port of `tests/context.test.ts`.
//!
//! Upstream keys a context with `Symbol()` and reads it back untyped. Here the
//! channel carries its value type, so `get_context::<T>` needs no cast and a
//! mismatched read is a compile error. The behaviour under test — owner
//! scoping, copy-on-write, and the two error cases — is unchanged.

use solid_rs::*;

/// `should create context`
#[test]
fn should_create_context() {
    let context = create_context_with_default(|| 1);

    assert_eq!(context.default_value(), Some(1));

    create_root(move |_| {
        set_context(context, None, None).unwrap();
        assert_eq!(get_context(context, None).unwrap(), 1);
    });
}

/// `should forward context across roots`
#[test]
fn should_forward_context_across_roots() {
    let context = create_context_with_default(|| 1);
    create_root(move |_| {
        set_context(context, Some(2), None).unwrap();
        create_root(move |_| {
            assert_eq!(get_context(context, None).unwrap(), 2);
            create_root(move |_| {
                assert_eq!(get_context(context, None).unwrap(), 2);
            });
        });
    });
}

/// `should not expose context on parent when set in child`
#[test]
fn should_not_expose_context_on_parent_when_set_in_child() {
    let context = create_context_with_default(|| 1);
    create_root(move |_| {
        create_root(move |_| {
            set_context(context, Some(4), None).unwrap();
        });

        assert_eq!(get_context(context, None).unwrap(), 1);
    });
}

/// `should throw error when trying to get context outside owner`
#[test]
fn should_error_when_trying_to_get_context_outside_owner() {
    let context = create_context::<i32>();
    assert_eq!(
        get_context(context, None),
        Err(ContextError::NoOwner(NoOwnerError))
    );
}

/// `should throw error when trying to set context outside owner`
#[test]
fn should_error_when_trying_to_set_context_outside_owner() {
    let context = create_context::<i32>();
    assert_eq!(set_context(context, Some(1), None), Err(NoOwnerError));
}

/// `should throw error when trying to get context without setting it first`
#[test]
fn should_error_when_trying_to_get_context_without_setting_it_first() {
    let context = create_context::<i32>();
    create_root(move |_| {
        assert_eq!(
            get_context(context, None),
            Err(ContextError::NotFound(ContextNotFoundError))
        );
    });
}

/// Not upstream: pins that an owned value (not just a `&'static`) round-trips,
/// since the Rust API distinguishes the two.
#[test]
fn owned_context_values_round_trip() {
    let context = create_context::<String>();
    create_root(move |_| {
        set_context(context, Some("hello".to_string()), None).unwrap();
        assert_eq!(get_context(context, None).unwrap(), "hello");
        create_root(move |_| {
            assert_eq!(get_context(context, None).unwrap(), "hello");
        });
    });
}
