//! Which shape each of the viewer's shape slots holds, as B-send defined
//! them: the host's half of the slot tables, the viewer keeping the other.

use crate::{
    CursorShape,
    protocol::{CURSOR_SLOTS, ShapeSlot},
};

/// The shapes B-send wrote, by slot. A shape no slot holds takes a free
/// slot, or else the one named longest ago, so the table never grows past
/// `CURSOR_SLOTS` and needs no eviction message: the viewer's slot simply
/// gets the new definition.
#[derive(Default)]
pub(super) struct ShapeSlots {
    /// Each slot's shape, and when a report last named it.
    slots: Vec<(CursorShape, u64)>,
    /// Counts the reports that named a shape.
    clock: u64,
    /// The slot named last, which the next report most likely names too.
    last: usize,
}
impl ShapeSlots {
    /// The slot that holds `shape`, and whether B-send must define it there
    /// first.
    pub(super) fn slot(&mut self, shape: &CursorShape) -> (ShapeSlot, bool) {
        self.clock += 1;
        let clock = self.clock;
        let held = |slot: &(CursorShape, u64)| slot.0.shares_bitmap(shape) || slot.0 == *shape;
        let found = if self.slots.get(self.last).is_some_and(held) {
            Some(self.last)
        } else {
            self.slots.iter().position(held)
        };
        let (index, define) = match found {
            Some(index) => (index, false),
            None if self.slots.len() < CURSOR_SLOTS => {
                self.slots.push((shape.clone(), clock));
                (self.slots.len() - 1, true)
            }
            None => {
                let oldest = self
                    .slots
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, (_, named))| *named)
                    .map_or(0, |(index, _)| index);
                if let Some(slot) = self.slots.get_mut(oldest) {
                    slot.0 = shape.clone();
                }
                (oldest, true)
            }
        };
        if let Some(slot) = self.slots.get_mut(index) {
            slot.1 = clock;
        }
        self.last = index;
        // The table never holds more than `CURSOR_SLOTS`.
        let slot = ShapeSlot::new(index).unwrap_or_else(|| unreachable!("slot {index} in bounds"));
        (slot, define)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CursorKind;

    fn shape(shade: u8) -> CursorShape {
        CursorShape::new(CursorKind::Custom, 1, 1, (0, 0), vec![shade, 0, 0, 255]).unwrap()
    }

    /// An equal shape names its slot without a definition, under a new
    /// bitmap too; a new one takes a free slot, and once all are taken,
    /// overwrites the one named longest ago, never the one in use.
    #[test]
    fn a_full_table_overwrites_the_slot_named_longest_ago() {
        let mut slots = ShapeSlots::default();
        let defined: Vec<_> = (0..CURSOR_SLOTS as u8)
            .map(|shade| slots.slot(&shape(shade)))
            .collect();
        assert!(
            defined
                .iter()
                .enumerate()
                .all(|(index, (slot, define))| { slot.index() == index && *define })
        );
        // Slot 0 is named again, under an equal shape read anew.
        assert_eq!(slots.slot(&shape(0)), (defined[0].0, false));
        let (slot, define) = slots.slot(&shape(200));
        assert_eq!((slot.index(), define), (1, true));
        assert_eq!(slots.slot(&shape(200)), (slot, false));
        // The shape it replaced is new again.
        assert_eq!(slots.slot(&shape(1)).0.index(), 2);
    }
}
