//! Model-checks the [`Atomic`] backend under all possible interleavings of
//! two concurrent inserters.
//!
//! Gated on `--features loom`. Loom redefines `AtomicU64` to instrument every
//! load/store, then exhaustively explores the interleaving space. Loom and
//! `serde` are mutually exclusive (loom's atomics aren't `Serialize`), so
//! this file is also gated on `not(feature = "serde")`.
//!
//! What we're checking: after both threads finish, every item *that thread X
//! inserted* must be `contains()`-true. That's the only correctness property
//! the Atomic backend promises; bit interleaving is fine because Bloom
//! membership is monotonic.

#![cfg(all(feature = "loom", feature = "atomic"))]

use bouquet::backend::Backend;
use bouquet::Atomic;
use loom::sync::Arc;
use loom::thread;

#[test]
fn two_threads_concurrent_insert() {
    loom::model(|| {
        let f = Arc::new(Atomic::with_capacity(64));

        // Pick small, deterministic seed hashes — small `num_bits` and `k`
        // keeps the loom search tree manageable.
        let h1 = 0x1234_5678_dead_beefu64;
        let h2 = 0xcafe_babe_feed_facau64;
        let k = 3u32;

        let f1 = Arc::clone(&f);
        let t1 = thread::spawn(move || {
            f1.set_item(h1, k);
        });
        let f2 = Arc::clone(&f);
        let t2 = thread::spawn(move || {
            f2.set_item(h2, k);
        });

        t1.join().unwrap();
        t2.join().unwrap();

        // Both items must be observable from any interleaving.
        assert!(f.check_item(h1, k));
        assert!(f.check_item(h2, k));
    });
}
