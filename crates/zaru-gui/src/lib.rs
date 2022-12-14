//! A simple, high-level debug GUI.

mod renderer;
mod shaders;

use std::{
    collections::HashMap,
    rc::Rc,
    sync::{mpsc::sync_channel, Mutex},
};

use once_cell::sync::OnceCell;
use winit::{
    event::Event,
    event_loop::{ControlFlow, EventLoop, EventLoopBuilder, EventLoopClosed, EventLoopProxy},
    platform::unix::EventLoopBuilderExtUnix,
    window::WindowId,
};

use zaru_image::{Image, Resolution};

use self::renderer::{Gpu, Renderer, Window};

struct Gui {
    gpu: Rc<Gpu>,
    windows: HashMap<String, Renderer>,
    win_id_to_key: HashMap<WindowId, String>,
}

impl Gui {
    fn new() -> Self {
        Self {
            gpu: Rc::new(pollster::block_on(Gpu::open()).unwrap()),
            windows: HashMap::new(),
            win_id_to_key: HashMap::new(),
        }
    }

    fn get_renderer_mut(&mut self, win: WindowId) -> &mut Renderer {
        let key = &self.win_id_to_key[&win];
        self.windows.get_mut(key).unwrap()
    }

    fn run(mut self, event_loop: EventLoop<Msg>) -> ! {
        event_loop.run(move |event, target, flow| {
            *flow = ControlFlow::Wait;
            match event {
                Event::UserEvent(msg) => match msg {
                    Msg::Image { key, res, data } => {
                        let renderer = self.windows.entry(key.clone()).or_insert_with(|| {
                            log::debug!("creating window for image '{key}' at {res}");

                            let win = Window::open(&target, &key, res).unwrap();
                            let win_id = win.win.id();
                            let renderer = Renderer::new(win, self.gpu.clone()).unwrap();

                            self.win_id_to_key.insert(win_id, key.clone());

                            renderer
                        });

                        renderer.update_texture(res, &data);
                        renderer.window().request_redraw();
                    }
                },
                Event::RedrawRequested(window) => {
                    let renderer = self.get_renderer_mut(window);
                    renderer.redraw();
                }
                _ => {}
            }
        });
    }
}

#[derive(Debug)]
enum Msg {
    Image {
        key: String,
        res: Resolution,
        data: Vec<u8>,
    },
}

static SENDER: OnceCell<Mutex<EventLoopProxy<Msg>>> = OnceCell::new();

fn start_gui() -> EventLoopProxy<Msg> {
    let (sender, recv) = sync_channel(0);

    std::thread::Builder::new()
        .name("gui".into())
        .spawn(move || {
            let event_loop = EventLoopBuilder::with_user_event()
                .with_any_thread(true)
                .build();
            let proxy = event_loop.create_proxy();
            sender.send(proxy).unwrap();

            let gui = Gui::new();
            gui.run(event_loop);
        })
        .unwrap();

    recv.recv().unwrap()
}

fn send(msg: Msg) {
    // TODO: backpressure
    SENDER
        .get_or_init(|| Mutex::new(start_gui()))
        .lock()
        .unwrap()
        .send_event(msg)
        .map_err(|_closed| EventLoopClosed(()))
        .unwrap();
}

/// Displays an image in a window.
pub fn show_image(key: impl Into<String>, image: &Image) {
    // Image data is RGBA8 internally so that no conversion before GPU upload is needed.
    let data = image.data().to_vec();

    send(Msg::Image {
        key: key.into(),
        res: Resolution::new(image.width(), image.height()),
        data,
    });

    // FIXME: does not handle image resolution changes
}
