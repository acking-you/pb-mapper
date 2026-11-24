//! `video_bench --gpu-time`'s: D3D11 timestamp queries around the GPU work
//! each submitted picture queues on the capture device's immediate context
//! (a new picture's conversion, with its copy out of the duplication where
//! there is one; a repeat's copy of the last sample), read back once the GPU
//! has them, never waited for. A session never times.
//!
//! The span is what the device's timestamps saw between the two: a video
//! processor that runs on another engine of the GPU may not lie wholly
//! inside it, which GPUView or PIX tells. A new picture of an HDR desktop
//! also has a timestamp after its tone map's draw (`Split`), which times
//! that pass apart.
use crate::{Result, error};
use std::collections::VecDeque;
use windows::{
    Win32::{Foundation::S_OK, Graphics::Direct3D11::*},
    core::Interface,
};

/// The spans the GPU may still owe: beyond them a picture goes untimed
/// rather than queries piling up behind a stalled GPU.
const MAX_ISSUED: usize = 16;

/// What the GPU took for one submitted picture's work, from the timestamp
/// before it to the one after.
#[derive(Clone, Copy, Debug)]
pub struct GpuSpan {
    /// A repeat of the last picture, a copy of its NV12 sample; else a new
    /// picture's conversion.
    pub repeat: bool,
    pub micros: f64,
    /// For a new picture of an HDR desktop, the part of `micros` before
    /// the timestamp after its tone map's draw: that pass, with the copy
    /// it reads from where it makes one. `None` for any other.
    pub tone_map: Option<f64>,
}

/// One span's queries: the disjoint query around two timestamps, and the
/// timestamp a `Split` may take between them.
pub(super) struct Queries {
    disjoint: ID3D11Query,
    start: ID3D11Query,
    split: ID3D11Query,
    end: ID3D11Query,
}
impl Queries {
    fn new(device: &ID3D11Device) -> Result<Self> {
        let query = |kind| {
            let mut query = None;
            unsafe {
                device
                    .CreateQuery(
                        &D3D11_QUERY_DESC {
                            Query: kind,
                            MiscFlags: 0,
                        },
                        Some(&mut query),
                    )
                    .map_err(error)?;
            }
            query.ok_or_else(|| error("missing D3D11 query"))
        };
        Ok(Self {
            disjoint: query(D3D11_QUERY_TIMESTAMP_DISJOINT)?,
            start: query(D3D11_QUERY_TIMESTAMP)?,
            split: query(D3D11_QUERY_TIMESTAMP)?,
            end: query(D3D11_QUERY_TIMESTAMP)?,
        })
    }
}

/// Where a picture's work, timed or not, says its tone map's draw is
/// queued: the timestamp `GpuSpan::tone_map` ends at, taken once, and only
/// while timed.
pub(super) struct Split<'a> {
    timed: Option<(&'a ID3D11DeviceContext, &'a ID3D11Query)>,
    marked: bool,
}
impl<'a> Split<'a> {
    /// Inside the span of `queries` on `context`, or untimed.
    pub(super) fn new(context: &'a ID3D11DeviceContext, queries: Option<&'a Queries>) -> Self {
        Self {
            timed: queries.map(|queries| (context, &queries.split)),
            marked: false,
        }
    }

    /// The tone map's draw is queued.
    pub(super) fn tone_mapped(&mut self) {
        if let Some((context, query)) = self.timed.take() {
            // SAFETY: a timestamp query of this context's device, ended on
            // the context that queues the span's work.
            unsafe { context.End(query) };
            self.marked = true;
        }
    }

    /// Whether the split's timestamp was taken.
    pub(super) fn marked(&self) -> bool {
        self.marked
    }
}

/// A span issued and not yet read: its queries, whether it was a repeat's,
/// and whether its split was taken.
struct Issued {
    queries: Queries,
    repeat: bool,
    split: bool,
}

/// The spans issued and not yet read, oldest first, and read ones kept for
/// reuse.
#[derive(Default)]
pub(super) struct GpuTimer {
    issued: VecDeque<Issued>,
    free: Vec<Queries>,
}
impl GpuTimer {
    /// Open a span: the work `context` queues until `end` is timed. `None`
    /// while `MAX_ISSUED` spans are unread.
    pub(super) fn begin(
        &mut self,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
    ) -> Result<Option<Queries>> {
        if self.issued.len() >= MAX_ISSUED {
            return Ok(None);
        }
        let queries = match self.free.pop() {
            Some(queries) => queries,
            None => Queries::new(device)?,
        };
        unsafe {
            context.Begin(&queries.disjoint);
            context.End(&queries.start);
        }
        Ok(Some(queries))
    }

    /// Close the span `begin` opened, for a `repeat` or a new picture,
    /// whose `split` was taken or not.
    pub(super) fn end(
        &mut self,
        context: &ID3D11DeviceContext,
        queries: Queries,
        repeat: bool,
        split: bool,
    ) {
        unsafe {
            context.End(&queries.end);
            context.End(&queries.disjoint);
        }
        self.issued.push_back(Issued {
            queries,
            repeat,
            split,
        });
    }

    /// Move the spans the GPU has finished into `out`, in the order they
    /// were opened. A span whose timestamps the GPU calls disjoint (its
    /// clock changed meanwhile) is left out.
    pub(super) fn collect(&mut self, context: &ID3D11DeviceContext, out: &mut Vec<GpuSpan>) {
        while let Some(Issued { queries, split, .. }) = self.issued.front() {
            let Some(clock) =
                data::<D3D11_QUERY_DATA_TIMESTAMP_DISJOINT>(context, &queries.disjoint)
            else {
                break;
            };
            // The disjoint query ended after the timestamps, so they are in.
            let span = data::<u64>(context, &queries.start)
                .zip(data::<u64>(context, &queries.end))
                .filter(|_| !clock.Disjoint.as_bool() && clock.Frequency > 0);
            let split = split
                .then(|| data::<u64>(context, &queries.split))
                .flatten();
            let Some(Issued {
                queries, repeat, ..
            }) = self.issued.pop_front()
            else {
                break;
            };
            if let Some((start, end)) = span {
                let micros =
                    |to: u64| to.saturating_sub(start) as f64 * 1e6 / clock.Frequency as f64;
                out.push(GpuSpan {
                    repeat,
                    micros: micros(end),
                    tone_map: split.map(micros),
                });
            }
            self.free.push(queries);
        }
    }
}

/// `query`'s result once the GPU has it, without flushing. `GetData` says
/// "not yet" with `S_FALSE`, which the `windows` crate's wrapper takes for
/// success, so its entry is called directly and only `S_OK` counts.
fn data<T: Default>(context: &ID3D11DeviceContext, query: &ID3D11Query) -> Option<T> {
    let mut value = T::default();
    // SAFETY: `GetData` writes at most `size_of::<T>()` bytes into `value`,
    // which is that large, and `T` is the result type of `query`'s kind;
    // `ID3D11Query` derives from `ID3D11Asynchronous`, so its pointer is one.
    let result = unsafe {
        (Interface::vtable(context).GetData)(
            context.as_raw(),
            query.as_raw(),
            (&mut value as *mut T).cast(),
            size_of::<T>() as u32,
            D3D11_ASYNC_GETDATA_DONOTFLUSH.0 as u32,
        )
    };
    (result == S_OK).then_some(value)
}
