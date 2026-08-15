//! Not a ported test — a minimal end-to-end check that the graph is wired up.
//! The ported suites are the real verification.

use solid_rs::*;
use std::cell::RefCell;
use std::rc::Rc;

#[test]
fn signal_read_write_stages_and_commits() {
    let (x, set_x) = create_signal(1);
    assert_eq!(x.get(), 1);
    set_x.set(2);
    assert_eq!(x.get(), 1, "writes stage until the tick commits");
    flush();
    assert_eq!(x.get(), 2);
}

#[test]
fn memo_tracks_and_recomputes() {
    let (x, set_x) = create_signal(1);
    let (y, _set_y) = create_signal(10);
    let sum = create_memo(move |_| x.get() + y.get());
    assert_eq!(sum.get(), 11);
    set_x.set(2);
    flush();
    assert_eq!(sum.get(), 12);
}

#[test]
fn effect_runs_on_flush() {
    let log = Rc::new(RefCell::new(Vec::new()));
    let sink = log.clone();
    let (x, set_x) = create_signal(0);
    create_root(move |_| {
        create_effect(
            move |_| x.get(),
            move |v: &i32, _| sink.borrow_mut().push(*v),
        );
    });
    flush();
    assert_eq!(*log.borrow(), vec![0]);
    set_x.set(1);
    flush();
    assert_eq!(*log.borrow(), vec![0, 1]);
}

#[test]
fn diamond_updates_tail_once() {
    let (x, set_x) = create_signal("a".to_string());
    let a = create_memo(move |_| x.get());
    let b = create_memo(move |_| x.get());
    let runs = Rc::new(RefCell::new(0));
    let r = runs.clone();
    let c = create_memo(move |_| {
        *r.borrow_mut() += 1;
        format!("{} {}", a.get(), b.get())
    });
    assert_eq!(c.get(), "a a");
    assert_eq!(*runs.borrow(), 1);
    set_x.set("aa".to_string());
    flush();
    assert_eq!(c.get(), "aa aa");
    assert_eq!(*runs.borrow(), 2);
}
