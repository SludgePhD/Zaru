use zaru::{
    gui,
    hand::detection::{self, PalmDetector},
    timer::FpsCounter,
    webcam::Webcam,
};

const USE_FULL_DETECTION_NETWORK: bool = true;

fn main() -> Result<(), zaru::Error> {
    zaru::init_logger!();

    let mut palm_detector = if USE_FULL_DETECTION_NETWORK {
        PalmDetector::new(detection::FullNetwork)
    } else {
        PalmDetector::new(detection::LiteNetwork)
    };

    let mut fps = FpsCounter::new("hand tracker");
    let mut webcam = Webcam::open()?;

    loop {
        let mut image = webcam.read()?;

        for det in palm_detector.detect(&image) {
            det.draw(&mut image);
        }

        gui::show_image("palm detection", &image);

        fps.tick_with(webcam.timers().chain(palm_detector.timers()));
    }
}
