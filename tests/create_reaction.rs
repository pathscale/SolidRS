//! Port of `tests/createReaction.test.ts`.

mod common;

use common::Counter;
use solid_rs::*;
use std::cell::Cell;
use std::rc::Rc;

/// `Create and trigger a Reaction`
#[test]
fn create_and_trigger_a_reaction() {
    let count = Counter::new();
    let c = count.clone();
    let (sign, set_sign) = create_signal("thoughts".to_string());

    let track = create_root(move |_| {
        create_reaction(move || {
            c.inc();
        })
    });
    flush();
    assert_eq!(count.get(), 0);
    track(Box::new(move || {
        sign.get();
    }));
    assert_eq!(count.get(), 0);
    flush();
    assert_eq!(count.get(), 0);

    set_sign.set("mind".to_string());
    flush();
    assert_eq!(count.get(), 1);

    set_sign.set("body".to_string());
    flush();
    assert_eq!(count.get(), 1);

    track(Box::new(move || {
        sign.get();
    }));
    set_sign.set("everything".to_string());
    flush();
    assert_eq!(count.get(), 2);
}

/// `fires and disposes cleanly when the rerun tracks zero dependencies`
#[test]
fn fires_and_disposes_cleanly_when_the_rerun_tracks_zero_dependencies() {
    let fired = Counter::new();
    let f = fired.clone();
    let read_dep = Rc::new(Cell::new(true));
    let rd = read_dep.clone();
    let (sign, set_sign) = create_signal("a".to_string());

    create_root(move |_| {
        let track = create_reaction(move || {
            f.inc();
        });
        track(Box::new(move || {
            // The initial run reads a dep; the invalidating rerun reads none.
            if rd.get() {
                sign.get();
            }
        }));
    });
    flush();
    assert_eq!(fired.get(), 0);

    read_dep.set(false);
    set_sign.set("b".to_string());
    flush();
    assert_eq!(fired.get(), 1);
}

/// `re-arming before a fire replaces tracked sources (#2861)`
#[test]
fn re_arming_before_a_fire_replaces_tracked_sources() {
    let count = Counter::new();
    let c = count.clone();
    let (a, set_a) = create_signal(0);
    let (b, set_b) = create_signal(0);

    let track = create_root(move |_| {
        create_reaction(move || {
            c.inc();
        })
    });

    track(Box::new(move || {
        a.get();
    }));
    // Supersedes the `a` arm before any fire.
    track(Box::new(move || {
        b.get();
    }));

    set_a.set(1);
    flush();
    assert_eq!(count.get(), 0, "superseded arm must not fire");

    set_b.set(1);
    flush();
    assert_eq!(count.get(), 1);

    // Disarmed after the fire — nothing accumulated.
    set_b.set(2);
    set_a.set(2);
    flush();
    assert_eq!(count.get(), 1);
}

/// `re-arm inside the reaction callback arms the next cycle (#2861)`
#[test]
fn re_arm_inside_the_reaction_callback_arms_the_next_cycle() {
    let count = Counter::new();
    let (a, set_a) = create_signal(0);

    // The callback re-arms the same reaction, so the handle has to be visible
    // from inside itself.
    type Track = Rc<dyn Fn(Box<dyn Fn()>)>;
    let slot: Rc<std::cell::RefCell<Option<Track>>> = Rc::new(std::cell::RefCell::new(None));

    let c = count.clone();
    let inner_slot = slot.clone();
    let track: Track = create_root(move |_| {
        Rc::new(create_reaction(move || {
            c.inc();
            let t = inner_slot.borrow().clone().expect("armed");
            t(Box::new(move || {
                a.get();
            }));
        })) as Track
    });
    *slot.borrow_mut() = Some(track.clone());

    track(Box::new(move || {
        a.get();
    }));
    set_a.set(1);
    flush();
    assert_eq!(count.get(), 1);

    set_a.set(2);
    flush();
    assert_eq!(count.get(), 2);
}

/// `throws on invalid cleanup values`
#[test]
#[ignore = "not representable: the reaction callback returns `()`, so an invalid cleanup value is \
            a compile error rather than a runtime throw"]
fn throws_on_invalid_cleanup_values() {}
