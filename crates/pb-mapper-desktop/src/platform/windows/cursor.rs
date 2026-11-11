//! The pointer on Windows: where it is, whether it shows, and its shape.
use crate::{CursorKind, CursorMode, CursorShape, Result, error, platform::CursorSource};
use windows::Win32::{
    Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
        DeleteObject, GetDIBits, GetObjectW, HBITMAP, HDC, RGBQUAD,
    },
    UI::WindowsAndMessaging::{
        CURSOR_SHOWING, CURSOR_SUPPRESSED, CURSORINFO, GetCursorInfo, GetIconInfo, HCURSOR, HICON,
        ICONINFO, IDC_APPSTARTING, IDC_ARROW, IDC_CROSS, IDC_HAND, IDC_HELP, IDC_IBEAM, IDC_NO,
        IDC_SIZEALL, IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE, IDC_UPARROW, IDC_WAIT,
        LoadCursorW,
    },
};
use windows::core::PCWSTR;

/// The system cursors a viewer's OS has too, by the id `LoadCursorW` takes.
const STANDARD: [(PCWSTR, CursorKind); 14] = [
    (IDC_ARROW, CursorKind::Arrow),
    (IDC_IBEAM, CursorKind::Ibeam),
    (IDC_HAND, CursorKind::Hand),
    (IDC_WAIT, CursorKind::Wait),
    (IDC_APPSTARTING, CursorKind::Progress),
    (IDC_CROSS, CursorKind::Crosshair),
    (IDC_HELP, CursorKind::Help),
    (IDC_NO, CursorKind::NotAllowed),
    (IDC_SIZEALL, CursorKind::Move),
    (IDC_SIZENS, CursorKind::ResizeNs),
    (IDC_SIZEWE, CursorKind::ResizeEw),
    (IDC_SIZENWSE, CursorKind::ResizeNwse),
    (IDC_SIZENESW, CursorKind::ResizeNesw),
    (IDC_UPARROW, CursorKind::UpArrow),
];

#[derive(Default)]
pub(crate) struct WinCursor {
    /// The cursor the last `read` saw; null when none shows.
    seen: HCURSOR,
    /// The cursor whose shape `shape` last reported, or failed to read.
    shaped: HCURSOR,
    /// The system cursors' shared handles, loaded at the first shape read.
    standard: Vec<(HCURSOR, CursorKind)>,
}
impl CursorSource for WinCursor {
    const MODE: CursorMode = CursorMode::Separate;
    const SHAPES: bool = true;
    fn read(&mut self) -> Result<Option<(i32, i32, bool)>> {
        let mut cursor = CURSORINFO {
            cbSize: size_of::<CURSORINFO>() as u32,
            ..Default::default()
        };
        unsafe {
            GetCursorInfo(&mut cursor).map_err(error)?;
        }
        let showing =
            cursor.flags.0 & CURSOR_SHOWING.0 != 0 && cursor.flags.0 & CURSOR_SUPPRESSED.0 == 0;
        self.seen = cursor.hCursor;
        let point = cursor.ptScreenPos;
        Ok(Some((point.x, point.y, showing)))
    }
    /// A new handle is read once: its kind by the system cursors' handles,
    /// its bitmap through `GetIconInfo`.
    fn shape(&mut self) -> Result<Option<CursorShape>> {
        if self.seen == self.shaped || self.seen.is_invalid() {
            return Ok(None);
        }
        self.shaped = self.seen;
        if self.standard.is_empty() {
            self.standard = STANDARD
                .iter()
                // SAFETY: a system cursor id; a shared handle, never freed.
                .filter_map(|(id, kind)| Some((unsafe { LoadCursorW(None, *id) }.ok()?, *kind)))
                .collect();
        }
        let kind = self
            .standard
            .iter()
            .find(|(handle, _)| *handle == self.seen)
            .map_or(CursorKind::Custom, |(_, kind)| *kind);
        read_shape(self.seen, kind).map(Some)
    }
}

/// The two bitmaps `GetIconInfo` makes, deleted however the read ends.
struct IconBitmaps(ICONINFO);
impl Drop for IconBitmaps {
    fn drop(&mut self) {
        for bitmap in [self.0.hbmColor, self.0.hbmMask] {
            if !bitmap.is_invalid() {
                // SAFETY: GetIconInfo created it for this caller, who owns it.
                let _ = unsafe { DeleteObject(bitmap.into()) };
            }
        }
    }
}
/// A memory DC for `GetDIBits`, deleted when dropped.
struct MemoryDc(HDC);
impl Drop for MemoryDc {
    fn drop(&mut self) {
        // SAFETY: CreateCompatibleDC made it, and nothing else holds it.
        let _ = unsafe { DeleteDC(self.0) };
    }
}

/// `cursor`'s bitmap and hotspot: a colour cursor's own pixels, with its
/// alpha or else its AND mask; a monochrome one from its AND and XOR masks.
fn read_shape(cursor: HCURSOR, kind: CursorKind) -> Result<CursorShape> {
    let mut info = ICONINFO::default();
    // SAFETY: a cursor handle the system reported, and an out struct.
    unsafe { GetIconInfo(HICON::from(cursor), &mut info) }.map_err(error)?;
    let bitmaps = IconBitmaps(info);
    let (mask, color) = (bitmaps.0.hbmMask, bitmaps.0.hbmColor);
    let (width, mask_height) = size(mask)?;
    // A monochrome cursor's mask is its AND mask over its XOR mask.
    let height = if color.is_invalid() {
        mask_height / 2
    } else {
        mask_height
    };
    if width == 0 || height == 0 {
        return Err(error("an empty cursor"));
    }
    // SAFETY: a DC compatible with the screen, deleted by `MemoryDc`.
    let dc = MemoryDc(unsafe { CreateCompatibleDC(None) });
    if dc.0.is_invalid() {
        return Err(error("no memory DC for the cursor"));
    }
    let masks = mask_bits(&dc, mask, width, mask_height)?;
    let color = if color.is_invalid() {
        None
    } else {
        Some(color_bits(&dc, color, width, height)?)
    };
    let rgba = compose(color.as_deref(), &masks, width, height);
    CursorShape::fitted(
        kind,
        width,
        height,
        (bitmaps.0.xHotspot, bitmaps.0.yHotspot),
        rgba,
    )
}

/// A bitmap's width and height.
fn size(bitmap: HBITMAP) -> Result<(u32, u32)> {
    let mut header = BITMAP::default();
    // SAFETY: a bitmap handle and an out struct of the size passed.
    let read = unsafe {
        GetObjectW(
            bitmap.into(),
            size_of::<BITMAP>() as i32,
            Some((&mut header as *mut BITMAP).cast()),
        )
    };
    if read == 0 {
        return Err(error("cannot read the cursor's bitmap"));
    }
    Ok((
        u32::try_from(header.bmWidth).map_err(error)?,
        u32::try_from(header.bmHeight).map_err(error)?,
    ))
}

/// A `BITMAPINFO` with room for the two colours of a 1-bit DIB.
#[repr(C)]
struct MonoInfo {
    header: BITMAPINFOHEADER,
    colors: [RGBQUAD; 2],
}

fn header(width: u32, height: u32, bits: u16) -> Result<BITMAPINFOHEADER> {
    Ok(BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: i32::try_from(width).map_err(error)?,
        // Negative: top row first.
        biHeight: -i32::try_from(height).map_err(error)?,
        biPlanes: 1,
        biBitCount: bits,
        biCompression: BI_RGB.0,
        ..Default::default()
    })
}

/// `bitmap` as a 1-bit DIB, top row first, each row padded to 4 bytes.
fn mask_bits(dc: &MemoryDc, bitmap: HBITMAP, width: u32, height: u32) -> Result<Vec<u8>> {
    let mut info = MonoInfo {
        header: header(width, height, 1)?,
        colors: [RGBQUAD::default(); 2],
    };
    let mut bits = vec![0; mask_stride(width) * height as usize];
    // SAFETY: `bits` holds `height` rows of the stride a 1-bit DIB of this
    // width has, and `info` the header and both colours it describes.
    let rows = unsafe {
        GetDIBits(
            dc.0,
            bitmap,
            0,
            height,
            Some(bits.as_mut_ptr().cast()),
            (&mut info as *mut MonoInfo).cast::<BITMAPINFO>(),
            DIB_RGB_COLORS,
        )
    };
    if u32::try_from(rows).ok() != Some(height) {
        return Err(error("cannot read the cursor's mask"));
    }
    Ok(bits)
}

/// `bitmap` as a 32-bit BGRA DIB, top row first.
fn color_bits(dc: &MemoryDc, bitmap: HBITMAP, width: u32, height: u32) -> Result<Vec<u8>> {
    let mut info = BITMAPINFO {
        bmiHeader: header(width, height, 32)?,
        ..Default::default()
    };
    let mut bits = vec![0; CursorShape::bytes(width, height)];
    // SAFETY: `bits` holds `height` rows of `width` 4-byte pixels, as the
    // header asks for.
    let rows = unsafe {
        GetDIBits(
            dc.0,
            bitmap,
            0,
            height,
            Some(bits.as_mut_ptr().cast()),
            &mut info,
            DIB_RGB_COLORS,
        )
    };
    if u32::try_from(rows).ok() != Some(height) {
        return Err(error("cannot read the cursor's colours"));
    }
    Ok(bits)
}

fn mask_stride(width: u32) -> usize {
    (width as usize).div_ceil(32) * 4
}

/// What one pixel shows.
#[derive(Clone, Copy, PartialEq)]
enum Pixel {
    Clear,
    Paint([u8; 4]),
    /// Inverts the screen beneath: shown black, outlined in white.
    Invert,
}

/// Straight RGBA of a cursor: `color` its BGRA pixels, `None` for a
/// monochrome one, whose `masks` hold the AND rows over the XOR rows; a
/// colour one's hold the AND rows alone. A colour cursor with alpha keeps
/// it; one without takes its transparency from the AND mask. A pixel that
/// inverts the screen has no colour of its own: it is drawn black with a
/// white outline, so it shows on any picture.
fn compose(color: Option<&[u8]>, masks: &[u8], width: u32, height: u32) -> Vec<u8> {
    let stride = mask_stride(width);
    let bit = |row: usize, x: usize| {
        masks
            .get(row * stride + x / 8)
            .is_some_and(|byte| byte & (0x80 >> (x % 8)) != 0)
    };
    let alpha = color.is_some_and(|color| color.as_chunks::<4>().0.iter().any(|[.., a]| *a != 0));
    let (width, height) = (width as usize, height as usize);
    let pixels: Vec<Pixel> = (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .map(|(x, y)| {
            let and = bit(y, x);
            match color {
                Some(color) => {
                    let at = (y * width + x) * 4;
                    let Some(&[b, g, r, a]) = color.get(at..at + 4) else {
                        return Pixel::Clear;
                    };
                    match (alpha, and, [r, g, b] == [0; 3]) {
                        (true, ..) if a == 0 => Pixel::Clear,
                        (true, ..) => Pixel::Paint([r, g, b, a]),
                        (false, false, _) => Pixel::Paint([r, g, b, 255]),
                        (false, true, true) => Pixel::Clear,
                        (false, true, false) => Pixel::Invert,
                    }
                }
                None => match (and, bit(y + height, x)) {
                    (false, false) => Pixel::Paint([0, 0, 0, 255]),
                    (false, true) => Pixel::Paint([255; 4]),
                    (true, false) => Pixel::Clear,
                    (true, true) => Pixel::Invert,
                },
            }
        })
        .collect();
    let at = |x: usize, y: usize| pixels.get(y * width + x).copied();
    let beside_invert = |x: usize, y: usize| {
        [
            x.checked_sub(1).map(|x| (x, y)),
            (x + 1 < width).then_some((x + 1, y)),
            y.checked_sub(1).map(|y| (x, y)),
            (y + 1 < height).then_some((x, y + 1)),
        ]
        .into_iter()
        .flatten()
        .any(|(x, y)| at(x, y) == Some(Pixel::Invert))
    };
    let mut rgba = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        for x in 0..width {
            rgba.extend_from_slice(&match at(x, y) {
                Some(Pixel::Paint(pixel)) => pixel,
                Some(Pixel::Invert) => [0, 0, 0, 255],
                Some(Pixel::Clear) | None if beside_invert(x, y) => [255; 4],
                Some(Pixel::Clear) | None => [0; 4],
            });
        }
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A monochrome cursor: black, white, clear and an inverting pixel,
    /// which shows black with a white outline where it was clear.
    #[test]
    fn a_monochrome_cursor_inverts_as_black_outlined_in_white() {
        // 4 x 1: AND row then XOR row, MSB first, rows padded to 4 bytes.
        let masks = [0b0011_0000, 0, 0, 0, 0b0101_0000, 0, 0, 0];
        let rgba = compose(None, &masks, 4, 1);
        assert_eq!(
            rgba.chunks(4).collect::<Vec<_>>(),
            [[0, 0, 0, 255], [255; 4], [255; 4], [0, 0, 0, 255]],
        );
        // Clear but for one inverting pixel: outlined, and clear further on.
        let masks = [0b1111_0000, 0, 0, 0, 0b0100_0000, 0, 0, 0];
        let rgba = compose(None, &masks, 4, 1);
        assert_eq!(
            rgba.chunks(4).collect::<Vec<_>>(),
            [[255; 4], [0, 0, 0, 255], [255; 4], [0; 4]],
        );
    }
}
