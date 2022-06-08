//! Facial landmark detection.
//!
//! Also known as *face alignment* or *registration*.
//!
//! This uses one of the neural networks also used in MediaPipe's [Face Mesh] pipeline.
//!
//! [Face Mesh]: https://google.github.io/mediapipe/solutions/face_mesh.html

use once_cell::sync::Lazy;

use crate::{
    image::{AsImageView, ImageView, Rect},
    iter::zip_exact,
    landmark::{self, Landmarks},
    nn::{create_linear_color_mapper, unadjust_aspect_ratio, Cnn, CnnInputShape, NeuralNetwork},
    num::sigmoid,
    resolution::Resolution,
    timer::Timer,
};

const MODEL_DATA: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/3rdparty/onnx/face_landmark.onnx"
));

static MODEL: Lazy<Cnn> = Lazy::new(|| {
    Cnn::new(
        NeuralNetwork::from_onnx(MODEL_DATA)
            .unwrap()
            .load()
            .unwrap(),
        CnnInputShape::NCHW,
        create_linear_color_mapper(-1.0..=1.0),
    )
    .unwrap()
});

/// A neural network based facial landmark predictor.
pub struct Landmarker {
    model: &'static Cnn,
    t_resize: Timer,
    t_infer: Timer,
    /// Large, so keep one around and return by ref.
    result_buffer: LandmarkResult,
}

impl Landmarker {
    /// Creates a new facial landmark calculator.
    pub fn new() -> Self {
        Self {
            t_resize: Timer::new("resize"),
            t_infer: Timer::new("infer"),
            result_buffer: LandmarkResult {
                landmarks: Landmarks::new(468),
                face_flag: 0.0,
            },
            model: &MODEL,
        }
    }

    /// Returns the expected input resolution of the internal neural network.
    pub fn input_resolution(&self) -> Resolution {
        self.model.input_resolution()
    }

    /// Computes facial landmarks in `image`.
    ///
    /// `image` must be a cropped image of a face. When using [`Detector`], the
    /// rectangle returned by [`Detection::bounding_rect_loose`] produces good results.
    ///
    /// The image should depict a face that is mostly upright. Results will be poor if the face is
    /// rotated too much.
    ///
    /// [`Detector`]: super::detection::Detector
    /// [`Detection::bounding_rect_loose`]: super::detection::Detection::bounding_rect_loose
    pub fn compute<V: AsImageView>(&mut self, image: &V) -> &LandmarkResult {
        self.compute_impl(image.as_view())
    }

    fn compute_impl(&mut self, image: ImageView<'_>) -> &LandmarkResult {
        let input_res = self.model.input_resolution();
        let full_res = image.resolution();
        let orig_aspect = full_res.aspect_ratio();

        let mut image = image.reborrow();
        let resized;
        if image.resolution() != input_res {
            resized = self.t_resize.time(|| image.aspect_aware_resize(input_res));
            image = resized.as_view();
        }
        let result = self.t_infer.time(|| self.model.estimate(&image)).unwrap();
        log::trace!("inference result: {:?}", result);

        self.result_buffer.face_flag = sigmoid(result[1].index([0, 0, 0, 0]).as_singular());
        for (coords, out) in zip_exact(
            result[0].index([0, 0, 0]).as_slice().chunks(3),
            self.result_buffer.landmarks.positions_mut(),
        ) {
            let [x, y, z] = [coords[0], coords[1], coords[2]];
            let (x, y) = unadjust_aspect_ratio(
                x / input_res.width() as f32,
                y / input_res.height() as f32,
                orig_aspect,
            );
            let (x, y) = (x * full_res.width() as f32, y * full_res.height() as f32);

            out[0] = x;
            out[1] = y;
            out[2] = z;
        }

        &self.result_buffer
    }

    /// Returns profiling timers for image resizing and neural inference.
    pub fn timers(&self) -> impl Iterator<Item = &Timer> + '_ {
        [&self.t_resize, &self.t_infer].into_iter()
    }
}

impl landmark::Estimator for Landmarker {
    type Estimation = LandmarkResult;

    fn estimate<V: AsImageView>(&mut self, image: &V) -> &Self::Estimation {
        self.compute(image)
    }
}

/// Landmark results returned by [`Landmarker::compute`].
#[derive(Clone)]
pub struct LandmarkResult {
    landmarks: Landmarks,
    face_flag: f32,
}

impl LandmarkResult {
    /// Returns the 3D landmark positions in the input image's coordinate system.
    pub fn landmark_positions(&self) -> impl Iterator<Item = (f32, f32, f32)> + '_ {
        (0..self.landmark_count()).map(|index| self.landmark_position(index))
    }

    /// Returns a landmark's position in the input image's coordinate system.
    pub fn landmark_position(&self, index: usize) -> (f32, f32, f32) {
        let [x, y, z] = self.landmarks.landmark(index).position();
        (x, y, z)
    }

    #[inline]
    pub fn landmark_count(&self) -> usize {
        self.landmarks.len()
    }

    /// Returns a reference to the raw landmarks, unadjusted for the input image resolution.
    #[inline]
    pub fn raw_landmarks(&self) -> &Landmarks {
        &self.landmarks
    }

    /// Returns the confidence that the input image contains a proper face.
    ///
    /// The returned value is in range 0.0 to 1.0.
    ///
    /// This can be used to estimate the fit quality, or to re-run face detection if that isn't done
    /// each frame.
    #[inline]
    pub fn face_confidence(&self) -> f32 {
        self.face_flag
    }

    /// Returns a [`Rect`] containing the left eye.
    pub fn left_eye(&self) -> Rect {
        Rect::bounding(
            [
                LandmarkIdx::LeftEyeBottom,
                LandmarkIdx::LeftEyeLeftCorner,
                LandmarkIdx::LeftEyeRightCorner,
                LandmarkIdx::LeftEyeTop,
            ]
            .into_iter()
            .map(|idx| {
                let pos = self.landmark_position(idx as usize);
                (pos.0 as i32, pos.1 as i32)
            }),
        )
        .unwrap()
    }

    /// Returns a [`Rect`] containing the right eye.
    pub fn right_eye(&self) -> Rect {
        Rect::bounding(
            [
                LandmarkIdx::RightEyeBottom,
                LandmarkIdx::RightEyeLeftCorner,
                LandmarkIdx::RightEyeRightCorner,
                LandmarkIdx::RightEyeTop,
            ]
            .into_iter()
            .map(|idx| {
                let pos = self.landmark_position(idx as usize);
                (pos.0 as i32, pos.1 as i32)
            }),
        )
        .unwrap()
    }
}

impl landmark::Estimation for LandmarkResult {
    fn confidence(&self) -> f32 {
        self.face_confidence()
    }

    fn landmarks(&self) -> &Landmarks {
        self.raw_landmarks()
    }
}

include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/3rdparty/3d/canonical_face_model.rs"
));

/// Returns an iterator over the vertices of the reference face model.
///
/// Each point yielded by the returned iterator corresponds to the same point in the sequence
/// of landmarks output by [`Landmarker`], but the scale and coordinate system does not: The points
/// returned by this function have Y pointing up, and X and Y are in a smaller range around `(0,0)`,
/// while [`Landmarker`] yields points that have Y point down, and X and Y are in term of the input
/// image's coordinates.
pub fn reference_positions() -> impl Iterator<Item = (f32, f32, f32)> {
    REFERENCE_POSITIONS.iter().copied()
}

/// Assigns a name to certain important landmark indices.
///
/// "Left" and "Right" are relative to the input image, not from the PoV of the depicted person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LandmarkIdx {
    MouthLeft = 78,
    MouthRight = 308,
    MouthTop = 13,
    MouthBottom = 14,
    LeftEyeLeftCorner = 33,
    LeftEyeRightCorner = 133,
    LeftEyeTop = 159,
    LeftEyeBottom = 145,
    RightEyeLeftCorner = 362,
    RightEyeRightCorner = 263,
    RightEyeTop = 386,
    RightEyeBottom = 374,
    RightEyebrowLeftCorner = 295,
    LeftEyebrowRightCorner = 65,
}

impl Into<usize> for LandmarkIdx {
    #[inline]
    fn into(self) -> usize {
        self as usize
    }
}
