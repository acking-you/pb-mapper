//! The galaxy view's per-frame geometry, exported for `dart:ffi`. The geometry
//! itself lives in `pb-mapper-galaxy`; this is only the pointer boundary.

use std::slice;

/// Larger buffers than these are refused as malformed.
const MAX_SCENE: u32 = 1 << 22;
const MAX_OUT: u32 = 1 << 24;

/// Computes one galaxy frame through [`pb_mapper_galaxy::compute`] (the
/// buffer layouts are in [`pb_mapper_galaxy`]'s crate docs).
///
/// Returns the number of values written to `out`; when `capacity` is too
/// small, minus the number needed (and `out` holds nothing usable); -1 for a
/// null pointer, an oversized buffer, or a malformed scene or frame.
///
/// A leaf call, unlike the JSON-envelope exports: it allocates nothing the
/// caller must free, takes no lock, makes no callback, and catches any panic
/// (reported as -1), so the UI can call it once per frame on its own thread.
///
/// # Safety
///
/// `scene`, `frame` and `out` must be valid and aligned for `scene_len`,
/// `frame_len` and `capacity` elements for the duration of the call, and
/// `out` must not overlap the other two. `out` may be null only when
/// `capacity` is 0. The caller keeps ownership of all three.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_galaxy_frame_v1(
    scene: *const f64,
    scene_len: u32,
    frame: *const f64,
    frame_len: u32,
    out: *mut f32,
    capacity: u32,
) -> i64 {
    if scene.is_null() || frame.is_null() || scene_len > MAX_SCENE || capacity > MAX_OUT {
        return -1;
    }
    if out.is_null() && capacity > 0 {
        return -1;
    }
    // SAFETY: both pointers are non-null, and the caller guarantees each is
    // valid and aligned for its length for the duration of the call.
    let (scene, frame) = unsafe {
        (
            slice::from_raw_parts(scene, scene_len as usize),
            slice::from_raw_parts(frame, frame_len as usize),
        )
    };
    let out: &mut [f32] = if capacity == 0 {
        &mut []
    } else {
        // SAFETY: as above; `out` is non-null, writable for `capacity`
        // elements, and overlaps neither input.
        unsafe { slice::from_raw_parts_mut(out, capacity as usize) }
    };
    pb_mapper_galaxy::compute(scene, frame, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;

    /// A scene with nothing in it: still a whole frame (an undrawn plane and
    /// empty counts), so the length convention can be checked on its own.
    const EMPTY_SCENE: [f64; 5] = [pb_mapper_galaxy::VERSION, 0.0, 0.0, 0.0, 0.0];
    const FRAME: [f64; 13] = [
        800.0, 600.0, 0.6, 0.52, 9.0, 0.0, 0.0, 0.0, 1.0, 0.0, -1.0, -1.0, -1.0,
    ];

    fn call(scene: *const f64, scene_len: u32, out: &mut [f32]) -> i64 {
        let out_ptr = if out.is_empty() {
            ptr::null_mut()
        } else {
            out.as_mut_ptr()
        };
        // SAFETY: every non-null pointer comes from a live slice of that length.
        unsafe {
            pb_mapper_galaxy_frame_v1(
                scene,
                scene_len,
                FRAME.as_ptr(),
                FRAME.len() as u32,
                out_ptr,
                out.len() as u32,
            )
        }
    }

    /// What the export adds over `compute`: the pointer and size checks, and
    /// a null `out` standing for "tell me how much room you need".
    #[test]
    fn the_export_checks_pointers_and_then_defers_to_compute() {
        let scene = EMPTY_SCENE;
        let needed = call(scene.as_ptr(), scene.len() as u32, &mut []);
        assert!(needed < -1, "{needed}");

        let mut out = vec![0f32; (-needed) as usize];
        assert_eq!(call(scene.as_ptr(), scene.len() as u32, &mut out), -needed);
        let mut direct = vec![0f32; out.len()];
        assert_eq!(
            pb_mapper_galaxy::compute(&scene, &FRAME, &mut direct),
            -needed
        );
        assert_eq!(out, direct);

        assert_eq!(call(ptr::null(), 0, &mut out), -1);
        assert_eq!(call(scene.as_ptr(), MAX_SCENE + 1, &mut out), -1);
        // SAFETY: the null `frame` is refused before anything is read.
        let null_frame = unsafe {
            pb_mapper_galaxy_frame_v1(scene.as_ptr(), 5, ptr::null(), 13, ptr::null_mut(), 0)
        };
        assert_eq!(null_frame, -1);
        // SAFETY: a null `out` with room claimed is refused before any write.
        let null_out = unsafe {
            pb_mapper_galaxy_frame_v1(scene.as_ptr(), 5, FRAME.as_ptr(), 13, ptr::null_mut(), 8)
        };
        assert_eq!(null_out, -1);
        // SAFETY: an oversized capacity is refused before `out` is touched.
        let oversized = unsafe {
            pb_mapper_galaxy_frame_v1(
                scene.as_ptr(),
                5,
                FRAME.as_ptr(),
                13,
                out.as_mut_ptr(),
                MAX_OUT + 1,
            )
        };
        assert_eq!(oversized, -1);
    }
}
