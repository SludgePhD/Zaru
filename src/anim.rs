//! Animated images.

use std::{
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
    slice,
    time::Duration,
};

use image::{
    codecs::{gif::GifDecoder, png::PngDecoder},
    AnimationDecoder, Frame, SubImage,
};

use crate::image::ImageView;

#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum AnimationFormat {
    Gif,
    Apng,
}

/// A timed sequence of images.
pub struct Animation {
    frames: Vec<Frame>,
}

impl Animation {
    /// Loads an animation from the filesystem.
    ///
    /// The path must have a supported extension.
    pub fn from_path<P: AsRef<Path>>(path: P) -> Result<Self, crate::Error> {
        let path = path.as_ref();
        match path.extension() {
            Some(ext) => {
                let format = if ext == "gif" {
                    AnimationFormat::Gif
                } else if ext == "apng" || ext == "png" {
                    AnimationFormat::Apng
                } else {
                    return Err(format!(
                        "file extension `{}` is not supported for animations",
                        ext.to_string_lossy()
                    )
                    .into());
                };

                Self::from_reader(BufReader::new(File::open(path)?), format)
            }
            _ => Err(format!("animation path must have a supported extension").into()),
        }
    }

    /// Loads a animation from an in-memory byte slice.
    pub fn from_data(data: &[u8], format: AnimationFormat) -> Result<Self, crate::Error> {
        Self::from_reader(data, format)
    }

    /// Loads a animation from a [`BufRead`] implementor.
    pub fn from_reader<R: BufRead>(
        reader: R,
        format: AnimationFormat,
    ) -> Result<Self, crate::Error> {
        let frames = match format {
            AnimationFormat::Gif => GifDecoder::new(reader)?.into_frames().collect_frames()?,
            AnimationFormat::Apng => {
                let dec = PngDecoder::new(reader)?;
                if !dec.is_apng() {
                    return Err(
                        format!("attempted to load APNG animation from still image PNG").into(),
                    );
                }
                dec.apng().into_frames().collect_frames()?
            }
        };

        Ok(Self { frames })
    }

    /// Returns an iterator over the frames of this animation.
    ///
    /// Note that every frame is only yielded *once* (ie. the iterator does not loop, even if the
    /// animation does). Call [`Iterator::cycle`] to loop the animation.
    pub fn frames(&self) -> FrameIter<'_> {
        FrameIter {
            frames: self.frames.iter(),
        }
    }
}

/// An iterator over the [`AnimationFrame`]s that make up an [`Animation`].
#[derive(Clone)]
pub struct FrameIter<'a> {
    frames: slice::Iter<'a, Frame>,
}

impl<'a> Iterator for FrameIter<'a> {
    type Item = AnimationFrame<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        self.frames.next().map(|frame| AnimationFrame {
            image: ImageView {
                sub_image: SubImage::new(
                    frame.buffer(),
                    0,
                    0,
                    frame.buffer().width(),
                    frame.buffer().height(),
                ),
            },
            duration: frame.delay().into(),
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.frames.size_hint()
    }
}

impl<'a> ExactSizeIterator for FrameIter<'a> {}

/// A frame of an animation, consisting of image data and a duration.
pub struct AnimationFrame<'a> {
    // NB: only exposes `ImageView` here so that all frames could be stored in a texture atlas.
    image: ImageView<'a>,
    duration: Duration,
}

impl<'a> AnimationFrame<'a> {
    /// Returns an [`ImageView`] of the image data for this frame.
    pub fn image_view(&self) -> &ImageView<'a> {
        &self.image
    }

    /// Returns the [`Duration`] for which this frame should be displayed before proceeding to the
    /// next one.
    pub fn duration(&self) -> Duration {
        self.duration
    }
}