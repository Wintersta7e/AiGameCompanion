//! Rectangles on a captured frame.

/// A rectangle in a frame's pixels, its origin at the frame's top-left corner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PixelRect {
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
}
