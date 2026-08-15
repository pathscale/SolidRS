//! Port of `tests/createSignal.test.ts`.

mod common;

use common::Spy;
use solid_rs::*;
use std::rc::Rc;

/// `should store and return value on read`
#[test]
fn should_store_and_return_value_on_read() {
    let (x, _set_x) = create_signal(1);
    assert_eq!(x.get(), 1);
}

/// `should update signal via setter`
#[test]
fn should_update_signal_via_setter() {
    let (x, set_x) = create_signal(1);
    set_x.set(2);
    assert_eq!(x.get(), 1);
    flush();
    assert_eq!(x.get(), 2);
}

/// `should update signal via update function`
#[test]
fn should_update_signal_via_update_function() {
    let (x, set_x) = create_signal(1);
    set_x.update(|n| n + 1);
    assert_eq!(x.get(), 1);
    flush();
    assert_eq!(x.get(), 2);
}

/// `should accept equals option`
#[test]
fn should_accept_equals_option() {
    // Skip even numbers.
    let (x, set_x) = create_signal_with(1, Options::equals(|prev: &i32, next: &i32| prev + 1 == *next));

    set_x.set(11);
    assert_eq!(x.get(), 1);
    flush();
    assert_eq!(x.get(), 11);

    set_x.set(12);
    flush();
    assert_eq!(x.get(), 11);

    set_x.set(13);
    flush();
    assert_eq!(x.get(), 13);

    set_x.set(14);
    flush();
    assert_eq!(x.get(), 13);
}

/// `should update signal with functional value`
///
/// Upstream distinguishes "a function *as* the value" from "an updater
/// function" by requiring the former to be wrapped. Rust's type system makes
/// that distinction statically — `set` takes a value, `update` takes a
/// closure — so the wrapping ceremony disappears. `never_equal` stands in for
/// `===` on function identity, which Rust closures do not have.
#[test]
fn should_update_signal_with_functional_value() {
    type Thunk = Rc<dyn Fn() -> i32>;
    let (x, set_x) = create_signal_with(Rc::new(|| 10) as Thunk, Options::never_equal());
    assert_eq!(x.with(|f| f()), 10);
    set_x.set(Rc::new(|| 20) as Thunk);
    flush();
    assert_eq!(x.with(|f| f()), 20);
}

/// `should create signal derived from another signal`
#[test]
fn should_create_signal_derived_from_another_signal() {
    let (x, set_x) = create_signal(1);
    let (y, set_y) = create_computed_signal(move |_| x.get() + 1);
    assert_eq!(y.get(), 2);
    set_y.set(1);
    flush();
    assert_eq!(y.get(), 1);
    set_x.set(2);
    flush();
    assert_eq!(y.get(), 3);
}

/// `should call unobserved callback when subscriber is disposed`
#[test]
fn should_call_unobserved_callback_when_subscriber_is_disposed() {
    let unobserved = Spy::<()>::new();
    let u = unobserved.clone();
    let (x, set_x) = create_signal_with(1, Options::default().unobserved(move || u.hit()));

    let disposer = create_root(move |dispose| {
        create_memo(move |_| x.get());
        dispose
    });

    assert_eq!(unobserved.times(), 0);
    disposer.dispose();
    assert_eq!(unobserved.times(), 1);

    set_x.set(2);
    flush();
    assert_eq!(unobserved.times(), 1);
}

/// `should call unobserved callback when signal is unobserved`
#[test]
fn should_call_unobserved_callback_when_signal_is_unobserved() {
    let unobserved = Spy::<()>::new();
    let u = unobserved.clone();
    let (x, set_x) = create_signal(true);
    let (y, _set_y) = create_signal_with(1, Options::default().unobserved(move || u.hit()));

    create_root(move |_| {
        create_memo(move |_| if x.get() { Some(y.get()) } else { None });
    });

    assert_eq!(unobserved.times(), 0);

    set_x.set(false);
    flush();
    assert_eq!(unobserved.times(), 1);
}

// ---------------------------------------------------------------------------
// `describe("async computed transition entanglement")`
// ---------------------------------------------------------------------------

/// `async computed's first settle does not permanently entangle unrelated effects`
#[test]
#[ignore = "blocked on lanes.ts: transition entanglement across independent async sources"]
fn async_computeds_first_settle_does_not_permanently_entangle_unrelated_effects() {}

/// `without the async computed, the sync effect is not entangled`
#[test]
#[ignore = "blocked on lanes.ts: paired control for the test above"]
fn without_the_async_computed_the_sync_effect_is_not_entangled() {}
