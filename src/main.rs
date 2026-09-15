#![windows_subsystem = "windows"]

mod doc;
mod render;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, SharedString, VecModel};

use doc::{Canvas, Node};
use render::Renderer;

slint::include_modules!();

const UNSAVED_WARN_AFTER: Duration = Duration::from_secs(10 * 60);
const DEFAULT_WIDTH: f64 = 320.0;

struct App {
    doc: Canvas,
    path: Option<PathBuf>,
    dirty: bool,
    dirty_since: Option<Instant>,
    undo: Vec<Vec<Node>>,
    redo: Vec<Vec<Node>>,
    selected: Option<usize>,
    drag_orig: Option<(f64, f64)>,
    resize_orig: Option<f64>,
    message: String,
    renderer: Renderer,
    model: Rc<VecModel<NodeVm>>,
}

impl App {
    fn root_dir(&self) -> PathBuf {
        self.path
            .as_ref()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }

    fn build_vm(&self, i: usize) -> NodeVm {
        let node = &self.doc.nodes[i];
        let mut vm = NodeVm {
            id: node.id.clone().into(),
            x: node.x as f32,
            y: node.y as f32,
            w: node.width as f32,
            h: node.height.unwrap_or(40.0) as f32,
            img: Default::default(),
            error: SharedString::default(),
            selected: self.selected == Some(i),
        };
        match node.kind.as_str() {
            "image" => {
                let src = node.src.clone().unwrap_or_default();
                let path = if Path::new(&src).is_absolute() {
                    PathBuf::from(&src)
                } else {
                    self.root_dir().join(&src)
                };
                match slint::Image::load_from_path(&path) {
                    Ok(img) => {
                        let size = img.size();
                        if node.height.is_none() && size.width > 0 {
                            vm.h = (node.width * size.height as f64 / size.width as f64) as f32;
                        }
                        vm.img = img;
                    }
                    Err(_) => {
                        vm.error = format!("cannot load image: {}", path.display()).into();
                        vm.h = 60.0;
                    }
                }
            }
            _ => {
                match self.renderer.render(
                    &self.root_dir(),
                    &self.doc.preamble,
                    &node.source,
                    node.width,
                ) {
                    Ok(r) => {
                        match slint::Image::load_from_svg_data(r.svg.as_bytes()) {
                            Ok(img) => vm.img = img,
                            Err(e) => vm.error = format!("svg error: {e:?}").into(),
                        }
                        if node.height.is_none() {
                            vm.h = r.height.max(24.0) as f32;
                        }
                    }
                    Err(e) => {
                        vm.error = e.into();
                        if node.height.is_none() {
                            vm.h = 80.0;
                        }
                    }
                }
            }
        }
        vm
    }

    fn refresh_node(&self, i: usize) {
        self.model.set_row_data(i, self.build_vm(i));
    }

    fn refresh_all(&self) {
        let vms: Vec<NodeVm> = (0..self.doc.nodes.len()).map(|i| self.build_vm(i)).collect();
        self.model.set_vec(vms);
    }

    fn update_selection_flags(&self) {
        for i in 0..self.model.row_count() {
            if let Some(mut vm) = self.model.row_data(i) {
                let want = self.selected == Some(i);
                if vm.selected != want {
                    vm.selected = want;
                    self.model.set_row_data(i, vm);
                }
            }
        }
    }

    fn snapshot(&mut self) {
        self.undo.push(self.doc.nodes.clone());
        if self.undo.len() > 200 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn mark_dirty(&mut self) {
        if !self.dirty {
            self.dirty = true;
            self.dirty_since = Some(Instant::now());
        }
    }

    fn save(&mut self) -> Result<(), String> {
        let path = self
            .path
            .clone()
            .unwrap_or_else(|| PathBuf::from("untitled.canvas.json"));
        self.doc.save(&path)?;
        self.path = Some(path);
        self.dirty = false;
        self.dirty_since = None;
        Ok(())
    }

    /// Replace the document and reset all editing state.
    fn load_doc(&mut self, doc: Canvas, path: Option<PathBuf>, ui: &MainWindow) {
        ui.set_pan_x(doc.view.x as f32);
        ui.set_pan_y(doc.view.y as f32);
        ui.set_zoom(doc.view.zoom as f32);
        self.doc = doc;
        self.path = path;
        self.dirty = false;
        self.dirty_since = None;
        self.undo.clear();
        self.redo.clear();
        self.selected = None;
        self.drag_orig = None;
        self.resize_orig = None;
        self.message.clear();
        self.refresh_all();
    }

    fn store_view(&mut self, ui: &MainWindow) {
        self.doc.view.x = ui.get_pan_x() as f64;
        self.doc.view.y = ui.get_pan_y() as f64;
        self.doc.view.zoom = ui.get_zoom() as f64;
    }

    fn save_as_dialog(&self) -> Option<PathBuf> {
        let mut d = rfd::FileDialog::new()
            .add_filter("canvas", &["json"])
            .set_directory(self.root_dir());
        if let Some(name) = self.path.as_ref().and_then(|p| p.file_name()) {
            d = d.set_file_name(name.to_string_lossy());
        } else {
            d = d.set_file_name("untitled.canvas.json");
        }
        d.save_file()
    }

    /// Save to the current path, or prompt for one. Returns false if cancelled.
    fn save_interactive(&mut self, ui: &MainWindow, force_prompt: bool) -> bool {
        if self.path.is_none() || force_prompt {
            match self.save_as_dialog() {
                Some(p) => self.path = Some(p),
                None => return false,
            }
        }
        self.store_view(ui);
        match self.save() {
            Ok(()) => {
                self.message = "saved".into();
                true
            }
            Err(e) => {
                self.message = format!("save failed: {e}");
                false
            }
        }
    }

    /// Ask what to do with unsaved changes. Returns true if it is OK to proceed.
    fn confirm_discard(&mut self, ui: &MainWindow) -> bool {
        if !self.dirty {
            return true;
        }
        use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
        let r = MessageDialog::new()
            .set_title("mcanvas")
            .set_level(MessageLevel::Warning)
            .set_description("The canvas has unsaved changes. Save them?")
            .set_buttons(MessageButtons::YesNoCancel)
            .show();
        match r {
            MessageDialogResult::Yes => self.save_interactive(ui, false),
            MessageDialogResult::No => true,
            _ => false,
        }
    }

    /// Zoom and pan so every node is visible.
    fn zoom_fit(&self, ui: &MainWindow) {
        if self.doc.nodes.is_empty() {
            return;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for (i, n) in self.doc.nodes.iter().enumerate() {
            let h = self.model.row_data(i).map(|v| v.h as f64).unwrap_or(0.0);
            x0 = x0.min(n.x);
            y0 = y0.min(n.y);
            x1 = x1.max(n.x + n.width);
            y1 = y1.max(n.y + h);
        }
        let pad = 40.0;
        let vw = ui.get_view_width() as f64;
        let vh = ui.get_view_height() as f64;
        let bw = (x1 - x0).max(1.0);
        let bh = (y1 - y0).max(1.0);
        let z = ((vw - 2.0 * pad) / bw)
            .min((vh - 2.0 * pad) / bh)
            .clamp(0.1, 8.0);
        ui.set_zoom(z as f32);
        ui.set_pan_x(((vw - bw * z) / 2.0 - x0 * z) as f32);
        ui.set_pan_y(((vh - bh * z) / 2.0 - y0 * z) as f32);
    }

    fn add_node(&mut self, x: f64, y: f64) -> usize {
        self.snapshot();
        let node = Node {
            id: doc::new_id(),
            kind: "typst".into(),
            x: self.doc.snap(x),
            y: self.doc.snap(y),
            width: DEFAULT_WIDTH,
            height: None,
            source: String::new(),
            src: None,
        };
        self.doc.nodes.push(node);
        let i = self.doc.nodes.len() - 1;
        self.selected = Some(i);
        self.model.push(self.build_vm(i));
        self.update_selection_flags();
        self.mark_dirty();
        i
    }

    fn delete_selected(&mut self) {
        if let Some(i) = self.selected {
            self.snapshot();
            self.doc.nodes.remove(i);
            self.model.remove(i);
            self.selected = None;
            self.mark_dirty();
        }
    }

    fn undo(&mut self) {
        if let Some(prev) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.doc.nodes, prev));
            self.selected = None;
            self.refresh_all();
            self.mark_dirty();
        }
    }

    fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.doc.nodes, next));
            self.selected = None;
            self.refresh_all();
            self.mark_dirty();
        }
    }

    /// Spatial navigation: pick the node in the given direction whose center
    /// lies within a 90 degree cone, minimizing along + 2 * across distance.
    fn navigate(&mut self, dx: f64, dy: f64) {
        let Some(cur) = self.selected else {
            if !self.doc.nodes.is_empty() {
                self.selected = Some(0);
                self.update_selection_flags();
            }
            return;
        };
        let center = |i: usize| {
            let n = &self.doc.nodes[i];
            let h = self.model.row_data(i).map(|v| v.h as f64).unwrap_or(0.0);
            (n.x + n.width / 2.0, n.y + h / 2.0)
        };
        let (cx, cy) = center(cur);
        let mut best: Option<(f64, usize)> = None;
        for i in 0..self.doc.nodes.len() {
            if i == cur {
                continue;
            }
            let (px, py) = center(i);
            let vx = px - cx;
            let vy = py - cy;
            let along = vx * dx + vy * dy;
            let across = (vx * dy - vy * dx).abs();
            if along <= 0.0 || across > along {
                continue;
            }
            let score = along + 2.0 * across;
            if best.map_or(true, |(s, _)| score < s) {
                best = Some((score, i));
            }
        }
        if let Some((_, i)) = best {
            self.selected = Some(i);
            self.update_selection_flags();
        }
    }
}

fn status(app: &App, ui: &MainWindow) {
    let name = app
        .path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "untitled".into());
    let dirty = if app.dirty { " [modified]" } else { "" };
    let mut left = format!("{name}{dirty}");
    if !app.message.is_empty() {
        left.push_str("    ");
        left.push_str(&app.message);
    }
    ui.set_status_left(left.into());
    ui.set_status_right(
        format!(
            "{} nodes   zoom {:.0}%",
            app.doc.nodes.len(),
            ui.get_zoom() * 100.0
        )
        .into(),
    );
}

fn main() {
    let path = std::env::args().nth(1).map(PathBuf::from);
    let doc = match &path {
        Some(p) if p.exists() => match Canvas::load(p) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("failed to load {}: {e}", p.display());
                std::process::exit(1);
            }
        },
        _ => Canvas::default(),
    };

    let ui = MainWindow::new().unwrap();
    let model = Rc::new(VecModel::<NodeVm>::default());
    ui.set_nodes(model.clone().into());
    ui.set_pan_x(doc.view.x as f32);
    ui.set_pan_y(doc.view.y as f32);
    ui.set_zoom(doc.view.zoom as f32);

    let app = Rc::new(RefCell::new(App {
        doc,
        path,
        dirty: false,
        dirty_since: None,
        undo: Vec::new(),
        redo: Vec::new(),
        selected: None,
        drag_orig: None,
        resize_orig: None,
        message: String::new(),
        renderer: Renderer::new(),
        model,
    }));
    app.borrow().refresh_all();
    status(&app.borrow(), &ui);

    macro_rules! hook {
        ($cb:ident, |$a:ident, $u:ident $(, $arg:ident)*| $body:block) => {{
            let app = app.clone();
            let weak = ui.as_weak();
            ui.$cb(move |$($arg),*| {
                let $u = weak.unwrap();
                let mut $a = app.borrow_mut();
                let r = (|| $body)();
                status(&$a, &$u);
                r
            });
        }};
    }

    hook!(on_node_pressed, |a, _u, i| {
        let i = i as usize;
        a.selected = Some(i);
        a.update_selection_flags();
        a.drag_orig = Some((a.doc.nodes[i].x, a.doc.nodes[i].y));
        a.message.clear();
    });

    hook!(on_node_drag, |a, _u, i, dx, dy| {
        let i = i as usize;
        if let Some((ox, oy)) = a.drag_orig {
            a.doc.nodes[i].x = ox + dx as f64;
            a.doc.nodes[i].y = oy + dy as f64;
            if let Some(mut vm) = a.model.row_data(i) {
                vm.x = a.doc.nodes[i].x as f32;
                vm.y = a.doc.nodes[i].y as f32;
                a.model.set_row_data(i, vm);
            }
        }
    });

    hook!(on_node_drag_end, |a, _u, i| {
        let i = i as usize;
        if let Some((ox, oy)) = a.drag_orig.take() {
            let n = &a.doc.nodes[i];
            let (nx, ny) = (a.doc.snap(n.x), a.doc.snap(n.y));
            if (nx, ny) != (ox, oy) {
                // Record the pre-drag state for undo.
                let mut before = a.doc.nodes.clone();
                before[i].x = ox;
                before[i].y = oy;
                a.undo.push(before);
                a.redo.clear();
                a.doc.nodes[i].x = nx;
                a.doc.nodes[i].y = ny;
                if let Some(mut vm) = a.model.row_data(i) {
                    vm.x = nx as f32;
                    vm.y = ny as f32;
                    a.model.set_row_data(i, vm);
                }
                a.mark_dirty();
            } else {
                a.doc.nodes[i].x = ox;
                a.doc.nodes[i].y = oy;
                if let Some(mut vm) = a.model.row_data(i) {
                    vm.x = ox as f32;
                    vm.y = oy as f32;
                    a.model.set_row_data(i, vm);
                }
            }
        }
    });

    hook!(on_node_resize, |a, _u, i, dw| {
        let i = i as usize;
        if a.resize_orig.is_none() {
            a.resize_orig = Some(a.doc.nodes[i].width);
        }
        let ow = a.resize_orig.unwrap();
        let w = (ow + dw as f64).max(40.0);
        if let Some(mut vm) = a.model.row_data(i) {
            vm.w = w as f32;
            a.model.set_row_data(i, vm);
        }
    });

    hook!(on_node_resize_end, |a, _u, i| {
        let i = i as usize;
        if let Some(ow) = a.resize_orig.take() {
            let w = a
                .model
                .row_data(i)
                .map(|v| v.w as f64)
                .unwrap_or(ow);
            let w = a.doc.snap(w).max(40.0);
            if w != ow {
                a.snapshot();
                a.doc.nodes[i].width = w;
                a.refresh_node(i);
                a.mark_dirty();
            } else {
                a.refresh_node(i);
            }
        }
    });

    hook!(on_node_edit_request, |a, u, i| {
        let i = i as usize;
        a.selected = Some(i);
        a.update_selection_flags();
        a.drag_orig = None;
        u.invoke_begin_edit(i as i32, a.doc.nodes[i].source.clone().into());
    });

    hook!(on_background_clicked, |a, _u| {
        a.selected = None;
        a.update_selection_flags();
        a.message.clear();
    });

    hook!(on_background_double_clicked, |a, u, x, y| {
        let i = a.add_node(x as f64, y as f64);
        u.invoke_begin_edit(i as i32, SharedString::default());
    });

    hook!(on_commit_edit, |a, u, i, text| {
        let i = i as usize;
        let text = text.to_string();
        if a.doc.nodes[i].source != text {
            a.snapshot();
            a.doc.nodes[i].source = text;
            a.refresh_node(i);
            a.mark_dirty();
        }
        u.invoke_end_edit();
    });

    hook!(on_cancel_edit, |a, u| {
        let i = u.get_edit_index();
        u.invoke_end_edit();
        if i >= 0 {
            let i = i as usize;
            if i < a.doc.nodes.len() && a.doc.nodes[i].source.is_empty() {
                a.selected = Some(i);
                a.delete_selected();
            }
        }
    });

    hook!(on_key, |a, u, text, ctrl, shift| {
        let key = text.to_string();
        let k = key.as_str();
        let is = |k2: slint::platform::Key| SharedString::from(k2) == text;
        use slint::platform::Key;
        if ctrl {
            // Most Ctrl shortcuts are declared on the menu bar and never reach here.
            if shift && (k == "z" || k == "Z") {
                a.redo();
                return true;
            }
            return false;
        }
        if is(Key::Delete) || is(Key::Backspace) {
            a.delete_selected();
        } else if is(Key::Return) {
            if let Some(i) = a.selected {
                u.invoke_begin_edit(i as i32, a.doc.nodes[i].source.clone().into());
            }
        } else if is(Key::LeftArrow) {
            a.navigate(-1.0, 0.0);
        } else if is(Key::RightArrow) {
            a.navigate(1.0, 0.0);
        } else if is(Key::UpArrow) {
            a.navigate(0.0, -1.0);
        } else if is(Key::DownArrow) {
            a.navigate(0.0, 1.0);
        } else if is(Key::Escape) {
            a.selected = None;
            a.update_selection_flags();
            a.message.clear();
        } else if is(Key::Home) {
            let f = 1.0 / u.get_zoom();
            u.invoke_zoom_at(f, 0.0, 0.0);
            u.set_pan_x(0.0);
            u.set_pan_y(0.0);
        } else {
            return false;
        }
        true
    });

    hook!(on_command, |a, u, name| {
        let editing = u.get_edit_index() >= 0;
        let zoom_step = |u: &MainWindow, f: f32| {
            let z = (u.get_zoom() * f).clamp(0.1, 8.0);
            let f = z / u.get_zoom();
            u.invoke_zoom_at(f, u.get_view_width() / 2.0, u.get_view_height() / 2.0);
        };
        match name.as_str() {
            "new" => {
                if editing {
                    u.invoke_end_edit();
                }
                if a.confirm_discard(&u) {
                    a.load_doc(Canvas::default(), None, &u);
                }
            }
            "open" => {
                if editing {
                    u.invoke_end_edit();
                }
                if !a.confirm_discard(&u) {
                    return;
                }
                let picked = rfd::FileDialog::new()
                    .add_filter("canvas", &["json"])
                    .set_directory(a.root_dir())
                    .pick_file();
                if let Some(p) = picked {
                    match Canvas::load(&p) {
                        Ok(d) => a.load_doc(d, Some(p), &u),
                        Err(e) => a.message = format!("failed to load {}: {e}", p.display()),
                    }
                }
            }
            "save" => {
                a.save_interactive(&u, false);
            }
            "save-as" => {
                a.save_interactive(&u, true);
            }
            "quit" => {
                if a.confirm_discard(&u) {
                    slint::quit_event_loop().ok();
                }
            }
            "undo" if !editing => a.undo(),
            "redo" if !editing => a.redo(),
            "add-node" if !editing => {
                let x = (u.get_view_width() / 2.0 - u.get_pan_x()) / u.get_zoom();
                let y = (u.get_view_height() / 2.0 - u.get_pan_y()) / u.get_zoom();
                let i = a.add_node(x as f64 - DEFAULT_WIDTH / 2.0, y as f64);
                u.invoke_begin_edit(i as i32, SharedString::default());
            }
            "edit-node" if !editing => {
                if let Some(i) = a.selected {
                    u.invoke_begin_edit(i as i32, a.doc.nodes[i].source.clone().into());
                }
            }
            "delete" if !editing => a.delete_selected(),
            "zoom-in" => zoom_step(&u, 1.25),
            "zoom-out" => zoom_step(&u, 0.8),
            "zoom-100" => {
                let f = 1.0 / u.get_zoom();
                u.invoke_zoom_at(f, u.get_view_width() / 2.0, u.get_view_height() / 2.0);
            }
            "zoom-fit" => a.zoom_fit(&u),
            "reset-view" => {
                u.set_zoom(1.0);
                u.set_pan_x(0.0);
                u.set_pan_y(0.0);
            }
            _ => {}
        }
        if !editing {
            u.invoke_focus_canvas();
        }
    });

    // Close button: confirm when dirty.
    {
        let app = app.clone();
        let weak = ui.as_weak();
        ui.window().on_close_requested(move || {
            let u = weak.unwrap();
            let mut a = app.borrow_mut();
            if a.confirm_discard(&u) {
                slint::CloseRequestResponse::HideWindow
            } else {
                status(&a, &u);
                slint::CloseRequestResponse::KeepWindowShown
            }
        });
    }

    // Periodic unsaved-changes reminder.
    let timer = slint::Timer::default();
    {
        let app = app.clone();
        let weak = ui.as_weak();
        timer.start(slint::TimerMode::Repeated, Duration::from_secs(30), move || {
            let u = weak.unwrap();
            let mut a = app.borrow_mut();
            if let Some(since) = a.dirty_since {
                let elapsed = since.elapsed();
                if elapsed >= UNSAVED_WARN_AFTER {
                    a.message = format!(
                        "unsaved for {} min, Ctrl+S to save",
                        elapsed.as_secs() / 60
                    );
                    status(&a, &u);
                }
            }
        });
    }

    ui.invoke_focus_canvas();
    ui.run().unwrap();
}
