//! Cycle detection for walks with one successor per step, in constant space.

/// Detects that a walk with one successor per step has entered a cycle,
/// without remembering the items it passed (Brent's algorithm). It keeps one
/// saved item, moved forward at steps 1, 2, 4, 8 and so on, and reports a
/// cycle when the walk reaches the saved item again.
///
/// Detection is not immediate. With a lead-in of `mu` items before a cycle
/// of `lambda`, the walk passes at most about `2 * max(mu, lambda) +
/// lambda` items before the cycle is reported, so it may go around the
/// cycle more than once. A walk that records each step must allow for that;
/// one that only reads is unaffected.
pub(crate) struct WalkCycle<T> {
    saved: Option<T>,
    power: usize,
    since_saved: usize,
}

impl<T: Copy + Eq> WalkCycle<T> {
    pub(crate) fn new() -> Self {
        Self {
            saved: None,
            power: 1,
            since_saved: 0,
        }
    }

    /// Records the walk's next item. Returns `false` once the walk has come
    /// back to an item it passed; never before it enters a cycle.
    pub(crate) fn advance(&mut self, item: T) -> bool {
        if self.saved == Some(item) {
            return false;
        }
        self.since_saved += 1;
        if self.since_saved == self.power {
            self.saved = Some(item);
            self.power *= 2;
            self.since_saved = 0;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Walks `successor` from `start` until it ends or a cycle is detected,
    /// returning the steps taken and whether a cycle ended it.
    fn walk(start: usize, successor: impl Fn(usize) -> Option<usize>) -> (usize, bool) {
        let mut cycle = WalkCycle::new();
        let mut item = start;
        let mut steps = 0;
        loop {
            if !cycle.advance(item) {
                return (steps, true);
            }
            steps += 1;
            match successor(item) {
                Some(next) => item = next,
                None => return (steps, false),
            }
        }
    }

    #[test]
    fn an_acyclic_walk_reaches_its_end() {
        assert_eq!(
            walk(0, |item| (item < 1_000).then_some(item + 1)),
            (1_001, false)
        );
    }

    /// Every combination of a lead-in of length `mu` and a cycle of length
    /// `lambda` is detected within `2 * max(mu + 1, lambda) + lambda` steps,
    /// and never before the walk has passed `mu + lambda` items.
    #[test]
    fn a_cycle_is_detected_soon_after_it_repeats() {
        for mu in 0..40 {
            for lambda in 1..40 {
                let (steps, cycled) = walk(0, |item| {
                    Some(if item + 1 == mu + lambda {
                        mu
                    } else {
                        item + 1
                    })
                });
                assert!(cycled, "mu {mu}, lambda {lambda}");
                assert!(steps >= mu + lambda, "mu {mu}, lambda {lambda}: {steps}");
                let bound = 2 * (mu + 1).max(lambda) + lambda;
                assert!(steps <= bound, "mu {mu}, lambda {lambda}: {steps}");
            }
        }
    }
}
