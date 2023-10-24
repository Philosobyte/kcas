#![cfg(loom)]

mod common;

#[test]
fn two_thread_loom_test() {
    loom::model(|| {
        common::concurrency_test::<2, 3>();
    })
}
