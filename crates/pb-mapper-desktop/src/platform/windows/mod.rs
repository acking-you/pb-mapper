//! The Windows host: the ordinary desktop, the DXGI to Media Foundation
//! encoder, DPAPI and the privileged helper.
pub(crate) mod desktop;
pub mod dpapi;
pub mod helper;
pub mod video;

#[cfg(feature = "windows-helper")]
pub(crate) use desktop::create_privileged;
pub(crate) use desktop::{create, displays, primary};
