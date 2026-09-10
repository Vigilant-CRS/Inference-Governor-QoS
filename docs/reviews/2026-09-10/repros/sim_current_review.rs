//! Counterexample for the review snapshot; temporarily install as sim integration test.
use vig_core::{Duration, Instant};
use vig_sim::coverage::CoverageTracker;
fn ms(n: u64) -> Duration { Duration::from_millis(n).unwrap() }
fn at(n: u64) -> Instant { Instant::ZERO.checked_add(ms(n)).unwrap() }

#[test]
fn stale_deliveries_must_not_shorten_a_continuous_supply_gap() {
    let mut t = CoverageTracker::new(ms(10), ms(10), at(0), ms(150));
    t.record_delivery(at(50), at(0));
    t.record_delivery(at(100), at(50));
    let c = t.finish();
    assert_eq!(c.consumer_covered, 0);
    assert_eq!(c.longest_gap_ns, ms(150).as_nanos(), "no usable result existed at ANY point in the 150ms interval");
}
