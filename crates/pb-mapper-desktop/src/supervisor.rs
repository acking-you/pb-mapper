//! Reconnecting a viewer by policy: backoff within a window, a limit on
//! protocol failures in a row, a stall that counts as an end, and H.265
//! falling back to H.264 after a decoder failure.
