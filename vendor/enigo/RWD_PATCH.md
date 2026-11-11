# Local macOS pointer patch

This directory vendors **Enigo 0.6.1** from crates.io, upstream revision
`b297a14e807abdf818b7809a70b445ade4fc5897`. Its original MIT license is retained
in `LICENSE`; cross-platform keyboard, text, scroll and event construction stay
in the established library.

Native browser testing found a reproducible macOS ordering error: a click sent
immediately after moving to a different target was posted at the old cursor
position. The original code queries AppKit position and button state immediately
after posting asynchronous Quartz events. Slowing input down can hide this bug.

The narrowly modified files are `src/macos/macos_impl.rs`, `src/macos/mod.rs`
and the new pure bookkeeping module `src/macos/pointer_state.rs`:

- Backport the pending-position and pending-button strategy already implemented
  in [upstream Enigo at a88d9b7e2cec7043ab5f03e754500a091ea928d1](https://github.com/enigo-rs/enigo/blob/a88d9b7e2cec7043ab5f03e754500a091ea928d1/src/macos/macos_impl.rs).
  A 60 ms cache lifetime means **no sleeping**: events post immediately, while
  old cached positions expire so later physical mouse motion remains observable.
- Choose drag events from injected button state and preserve Enigo modifier flags.
  Recently released buttons override stale OS state. Middle-button drags also work.
- Read fallback locations from a Quartz event in global display coordinates.
- Attach the pending position to keyboard/text/scroll events so rapidly queued
  events do not restore an obsolete pointer position.
- Correct absolute motion delta direction to destination minus origin, matching
  upstream; reset click counts when the next press is over 4 logical points away
  in either axis. The system double-click interval and same-location multi-clicks
  remain in use.
- Replace the original accumulating `20 ms × event count − elapsed` teardown
  wait with a grace ending 60 ms after the latest post. Final key-up events
  refresh that grace. Input injection never sleeps, and long sessions no longer
  accumulate seconds of controller-release delay. This is a conservative,
  bounded source-lifetime grace, not a Quartz delivery acknowledgement; native
  burst/disconnect tests must still verify final key/mouse releases. Pinned
  upstream still uses the accumulator, so this part is a local correction.
- Remove four redundant `unsafe` blocks around safe `NSEvent` methods in the
  locked `objc2-app-kit` 0.3.2 bindings. This fixes `unused_unsafe` build warnings
  without changing input behavior; the raw `CGEvent` conversion remains unsafe.

The added tests are pure state tests; they send no native input. They are included
in the product's normal `cargo test` run, and can also be selected by filter.
Do not run Enigo's complete original suite against a user's active desktop:

```sh
cargo test macos_pointer_regression
```

The product-level native browser fixture verifies actual event targets and
coordinates across rapid alternating clicks, plus drag, keyboard, Unicode and
disconnect cleanup. This patch addresses correctness without adding input delay;
it does not claim a measured encoder/stream throughput improvement.

Reproduce the synthetic teardown-policy comparison without OS input:

```sh
rustc --edition=2024 -O vendor/enigo/bench-drain.rs -o target/bench-drain
target/bench-drain --measure
```

The 60 Hz / 60 second workload requests approximately 12,016.7 ms of teardown
delay with the original accumulator versus at most 60 ms with the correction.
This deterministic policy calculation and the optional short sleep measurement
are separate from native end-to-end disconnect timing.
