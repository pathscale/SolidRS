//! Test helpers standing in for what vitest gives the upstream suite.
//!
//! `Spy` is `vi.fn()`: a call counter that also records arguments. `Order` is
//! the `order += "t1"` idiom several graph tests use to pin evaluation order.
//!
//! Each Rust test runs on its own thread, and the runtime is thread-local, so
//! tests are isolated without the `afterEach(() => flush())` upstream needs.

#![allow(dead_code)]

use std::cell::RefCell;
use std::fmt::Debug;
use std::rc::Rc;

/// Port of `vi.fn()`.
pub struct Spy<T = ()> {
    calls: Rc<RefCell<Vec<T>>>,
}

impl<T> Clone for Spy<T> {
    fn clone(&self) -> Self {
        Spy {
            calls: self.calls.clone(),
        }
    }
}

impl<T> Default for Spy<T> {
    fn default() -> Self {
        Spy::new()
    }
}

impl<T> Spy<T> {
    pub fn new() -> Spy<T> {
        Spy {
            calls: Rc::new(RefCell::new(Vec::new())),
        }
    }

    pub fn call(&self, arg: T) {
        self.calls.borrow_mut().push(arg);
    }

    /// `expect(spy).toHaveBeenCalledTimes(n)`
    pub fn times(&self) -> usize {
        self.calls.borrow().len()
    }

    pub fn reset(&self) {
        self.calls.borrow_mut().clear();
    }
}

impl Spy<()> {
    pub fn hit(&self) {
        self.call(());
    }
}

impl<T: Clone> Spy<T> {
    pub fn args(&self) -> Vec<T> {
        self.calls.borrow().clone()
    }

    /// `expect(spy).toHaveBeenLastCalledWith(v)`
    pub fn last(&self) -> Option<T> {
        self.calls.borrow().last().cloned()
    }
}

impl<T: Clone + PartialEq + Debug> Spy<T> {
    /// `expect(spy).toHaveBeenCalledWith(v)`
    pub fn was_called_with(&self, v: &T) -> bool {
        self.calls.borrow().iter().any(|c| c == v)
    }
}

/// Accumulates a string, for the `order += "t1"` evaluation-order tests.
#[derive(Clone, Default)]
pub struct Order(Rc<RefCell<String>>);

impl Order {
    pub fn new() -> Order {
        Order::default()
    }

    pub fn push(&self, s: &str) {
        self.0.borrow_mut().push_str(s);
    }

    pub fn get(&self) -> String {
        self.0.borrow().clone()
    }

    pub fn reset(&self) {
        self.0.borrow_mut().clear();
    }
}

/// A shared list, for tests that collect emitted values.
#[derive(Clone)]
pub struct Log<T>(Rc<RefCell<Vec<T>>>);

impl<T> Default for Log<T> {
    fn default() -> Self {
        Log(Rc::new(RefCell::new(Vec::new())))
    }
}

impl<T: Clone> Log<T> {
    pub fn new() -> Log<T> {
        Log::default()
    }
    pub fn push(&self, v: T) {
        self.0.borrow_mut().push(v);
    }
    pub fn get(&self) -> Vec<T> {
        self.0.borrow().clone()
    }
    pub fn len(&self) -> usize {
        self.0.borrow().len()
    }
    pub fn reset(&self) {
        self.0.borrow_mut().clear();
    }
}

/// A shared counter.
#[derive(Clone, Default)]
pub struct Counter(Rc<std::cell::Cell<usize>>);

impl Counter {
    pub fn new() -> Counter {
        Counter::default()
    }
    pub fn inc(&self) -> usize {
        self.0.set(self.0.get() + 1);
        self.0.get()
    }
    pub fn get(&self) -> usize {
        self.0.get()
    }
    pub fn reset(&self) {
        self.0.set(0);
    }
}
