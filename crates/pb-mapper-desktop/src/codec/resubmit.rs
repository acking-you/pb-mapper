//! Sending a dropped picture again. A rate control may drop a picture it
//! cannot fit, as VideoToolbox's low-latency one does to about every other
//! picture while it pays an IDR back on a slow link. A later picture
//! replaces a dropped one, but when the dropped one was the last change (a
//! typed character, the end of a scroll) nothing replaces it, and the viewer
//! would keep the picture before until the liveness repeat. So the last
//! picture is submitted again at the next tick, as a new picture, when the
//! newest unit handed over was dropped and carried a picture the encoder
//! never kept: at most `MAX_RESUBMITS` times a picture, until the encoder
//! keeps one or a new picture comes.
//!
//! Which unit is a duplicate is decided as the encoder keeps it, by whether
//! a unit before it already carried its picture out: a liveness or keyframe
//! repeat of a picture the encoder dropped each time is the viewer's first
//! sight of it, and is counted as new.

/// What a unit handed to the encoder carries, and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unit {
    /// A new picture.
    Fresh,
    /// The last picture again, for a keyframe or the liveness repeat.
    Repeat,
    /// The last picture again because the encoder dropped the newest unit,
    /// which carried a picture it never kept.
    Resubmit,
}

impl Unit {
    /// What a submit hands over: a new picture when the capture gave one,
    /// else the last again, as a resubmit when one is due.
    pub(crate) fn of(fresh: bool, resubmit: bool) -> Self {
        match (fresh, resubmit) {
            (true, _) => Self::Fresh,
            (false, true) => Self::Resubmit,
            (false, false) => Self::Repeat,
        }
    }
}

/// Resubmits of one picture at most.
pub(crate) const MAX_RESUBMITS: u8 = 2;

/// Which picture each unit carries, which of them the encoder kept, and
/// whether the last picture is due again. The encoder's output callback and
/// thread C share it under the encoder's lock.
#[derive(Debug, Default)]
pub(crate) struct Resubmit {
    due: bool,
    /// Resubmits of the newest picture made.
    made: u8,
    /// New pictures handed over: the newest's number, which every unit
    /// handed over since carries. 0 before the first.
    newest: u64,
    /// The newest picture a kept unit carried; 0 for none.
    kept: u64,
}

impl Resubmit {
    /// A unit of `unit` is handed over: the number of the picture it
    /// carries, which its output reports back. It carries the newest
    /// picture, so it replaces any unit dropped before it: a resubmit due
    /// is due no longer.
    pub(crate) fn submitted(&mut self, unit: Unit) -> u64 {
        self.due = false;
        match unit {
            Unit::Fresh => {
                self.newest += 1;
                self.made = 0;
            }
            Unit::Resubmit => self.made = self.made.saturating_add(1),
            Unit::Repeat => {}
        }
        self.newest
    }

    /// The encoder dropped a unit carrying `picture`; `last` when no unit
    /// handed over after it is in flight to replace it. Whether a resubmit
    /// became due, which thread C is woken for: none for a picture the
    /// encoder kept before, the viewer's already.
    pub(crate) fn dropped(&mut self, picture: u64, last: bool) -> bool {
        if picture <= self.kept || !last || self.made >= MAX_RESUBMITS {
            return false;
        }
        !std::mem::replace(&mut self.due, true)
    }

    /// The encoder kept a unit carrying `picture`. Whether it is a duplicate
    /// (`VideoFormat::duplicate`): a unit before it carried that picture out.
    pub(crate) fn kept(&mut self, picture: u64) -> bool {
        let duplicate = picture <= self.kept;
        self.kept = self.kept.max(picture);
        duplicate
    }

    /// Whether a resubmit is due, which the next submit takes.
    pub(crate) fn take(&mut self) -> bool {
        std::mem::take(&mut self.due)
    }

    pub(crate) fn due(&self) -> bool {
        self.due
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A dropped new picture is due again, and wakes thread C once; a
    /// dropped resubmit is due again too, up to two resubmits; the third
    /// drop leaves it to the liveness repeat, whose drop starts nothing
    /// either. A new picture has resubmits of its own.
    #[test]
    fn a_dropped_newest_picture_is_resubmitted_at_most_twice() {
        let mut resubmit = Resubmit::default();
        let picture = resubmit.submitted(Unit::Fresh);
        assert!(resubmit.dropped(picture, true));
        assert!(!resubmit.dropped(picture, true), "woken once");
        assert!(resubmit.due() && resubmit.take() && !resubmit.take());
        for made in 1..=MAX_RESUBMITS {
            assert_eq!(resubmit.submitted(Unit::Resubmit), picture);
            let again = resubmit.dropped(picture, true);
            assert_eq!(again, made < MAX_RESUBMITS, "resubmit {made}");
            assert_eq!(resubmit.take(), again);
        }
        assert_eq!(resubmit.submitted(Unit::Repeat), picture);
        assert!(!resubmit.dropped(picture, true));
        let next = resubmit.submitted(Unit::Fresh);
        assert!(next > picture);
        assert!(resubmit.dropped(next, true) && resubmit.take());
    }

    /// A picture the encoder kept is not sent again, nor is one with a
    /// later unit in flight that replaces it.
    #[test]
    fn a_kept_or_a_superseded_picture_is_not_resubmitted() {
        let mut resubmit = Resubmit::default();
        let picture = resubmit.submitted(Unit::Fresh);
        assert!(!resubmit.kept(picture));
        assert_eq!(resubmit.submitted(Unit::Repeat), picture);
        assert!(!resubmit.dropped(picture, true));
        let next = resubmit.submitted(Unit::Fresh);
        assert!(!resubmit.dropped(next, false));
        assert!(!resubmit.due());
        // A repeat before any new picture carries nothing new.
        let mut resubmit = Resubmit::default();
        let none = resubmit.submitted(Unit::Repeat);
        assert!(!resubmit.dropped(none, true));
    }

    /// A drop reported after a newer unit was handed over, between thread
    /// C taking the resubmit flag and handing that unit over, leaves nothing
    /// due: the newer unit replaces the dropped one.
    #[test]
    fn a_unit_handed_over_after_a_drop_replaces_it() {
        for unit in [Unit::Fresh, Unit::Repeat, Unit::Resubmit] {
            let mut resubmit = Resubmit::default();
            let picture = resubmit.submitted(Unit::Fresh);
            assert!(!resubmit.take());
            assert!(resubmit.dropped(picture, true));
            resubmit.submitted(unit);
            assert!(!resubmit.take(), "{unit:?}");
        }
    }

    /// A unit is a duplicate only when one before it carried its picture
    /// out: never a new picture, nor a resubmit, nor a repeat of a picture
    /// every unit of which the encoder dropped; a repeat of a kept picture
    /// is.
    #[test]
    fn only_a_picture_carried_out_before_is_a_duplicate() {
        // A unit handed over and kept: whether it is a duplicate.
        let out = |resubmit: &mut Resubmit, unit| {
            let picture = resubmit.submitted(unit);
            resubmit.kept(picture)
        };
        assert_eq!(Unit::of(true, true), Unit::Fresh);
        assert_eq!(Unit::of(true, false), Unit::Fresh);
        assert_eq!(Unit::of(false, true), Unit::Resubmit);
        assert_eq!(Unit::of(false, false), Unit::Repeat);
        let mut resubmit = Resubmit::default();
        let first = resubmit.submitted(Unit::Fresh);
        assert!(!resubmit.kept(first));
        assert!(out(&mut resubmit, Unit::Repeat));
        // Dropped, resubmitted and dropped twice: the liveness repeat is
        // the viewer's first sight of it, and a repeat after that is not.
        let second = resubmit.submitted(Unit::Fresh);
        assert!(resubmit.dropped(second, true) && resubmit.take());
        for _ in 0..MAX_RESUBMITS {
            resubmit.submitted(Unit::Resubmit);
            resubmit.dropped(second, true);
            resubmit.take();
        }
        assert!(!out(&mut resubmit, Unit::Repeat));
        assert!(out(&mut resubmit, Unit::Repeat));
        // A resubmit kept is new, and a repeat kept after it is not.
        let third = resubmit.submitted(Unit::Fresh);
        assert!(resubmit.dropped(third, true) && resubmit.take());
        assert!(!out(&mut resubmit, Unit::Resubmit));
        assert!(out(&mut resubmit, Unit::Repeat));
        // An older picture's unit kept after a newer one was handed over.
        let fourth = resubmit.submitted(Unit::Fresh);
        let fifth = resubmit.submitted(Unit::Fresh);
        assert!(!resubmit.kept(fourth) && !resubmit.kept(fifth));
    }
}
