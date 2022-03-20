use crossbeam::channel;
use log::LevelFilter;
use mizaru::detector::Detector;
use mizaru::timer::FpsCounter;
use mizaru::webcam::Webcam;
use mizaru::{gui, Error};

fn main() -> Result<(), Error> {
    let log_level = if cfg!(debug_assertions) {
        LevelFilter::Trace
    } else {
        LevelFilter::Debug
    };
    env_logger::Builder::new()
        .filter(Some(env!("CARGO_CRATE_NAME")), log_level)
        .filter(Some("wgpu"), LevelFilter::Warn)
        .init();

    let mut detector = Detector::new();

    let mut webcam = Webcam::open()?;

    let (img_sender, img_recv) = channel::bounded(0);

    crossbeam::scope(|scope| {
        scope
            .builder()
            .name("Webcam Decoder".into())
            .spawn(|_| {
                let mut fps = FpsCounter::new("webcam");
                loop {
                    let image = match webcam.read() {
                        Ok(image) => image,
                        Err(e) => {
                            log::error!("failed to fetch image from webcam: {e}");
                            break;
                        }
                    };
                    if img_sender.send(image).is_err() {
                        break;
                    }
                    fps.tick_with(webcam.timers());
                }
            })
            .unwrap();

        scope
            .builder()
            .name("Face Detector".into())
            .spawn(|_| {
                let mut fps = FpsCounter::new("detector");

                for mut image in img_recv {
                    let detections = detector.detect(&image);
                    log::trace!("{:?}", detections);

                    for det in detections {
                        det.draw(&mut image);
                    }
                    gui::show_image("face_detect", &image);

                    fps.tick_with(detector.timers());
                }
            })
            .unwrap();
    })
    .unwrap();

    // TODO take tracking result from end of thread pipeline and forward panics
    Ok(())
}
