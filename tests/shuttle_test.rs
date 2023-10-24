#![cfg(feature = "shuttle")]

mod common;

use common::concurrency_test;
use kcas::ArcStateWrapper;
use shuttle::scheduler::RandomScheduler;
use shuttle::{Config, PortfolioRunner};
use std::fs::File;
use std::io::Read;
use test_log::test;

#[test]
fn two_thread_shuttle_test() {
    let mut portfolio_runner = PortfolioRunner::new(true, Config::new());
    for i in 0..32 {
        portfolio_runner.add(RandomScheduler::new(10000usize));
    }
    portfolio_runner.run(|| {
        common::concurrency_test::<2, 3>();
    });
}

fn replay_test<const NUM_THREADS: usize, const NUM_WORDS: usize>(path_to_failing_iteration: &str) {
    let mut file: File = File::open(path_to_failing_iteration).unwrap();
    let mut replay_string: String = String::new();
    file.read_to_string(&mut replay_string);
    shuttle::replay(
        || {
            concurrency_test::<NUM_THREADS, NUM_WORDS>();
        },
        &*replay_string,
    );
}
