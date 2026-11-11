//! Pending Quartz mouse state. Pure bookkeeping: no OS calls or delays.
use std::time::{Duration, Instant};

// Backported from Enigo's upstream asynchronous mouse handling. After this
// interval, query the OS again so local physical mouse movement is respected.
const SETTLE_WINDOW: Duration = Duration::from_millis(60);

/// A bounded teardown grace for the most recent posted event. This deliberately
/// does not model an event queue: Quartz consumes events concurrently.
#[derive(Default)]
pub(super) struct EventDrain {
    last_post: Option<Instant>,
}

impl EventDrain {
    pub(super) fn record(&mut self, now: Instant) {
        self.last_post = Some(now);
    }

    pub(super) fn remaining(&self, now: Instant) -> Duration {
        self.last_post.map_or(Duration::ZERO, |last| {
            SETTLE_WINDOW.saturating_sub(now.saturating_duration_since(last))
        })
    }
}

#[derive(Default)]
pub(super) struct PointerState {
    moved: Option<((i32, i32), Instant)>,
    buttons: [Option<(bool, Instant)>; 5],
}

impl PointerState {
    pub(super) fn record_move(&mut self, point: (i32, i32), now: Instant) {
        self.moved = Some((point, now));
    }

    pub(super) fn pending_position(&self, now: Instant) -> Option<(i32, i32)> {
        self.moved
            .filter(|(_, when)| now.duration_since(*when) < SETTLE_WINDOW)
            .map(|(point, _)| point)
    }

    pub(super) fn record_button(&mut self, index: usize, down: bool, now: Instant) {
        self.buttons[index] = Some((down, now));
    }

    // Some(Some(index)) = injected drag; Some(None) = recently released;
    // None = no pending state, so the caller may query physical mouse buttons.
    pub(super) fn pending_drag(&self, now: Instant) -> Option<Option<usize>> {
        if let Some((index, _)) = self
            .buttons
            .iter()
            .enumerate()
            .filter_map(|(index, state)| match state {
                Some((true, when)) => Some((index, when)),
                _ => None,
            })
            .min_by_key(|(_, when)| *when)
        {
            return Some(Some(index));
        }
        self.buttons
            .iter()
            .flatten()
            .any(|(_, when)| now.duration_since(*when) < SETTLE_WINDOW)
            .then_some(None)
    }
}

pub(super) fn click_count(
    previous: (i64, Instant, (i32, i32)),
    point: (i32, i32),
    now: Instant,
    interval: Duration,
) -> i64 {
    let near = (i64::from(previous.2.0) - i64::from(point.0)).abs() <= 4
        && (i64::from(previous.2.1) - i64::from(point.1)).abs() <= 4;
    if previous.0 > 0 && now.duration_since(previous.1) < interval && near {
        previous.0.saturating_add(1)
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn teardown_grace_is_bounded_after_sustained_and_burst_input() {
        let now = Instant::now();
        let mut drain = EventDrain::default();
        assert_eq!(drain.remaining(now), Duration::ZERO);
        for event in 0..10_000 {
            drain.record(now + Duration::from_millis(event * 8));
        }
        let last = now + Duration::from_millis(9_999 * 8);
        assert_eq!(drain.remaining(last), Duration::from_millis(60));
        for _ in 0..10_000 {
            drain.record(last);
        }
        assert_eq!(drain.remaining(last), Duration::from_millis(60));
        assert_eq!(
            drain.remaining(last + Duration::from_millis(60)),
            Duration::ZERO
        );
    }

    #[test]
    fn final_key_release_refreshes_teardown_grace_after_idle() {
        let now = Instant::now();
        let mut drain = EventDrain::default();
        drain.record(now);
        assert_eq!(
            drain.remaining(now + Duration::from_secs(1)),
            Duration::ZERO
        );
        let release = now + Duration::from_secs(2);
        drain.record(release);
        assert_eq!(drain.remaining(release), Duration::from_millis(60));
        assert_eq!(
            drain.remaining(release + Duration::from_millis(50)),
            Duration::from_millis(10)
        );
    }

    #[test]
    fn move_then_click_uses_destination_without_waiting_for_os() {
        let now = Instant::now();
        let mut state = PointerState::default();
        assert_eq!(state.pending_position(now), None);
        state.record_move((650, 410), now);
        assert_eq!(state.pending_position(now), Some((650, 410)));
        state.record_move((40, 200), now + Duration::from_millis(1));
        assert_eq!(
            state.pending_position(now + Duration::from_millis(2)),
            Some((40, 200))
        );
        assert_eq!(
            state.pending_position(now + Duration::from_millis(61)),
            None
        );
    }

    #[test]
    fn immediate_drag_and_release_override_lagging_os_state() {
        let now = Instant::now();
        let mut state = PointerState::default();
        assert_eq!(state.pending_drag(now), None);
        state.record_button(0, true, now);
        assert_eq!(state.pending_drag(now), Some(Some(0)));
        assert_eq!(
            state.pending_drag(now + Duration::from_secs(5)),
            Some(Some(0))
        );
        state.record_button(0, false, now + Duration::from_secs(5));
        assert_eq!(state.pending_drag(now + Duration::from_secs(5)), Some(None));
        assert_eq!(state.pending_drag(now + Duration::from_millis(5060)), None);
    }

    #[test]
    fn oldest_held_button_defines_drag_including_middle_and_right() {
        let now = Instant::now();
        let mut state = PointerState::default();
        state.record_button(2, true, now);
        state.record_button(1, true, now + Duration::from_millis(1));
        assert_eq!(
            state.pending_drag(now + Duration::from_millis(2)),
            Some(Some(2))
        );
        state.record_button(2, false, now + Duration::from_millis(3));
        assert_eq!(
            state.pending_drag(now + Duration::from_millis(4)),
            Some(Some(1))
        );
    }

    #[test]
    fn distant_rapid_clicks_reset_count_while_near_clicks_remain_double_clicks() {
        let now = Instant::now();
        let interval = Duration::from_millis(500);
        let first = (1, now, (100, 100));
        assert_eq!(
            click_count(first, (300, 240), now + Duration::from_millis(5), interval),
            1
        );
        assert_eq!(
            click_count(first, (102, 100), now + Duration::from_millis(5), interval),
            2
        );
        assert_eq!(click_count(first, (100, 100), now + interval, interval), 1);
    }
}
