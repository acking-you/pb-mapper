//! The pointer as the injector leaves it. `CGEventPost` returns before the
//! window server applies a move, so the pointer read right after a move can
//! still show where it was: until the window server shows the injected
//! pixel, or `SETTLE` passes (the system stopped the move short, or someone
//! at the Mac moved the mouse), the injected pixel is the pointer
//! (`input::cursor::Pointer`, which the Windows host shares).

use crate::input::cursor::Pointer;
use std::{cell::Cell, rc::Rc};

/// The injector's and the cursor's, on thread C.
pub(crate) type Shared = Rc<Cell<Pointer>>;
