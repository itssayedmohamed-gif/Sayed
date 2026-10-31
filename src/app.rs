use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use eframe::egui::{self, Color32, RichText, TextureHandle, TextureOptions};

use crate::{
    android,
    loader::Loader,
    scan::{self, Photo},
};

const PREFETCH_AHEAD: usize = 8;
const PREFETCH_BEHIND: usize = 2;
const CACHE_BEHIND: usize = 4;
const CACHE_AHEAD_EXTRA: usize = 2;
const MAX_HISTORY: usize = 50;
const MAX_UPLOADS_PER_FRAME: usize = 3;

enum Entry {
    Loading,
    Ready(TextureHandle),
    Failed,
}

#[derive(Clone)]
enum Action {
    Keep,
    Delete { orig: PathBuf, trash: PathBuf },
}

enum Phase {
    NoAccess,
    Scanning,
    Ready,
}

pub struct PhotoApp {
    ctx: egui::Context,
    data_dir: Option<PathBuf>,
    phase: Phase,
    photos: Vec<Photo>,
    cur: usize,
    history: Vec<Action>,
    cache: HashMap<PathBuf, Entry>,
    loader: Loader,
    scan_rx: Option<mpsc::Receiver<Vec<Photo>>>,
    last_access_check: Instant,
    dirty: bool,
    status: String,
    saved_pos: Option<(u64, PathBuf)>,
    trash_count: usize,
    confirm_empty: bool,
}

// ---------- ملفات ----------

fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(parent) = to.parent() {
        let _ = fs::create_dir_all(parent);
    }
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(_) => {
            fs::copy(from, to)?;
            fs::remove_file(from)
        }
    }
}

fn trash_move(path: &Path) -> std::io::Result<PathBuf> {
    let dir = Path::new(scan::TRASH);
    fs::create_dir_all(dir)?;
    let nomedia = dir.join(".nomedia");
    if !nomedia.exists() {
        let _ = fs::File::create(nomedia);
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "photo".to_string());
    let dest = dir.join(format!("{}_{}", nanos, name));
    move_file(path, &dest)?;
    Ok(dest)
}

fn load_state(dir: &Option<PathBuf>) -> (Option<(u64, PathBuf)>, Vec<Action>) {
    let mut pos: Option<(u64, PathBuf)> = None;
    let mut hist: Vec<Action> = Vec::new();
    let dir = match dir {
        Some(d) => d,
        None => return (pos, hist),
    };
    let txt = match fs::read_to_string(dir.join("state.txt")) {
        Ok(t) => t,
        Err(_) => return (pos, hist),
    };
    for line in txt.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        match parts.as_slice() {
            ["P", m, p] => {
                if let Ok(m) = m.parse::<u64>() {
                    pos = Some((m, PathBuf::from(*p)));
                }
            }
            ["K"] => hist.push(Action::Keep),
            ["D", o, t] => {
                if Path::new(*t).exists() {
                    hist.push(Action::Delete {
                        orig: PathBuf::from(*o),
                        trash: PathBuf::from(*t),
                    });
                }
            }
            _ => {}
        }
    }
    (pos, hist)
}

impl PhotoApp {
    pub fn new(cc: &eframe::CreationContext<'_>, data_dir: Option<PathBuf>) -> Self {
        let ctx = cc.egui_ctx.clone();
        ctx.set_visuals(egui::Visuals::dark());
        ctx.style_mut(|s| {
            for f in s.text_styles.values_mut() {
                f.size *= 1.3;
            }
        });
        let (saved_pos, history) = load_state(&data_dir);
        let trash_count = history
            .iter()
            .filter(|a| matches!(a, Action::Delete { .. }))
            .count();
        let loader = Loader::new(ctx.clone());
        let mut app = PhotoApp {
            ctx,
            data_dir,
            phase: Phase::NoAccess,
            photos: Vec::new(),
            cur: 0,
            history,
            cache: HashMap::new(),
            loader,
            scan_rx: None,
            last_access_check: Instant::now(),
            dirty: false,
            status: String::new(),
            saved_pos,
            trash_count,
            confirm_empty: false,
        };
        if android::has_all_files_access() {
            app.start_scan();
        }
        app
    }

    fn start_scan(&mut self) {
        self.phase = Phase::Scanning;
        let (tx, rx) = mpsc::channel();
        self.scan_rx = Some(rx);
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let v = scan::scan();
            let _ = tx.send(v);
            ctx.request_repaint();
        });
    }

    fn locate(&self) -> usize {
        match &self.saved_pos {
            Some((m, p)) => {
                if !p.as_os_str().is_empty() {
                    if let Some(i) = self.photos.iter().position(|x| &x.path == p) {
                        return i;
                    }
                }
                self.photos.partition_point(|x| x.mtime > *m)
            }
            None => 0,
        }
    }

    fn save_state(&self) {
        let dir = match &self.data_dir {
            Some(d) => d,
            None => return,
        };
        let mut s = String::new();
        match self.photos.get(self.cur) {
            Some(p) => s.push_str(&format!("P\t{}\t{}\n", p.mtime, p.path.display())),
            None => s.push_str("P\t0\t\n"),
        }
        for a in &self.history {
            match a {
                Action::Keep => s.push_str("K\n"),
                Action::Delete { orig, trash } => {
                    s.push_str(&format!("D\t{}\t{}\n", orig.display(), trash.display()))
                }
            }
        }
        let tmp = dir.join("state.tmp");
        if fs::write(&tmp, s).is_ok() {
            let _ = fs::rename(&tmp, dir.join("state.txt"));
        }
    }

    fn after_action(&mut self) {
        self.confirm_empty = false;
        self.dirty = true;
        self.save_state();
    }

    fn push_history(&mut self, a: Action) {
        self.history.push(a);
        while self.history.len() > MAX_HISTORY {
            let old = self.history.remove(0);
            if let Action::Delete { trash, .. } = old {
                let _ = fs::remove_file(trash);
                self.trash_count = self.trash_count.saturating_sub(1);
            }
        }
    }

    // ---------- الاوامر ----------

    fn keep_current(&mut self) {
        if self.cur >= self.photos.len() {
            return;
        }
        self.cur += 1;
        self.status.clear();
        self.push_history(Action::Keep);
        self.after_action();
    }

    fn delete_current(&mut self) {
        if self.cur >= self.photos.len() {
            return;
        }
        let photo = self.photos.remove(self.cur);
        match trash_move(&photo.path) {
            Ok(trash) => {
                android::scan_async(vec![photo.path.to_string_lossy().into_owned()]);
                self.push_history(Action::Delete {
                    orig: photo.path,
                    trash,
                });
                self.trash_count += 1;
                self.status.clear();
            }
            Err(e) => {
                self.status = format!("Delete failed: {}", e);
                self.photos.insert(self.cur, photo);
                return;
            }
        }
        self.after_action();
    }

    fn undo(&mut self) {
        let action = match self.history.pop() {
            Some(a) => a,
            None => return,
        };
        match action {
            Action::Keep => {
                if self.cur > 0 {
                    self.cur -= 1;
                }
                self.status.clear();
            }
            Action::Delete { orig, trash } => match move_file(&trash, &orig) {
                Ok(()) => {
                    let mtime = scan::mtime_of(&orig);
                    let idx = self.photos.partition_point(|x| x.mtime > mtime);
                    android::scan_async(vec![orig.to_string_lossy().into_owned()]);
                    self.photos.insert(idx, Photo { path: orig, mtime });
                    self.cur = idx;
                    self.trash_count = self.trash_count.saturating_sub(1);
                    self.status.clear();
                }
                Err(e) => {
                    self.status = format!("Undo failed: {}", e);
                }
            },
        }
        self.after_action();
    }

    fn empty_trash(&mut self) {
        if let Ok(rd) = fs::read_dir(scan::TRASH) {
            for e in rd.flatten() {
                if e.file_name().to_str() == Some(".nomedia") {
                    continue;
                }
                let _ = fs::remove_file(e.path());
            }
        }
        self.history.retain(|a| matches!(a, Action::Keep));
        self.trash_count = 0;
        self.status = "Space freed".to_string();
        self.after_action();
    }

    // ---------- تحميل الصور ----------

    fn poll_loaded(&mut self) {
        let mut uploaded = 0;
        while uploaded < MAX_UPLOADS_PER_FRAME {
            let (path, img) = match self.loader.rx.try_recv() {
                Ok(v) => v,
                Err(_) => return,
            };
            let is_loading = matches!(self.cache.get(&path), Some(Entry::Loading));
            if !is_loading {
                continue;
            }
            let entry = match img {
                Some(ci) => Entry::Ready(self.ctx.load_texture(
                    path.to_string_lossy().into_owned(),
                    ci,
                    TextureOptions::LINEAR,
                )),
                None => Entry::Failed,
            };
            self.cache.insert(path, entry);
            uploaded += 1;
        }
        // لسه فيه نتائج مستنية
        self.ctx.request_repaint();
    }

    fn prefetch(&mut self) {
        if self.photos.is_empty() {
            self.cache.clear();
            return;
        }
        let len = self.photos.len();
        let cur = self.cur.min(len);

        // الترتيب حسب الاولوية: الحالية، التالية، اللي بعدها... وبعدين اللي ورا
        let mut want: Vec<PathBuf> = Vec::new();
        for i in 0..=PREFETCH_AHEAD {
            if let Some(p) = self.photos.get(cur + i) {
                want.push(p.path.clone());
            }
        }
        for b in 1..=PREFETCH_BEHIND {
            if cur >= b {
                if let Some(p) = self.photos.get(cur - b) {
                    want.push(p.path.clone());
                }
            }
        }

        // النافذة اللي بنحتفظ بيها في الذاكرة
        let lo = cur.saturating_sub(CACHE_BEHIND);
        let hi = (cur + PREFETCH_AHEAD + CACHE_AHEAD_EXTRA).min(len.saturating_sub(1));
        let mut window: HashSet<PathBuf> = HashSet::new();
        for i in lo..=hi {
            if let Some(p) = self.photos.get(i) {
                window.insert(p.path.clone());
            }
        }

        let mut st = self.loader.shared.state.lock().unwrap();
        st.queue.clear();
        let inflight = st.inflight.clone();
        self.cache.retain(|k, v| match v {
            Entry::Loading => inflight.contains(k),
            _ => window.contains(k),
        });
        for p in want {
            if !self.cache.contains_key(&p) {
                self.cache.insert(p.clone(), Entry::Loading);
                st.queue.push_back(p);
            }
        }
        drop(st);
        self.loader.shared.cv.notify_all();
    }

    // ---------- الشاشات ----------

    fn ui_no_access(&mut self, ctx: &egui::Context) {
        if self.last_access_check.elapsed() > Duration::from_millis(500) {
            self.last_access_check = Instant::now();
            if android::has_all_files_access() {
                self.start_scan();
                ctx.request_repaint();
                return;
            }
        }
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(120.0);
                ui.heading("PhotoSwipe");
                ui.add_space(16.0);
                ui.label("Needs All files access to delete photos instantly.");
                ui.label("Tap the button, switch it on, then come back.");
                ui.add_space(24.0);
                if ui
                    .add_sized([260.0, 64.0], egui::Button::new("Allow access"))
                    .clicked()
                {
                    android::request_all_files_access();
                }
            });
        });
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    fn ui_scanning(&mut self, ctx: &egui::Context) {
        let done = self.scan_rx.as_ref().and_then(|rx| rx.try_recv().ok());
        if let Some(v) = done {
            self.photos = v;
            self.cur = self.locate();
            self.scan_rx = None;
            self.phase = Phase::Ready;
            self.dirty = true;
            ctx.request_repaint();
            return;
        }
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.centered_and_justified(|ui| {
                ui.label("Scanning photos...");
            });
        });
        ctx.request_repaint_after(Duration::from_millis(200));
    }

    fn ui_main(&mut self, ctx: &egui::Context) {
        self.poll_loaded();
        if self.dirty {
            self.prefetch();
            self.dirty = false;
        }

        let can_act = match self.photos.get(self.cur) {
            Some(p) => matches!(
                self.cache.get(&p.path),
                Some(Entry::Ready(_)) | Some(Entry::Failed)
            ),
            None => false,
        };
        let can_undo = !self.history.is_empty();

        let mut do_delete = false;
        let mut do_keep = false;
        let mut do_undo = false;
        let mut do_empty = false;

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(18.0);
            ui.horizontal(|ui| {
                let total = self.photos.len();
                let txt = if total == 0 {
                    "0".to_string()
                } else {
                    format!("{} / {}", (self.cur + 1).min(total), total)
                };
                ui.label(RichText::new(txt).strong());
                if self.trash_count > 0 {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let label = if self.confirm_empty {
                            format!("Sure? Free {}", self.trash_count)
                        } else {
                            format!("Free space ({})", self.trash_count)
                        };
                        if ui.button(label).clicked() {
                            do_empty = true;
                        }
                    });
                }
            });
            if !self.status.is_empty() {
                ui.label(
                    RichText::new(self.status.clone())
                        .small()
                        .color(Color32::LIGHT_YELLOW),
                );
            }
            ui.add_space(4.0);
        });

        egui::TopBottomPanel::bottom("bottom").show(ctx, |ui| {
            ui.add_space(10.0);
            let gap = 10.0;
            ui.spacing_mut().item_spacing.x = gap;
            let undo_w = 92.0;
            let big = ((ui.available_width() - undo_w - gap * 2.0) / 2.0).max(60.0);
            let h = 72.0;
            ui.horizontal(|ui| {
                let del = egui::Button::new(
                    RichText::new("DELETE")
                        .size(24.0)
                        .strong()
                        .color(Color32::WHITE),
                )
                .fill(Color32::from_rgb(185, 40, 40))
                .min_size(egui::vec2(big, h));
                if ui.add_enabled(can_act, del).clicked() {
                    do_delete = true;
                }

                let undo = egui::Button::new(RichText::new("UNDO").size(18.0))
                    .min_size(egui::vec2(undo_w, h));
                if ui.add_enabled(can_undo, undo).clicked() {
                    do_undo = true;
                }

                let keep = egui::Button::new(
                    RichText::new("KEEP")
                        .size(24.0)
                        .strong()
                        .color(Color32::WHITE),
                )
                .fill(Color32::from_rgb(35, 150, 85))
                .min_size(egui::vec2(big, h));
                if ui.add_enabled(can_act, keep).clicked() {
                    do_keep = true;
                }
            });
            ui.add_space(18.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let rect = ui.available_rect_before_wrap();
            match self.photos.get(self.cur) {
                None => {
                    ui.centered_and_justified(|ui| {
                        let msg = if self.photos.is_empty() {
                            "No photos found"
                        } else {
                            "All done"
                        };
                        ui.label(RichText::new(msg).size(22.0));
                    });
                }
                Some(p) => match self.cache.get(&p.path) {
                    Some(Entry::Ready(tex)) => {
                        let sz = tex.size_vec2();
                        let scale = (rect.width() / sz.x).min(rect.height() / sz.y);
                        let r = egui::Rect::from_center_size(rect.center(), sz * scale);
                        ui.painter().image(
                            tex.id(),
                            r,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                    Some(Entry::Failed) => {
                        ui.centered_and_justified(|ui| {
                            ui.label("Can't preview this file");
                        });
                    }
                    _ => {
                        ui.put(
                            egui::Rect::from_center_size(rect.center(), egui::vec2(36.0, 36.0)),
                            egui::Spinner::new(),
                        );
                    }
                },
            }
        });

        if do_delete {
            self.delete_current();
        } else if do_keep {
            self.keep_current();
        } else if do_undo {
            self.undo();
        } else if do_empty {
            if self.confirm_empty {
                self.empty_trash();
            } else {
                self.confirm_empty = true;
            }
        }
    }
}

impl eframe::App for PhotoApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        match self.phase {
            Phase::NoAccess => self.ui_no_access(ctx),
            Phase::Scanning => self.ui_scanning(ctx),
            Phase::Ready => self.ui_main(ctx),
        }
    }
}
