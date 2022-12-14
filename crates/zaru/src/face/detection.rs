//! Face detection module.
//!
//! This uses one of the "BlazeFace" neural networks also used in MediaPipe's [Face Detection]
//! module.
//!
//! [Face Detection]: https://google.github.io/mediapipe/solutions/face_detection

use nalgebra::{Rotation2, Vector2};
use once_cell::sync::Lazy;
use zaru_image::{
    draw, AsImageView, AsImageViewMut, Color, ImageView, ImageViewMut, Rect, Resolution,
    RotatedRect,
};
use zaru_utils::num::{sigmoid, TotalF32};

use crate::{
    detection::{
        nms::NonMaxSuppression,
        ssd::{Anchor, AnchorParams, Anchors, LayerInfo},
        BoundingRect, RawDetection,
    },
    nn::{create_linear_color_mapper, point_to_img, Cnn, CnnInputShape, NeuralNetwork},
    timer::Timer,
};

/// Neural-Network based face detector.
pub struct Detector {
    model: &'static Cnn,
    anchors: Anchors,
    t_resize: Timer,
    t_infer: Timer,
    t_nms: Timer,
    thresh: f32,
    nms: NonMaxSuppression,
    raw_detections: Vec<RawDetection>,
    detections: Vec<Detection>,
}

/// The default detector uses the short-range neural network.
impl Default for Detector {
    fn default() -> Self {
        Self::new(ShortRangeNetwork)
    }
}

impl Detector {
    const DEFAULT_THRESH: f32 = 0.5;

    /// Creates a new face detector.
    pub fn new<N: DetectionNetwork>(network: N) -> Self {
        drop(network);
        Self {
            model: N::cnn(),
            anchors: N::anchors(),
            t_resize: Timer::new("resize"),
            t_infer: Timer::new("infer"),
            t_nms: Timer::new("NMS"),
            thresh: Self::DEFAULT_THRESH,
            nms: NonMaxSuppression::new(),
            raw_detections: Vec::new(),
            detections: Vec::new(),
        }
    }

    /// Returns the expected input resolution of the internal neural network.
    pub fn input_resolution(&self) -> Resolution {
        self.model.input_resolution()
    }

    /// Runs face detections on an input image, returning the filtered detections.
    ///
    /// The image will be scaled to the input size expected by the neural network, and detections
    /// will be back-mapped to input image coordinates.
    ///
    /// Note that the computed detections have a large amount of jitter when applying the detection
    /// to subsequent frames of a video. To reduce jitter,
    pub fn detect<V: AsImageView>(&mut self, image: &V) -> &[Detection] {
        self.detect_impl(image.as_view())
    }

    fn detect_impl(&mut self, image: ImageView<'_>) -> &[Detection] {
        self.raw_detections.clear();
        self.detections.clear();

        let full_res = image.resolution();
        let input_resolution = self.input_resolution();

        let mut image = image;
        let resized;
        if image.resolution() != input_resolution {
            resized = self
                .t_resize
                .time(|| image.aspect_aware_resize(self.model.input_resolution()));
            image = resized.as_view();
        }
        let result = self.t_infer.time(|| self.model.estimate(&image)).unwrap();
        log::trace!("inference result: {:?}", result);

        let num_anchors = self.anchors.anchor_count();
        self.t_nms.time(|| {
            let boxes = &result[0];
            let confidences = &result[1];

            assert_eq!(boxes.shape(), &[1, num_anchors, 16]);
            assert_eq!(confidences.shape(), &[1, num_anchors, 1]);
            for (index, view) in confidences.index([0]).iter().enumerate() {
                let conf = sigmoid(view.as_slice()[0]);
                if conf < self.thresh {
                    continue;
                }

                let tensor_view = boxes.index([0, index]);
                let box_params = tensor_view.as_slice();
                self.raw_detections.push(extract_detection(
                    &self.anchors[index],
                    input_resolution,
                    box_params,
                    conf,
                ));
            }

            if self.raw_detections.is_empty() {
                log::trace!(
                    "no detection above threshold; max confidence = {}",
                    confidences
                        .index([0])
                        .iter()
                        .map(|view| sigmoid(view.as_slice()[0]))
                        .max_by_key(|f| TotalF32(*f))
                        .unwrap()
                );
            }

            let detections = self.nms.process(&mut self.raw_detections);
            for raw in detections {
                self.detections.push(Detection { raw, full_res });
            }
        });

        &self.detections
    }

    /// Returns profiling timers for image resizing, neural inference, and detection filtering.
    pub fn timers(&self) -> impl Iterator<Item = &Timer> + '_ {
        [&self.t_resize, &self.t_infer, &self.t_nms].into_iter()
    }
}

/// A detected face, consisting of a bounding box and landmarks.
#[derive(Debug, Clone)]
pub struct Detection {
    raw: RawDetection,
    full_res: Resolution,
}

impl Detection {
    /// Returns the raw bounding box of the face as output by the network (adjusted for the input image).
    ///
    /// This box is *very* tight and does not include the head boundary or much of the forehead.
    pub fn bounding_rect_raw(&self) -> Rect {
        self.raw.bounding_rect().to_rect(&self.full_res)
    }

    /// Returns the bounding box of the detected face, adjusted to include the whole head boundary.
    pub fn bounding_rect_loose(&self) -> Rect {
        const LOOSEN_LEFT: f32 = 0.08;
        const LOOSEN_RIGHT: f32 = 0.08;
        const LOOSEN_TOP: f32 = 0.55;
        const LOOSEN_BOTTOM: f32 = 0.2;

        self.raw
            .bounding_rect()
            .grow_rel(LOOSEN_LEFT, LOOSEN_RIGHT, LOOSEN_TOP, LOOSEN_BOTTOM)
            .to_rect(&self.full_res)
    }

    /// Returns the confidence of this detection (in range 0 to 1).
    pub fn confidence(&self) -> f32 {
        self.raw.confidence()
    }

    /// Estimated clockwise rotation of the face.
    ///
    /// Note that this value is quite imprecise. If you need a more accurate angle, compute facial
    /// landmarks instead and compute their rotation.
    pub fn rotation_radians(&self) -> f32 {
        let left_eye = self.left_eye();
        let right_eye = self.right_eye();
        let left_to_right_eye = Vector2::new(
            (right_eye.0 - left_eye.0) as f32,
            (right_eye.1 - left_eye.1) as f32,
        );
        Rotation2::rotation_between(&Vector2::x(), &left_to_right_eye).angle()
    }

    /// Returns the coordinates of the left eye's landmark (from the perspective of the input image,
    /// not the depicted person).
    pub fn left_eye(&self) -> (i32, i32) {
        let lm = &self.raw.keypoints()[0];
        point_to_img(lm.x(), lm.y(), &self.full_res)
    }

    /// Returns the coordinates of the right eye's landmark (from the perspective of the input image,
    /// not the depicted person).
    pub fn right_eye(&self) -> (i32, i32) {
        let lm = self.raw.keypoints()[1];
        point_to_img(lm.x(), lm.y(), &self.full_res)
    }

    /// Draws this detection onto an image.
    ///
    /// # Panics
    ///
    /// The image must have the same resolution as the image the detection was performed on,
    /// otherwise this method will panic.
    pub fn draw<I: AsImageViewMut>(&self, image: &mut I) {
        self.draw_impl(&mut image.as_view_mut());
    }

    fn draw_impl(&self, image: &mut ImageViewMut<'_>) {
        let res = Resolution::new(image.width(), image.height());
        assert_eq!(
            res, self.full_res,
            "attempted to draw `Detection` onto canvas with mismatched size",
        );

        draw::rect(image, self.bounding_rect_raw()).color(Color::from_rgb8(170, 0, 0));
        for lm in self.raw.keypoints() {
            let (x, y) = point_to_img(lm.x(), lm.y(), &self.full_res);
            draw::marker(image, x, y);
        }

        draw::rect(image, self.bounding_rect_loose()).color(Color::from_rgb8(0, 170, 0));

        draw::rotated_rect(
            image,
            RotatedRect::new(self.bounding_rect_loose(), self.rotation_radians()),
        );

        let color = match self.confidence() {
            0.8.. => Color::GREEN,
            0.4..=0.8 => Color::YELLOW,
            _ => Color::RED,
        };
        draw::text(
            image,
            self.bounding_rect_loose().x() + (self.bounding_rect_loose().width() / 2) as i32,
            self.bounding_rect_loose().y(),
            &format!("conf={:.01}", self.confidence()),
        )
        .align_top()
        .color(color);

        let alignment_color = Color::WHITE;
        let (x0, y0) = self.left_eye();
        let (x1, y1) = self.right_eye();
        draw::line(image, x0, y0, x1, y1).color(alignment_color);
        // FIXME: builtin font does not have ?? symbol
        let rot = format!("{:.01} deg", self.rotation_radians().to_degrees());
        draw::text(image, (x0 + x1) / 2, (y0 + y1) / 2 - 10, &rot).color(alignment_color);
    }
}

fn extract_detection(
    anchor: &Anchor,
    input_res: Resolution,
    box_params: &[f32],
    confidence: f32,
) -> RawDetection {
    assert_eq!(box_params.len(), 16);

    let input_w = input_res.width() as f32;
    let input_h = input_res.height() as f32;

    let xc = box_params[0] / input_w + anchor.x_center();
    let yc = box_params[1] / input_h + anchor.y_center();
    let w = box_params[2] / input_w;
    let h = box_params[3] / input_h;
    let lm = |x, y| {
        crate::detection::Keypoint::new(
            x / input_w + anchor.x_center(),
            y / input_h + anchor.y_center(),
        )
    };

    RawDetection::with_keypoints(
        confidence,
        BoundingRect::from_center(xc, yc, w, h),
        vec![
            lm(box_params[4], box_params[5]),
            lm(box_params[6], box_params[7]),
            lm(box_params[8], box_params[9]),
            lm(box_params[10], box_params[11]),
            lm(box_params[12], box_params[13]),
            lm(box_params[14], box_params[15]),
        ],
    )
}

/// Trait for supported face detection networks.
///
/// This is a trait instead of an enum to ensure that only the networks used by the application are
/// included in the binary.
pub trait DetectionNetwork {
    fn cnn() -> &'static Cnn;
    fn anchors() -> Anchors;

    #[doc(hidden)]
    fn __private_dont_implement();
}

/// A small and efficient face detection network, best for faces in <3m of the camera.
pub struct ShortRangeNetwork;

impl DetectionNetwork for ShortRangeNetwork {
    fn cnn() -> &'static Cnn {
        static MODEL: Lazy<Cnn> = Lazy::new(|| {
            let model_data =
                include_blob::include_bytes!("../../3rdparty/onnx/face_detection_short_range.onnx");
            Cnn::new(
                NeuralNetwork::from_onnx(model_data)
                    .unwrap()
                    .load()
                    .unwrap(),
                CnnInputShape::NCHW,
                create_linear_color_mapper(-1.0..=1.0),
            )
            .unwrap()
        });

        &MODEL
    }

    fn anchors() -> Anchors {
        Anchors::calculate(&AnchorParams {
            layers: &[LayerInfo::new(2, 16, 16), LayerInfo::new(6, 8, 8)],
        })
    }

    fn __private_dont_implement() {}
}

/// A larger detection network with a greater detection range, but slower inference speed (around 5
/// times that of [`ShortRangeNetwork`]).
pub struct FullRangeNetwork;

impl DetectionNetwork for FullRangeNetwork {
    fn cnn() -> &'static Cnn {
        static MODEL: Lazy<Cnn> = Lazy::new(|| {
            let model_data =
                include_blob::include_bytes!("../../3rdparty/onnx/face_detection_full_range.onnx");
            Cnn::new(
                NeuralNetwork::from_onnx(model_data)
                    .unwrap()
                    .load()
                    .unwrap(),
                CnnInputShape::NCHW,
                create_linear_color_mapper(-1.0..=1.0),
            )
            .unwrap()
        });

        &MODEL
    }

    fn anchors() -> Anchors {
        Anchors::calculate(&AnchorParams {
            layers: &[LayerInfo::new(1, 48, 48)],
        })
    }

    fn __private_dont_implement() {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test;

    #[test]
    fn detects_face() {
        let mut det = Detector::new(ShortRangeNetwork);
        let detections = det.detect(test::sad_linus_full());
        assert_eq!(detections.len(), 1);

        let detection = &detections[0];
        assert!(detection.confidence() >= 0.9, "{}", detection.confidence());
        let angle = detection.rotation_radians().to_degrees();
        assert!(angle < 5.0, "{angle}");

        assert!(detection.left_eye().0 < detection.right_eye().0);
    }
}
