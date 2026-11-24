//! Sending a dropped picture again. A rate control may drop a picture it
//! cannot fit, as VideoToolbox's low-latency one does to about every other
//! picture while it pays an IDR back on a slow link. A later picture
//! replaces a dropped one, but when the dropped one was the last change (a
//! typed character, the end of a scroll) nothing replaces it, and the viewer
//! would keep the picture before until the liveness repeat. So the last
//! picture is submitted again at the next tick, as a new picture, when the
//! newest unit handed over was dropped: at most `MAX_RESUBMITS` times a
//! picture, until the encoder keeps one or a new picture comes.

/// What a unit handed to the encoder carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unit {
    /// A new picture.
    Fresh,
    /// The last picture again, for a keyframe or the liveness repeat: the
    /// viewer was sent it already.
    Repeat,
    /// The last picture again because the encoder dropped the unit that
    /// carried it: the viewer never had it.
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

    /// Whether the viewer was sent this picture already
    /// (`VideoFormat::duplicate`); never a resubmit's.
    pub(crate) fn duplicate(self) -> bool {
        self == Self::Repeat
    }
}

/// Resubmits of one picture at most.
pub(crate) const MAX_RESUBMITS: u8 = 2;

/// Whether the last picture is due again: the encoder's output callback
/// says so and thread C takes it, under the encoder's lock.
#[derive(Debug, Default)]
pub(crate) struct Resubmit {
    due: bool,
    /// Resubmits of the last picture made since the encoder last kept a
    /// picture.
    made: u8,
}

impl Resubmit {
    /// A unit of `unit` was handed over: a new picture starts the count
    /// again.
    pub(crate) fn submitted(&mut self, unit: Unit) {
        match unit {
            Unit::Fresh => self.made = 0,
            Unit::Resubmit => self.made = self.made.saturating_add(1),
            Unit::Repeat => {}
        }
    }

    /// The encoder dropped a unit of `unit`; `newest` when nothing handed
    /// over after it is still in flight, so that no later unit replaces it.
    /// Whether a resubmit became due, which thread C is woken for.
    pub(crate) fn dropped(&mut self, unit: Unit, newest: bool) -> bool {
        if unit.duplicate() || !newest || self.made >= MAX_RESUBMITS {
            return false;
        }
        !std::mem::replace(&mut self.due, true)
    }

    /// The encoder kept a unit of `unit`: a picture reached the viewer.
    pub(crate) fn kept(&mut self, unit: Unit) {
        if !unit.duplicate() {
            self.made = 0;
        }
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
    /// drop leaves it to the liveness repeat. A new picture, or a picture
    /// the encoder keeps, starts the count again.
    #[test]
    fn a_dropped_newest_picture_is_resubmitted_at_most_twice() {
        let mut resubmit = Resubmit::default();
        assert!(resubmit.dropped(Unit::Fresh, true));
        assert!(!resubmit.dropped(Unit::Fresh, true), "woken once");
        assert!(resubmit.due() && resubmit.take() && !resubmit.take());
        for made in 1..=MAX_RESUBMITS {
            resubmit.submitted(Unit::Resubmit);
            let again = resubmit.dropped(Unit::Resubmit, true);
            assert_eq!(again, made < MAX_RESUBMITS, "resubmit {made}");
            assert_eq!(resubmit.take(), again);
        }
        // A repeat is not a resubmit, and starts nothing.
        resubmit.submitted(Unit::Repeat);
        assert!(!resubmit.dropped(Unit::Resubmit, true));
        // A new picture has resubmits of its own.
        resubmit.submitted(Unit::Fresh);
        assert!(resubmit.dropped(Unit::Fresh, true) && resubmit.take());
        for _ in 0..MAX_RESUBMITS {
            resubmit.submitted(Unit::Resubmit);
        }
        assert!(!resubmit.dropped(Unit::Resubmit, true));
        // Once the encoder keeps a picture, the next drop starts again.
        resubmit.kept(Unit::Resubmit);
        assert!(resubmit.dropped(Unit::Resubmit, true));
    }

    /// A repeat the viewer has, or a unit with a later one in flight that
    /// replaces it, is not sent again; a kept repeat says nothing of the
    /// dropped picture.
    #[test]
    fn a_duplicate_or_a_superseded_drop_is_not_resubmitted() {
        let mut resubmit = Resubmit::default();
        assert!(!resubmit.dropped(Unit::Repeat, true));
        assert!(!resubmit.dropped(Unit::Fresh, false));
        assert!(!resubmit.dropped(Unit::Resubmit, false));
        assert!(!resubmit.due());
        for _ in 0..MAX_RESUBMITS {
            resubmit.submitted(Unit::Resubmit);
        }
        resubmit.kept(Unit::Repeat);
        assert!(!resubmit.dropped(Unit::Resubmit, true));
    }

    /// The last picture again is a duplicate only as a repeat: a resubmit
    /// carries a picture the viewer never had, and a new picture wins.
    #[test]
    fn a_resubmit_is_not_marked_duplicate() {
        assert_eq!(Unit::of(true, true), Unit::Fresh);
        assert_eq!(Unit::of(true, false), Unit::Fresh);
        assert_eq!(Unit::of(false, true), Unit::Resubmit);
        assert_eq!(Unit::of(false, false), Unit::Repeat);
        assert!(!Unit::Resubmit.duplicate() && !Unit::Fresh.duplicate());
        assert!(Unit::Repeat.duplicate());
    }
}
