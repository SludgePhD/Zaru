//! Image manipulation.
//!
//! # TODO
//!
//! ## View Clipping is problematic
//!
//! `view` and `view_mut` will clip the passed rectangle if it's partially outside the base view,
//! and return a zero-size view if it's completely outside. This is a problem if the caller doesn't
//! expect this:
//!
//! - the caller might then `blend` another image onto the view, or `blend` the view onto some other
//!   image, which will resize it if the view has an unexpected size.
//! - the user might use view-relative coordinates for computations, which are now offset by some
//!   number of pixels (this happened in `LandmarkTracker`).
//!
//! Alternative solutions:
//!
//! - Panic when the rectangle isn't completely inside the base view. This was the old behavior. It
//!   didn't work well because detection boxes are *typically* within the image, but sometimes
//!   aren't, and getting a panic only for edge cases you don't typically encounter is bad.
//! - Return `Option<_>`. This leaves it to the caller to handle the clipping.
//! - Always return a view of the requested size, but without backing data for any pixel outside
//!   the base view (ie. have writes ignored and return 0 on read).

mod blend;
mod draw;
mod rect;

#[cfg(test)]
mod tests;

use std::{fmt, ops::Index, path::Path};

use embedded_graphics::{pixelcolor::raw::RawU32, prelude::PixelColor};
use image::{GenericImage, GenericImageView, ImageBuffer, Rgba, RgbaImage};

use crate::resolution::Resolution;

pub use blend::*;
pub use draw::*;
pub use rect::*;

#[allow(dead_code)]
enum JpegBackend {
    JpegDecoder,
    Mozjpeg,
    ZuneJpeg,
}

const JPEG_BACKEND: JpegBackend = JpegBackend::ZuneJpeg;

#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
enum ImageFormat {
    Jpeg,
    Png,
}

impl ImageFormat {
    fn from_path(path: &Path) -> crate::Result<Self> {
        match path.extension().and_then(|ext| ext.to_str()) {
            Some("jpg" | "jpeg") => Ok(Self::Jpeg),
            Some("png") => Ok(Self::Png),
            _ => Err(format!(
                "invalid image path '{}' (must have one of the supported extensions)",
                path.display()
            )
            .into()),
        }
    }
}

/// An 8-bit sRGB image with alpha channel.
#[derive(Clone)]
pub struct Image {
    // Internal representation is meant to be compatible with wgpu's texture formats for easy GPU
    // up/downloading.
    pub(crate) buf: RgbaImage,
}

impl Image {
    /// Loads an image from the filesystem.
    ///
    /// The path must have a supported file extension (`jpeg`, `jpg` or `png`).
    pub fn load<A: AsRef<Path>>(path: A) -> Result<Self, crate::Error> {
        Self::load_impl(path.as_ref())
    }

    fn load_impl(path: &Path) -> Result<Self, crate::Error> {
        match ImageFormat::from_path(path)? {
            ImageFormat::Jpeg => {
                let data = std::fs::read(path)?;
                Self::decode_jpeg(&data)
            }
            ImageFormat::Png => {
                let data = std::fs::read(path)?;
                let buf =
                    image::load_from_memory_with_format(&data, image::ImageFormat::Png)?.to_rgba8();
                Ok(Self { buf })
            }
        }
    }

    /// Decodes a JFIF JPEG or Motion JPEG from a byte slice.
    pub fn decode_jpeg(data: &[u8]) -> Result<Self, crate::Error> {
        let buf = match JPEG_BACKEND {
            JpegBackend::JpegDecoder => {
                image::load_from_memory_with_format(data, image::ImageFormat::Jpeg)?.to_rgba8()
            }
            JpegBackend::Mozjpeg => {
                let decompressor = mozjpeg::Decompress::new_mem(data)?;
                let mut decomp = decompressor.rgba()?;
                let buf = decomp.read_scanlines_flat().unwrap();
                let buf = ImageBuffer::from_raw(decomp.width() as u32, decomp.height() as u32, buf)
                    .expect("failed to create ImageBuffer");
                buf
            }
            JpegBackend::ZuneJpeg => {
                let mut decomp = zune_jpeg::Decoder::new();
                decomp.set_num_threads(1)?;
                decomp.rgba();
                let buf = decomp.decode_buffer(data)?;
                let width = u32::from(decomp.width());
                let height = u32::from(decomp.height());
                let buf = ImageBuffer::from_raw(width, height, buf)
                    .expect("failed to create ImageBuffer");
                buf
            }
        };

        Ok(Self { buf })
    }

    /// Saves an image to the file system.
    ///
    /// The path must have a supported file extension (`jpeg`, `jpg` or `png`).
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<(), crate::Error> {
        self.save_impl(path.as_ref())
    }

    fn save_impl(&self, path: &Path) -> Result<(), crate::Error> {
        match ImageFormat::from_path(path)? {
            _ => Ok(self.buf.save(path)?),
        }
    }

    /// Creates an empty image of a specified size.
    ///
    /// The image will start out black and fully transparent.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            buf: ImageBuffer::new(width, height),
        }
    }

    /// Returns the width of this image, in pixels.
    #[inline]
    pub fn width(&self) -> u32 {
        self.buf.width()
    }

    /// Returns the height of this image, in pixels.
    #[inline]
    pub fn height(&self) -> u32 {
        self.buf.height()
    }

    /// Returns the size of this image.
    #[inline]
    pub fn resolution(&self) -> Resolution {
        Resolution::new(self.width(), self.height())
    }

    /// Returns a [`Rect`] covering this image.
    ///
    /// The rectangle will be positioned at `(0, 0)` and have the width and height of the image.
    #[inline]
    pub fn rect(&self) -> Rect {
        Rect::from_top_left(0, 0, self.width(), self.height())
    }

    /// Resizes this image to a new size, adding black bars to keep the original aspect ratio.
    ///
    /// For performance (as this runs on the CPU), this uses nearest neighbor interpolation, so the
    /// result won't look very good, but it should suffice for most use cases.
    pub fn aspect_aware_resize(&self, new_res: Resolution) -> Image {
        self.as_view().aspect_aware_resize(new_res)
    }

    /// Gets the image color at the given pixel coordinates.
    ///
    /// # Panics
    ///
    /// This will panic if `(x, y)` is outside the bounds of this image.
    #[cfg(test)]
    fn get(&self, x: u32, y: u32) -> Color {
        let rgb = &self.buf[(x, y)];
        Color(rgb.0)
    }

    /// Sets the image color at the given pixel coordinates.
    ///
    /// # Panics
    ///
    /// This will panic if `(x, y)` is outside the bounds of this image.
    #[cfg(test)]
    fn set(&mut self, x: u32, y: u32, color: Color) {
        self.buf[(x, y)] = Rgba(color.0);
    }

    /// Creates an immutable view into an area of this image, specified by `rect`.
    ///
    /// If `rect` lies partially outside of `self`, the resulting view is first clipped to `self`
    /// and will be smaller than `rect`. If `rect` lies fully outside of `self`, the resulting view
    /// will be empty.
    pub fn view(&self, rect: &Rect) -> ImageView<'_> {
        match self.rect().intersection(rect) {
            Some(rect) => ImageView {
                sub_image: self
                    .buf
                    .view(rect.x() as _, rect.y() as _, rect.width(), rect.height()),
            },
            None => ImageView {
                sub_image: self.buf.view(0, 0, 0, 0),
            },
        }
    }

    /// Creates a mutable view into an area of this image, specified by `rect`.
    ///
    /// If `rect` lies partially outside of `self`, the resulting view is first clipped to `self`
    /// and will be smaller than `rect`. If `rect` lies fully outside of `self`, the resulting view
    /// will be empty.
    pub fn view_mut(&mut self, rect: &Rect) -> ImageViewMut<'_> {
        match self.rect().intersection(rect) {
            Some(rect) => ImageViewMut {
                sub_image: self.buf.sub_image(
                    rect.x() as _,
                    rect.y() as _,
                    rect.width(),
                    rect.height(),
                ),
            },
            None => ImageViewMut {
                sub_image: self.buf.sub_image(0, 0, 0, 0),
            },
        }
    }

    pub fn flip_horizontal(&self) -> Image {
        Image {
            buf: image::imageops::flip_horizontal(&self.buf),
        }
    }

    pub fn flip_vertical(&self) -> Image {
        Image {
            buf: image::imageops::flip_vertical(&self.buf),
        }
    }

    pub fn flip_horizontal_in_place(&mut self) {
        image::imageops::flip_horizontal_in_place(&mut self.buf);
    }

    pub fn flip_vertical_in_place(&mut self) {
        image::imageops::flip_vertical_in_place(&mut self.buf);
    }

    /// Overwrites the data in `self` with a `src` image, stretching or shrinking `src` as
    /// necessary.
    ///
    /// Note that this always blends the *entire* `src` with the *entire* destination. A smaller
    /// source/destination area can be selected by creating a sub-view first.
    ///
    /// By default, this performs alpha blending.
    pub fn blend_from<'b, V: AsImageView>(&'b mut self, src: &'b V) -> Blend<'b> {
        Blend::new(self.as_view_mut(), src.as_view())
    }

    /// Clears the image, setting every pixel value to `color`.
    pub fn clear(&mut self, color: Color) {
        self.buf.pixels_mut().for_each(|pix| pix.0 = color.0);
    }

    #[inline]
    pub(crate) fn data(&self) -> &[u8] {
        self.buf.as_raw()
    }
}

impl fmt::Debug for Image {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{} Image", self.width(), self.height())
    }
}

/// An immutable view of a rectangular section of an [`Image`].
pub struct ImageView<'a> {
    pub(crate) sub_image: image::SubImage<&'a RgbaImage>,
}

impl<'a> ImageView<'a> {
    /// Returns the width of this view, in pixels.
    pub fn width(&self) -> u32 {
        self.sub_image.width()
    }

    /// Returns the height of this view, in pixels.
    pub fn height(&self) -> u32 {
        self.sub_image.height()
    }

    /// Returns the size of this view.
    #[inline]
    pub fn resolution(&self) -> Resolution {
        // FIXME: panics on empty views
        Resolution::new(self.width(), self.height())
    }

    /// Returns a [`Rect`] of the size of this view.
    ///
    /// The rectangle will be positioned at `(0, 0)` and have the width and height of the view.
    #[inline]
    pub fn rect(&self) -> Rect {
        Rect::from_top_left(0, 0, self.width(), self.height())
    }

    /// Gets the image color at the given pixel coordinates.
    ///
    /// # Panics
    ///
    /// This will panic if `(x, y)` is outside the bounds of this view.
    #[inline]
    pub(crate) fn get(&self, x: u32, y: u32) -> Color {
        let rgb = self.sub_image.get_pixel(x, y);
        Color(rgb.0)
    }

    /// Borrows an identical [`ImageView`] from `self` that may have a shorter lifetime.
    ///
    /// This is equivalent to the implicit "reborrowing" that happens on Rust references. It needs
    /// to be a method call here because user-defined types cannot opt into making this happen
    /// automatically.
    pub fn reborrow(&self) -> ImageView<'_> {
        ImageView {
            sub_image: self.sub_image.view(0, 0, self.width(), self.height()),
        }
    }

    /// Creates an immutable subview into an area of this view, specified by `rect`.
    ///
    /// If `rect` lies partially outside of `self`, the resulting view is first clipped to `self`
    /// and will be smaller than `rect`. If `rect` lies fully outside of `self`, the resulting view
    /// will be empty.
    pub fn view(&self, rect: &Rect) -> ImageView<'_> {
        match self.rect().intersection(rect) {
            Some(rect) => ImageView {
                sub_image: self.sub_image.view(
                    rect.x() as _,
                    rect.y() as _,
                    rect.width(),
                    rect.height(),
                ),
            },
            None => ImageView {
                sub_image: self.sub_image.view(0, 0, 0, 0),
            },
        }
    }

    pub fn flip_horizontal(&self) -> Image {
        Image {
            buf: image::imageops::flip_horizontal(&*self.sub_image),
        }
    }

    pub fn flip_vertical(&self) -> Image {
        Image {
            buf: image::imageops::flip_vertical(&*self.sub_image),
        }
    }

    /// Copies the contents of this view into a new [`Image`].
    pub fn to_image(&self) -> Image {
        Image {
            buf: self.sub_image.to_image(),
        }
    }

    /// Resizes this image to a new size, adding black bars to keep the original aspect ratio.
    ///
    /// For performance (as this runs on the CPU), this uses nearest neighbor interpolation, so the
    /// result won't look very good, but it should suffice for most use cases.
    pub fn aspect_aware_resize(&self, new_res: Resolution) -> Image {
        let cur_ratio = self.resolution().aspect_ratio();
        let new_ratio = new_res.aspect_ratio();

        log::trace!(
            "aspect-aware resize from {} -> {} ({} -> {})",
            self.resolution(),
            new_res,
            cur_ratio,
            new_ratio,
        );

        let mut out = Image {
            buf: ImageBuffer::new(new_res.width(), new_res.height()),
        };

        let target_rect = new_res.fit_aspect_ratio(self.resolution().aspect_ratio());
        let mut target_view = out.view_mut(&target_rect);

        for dest_y in 0..target_rect.height() {
            for dest_x in 0..target_rect.width() {
                let src_x = ((dest_x as f32 + 0.5) / target_rect.width() as f32
                    * self.width() as f32) as u32;
                let src_y = ((dest_y as f32 + 0.5) / target_rect.height() as f32
                    * self.height() as f32) as u32;

                let pixel = self.get(src_x, src_y);
                target_view.set(dest_x, dest_y, pixel);
            }
        }

        out
    }
}

impl fmt::Debug for ImageView<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{} ImageView", self.width(), self.height())
    }
}

/// A mutable view of a rectangular section of an [`Image`].
pub struct ImageViewMut<'a> {
    sub_image: image::SubImage<&'a mut RgbaImage>,
}

impl<'a> ImageViewMut<'a> {
    /// Returns the width of this view, in pixels.
    pub fn width(&self) -> u32 {
        self.sub_image.width()
    }

    /// Returns the height of this view, in pixels.
    pub fn height(&self) -> u32 {
        self.sub_image.height()
    }

    /// Returns the size of this view.
    #[inline]
    pub fn resolution(&self) -> Resolution {
        Resolution::new(self.width(), self.height())
    }

    /// Returns a [`Rect`] of the size of this view.
    ///
    /// The rectangle will be positioned at `(0, 0)` and have the width and height of the view.
    #[inline]
    pub fn rect(&self) -> Rect {
        Rect::from_top_left(0, 0, self.width(), self.height())
    }

    /// Gets the image color at the given pixel coordinates.
    ///
    /// # Panics
    ///
    /// This will panic if `(x, y)` is outside the bounds of this view.
    #[inline]
    fn get(&self, x: u32, y: u32) -> Color {
        let rgb = self.sub_image.get_pixel(x, y);
        Color(rgb.0)
    }

    /// Sets the image color at the given pixel coordinates.
    ///
    /// # Panics
    ///
    /// This will panic if `(x, y)` is outside the bounds of this view.
    #[inline]
    fn set(&mut self, x: u32, y: u32, color: Color) {
        self.sub_image.put_pixel(x, y, Rgba(color.0));
    }

    /// Borrows an identical [`ImageViewMut`] from `self` that may have a shorter lifetime.
    ///
    /// This is equivalent to the implicit "reborrowing" that happens on Rust references. It needs
    /// to be a method call here because user-defined types cannot opt into making this happen
    /// automatically.
    pub fn reborrow(&mut self) -> ImageViewMut<'_> {
        ImageViewMut {
            sub_image: self.sub_image.sub_image(0, 0, self.width(), self.height()),
        }
    }

    /// Creates an immutable subview into an area of this view, specified by `rect`.
    ///
    /// If `rect` lies partially outside of `self`, the resulting view is first clipped to `self`
    /// and will be smaller than `rect`. If `rect` lies fully outside of `self`, the resulting view
    /// will be empty.
    pub fn view(&self, rect: &Rect) -> ImageView<'_> {
        match self.rect().intersection(rect) {
            Some(rect) => ImageView {
                sub_image: self.sub_image.view(
                    rect.x() as _,
                    rect.y() as _,
                    rect.width(),
                    rect.height(),
                ),
            },
            None => ImageView {
                sub_image: self.sub_image.view(0, 0, 0, 0),
            },
        }
    }

    /// Creates a mutable view into an area of this view, specified by `rect`.
    ///
    /// If `rect` lies partially outside of `self`, the resulting view is first clipped to `self`
    /// and will be smaller than `rect`. If `rect` lies fully outside of `self`, the resulting view
    /// will be empty.
    pub fn view_mut(&mut self, rect: &Rect) -> ImageViewMut<'_> {
        match self.rect().intersection(rect) {
            Some(rect) => ImageViewMut {
                sub_image: self.sub_image.sub_image(
                    rect.x() as _,
                    rect.y() as _,
                    rect.width(),
                    rect.height(),
                ),
            },
            None => ImageViewMut {
                sub_image: self.sub_image.sub_image(0, 0, 0, 0),
            },
        }
    }

    pub fn flip_horizontal(&self) -> Image {
        Image {
            buf: image::imageops::flip_horizontal(&*self.sub_image),
        }
    }

    pub fn flip_vertical(&self) -> Image {
        Image {
            buf: image::imageops::flip_vertical(&*self.sub_image),
        }
    }

    pub fn flip_horizontal_in_place(&mut self) {
        image::imageops::flip_horizontal_in_place(&mut *self.sub_image);
    }

    pub fn flip_vertical_in_place(&mut self) {
        image::imageops::flip_vertical_in_place(&mut *self.sub_image);
    }

    /// Copies the contents of this view into a new [`Image`].
    pub fn to_image(&self) -> Image {
        Image {
            buf: self.sub_image.to_image(),
        }
    }

    /// Overwrites the data in `self` with a `src` image, stretching or shrinking `src` as
    /// necessary.
    ///
    /// Note that this always blends the *entire* `src` with the *entire* destination. A smaller
    /// source/destination area can be selected by creating a sub-view first.
    ///
    /// By default, this performs alpha blending.
    pub fn blend_from<'b, V: AsImageView>(&'b mut self, src: &'b V) -> Blend<'b> {
        Blend::new(self.as_view_mut(), src.as_view())
    }
}

impl fmt::Debug for ImageViewMut<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{} ImageViewMut", self.width(), self.height())
    }
}

/// An 8-bit RGBA color.
///
/// Colors are always in the sRGB color space and use non-premultiplied alpha.
#[derive(PartialEq, Eq, Clone, Copy)]
pub struct Color(pub(crate) [u8; 4]);

impl Color {
    pub const BLACK: Self = Self([0, 0, 0, 255]);
    pub const WHITE: Self = Self([255, 255, 255, 255]);
    pub const RED: Self = Self([255, 0, 0, 255]);
    pub const GREEN: Self = Self([0, 255, 0, 255]);
    pub const BLUE: Self = Self([0, 0, 255, 255]);
    pub const YELLOW: Self = Self([255, 255, 0, 255]);
    pub const MAGENTA: Self = Self([255, 0, 255, 255]);
    pub const CYAN: Self = Self([0, 255, 255, 255]);

    #[inline]
    pub const fn from_rgb8(r: u8, g: u8, b: u8) -> Self {
        Self([r, g, b, 255])
    }

    #[inline]
    pub fn r(&self) -> u8 {
        self.0[0]
    }

    #[inline]
    pub fn g(&self) -> u8 {
        self.0[1]
    }

    #[inline]
    pub fn b(&self) -> u8 {
        self.0[2]
    }

    #[inline]
    pub fn a(&self) -> u8 {
        self.0[3]
    }

    pub fn with_alpha(mut self, a: u8) -> Color {
        self.0[3] = a;
        self
    }
}

impl fmt::Debug for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "#{:02x}{:02x}{:02x}{:02x}",
            self.r(),
            self.g(),
            self.b(),
            self.a(),
        )
    }
}

impl Index<usize> for Color {
    type Output = u8;

    #[inline]
    fn index(&self, index: usize) -> &u8 {
        &self.0[index]
    }
}

// FIXME leaks `embedded-graphics` dependency
impl PixelColor for Color {
    type Raw = RawU32;
}

/// Trait for types that can be treated as read-only views of image data.
///
/// This allows abstracting over [`Image`] and [`ImageView`] and should be used by any code that
/// takes immutable image data as input.
pub trait AsImageView {
    /// Returns an [`ImageView`] covering `self`.
    fn as_view(&self) -> ImageView<'_>;
}

/// Trait for types that can be treated as mutable views of image data.
///
/// This allows abstracting over [`Image`] and [`ImageViewMut`] and should be used by any code that
/// writes to image data.
pub trait AsImageViewMut: AsImageView {
    /// Returns an [`ImageViewMut`] covering `self`.
    fn as_view_mut(&mut self) -> ImageViewMut<'_>;
}

impl AsImageView for Image {
    fn as_view(&self) -> ImageView<'_> {
        self.view(&Rect::from_top_left(0, 0, self.width(), self.height()))
    }
}

impl<'a> AsImageView for ImageView<'a> {
    fn as_view(&self) -> ImageView<'_> {
        self.reborrow()
    }
}

impl AsImageViewMut for Image {
    fn as_view_mut(&mut self) -> ImageViewMut<'_> {
        self.view_mut(&Rect::from_top_left(0, 0, self.width(), self.height()))
    }
}

impl<'a> AsImageView for ImageViewMut<'a> {
    fn as_view(&self) -> ImageView<'_> {
        ImageView {
            sub_image: self.sub_image.view(0, 0, self.width(), self.height()),
        }
    }
}

impl<'a> AsImageViewMut for ImageViewMut<'a> {
    fn as_view_mut(&mut self) -> ImageViewMut<'_> {
        self.reborrow()
    }
}

impl<'a, V: AsImageView> AsImageView for &'a V {
    fn as_view(&self) -> ImageView<'_> {
        (*self).as_view()
    }
}

impl<'a, V: AsImageView> AsImageView for &'a mut V {
    fn as_view(&self) -> ImageView<'_> {
        (**self).as_view()
    }
}

impl<'a, V: AsImageViewMut> AsImageViewMut for &'a mut V {
    fn as_view_mut(&mut self) -> ImageViewMut<'_> {
        (*self).as_view_mut()
    }
}
