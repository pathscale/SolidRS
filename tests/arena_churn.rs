//! Reproduces the port's deliberate no-reclamation deviation.
//!
//! This is an occupancy experiment, not a heap-byte benchmark: the public
//! debug counters report retained node slots and currently live dependency
//! links.

use solid_rs::{create_effect, create_memo, create_root, create_signal, debug, flush};

#[test]
fn disposed_subtrees_accumulate_arena_slots_linearly() {
    const CYCLES: usize = 1_000;

    let nodes_before = debug::node_count();
    let links_before = debug::link_count();

    for _ in 0..CYCLES {
        let dispose = create_root(|dispose| {
            let (source, _set_source) = create_signal(1_i32);
            let derived = create_memo(move |_| source.get() + 1);
            create_effect(move |_| derived.get(), move |_value: &i32, _| {});
            dispose
        });
        flush();
        dispose.dispose();
    }
    flush();

    let nodes_added = debug::node_count() - nodes_before;
    let links_added = debug::link_count() - links_before;

    assert_eq!(nodes_added, 4 * CYCLES);
    assert_eq!(links_added, 0, "disposal should reclaim dependency links");
}
