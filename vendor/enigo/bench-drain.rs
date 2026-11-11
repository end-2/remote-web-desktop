//! Synthetic policy comparison only: no capture, event posting, or native input.
//! Build: rustc --edition=2024 -O vendor/enigo/bench-drain.rs -o target/bench-drain
//! Run: target/bench-drain --measure
use std::time::{Duration, Instant};

#[allow(dead_code)]
#[path = "src/macos/pointer_state.rs"]
mod pointer_state;

fn compare(count: u64, interval: Duration) -> (Duration, Duration) {
    let start = Instant::now();
    let mut previous = start;
    let mut legacy_debt = Duration::ZERO;
    let mut bounded = pointer_state::EventDrain::default();
    for index in 0..count {
        let now = start + interval.mul_f64(index as f64);
        // Equivalent to Enigo 0.6.1 update_wait_time + immediate Drop.
        legacy_debt =
            legacy_debt.saturating_sub(now.duration_since(previous)) + Duration::from_millis(20);
        previous = now;
        bounded.record(now);
    }
    (legacy_debt, bounded.remaining(previous))
}

fn main() {
    println!("Synthetic Enigo teardown-policy benchmark; no OS events or native input.");
    println!("Numbers are requested grace delays, not measured OS event-delivery latency.");
    for (label, count, interval) in [
        (
            "60 events/sec for 60 sec",
            3600,
            Duration::from_nanos(1_000_000_000 / 60),
        ),
        ("250 events/sec for 1 sec", 250, Duration::from_millis(4)),
        ("500 events immediate burst", 500, Duration::ZERO),
    ] {
        let (old, new) = compare(count, interval);
        println!(
            "{label}: original={:.3} ms, bounded={:.3} ms",
            old.as_secs_f64() * 1000.0,
            new.as_secs_f64() * 1000.0
        );
    }
    if std::env::args().any(|arg| arg == "--measure") {
        let (old, new) = compare(20, Duration::ZERO);
        for (label, delay) in [("original", old), ("bounded", new)] {
            let start = Instant::now();
            std::thread::sleep(delay);
            println!(
                "Measured sleep for 20 synthetic events, {label}: {:.3} ms (requested {:.3} ms)",
                start.elapsed().as_secs_f64() * 1000.0,
                delay.as_secs_f64() * 1000.0
            );
        }
    }
}
