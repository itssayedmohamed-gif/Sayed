use eframe::egui;
use std::{
    collections::{HashSet, VecDeque},
    panic::{catch_unwind, AssertUnwindSafe},
    path::{Path, PathBuf},
    sync::{mpsc, Arc, Condvar, Mutex},
};

const MAX_SIDE: u32 = 1280;
const MAX_PIXELS: u64 = 90_000_000;

#[derive(Default)]
pub struct QState {
    pub queue: VecDeque<PathBuf>,
    pub inflight: HashSet<PathBuf>,
}

pub struct Shared {
    pub state: Mutex<QState>,
    pub cv: Condvar,
}

pub type Loaded = (PathBuf, Option<egui::ColorImage>);

pub struct Loader {
    pub shared: Arc<Shared>,
    pub rx: mpsc::Receiver<Loaded>,
}

impl Loader {
    pub fn new(ctx: egui::Context) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(QState::default()),
            cv: Condvar::new(),
        });
        let (tx, rx) = mpsc::channel::<Loaded>();
        let n = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(2, 4);
        for _ in 0..n {
            let shared = shared.clone();
            let tx = tx.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || loop {
                let path = {
                    let mut st = shared.state.lock().unwrap();
                    loop {
                        if let Some(p) = st.queue.pop_front() {
                            st.inflight.insert(p.clone());
                            break p;
                        }
                        st = shared.cv.wait(st).unwrap();
                    }
                };
                let img = catch_unwind(AssertUnwindSafe(|| decode(&path)))
                    .ok()
                    .flatten();
                let sent = tx.send((path.clone(), img)).is_ok();
                shared.state.lock().unwrap().inflight.remove(&path);
                if !sent {
                    break;
                }
                ctx.request_repaint();
            });
        }
        Loader { shared, rx }
    }
}

fn read_orientation(path: &Path) -> u32 {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return 1,
    };
    let mut br = std::io::BufReader::new(file);
    match exif::Reader::new().read_from_container(&mut br) {
        Ok(ex) => ex
            .get_field(exif::Tag::Orientation, exif::In::PRIMARY)
            .and_then(|fld| fld.value.get_uint(0))
            .unwrap_or(1),
        Err(_) => 1,
    }
}

fn apply_orientation(img: image::RgbaImage, o: u32) -> image::RgbaImage {
    use image::imageops::{flip_horizontal, flip_vertical, rotate180, rotate270, rotate90};
    match o {
        2 => flip_horizontal(&img),
        3 => rotate180(&img),
        4 => flip_vertical(&img),
        5 => flip_horizontal(&rotate90(&img)),
        6 => rotate90(&img),
        7 => flip_horizontal(&rotate270(&img)),
        8 => rotate270(&img),
        _ => img,
    }
}

fn decode(path: &Path) -> Option<egui::ColorImage> {
    let (w, h) = image::image_dimensions(path).ok()?;
    if (w as u64) * (h as u64) > MAX_PIXELS {
        return None;
    }
    let img = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    let img = if img.width().max(img.height()) > MAX_SIDE {
        img.thumbnail(MAX_SIDE, MAX_SIDE)
    } else {
        img
    };
    let rgba = apply_orientation(img.to_rgba8(), read_orientation(path));
    let size = [rgba.width() as usize, rgba.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()))
}
