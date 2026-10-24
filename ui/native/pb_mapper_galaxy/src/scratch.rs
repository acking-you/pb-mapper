//! The frame's temporaries, reused from frame to frame; the coordinates
//! themselves are written straight into the caller's buffer.
use super::{Hit, Out, Projected, Rect};

type Point = (f64, f64);
type Placed = (usize, Projected, f64, bool);

/// An orbit's runs on one side of its star (behind or in front).
#[derive(Default)]
pub(super) struct Runs {
    values: Vec<Vec<Point>>,
    used: usize,
}

impl Runs {
    pub(super) fn clear(&mut self) {
        self.used = 0;
    }
    pub(super) fn begin(&mut self, previous: Option<Point>, point: Point) -> usize {
        let at = self.used;
        if at == self.values.len() {
            self.values.push(Vec::new());
        }
        let run = &mut self.values[at];
        run.clear();
        run.extend(previous);
        run.push(point);
        self.used += 1;
        at
    }
    pub(super) fn push(&mut self, at: usize, point: Point) {
        self.values[at].push(point);
    }
    pub(super) fn last(&self, at: usize) -> Option<Point> {
        self.values[at].last().copied()
    }
    pub(super) fn write(&self, out: &mut Out) {
        out.push(self.used as f64);
        for run in &self.values[..self.used] {
            out.points(run);
        }
    }
    fn bytes(&self) -> usize {
        self.values.capacity() * std::mem::size_of::<Vec<Point>>()
            + self
                .values
                .iter()
                .map(|r| r.capacity() * std::mem::size_of::<Point>())
                .sum::<usize>()
    }
}

pub(super) struct Scratch {
    pub(super) circle: [(f64, f64); 97],
    pub(super) ordered: Vec<(usize, Projected)>,
    pub(super) hits: Vec<Hit>,
    pub(super) back: Runs,
    pub(super) front: Runs,
    pub(super) placed: Vec<Placed>,
    pub(super) label_order: Vec<usize>,
    pub(super) label_stars: Vec<Rect>,
    pub(super) label_rects: Vec<Rect>,
}

impl Scratch {
    /// Starts afresh once an unusually large scene has grown the temporaries
    /// past 1 MiB, so they do not stay that large.
    pub(super) fn trim(&mut self) {
        let bytes = self.back.bytes()
            + self.front.bytes()
            + self.ordered.capacity() * std::mem::size_of::<(usize, Projected)>()
            + self.hits.capacity() * std::mem::size_of::<Hit>()
            + self.placed.capacity() * std::mem::size_of::<Placed>()
            + self.label_order.capacity() * std::mem::size_of::<usize>()
            + (self.label_stars.capacity() + self.label_rects.capacity())
                * std::mem::size_of::<Rect>();
        if bytes > 1 << 20 {
            *self = Self::default();
        }
    }
}

impl Default for Scratch {
    fn default() -> Self {
        Self {
            circle: std::array::from_fn(|i| {
                let a = i as f64 / 96.0 * std::f64::consts::PI * 2.0;
                (a.cos(), a.sin())
            }),
            ordered: Vec::new(),
            hits: Vec::new(),
            back: Runs::default(),
            front: Runs::default(),
            placed: Vec::new(),
            label_order: Vec::new(),
            label_stars: Vec::new(),
            label_rects: Vec::new(),
        }
    }
}
