//! macOS display modes, by display UUID: listed with
//! `CGDisplayCopyAllDisplayModes`, low-resolution twins included, and
//! changed only for this app (`kCGConfigureForAppOnly`): the window server
//! puts the owner's mode back when the app exits, a crash included, and
//! never keeps one a host set. A change's scope is this process and the
//! display, so a journal entry a dead process left is stale: macOS already
//! undid it.

use super::{
    cf,
    desktop::{by_name, display_lost},
    mode_list::{self, Raw},
};
use crate::{
    Result, error,
    modes::{Mode, Setting},
    platform::DisplayModes,
};
use objc2_core_foundation::{CFArray, CFRetained};
use objc2_core_graphics::{
    CGBeginDisplayConfiguration, CGCancelDisplayConfiguration, CGCompleteDisplayConfiguration,
    CGConfigureDisplayWithDisplayMode, CGConfigureOption, CGDirectDisplayID,
    CGDisplayCopyAllDisplayModes, CGDisplayCopyDisplayMode, CGDisplayMode, CGDisplayRotation,
    CGRestorePermanentDisplayConfiguration, kCGDisplayShowDuplicateLowResolutionModes,
};
use std::sync::OnceLock;

/// The modes of the display a UUID names.
pub(crate) struct MacModes;

fn display(device: &str) -> Result<CGDirectDisplayID> {
    by_name(device).ok_or_else(|| display_lost("no display has that name"))
}

/// `mode` as the list reads it, turned like the display.
fn raw(mode: &CGDisplayMode, turned: bool) -> Raw {
    let side = |value: usize| u32::try_from(value).unwrap_or(u32::MAX);
    let mut pixels = (
        side(CGDisplayMode::pixel_width(Some(mode))),
        side(CGDisplayMode::pixel_height(Some(mode))),
    );
    let mut points = (
        side(CGDisplayMode::width(Some(mode))),
        side(CGDisplayMode::height(Some(mode))),
    );
    if turned {
        pixels = (pixels.1, pixels.0);
        points = (points.1, points.0);
    }
    Raw {
        pixels,
        points,
        hz: CGDisplayMode::refresh_rate(Some(mode))
            .round()
            .clamp(0.0, 1000.0) as u32,
        usable: CGDisplayMode::is_usable_for_desktop_gui(Some(mode)),
    }
}

/// A display on its side lists its modes upright.
fn turned(id: CGDirectDisplayID) -> bool {
    let degrees = CGDisplayRotation(id).round() as i64;
    degrees.rem_euclid(180) == 90
}

/// Every mode of `id`, low-resolution twins included, with its raw form.
fn all(id: CGDirectDisplayID) -> Vec<(CFRetained<CGDisplayMode>, Raw)> {
    // SAFETY: a static key, alive for the process.
    let key = unsafe { kCGDisplayShowDuplicateLowResolutionModes };
    let options = cf::dictionary(&[(key, cf::boolean(true))]);
    // SAFETY: the options are a dictionary of a CF string to a CF boolean.
    let Some(modes) = (unsafe { CGDisplayCopyAllDisplayModes(id, Some(&options)) }) else {
        return Vec::new();
    };
    // SAFETY: the array holds display modes.
    let modes = unsafe { CFRetained::cast_unchecked::<CFArray<CGDisplayMode>>(modes) };
    let turned = turned(id);
    modes
        .iter()
        .map(|mode| {
            let raw = raw(&mode, turned);
            (mode, raw)
        })
        .collect()
}

fn current_raw(id: CGDirectDisplayID) -> Result<Raw> {
    let mode = CGDisplayCopyDisplayMode(id)
        .ok_or_else(|| display_lost("the display's mode cannot be read"))?;
    Ok(raw(&mode, turned(id)))
}

/// Set `mode` on `id` for this app only.
fn configure(id: CGDirectDisplayID, mode: &CGDisplayMode) -> Result<()> {
    let mut config = std::ptr::null_mut();
    // SAFETY: `config` is a local the call fills.
    let begun = unsafe { CGBeginDisplayConfiguration(&mut config) };
    if begun.0 != 0 || config.is_null() {
        return Err(error(format!(
            "macOS refused a display configuration ({})",
            begun.0
        )));
    }
    // SAFETY: `config` was just begun; `mode` is one of `id`'s.
    let configured = unsafe { CGConfigureDisplayWithDisplayMode(config, id, Some(mode), None) };
    if configured.0 != 0 {
        // SAFETY: an open configuration is cancelled once.
        unsafe { CGCancelDisplayConfiguration(config) };
        return Err(error(format!(
            "the display refused the mode ({})",
            configured.0
        )));
    }
    // SAFETY: completing consumes the configuration.
    let completed =
        unsafe { CGCompleteDisplayConfiguration(config, CGConfigureOption::ForAppOnly) };
    if completed.0 != 0 {
        return Err(error(format!(
            "the display refused the mode change ({})",
            completed.0
        )));
    }
    Ok(())
}

impl DisplayModes for MacModes {
    const SUPPORTED: bool = true;
    fn available(&mut self, device: &str) -> bool {
        display(device).is_ok_and(|id| !all(id).is_empty())
    }
    fn list(&mut self, device: &str) -> Result<Vec<Mode>> {
        let id = display(device)?;
        let modes: Vec<Raw> = all(id).into_iter().map(|(_, raw)| raw).collect();
        Ok(mode_list::offered(&modes, &current_raw(id)?)
            .into_iter()
            .map(|mode| mode.size())
            .collect())
    }
    fn current(&mut self, device: &str) -> Result<Setting> {
        Ok(current_raw(display(device)?)?.setting())
    }
    fn target(&mut self, device: &str, mode: Mode) -> Result<Setting> {
        let id = display(device)?;
        let modes: Vec<Raw> = all(id).into_iter().map(|(_, raw)| raw).collect();
        mode_list::offered(&modes, &current_raw(id)?)
            .into_iter()
            .find(|offered| offered.size() == mode)
            .map(|offered| offered.setting())
            .ok_or_else(|| error("the display does not offer that resolution"))
    }
    fn apply(&mut self, device: &str, setting: &Setting) -> Result<()> {
        let id = display(device)?;
        let modes = all(id);
        let raws: Vec<Raw> = modes.iter().map(|(_, raw)| *raw).collect();
        let wanted = mode_list::find(&raws, setting)
            .ok_or_else(|| error("the display does not offer that resolution"))?;
        let (mode, _) = modes
            .iter()
            .find(|(_, raw)| *raw == wanted)
            .ok_or_else(|| error("the display does not offer that resolution"))?;
        configure(id, mode)
    }
    /// The owner's mode exactly; where the display no longer lists it, the
    /// configuration the owner keeps.
    fn reset(&mut self, device: &str, original: &Setting) -> Result<()> {
        if self.apply(device, original).is_err() {
            display(device)?;
            CGRestorePermanentDisplayConfiguration();
        }
        Ok(())
    }
    fn scope(&mut self, device: &str) -> Result<String> {
        display(device)?;
        Ok(format!("process {}; display {device}", process()))
    }
}

/// This process: its id and when it first changed a mode, which no later
/// process with that id repeats.
fn process() -> &'static str {
    static PROCESS: OnceLock<String> = OnceLock::new();
    PROCESS.get_or_init(|| {
        format!(
            "{} since {}",
            std::process::id(),
            pb_mapper_core::time::unix_millis()
        )
    })
}
