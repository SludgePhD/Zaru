//! Non-Maximum Suppression and Averaging.
//!
//! Typical Single-Shot MultiBox Detectors (SSD) produce duplicate detections for individual
//! objects. Non-Maximum Suppression (NMS) is an algorithm that filters these duplicates out,
//! leaving only a single detection with high confidence for each object.
//!
//! This module implements 2 variants of NMS, selected with [`SuppressionMode`]: The classic
//! Non-Maximum Suppression algorithm that removes any overlapping detections with lower confidence
//! ([`SuppressionMode::Remove`]), and the slightly smarter Non-Maximum Averaging
//! ([`SuppressionMode::Average`]) which instead computes a weighted average of overlapping
//! detections. Since the latter reduces jitter between frames, and does not seem to have any
//! appreciable drawbacks (outside of a minor computational cost), it is used by default.

use zaru_utils::{iter::zip_exact, num::TotalF32};

use super::{BoundingRect, Keypoint, RawDetection};

/// A non-maximum suppression algorithm.
pub struct NonMaxSuppression {
    iou_thresh: f32,
    avg_buf: Vec<RawDetection>,
    out_buf: Vec<RawDetection>,
    mode: SuppressionMode,
}

impl NonMaxSuppression {
    /// The default intersection-over-union threshold used to determine if two detections overlap.
    pub const DEFAULT_IOU_THRESH: f32 = 0.3;

    /// Creates a new non-maximum suppressor.
    ///
    /// The returned suppression algorithm will use [`SuppressionMode::Average`] and a default IOU
    /// threshold.
    pub fn new() -> Self {
        Self {
            iou_thresh: Self::DEFAULT_IOU_THRESH,
            avg_buf: Vec::new(),
            out_buf: Vec::new(),
            mode: SuppressionMode::Average,
        }
    }

    /// Sets the intersection-over-union threshold to consider two detections as overlapping.
    ///
    /// By default, [`Self::DEFAULT_IOU_THRESH`] is used.
    pub fn set_iou_thresh(&mut self, iou_thresh: f32) {
        self.iou_thresh = iou_thresh;
    }

    /// Sets the suppression mode.
    pub fn set_mode(&mut self, mode: SuppressionMode) {
        self.mode = mode;
    }

    /// Performs non-maximum suppression on `detections`.
    ///
    /// `detections` will be modified in the process. The filtered detections are returned as an
    /// iterator.
    pub fn process(
        &mut self,
        detections: &mut Vec<RawDetection>,
    ) -> impl Iterator<Item = RawDetection> + '_ {
        self.out_buf.clear();

        // Sort by ascending confidence, process highest confidence first by starting at the back.
        detections.sort_unstable_by_key(|det| TotalF32(det.confidence));

        while let Some(seed) = detections.pop() {
            match self.mode {
                SuppressionMode::Remove => {
                    detections.retain(|other| {
                        let iou = seed.bounding_rect().iou(&other.bounding_rect());
                        if iou >= self.iou_thresh {
                            false // remove from detection list
                        } else {
                            true
                        }
                    });
                    self.out_buf.push(seed);
                }
                SuppressionMode::Average => {
                    self.avg_buf.clear();
                    self.avg_buf.push(seed.clone());
                    detections.retain(|other| {
                        let iou = seed.bounding_rect().iou(&other.bounding_rect());
                        if iou >= self.iou_thresh {
                            // FIXME: unnecessary clone, required only because `drain_filter` is unstable
                            self.avg_buf.push(other.clone());
                            false // remove from detection list
                        } else {
                            true
                        }
                    });

                    // compute confidence-weighted average of the overlapping detections
                    let mut acc_rect = BoundingRect::from_center(0.0, 0.0, 0.0, 0.0);
                    let mut acc = RawDetection::new(seed.confidence(), acc_rect);
                    let mut divisor = 0.0;
                    for det in &self.avg_buf {
                        if acc.keypoints().is_empty() && !det.keypoints().is_empty() {
                            acc.keypoints_mut()
                                .resize(det.keypoints().len(), Keypoint::new(0.0, 0.0));
                        }

                        assert_eq!(
                            acc.keypoints().len(),
                            det.keypoints().len(),
                            "landmark count must be constant"
                        );

                        let factor = det.confidence;
                        divisor += factor;
                        for (acc, lm) in zip_exact(&mut acc.keypoints.iter_mut(), &det.keypoints) {
                            acc.x += lm.x * factor;
                            acc.y += lm.y * factor;
                        }
                        let rect = det.bounding_rect();
                        acc_rect.xc += rect.xc * factor;
                        acc_rect.yc += rect.yc * factor;
                        acc_rect.w += rect.w * factor;
                        acc_rect.h += rect.h * factor;
                    }

                    for lm in &mut acc.keypoints {
                        lm.x /= divisor;
                        lm.y /= divisor;
                    }
                    acc_rect.xc /= divisor;
                    acc_rect.yc /= divisor;
                    acc_rect.w /= divisor;
                    acc_rect.h /= divisor;

                    acc.set_bounding_rect(acc_rect);
                    self.out_buf.push(acc);
                }
            }
        }

        self.avg_buf.clear();
        self.out_buf.drain(..)
    }
}

/// Describes how [`NonMaxSuppression`] should deal with overlapping detections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SuppressionMode {
    /// Remove overlapping detections, only retain the detection with highest confidence score.
    Remove,

    /// Compute a confidence-weighted average of overlapping detections.
    Average,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nms_suppresses_non_maximum() {
        let mut nms = NonMaxSuppression::new();
        nms.set_mode(SuppressionMode::Remove);

        let rect = BoundingRect::from_center(0.0, 0.0, 1.0, 1.0);
        let a = RawDetection::new(0.6, rect);
        let b = RawDetection::new(0.55, rect.scale(1.5));
        let detections = nms.process(&mut vec![a, b]).collect::<Vec<_>>();
        assert_eq!(detections.len(), 1);

        let d = &detections[0];
        let rect = d.bounding_rect();
        assert_eq!(d.confidence(), 0.6);
        assert_eq!(rect.xc, 0.0);
        assert_eq!(rect.yc, 0.0);
        assert_eq!(rect.w, 1.0);
        assert_eq!(rect.h, 1.0);
    }

    #[test]
    fn nms_ignores_nonoverlapping() {
        let mut nms = NonMaxSuppression::new();
        nms.set_mode(SuppressionMode::Remove);

        let a = RawDetection::new(1.0, BoundingRect::from_center(0.0, 0.0, 1.0, 1.0));
        let b = RawDetection::new(1.0, BoundingRect::from_center(5.0, 0.0, 1.0, 1.0));

        let detections = nms.process(&mut vec![a, b]).collect::<Vec<_>>();
        assert_eq!(detections.len(), 2);
    }

    #[test]
    fn nma_averages_detections() {
        let mut nms = NonMaxSuppression::new();
        nms.set_mode(SuppressionMode::Average);
        nms.set_iou_thresh(0.0);

        let rect = BoundingRect::from_center(-1.0, 3.0, 1.0, 1.0);
        let a = RawDetection::new(1.0, rect);
        let b = RawDetection::new(0.5, rect.scale(4.0));
        let detections = nms.process(&mut vec![a, b]).collect::<Vec<_>>();
        assert_eq!(detections.len(), 1);

        let d = &detections[0];
        let rect = d.bounding_rect();
        assert_eq!(d.confidence(), 1.0);
        assert_eq!(rect.xc, -1.0);
        assert_eq!(rect.yc, 3.0);
        assert_eq!(rect.w, 2.0);
        assert_eq!(rect.h, 2.0);
    }
}
