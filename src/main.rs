#![windows_subsystem = "windows"]

mod arrows;
mod doc;
mod picture;
mod render;
mod table;

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, SharedString, VecModel};

use doc::{Arrow, Canvas, Node, Page, Routing, Side};
use render::Renderer;
use table::Table;

slint::include_modules!();

const UNSAVED_WARN_AFTER: Duration = Duration::from_secs(10 * 60);
const DEFAULT_WIDTH: f64 = 320.0;
/// Pasted images wider than this (canvas units) are scaled down to fit.
const MAX_PASTE_WIDTH: f64 = 960.0;

#[derive(PartialEq)]
struct ArrowCacheKey {
    nodes: Vec<(SharedString, f32, f32, f32, f32)>,
    arrows: Vec<Arrow>,
    selected: Option<usize>,
}

struct App {
    doc: Canvas,
    path: Option<PathBuf>,
    dirty: bool,
    dirty_since: Option<Instant>,
    /// Index into `doc.pages` of the page on screen.
    active: usize,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    selected: Option<usize>,
    selected_arrow: Option<usize>,
    arrow_start: Option<(String, Side)>,
    link_start: Option<(String, usize)>,
    link_model: Rc<VecModel<LinkSegmentVm>>,
    blocked_links: Cell<usize>,
    arrow_model: Rc<VecModel<ArrowVm>>,
    arrow_cache: RefCell<Option<ArrowCacheKey>>,
    /// Arrow index and route of each `arrow_model` row, for hit testing
    /// without rerouting on every pointer move.
    arrow_rows: RefCell<Vec<(usize, Vec<arrows::Point>)>>,
    /// `arrow_model` row under the pointer.
    hover_row: Cell<Option<usize>>,
    /// Selected nodes besides `selected`, added with Ctrl+click. `select`
    /// clears them, so every plain selection change drops the group.
    also_selected: Vec<usize>,
    /// Node pressed inside a multi-selection: a click without a drag selects
    /// only that node on release, while a drag moves the whole group.
    collapse_on_release: Option<usize>,
    /// Pre-drag positions of every node being dragged, the pressed one first.
    drag_orig: Vec<(usize, f64, f64)>,
    resize_orig: Option<f64>,
    message: String,
    renderer: Renderer,
    model: Rc<VecModel<NodeVm>>,
    tab_model: Rc<VecModel<PageTab>>,
    /// Working copy of the table being edited in the grid editor.
    edit_table: Option<Table>,
    /// Grid editor cell index -> (row, cell) in `edit_table.cells`.
    cell_map: Vec<(usize, usize)>,
    cell_model: Rc<VecModel<CellVm>>,
    edge_model: Rc<VecModel<ColEdge>>,
    /// Column width (canvas px) at the start of a divider drag.
    col_resize_orig: Option<f64>,
    /// Node copied with Ctrl+C, for pasting back onto a canvas.
    copied: Option<Copied>,
}

/// A copied node. The system clipboard gets its Typst source (or its pixels,
/// for an embedded image) so it also pastes into other apps; `stamp`
/// fingerprints what the clipboard held right after the copy, and Ctrl+V pastes
/// the node only while the clipboard still holds that.
struct Copied {
    node: Node,
    stamp: u64,
}

/// Undo state: every page, plus the page the change was made on so that undo
/// and redo bring it into view.
struct Snapshot {
    pages: Vec<Page>,
    active: usize,
}

/// Minimum on-screen width of the grid editor, in logical pixels.
const TABLE_EDITOR_MIN_WIDTH: f64 = 480.0;
/// Minimum width of a flexible column in the grid editor, in canvas units.
const TABLE_EDITOR_MIN_COL: f64 = 48.0;
/// Distance in screen pixels within which hovering or clicking hits an arrow.
const ARROW_HIT_RADIUS: f64 = 10.0;

/// File holding the folder of the last opened or saved canvas, so file dialogs
/// start there on the next launch.
fn last_dir_file() -> Option<PathBuf> {
    let var = |k| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = if cfg!(windows) {
        var("LOCALAPPDATA")?
    } else {
        var("XDG_STATE_HOME").or_else(|| var("HOME").map(|h| h.join(".local/state")))?
    };
    Some(base.join("mcanvas").join("last_dir"))
}

fn last_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::fs::read_to_string(last_dir_file()?).ok()?.trim_end());
    dir.is_dir().then_some(dir)
}

/// Best effort: failing to record the folder never affects opening or saving.
fn remember_dir(path: &std::path::Path) {
    let (Some(file), Some(dir)) = (
        last_dir_file(),
        std::path::absolute(path).ok().and_then(|p| p.parent().map(|d| d.to_path_buf())),
    ) else {
        return;
    };
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(file, dir.to_string_lossy().as_bytes());
}

impl App {
    fn arrow_routes(&self) -> Vec<(usize, arrows::Route)> {
        let rect = |id: &str| {
            self.model
                .iter()
                .find(|n| n.id == id)
                .map(|n| arrows::Rect {
                    x: n.x as f64,
                    y: n.y as f64,
                    w: n.w as f64,
                    h: n.h as f64,
                })
        };
        let mut indices = Vec::new();
        let mut routes = Vec::new();
        for (i, e) in self.doc.pages[self.active].arrows.iter().enumerate() {
            if let (Some(a), Some(b)) = (rect(&e.from), rect(&e.to)) {
                indices.push(i);
                routes.push(arrows::route(a, e.from_side, b, e.to_side, e.routing));
            }
        }
        let mut indexed: Vec<_> = indices.into_iter().zip(routes).collect();
        arrows::resolve_links(&self.doc.pages[self.active].arrows, &mut indexed);
        let (indices, mut routes): (Vec<_>, Vec<_>) = indexed.into_iter().unzip();
        arrows::normalize(&mut routes);
        indices.into_iter().zip(routes).collect()
    }

    fn refresh_arrows(&self) {
        let key = ArrowCacheKey {
            nodes: self
                .model
                .iter()
                .map(|n| (n.id, n.x, n.y, n.w, n.h))
                .collect(),
            arrows: self.doc.pages[self.active].arrows.clone(),
            selected: self.selected_arrow,
        };
        if self.arrow_cache.borrow().as_ref() == Some(&key) {
            return;
        }
        *self.arrow_cache.borrow_mut() = Some(key);
        let routes = self.arrow_routes();
        let page = &self.doc.pages[self.active];
        let suspended: std::collections::HashSet<_> = page.arrows.iter().enumerate()
            .filter(|(i,e)| e.trunk.is_some() && !routes.iter().any(|(j,r)| i==j && r.manual))
            .filter_map(|(_,e)| e.trunk.as_ref().map(|t| &t.id)).collect();
        self.blocked_links.set(suspended.len());
        let rows: Vec<ArrowVm> = routes
            .iter()
            .map(|(i, route)| arrow_vm(&route.points, self.selected_arrow == Some(*i), route.suspended))
            .collect();
        self.arrow_model.set_vec(rows);
        *self.arrow_rows.borrow_mut() =
            routes.into_iter().map(|(i, r)| (i, r.points)).collect();
        self.hover_row.set(None);
    }

    /// Row of the arrow a background click at canvas point `p` would select:
    /// the nearest within ARROW_HIT_RADIUS screen pixels, topmost on ties.
    fn arrow_row_at(&self, p: arrows::Point, zoom: f64) -> Option<usize> {
        if !zoom.is_finite() || zoom <= 0.0 {
            return None;
        }
        self.arrow_rows
            .borrow()
            .iter()
            .enumerate()
            .map(|(row, (_, points))| (row, arrows::distance(p, points) * zoom))
            .filter(|(_, d)| *d <= ARROW_HIT_RADIUS)
            .min_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)))
            .map(|(row, _)| row)
    }

    fn set_arrow_hover(&self, row: Option<usize>) {
        let old = self.hover_row.replace(row);
        if old == row {
            return;
        }
        for (r, hovered) in [(old, false), (row, true)] {
            if let Some(mut vm) = r.and_then(|r| self.arrow_model.row_data(r)) {
                vm.hovered = hovered;
                self.arrow_model.set_row_data(r.unwrap(), vm);
            }
        }
    }

    fn clear_link(&mut self, i: usize) {
        if let Some(trunk) = self.doc.pages[self.active]
            .arrows
            .get(i)
            .and_then(|e| e.trunk.clone())
        {
            for arrow in &mut self.doc.pages[self.active].arrows {
                if arrow.trunk.as_ref().is_some_and(|t| t.id == trunk.id) {
                    arrow.trunk = None;
                }
            }
        }
    }

    fn refresh_link_ui(&self, ui: &MainWindow) {
        ui.set_link_hover(-1);
        ui.set_link_hint("Move near a highlighted middle segment".into());
        ui.set_link_step(if self.link_start.is_some() {2} else {1});
        if ui.get_connect_mode()!=3 {
            if self.link_model.row_count()>0 { self.link_model.set_vec(Vec::new()); }
            return;
        }
        let routes=self.arrow_routes();
        let first=self.link_start.as_ref().and_then(|(id,k)| routes.iter()
            .find(|(i,_)| self.doc.pages[self.active].arrows[*i].id==*id).map(|(i,r)| (*i,*k,r)));
        let mut rows=Vec::new();
        for (i,r) in &routes {
            if !r.orthogonal { continue; }
            for (k,w) in r.points.windows(2).enumerate() {
                if w[0]==w[1] { continue; }
                let selected=first.is_some_and(|(j,segment,_)| j==*i && segment==k);
                let reason = if k==0 || k+2>=r.points.len() { "Choose a middle segment, not an attachment" }
                    else if self.doc.pages[self.active].arrows[*i].trunk.is_some() { "Already linked — unlink this arrow first" }
                    else if let Some((j,first_k,first_r))=first {
                        if j==*i { "Choose a different arrow" }
                        else if arrows::shared_trunk(first_r,first_k,r,k).is_none() { "Choose a parallel segment whose span overlaps or meets the first" }
                        else { "Click to link these two middles" }
                    } else { "Click to select the first middle" };
                let eligible=reason.starts_with("Click");
                rows.push(LinkSegmentVm { arrow:*i as i32, segment:k as i32,
                    x:w[0].0.min(w[1].0) as f32,y:w[0].1.min(w[1].1) as f32,
                    w:(w[0].0-w[1].0).abs() as f32,h:(w[0].1-w[1].1).abs() as f32,
                    selected,eligible,reason:reason.into() });
            }
        }
        rows.sort_by_key(|row| row.selected);
        self.link_model.set_vec(rows);
        if !self.link_model.iter().any(|s| s.eligible) {
            ui.set_link_hint("No compatible middle segments here. Cancel or go back to choose another.".into());
        }
    }

    fn link_segment_at(&self, ui: &MainWindow, x: f64, y: f64) -> Option<usize> {
        let candidates: Vec<_>=self.link_model.iter().map(|s| (
            (s.x as f64,s.y as f64),((s.x+s.w) as f64,(s.y+s.h) as f64),s.eligible)).collect();
        link_target(&candidates,(x,y),ui.get_zoom() as f64)
    }

    fn link_middle_clicked(&mut self, ui: &MainWindow, x: f64, y: f64) {
        let routes = self.arrow_routes();
        let hit = self.link_segment_at(ui,x,y).and_then(|row| self.link_model.row_data(row));
        let Some(hit) = hit.filter(|hit| hit.eligible) else { return; };
        let (i,k)=(hit.arrow as usize,hit.segment as usize);
        if self.doc.pages[self.active].arrows[i].trunk.is_some() {
            self.message =
                "This arrow is already linked; select it and Unlink middles first".into();
            return;
        }
        if let Some((id, first_k)) = &self.link_start {
            let Some((first_i, first_route)) = routes
                .iter()
                .find(|(j, _)| self.doc.pages[self.active].arrows[*j].id == *id)
            else {
                return;
            };
            let Some((_, second_route)) = routes.iter().find(|(j, _)| *j == i) else { return; };
            let Some(trunk) = arrows::shared_trunk(first_route, *first_k, second_route, k) else {
                self.message = "Choose parallel middle segments with overlapping spans".into();
                return;
            };
            let first_i = *first_i;
            self.snapshot();
            self.doc.pages[self.active].arrows[first_i].trunk = Some(trunk.clone());
            self.doc.pages[self.active].arrows[i].trunk = Some(trunk);
            self.link_start = None;
            self.selected_arrow = Some(i);
            self.mark_dirty();
            ui.set_connect_mode(0);
            self.message = "Middle segments linked; intent will resume automatically if temporarily paused".into();
        } else {
            self.link_start = Some((self.doc.pages[self.active].arrows[i].id.clone(), k));
            self.selected_arrow = Some(i);
            self.select(None);
            self.update_selection_flags();
            self.message =
                "Click a parallel middle segment on the other arrow (Esc cancels)".into();
        }
    }

    fn root_dir(&self) -> PathBuf {
        self.path
            .as_ref()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .filter(|d| !d.as_os_str().is_empty())
            .or_else(last_dir)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }

    fn nodes(&self) -> &Vec<Node> {
        &self.doc.pages[self.active].nodes
    }

    fn nodes_mut(&mut self) -> &mut Vec<Node> {
        &mut self.doc.pages[self.active].nodes
    }

    fn build_vm(&self, i: usize) -> NodeVm {
        let node = &self.nodes()[i];
        let mut vm = NodeVm {
            id: node.id.clone().into(),
            x: node.x as f32,
            y: node.y as f32,
            w: node.width as f32,
            h: node.height.unwrap_or(40.0) as f32,
            img: Default::default(),
            error: SharedString::default(),
            selected: self.is_selected(i),
            marquee: false,
        };
        match node.kind.as_str() {
            "image" => {
                let loaded = match &node.data {
                    Some(data) => picture::slint_image_from_base64(data)
                        .map_err(|e| format!("cannot decode embedded image: {e}")),
                    None => {
                        let src = node.src.clone().unwrap_or_default();
                        let path = if Path::new(&src).is_absolute() {
                            PathBuf::from(&src)
                        } else {
                            self.root_dir().join(&src)
                        };
                        slint::Image::load_from_path(&path)
                            .map_err(|_| format!("cannot load image: {}", path.display()))
                    }
                };
                match loaded {
                    Ok(img) => {
                        let size = img.size();
                        if node.height.is_none() && size.width > 0 {
                            vm.h = (node.width * size.height as f64 / size.width as f64) as f32;
                        }
                        vm.img = img;
                    }
                    Err(e) => {
                        vm.error = e.into();
                        vm.h = 60.0;
                    }
                }
            }
            "table" => match &node.table {
                Some(t) => self.render_typst(&mut vm, node, &t.to_typst()),
                None => {
                    vm.error = "table node has no \"table\" data".into();
                    vm.h = 60.0;
                }
            },
            _ => self.render_typst(&mut vm, node, &node.source),
        }
        vm
    }

    fn render_typst(&self, vm: &mut NodeVm, node: &Node, source: &str) {
        match self
            .renderer
            .render(&self.root_dir(), &self.doc.preamble, source, node.width)
        {
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

    fn refresh_node(&self, i: usize) {
        self.model.set_row_data(i, self.build_vm(i));
    }

    fn refresh_all(&self) {
        let vms: Vec<NodeVm> = (0..self.nodes().len()).map(|i| self.build_vm(i)).collect();
        self.model.set_vec(vms);
        self.refresh_arrows();
    }

    fn refresh_tabs(&self) {
        let tabs: Vec<PageTab> = self
            .doc
            .pages
            .iter()
            .enumerate()
            .map(|(i, p)| PageTab { name: p.name.clone().into(), active: i == self.active })
            .collect();
        replace_rows(&self.tab_model, tabs);
    }

    /// Make `i` the only selected node, or clear the node selection.
    fn select(&mut self, i: Option<usize>) {
        self.selected = i;
        self.also_selected.clear();
        self.collapse_on_release = None;
    }

    fn is_selected(&self, i: usize) -> bool {
        self.selected == Some(i) || self.also_selected.contains(&i)
    }

    /// Selected nodes, primary first.
    fn selection(&self) -> Vec<usize> {
        self.selected.into_iter().chain(self.also_selected.iter().copied()).collect()
    }

    /// Make `i` the primary node (the one Enter and arrow keys act on),
    /// keeping the rest of the selection.
    fn make_primary(&mut self, i: usize) {
        if self.selected == Some(i) {
            return;
        }
        self.also_selected.retain(|&j| j != i);
        self.also_selected.extend(self.selected.replace(i));
    }

    fn deselect(&mut self, i: usize) {
        if self.selected == Some(i) {
            self.selected = self.also_selected.pop();
        } else {
            self.also_selected.retain(|&j| j != i);
        }
    }

    /// Nodes a selection rectangle from (x0, y0) to (x1, y1) selects. Dragged
    /// rightwards it takes nodes fully inside; leftwards, any node it touches.
    fn marquee_hits(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> Vec<usize> {
        let (l, r, t, b) = (x0.min(x1), x0.max(x1), y0.min(y1), y0.max(y1));
        let crossing = x1 < x0;
        self.model
            .iter()
            .enumerate()
            .filter(|(_, n)| {
                if crossing {
                    n.x < r && n.x + n.w > l && n.y < b && n.y + n.h > t
                } else {
                    n.x >= l && n.x + n.w <= r && n.y >= t && n.y + n.h <= b
                }
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn set_marquee_preview(&self, hits: &[usize]) {
        for i in 0..self.model.row_count() {
            if let Some(mut vm) = self.model.row_data(i) {
                let want = hits.contains(&i);
                if vm.marquee != want {
                    vm.marquee = want;
                    self.model.set_row_data(i, vm);
                }
            }
        }
    }

    fn set_node_pos(&mut self, i: usize, x: f64, y: f64) {
        self.nodes_mut()[i].x = x;
        self.nodes_mut()[i].y = y;
        if let Some(mut vm) = self.model.row_data(i) {
            vm.x = x as f32;
            vm.y = y as f32;
            self.model.set_row_data(i, vm);
        }
    }

    fn update_selection_flags(&self) {
        for i in 0..self.model.row_count() {
            if let Some(mut vm) = self.model.row_data(i) {
                let want = self.is_selected(i);
                if vm.selected != want {
                    vm.selected = want;
                    self.model.set_row_data(i, vm);
                }
            }
        }
    }

    fn snapshot(&mut self) {
        self.undo.push(Snapshot { pages: self.doc.pages.clone(), active: self.active });
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
            .unwrap_or_else(|| PathBuf::from("untitled.mc"));
        self.doc.page = self.doc.pages[self.active].id.clone();
        self.doc.save(&path)?;
        remember_dir(&path);
        self.path = Some(path);
        self.dirty = false;
        self.dirty_since = None;
        Ok(())
    }

    /// Replace the document and reset all editing state.
    fn load_doc(&mut self, doc: Canvas, path: Option<PathBuf>, ui: &MainWindow) {
        self.active = doc.start_page();
        self.doc = doc;
        ui.set_connect_mode(0);
        self.apply_view(ui);
        if let Some(p) = &path {
            remember_dir(p);
        }
        self.path = path;
        self.dirty = false;
        self.dirty_since = None;
        self.undo.clear();
        self.redo.clear();
        self.select(None);
        self.selected_arrow = None;
        self.arrow_start = None;
        self.link_start = None;
        self.drag_orig.clear();
        self.resize_orig = None;
        self.message.clear();
        self.refresh_all();
        self.refresh_tabs();
    }

    /// Remember the viewport in the active page.
    fn store_view(&mut self, ui: &MainWindow) {
        let view = &mut self.doc.pages[self.active].view;
        view.x = ui.get_pan_x() as f64;
        view.y = ui.get_pan_y() as f64;
        view.zoom = ui.get_zoom() as f64;
    }

    /// Show the active page's saved viewport.
    fn apply_view(&self, ui: &MainWindow) {
        let view = &self.doc.pages[self.active].view;
        ui.set_pan_x(view.x as f32);
        ui.set_pan_y(view.y as f32);
        ui.set_zoom(view.zoom as f32);
    }

    /// Put page `i` on screen, keeping each page's own viewport.
    fn show_page(&mut self, ui: &MainWindow, i: usize) {
        if i >= self.doc.pages.len() {
            return;
        }
        if i != self.active {
            self.store_view(ui);
            self.active = i;
            ui.set_connect_mode(0);
            self.apply_view(ui);
            self.select(None);
            self.selected_arrow = None;
            self.arrow_start = None;
            self.link_start = None;
            self.drag_orig.clear();
            self.resize_orig = None;
            self.refresh_all();
        }
        self.refresh_tabs();
    }

    /// Page tab actions. `i` is the tab the action targets, or -1 for the
    /// active page. Open editors were committed by the UI beforehand.
    fn page_op(&mut self, ui: &MainWindow, op: &str, i: i32) {
        self.arrow_start = None;
        self.link_start = None;
        self.selected_arrow = None;
        ui.set_connect_mode(0);
        let n = self.doc.pages.len();
        let i = usize::try_from(i).ok().filter(|&i| i < n).unwrap_or(self.active);
        match op {
            "select" => self.show_page(ui, i),
            "next" => self.show_page(ui, (self.active + 1) % n),
            "prev" => self.show_page(ui, (self.active + n - 1) % n),
            "add" => {
                self.snapshot();
                let page = Page::new(self.doc.next_page_name());
                self.doc.pages.insert(self.active + 1, page);
                self.mark_dirty();
                self.show_page(ui, self.active + 1);
                ui.set_rename_page(self.active as i32);
            }
            "duplicate" => {
                self.snapshot();
                let mut page = self.doc.pages[i].clone();
                page.id = doc::new_id();
                page.name = format!("{} copy", page.name);
                // Node ids stay unique across the whole file.
                let mut ids = std::collections::HashMap::new();
                for node in &mut page.nodes {
                    let old = node.id.clone();
                    node.id = doc::new_id();
                    ids.insert(old, node.id.clone());
                }
                for arrow in &mut page.arrows {
                    arrow.id = doc::new_id();
                    if let Some(trunk) = &mut arrow.trunk {
                        trunk.id = ids.entry(trunk.id.clone()).or_insert_with(doc::new_id).clone();
                    }
                    if let Some(id) = ids.get(&arrow.from) { arrow.from = id.clone(); }
                    if let Some(id) = ids.get(&arrow.to) { arrow.to = id.clone(); }
                }
                if i == self.active {
                    self.store_view(ui);
                    page.view = self.doc.pages[i].view.clone();
                }
                self.doc.pages.insert(i + 1, page);
                if self.active > i {
                    self.active += 1;
                }
                self.mark_dirty();
                self.show_page(ui, i + 1);
            }
            "rename" => ui.set_rename_page(i as i32),
            "move-left" | "move-right" => {
                let j = if op == "move-left" { i.checked_sub(1) } else { Some(i + 1).filter(|&j| j < n) };
                let Some(j) = j else { return };
                self.snapshot();
                self.doc.pages.swap(i, j);
                if self.active == i {
                    self.active = j;
                } else if self.active == j {
                    self.active = i;
                }
                self.mark_dirty();
                self.refresh_tabs();
            }
            "delete" => {
                if n == 1 {
                    self.message = "cannot delete the only page".into();
                    return;
                }
                self.snapshot();
                self.store_view(ui);
                self.doc.pages.remove(i);
                let was_active = i == self.active;
                if self.active > i || self.active == n - 1 {
                    self.active -= 1;
                }
                if was_active {
                    ui.set_connect_mode(0);
                    self.apply_view(ui);
                    self.select(None);
                    self.selected_arrow = None;
                    self.arrow_start = None;
                    self.link_start = None;
                    self.refresh_all();
                }
                self.mark_dirty();
                self.refresh_tabs();
                self.message = "page deleted (Ctrl+Z to undo)".into();
            }
            _ => {}
        }
    }

    fn rename_page(&mut self, i: usize, name: &str) {
        let name = name.trim();
        if i < self.doc.pages.len() && !name.is_empty() && self.doc.pages[i].name != name {
            self.snapshot();
            self.doc.pages[i].name = name.to_string();
            self.mark_dirty();
        }
        self.refresh_tabs();
    }

    fn save_as_dialog(&self) -> Option<PathBuf> {
        let mut d = rfd::FileDialog::new()
            .add_filter("mcanvas", &["mc"])
            .set_directory(self.root_dir());
        if let Some(name) = self.path.as_ref().and_then(|p| p.file_name()) {
            d = d.set_file_name(name.to_string_lossy());
        } else {
            d = d.set_file_name("untitled.mc");
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
        if self.nodes().is_empty() {
            return;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for (i, n) in self.nodes().iter().enumerate() {
            let h = self.model.row_data(i).map(|v| v.h as f64).unwrap_or(0.0);
            x0 = x0.min(n.x);
            y0 = y0.min(n.y);
            x1 = x1.max(n.x + n.width);
            y1 = y1.max(n.y + h);
        }
        for (_, route) in self.arrow_routes() {
            for (x, y) in route.points {
                x0 = x0.min(x); y0 = y0.min(y);
                x1 = x1.max(x); y1 = y1.max(y);
            }
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
            data: None,
            table: None,
        };
        self.push_node(node)
    }

    fn add_table(&mut self, x: f64, y: f64) -> usize {
        self.snapshot();
        let node = Node {
            id: doc::new_id(),
            kind: "table".into(),
            x: self.doc.snap(x),
            y: self.doc.snap(y),
            width: DEFAULT_WIDTH,
            height: None,
            source: String::new(),
            src: None,
            data: None,
            table: Some(Table::starter()),
        };
        self.push_node(node)
    }

    fn push_node(&mut self, node: Node) -> usize {
        self.selected_arrow = None;
        self.arrow_start = None;
        self.link_start = None;
        self.nodes_mut().push(node);
        let i = self.nodes().len() - 1;
        self.select(Some(i));
        self.model.push(self.build_vm(i));
        self.update_selection_flags();
        self.mark_dirty();
        i
    }

    /// Canvas point under the mouse, if it is over the canvas.
    fn pointer_pos(&self, ui: &MainWindow) -> Option<(f64, f64)> {
        let z = ui.get_zoom() as f64;
        ui.get_pointer_in().then(|| {
            (
                (ui.get_pointer_x() - ui.get_pan_x()) as f64 / z,
                (ui.get_pointer_y() - ui.get_pan_y()) as f64 / z,
            )
        })
    }

    /// Paste the clipboard as a new node with its top-left corner at `at`, or
    /// centered in the view: a node copied with Ctrl+C pastes as itself, an
    /// image (e.g. a screen snip) becomes an embedded image node, text becomes
    /// a Typst node.
    fn paste(&mut self, ui: &MainWindow, at: Option<(f64, f64)>) {
        self.arrow_start = None;
        self.link_start = None;
        ui.set_connect_mode(0);
        let mut clip = match arboard::Clipboard::new() {
            Ok(c) => c,
            Err(e) => {
                self.message = format!("clipboard error: {e}");
                return;
            }
        };
        if self.copied.as_ref().is_some_and(|c| clipboard_stamp(&mut clip) == Some(c.stamp)) {
            self.paste_node(ui, at);
            return;
        }
        let cx = ((ui.get_view_width() / 2.0 - ui.get_pan_x()) / ui.get_zoom()) as f64;
        let cy = ((ui.get_view_height() / 2.0 - ui.get_pan_y()) / ui.get_zoom()) as f64;
        if let Ok(img) = clip.get_image() {
            let (w, h) = (img.width, img.height);
            let data = match picture::rgba_to_png_base64(w as u32, h as u32, img.bytes.into_owned()) {
                Ok(d) => d,
                Err(e) => {
                    self.message = format!("cannot encode pasted image: {e}");
                    return;
                }
            };
            // Show the snip at the size it had on screen: clipboard pixels are
            // physical, canvas units are logical.
            let scale = ui.window().scale_factor().max(0.1) as f64;
            let width = self.doc.snap((w as f64 / scale).min(MAX_PASTE_WIDTH)).max(self.doc.grid.max(8.0));
            let height = width * h as f64 / w.max(1) as f64;
            let (x, y) = at.unwrap_or((cx - width / 2.0, cy - height / 2.0));
            self.snapshot();
            self.push_node(Node {
                id: doc::new_id(),
                kind: "image".into(),
                x: self.doc.snap(x),
                y: self.doc.snap(y),
                width,
                height: None,
                source: String::new(),
                src: None,
                data: Some(data.into()),
                table: None,
            });
            self.message = format!("pasted {w}x{h} image");
        } else if let Ok(text) = clip.get_text() {
            if text.trim().is_empty() {
                self.message = "clipboard is empty".into();
                return;
            }
            let (x, y) = at.unwrap_or((cx - DEFAULT_WIDTH / 2.0, cy));
            self.snapshot();
            self.push_node(Node {
                id: doc::new_id(),
                kind: "typst".into(),
                x: self.doc.snap(x),
                y: self.doc.snap(y),
                width: DEFAULT_WIDTH,
                height: None,
                source: text,
                src: None,
                data: None,
                table: None,
            });
            self.message = "pasted text".into();
        } else {
            self.message = "nothing to paste".into();
        }
    }

    /// Paste a copy of the copied node with its top-left corner at `at`. Without
    /// a point it goes down and right of where it was copied (or last pasted)
    /// so repeated pastes cascade, or centered in the view when that spot is
    /// off screen.
    fn paste_node(&mut self, ui: &MainWindow, at: Option<(f64, f64)>) {
        let z = ui.get_zoom() as f64;
        let (pan_x, pan_y) = (ui.get_pan_x() as f64, ui.get_pan_y() as f64);
        let (x0, y0) = (-pan_x / z, -pan_y / z);
        let (x1, y1) = ((ui.get_view_width() as f64 - pan_x) / z, (ui.get_view_height() as f64 - pan_y) / z);
        let step = if self.doc.grid > 0.0 { self.doc.grid * 3.0 } else { 24.0 };
        let Some(c) = &mut self.copied else { return };
        let mut node = c.node.clone();
        node.id = doc::new_id();
        let (x, y) = at.unwrap_or((node.x + step, node.y + step));
        if at.is_some() || (x >= x0 && x < x1 && y >= y0 && y < y1) {
            (node.x, node.y) = (x, y);
        } else {
            let h = node.height.unwrap_or(0.0);
            (node.x, node.y) = ((x0 + x1 - node.width) / 2.0, (y0 + y1 - h) / 2.0);
        }
        node.x = self.doc.snap(node.x);
        node.y = self.doc.snap(node.y);
        (c.node.x, c.node.y) = (node.x, node.y);
        self.snapshot();
        self.push_node(node);
        self.message = "pasted node".into();
    }

    fn copy_selected(&mut self) {
        match self.selected {
            Some(i) => self.copy_node(i),
            None => self.message = "select a node to copy".into(),
        }
    }

    /// Copy a node for pasting with Ctrl+V.
    fn copy_node(&mut self, i: usize) {
        let node = self.nodes()[i].clone();
        let typst = self.node_as_typst(i);
        match put_node_on_clipboard(&node, typst) {
            Ok(stamp) => {
                self.copied = Some(Copied { node, stamp });
                self.message = "copied node".into();
            }
            Err(e) => self.message = format!("clipboard error: {e}"),
        }
    }

    /// The node as standalone Typst markup, for the clipboard.
    fn node_as_typst(&self, i: usize) -> String {
        let n = &self.nodes()[i];
        match n.kind.as_str() {
            "table" => n.table.as_ref().map(Table::to_typst).unwrap_or_default(),
            "image" => format!("#image(\"{}\")\n", n.src.clone().unwrap_or_default()),
            _ => {
                let mut s = n.source.clone();
                if !s.ends_with('\n') {
                    s.push('\n');
                }
                s
            }
        }
    }

    fn copy_to_clipboard(&mut self, text: String) {
        self.copied = None;
        match arboard::Clipboard::new().and_then(|mut c| c.set_text(text)) {
            Ok(()) => self.message = "copied as Typst".into(),
            Err(e) => self.message = format!("clipboard error: {e}"),
        }
    }

    /// Open the editor appropriate for the node's kind.
    fn open_editor(&mut self, ui: &MainWindow, i: usize) {
        self.selected_arrow = None;
        self.arrow_start = None;
        self.link_start = None;
        ui.set_connect_mode(0);
        self.select(Some(i));
        self.update_selection_flags();
        self.drag_orig.clear();
        let node = &self.nodes()[i];
        if node.kind == "table" {
            self.edit_table = Some(node.table.clone().unwrap_or_else(Table::starter));
            ui.invoke_begin_table_edit(i as i32);
            self.refresh_table_editor(ui);
        } else {
            ui.invoke_begin_edit(i as i32, node.source.clone().into());
        }
    }

    /// Rebuild the grid editor's cell and divider models from the working
    /// copy. Rows are updated in place when the count is unchanged so that a
    /// divider drag in progress keeps its TouchArea alive.
    fn refresh_table_editor(&mut self, ui: &MainWindow) {
        let Some(t) = &self.edit_table else { return };
        let i = ui.get_table_edit_index();
        let node_w = usize::try_from(i)
            .ok()
            .and_then(|i| self.nodes().get(i))
            .map_or(DEFAULT_WIDTH, |n| n.width);
        let zoom = ui.get_zoom().max(0.01) as f64;
        let total = node_w.max(TABLE_EDITOR_MIN_WIDTH / zoom);
        let widths = t.column_widths(total, TABLE_EDITOR_MIN_COL);
        let mut starts = vec![0.0; widths.len() + 1];
        for (k, w) in widths.iter().enumerate() {
            starts[k + 1] = starts[k] + w;
        }
        let (pos, nrows) = t.layout();
        self.cell_map.clear();
        let mut vms = Vec::new();
        for (ri, row) in t.cells.iter().enumerate() {
            for (ci, cell) in row.iter().enumerate() {
                let (x, y) = pos[ri][ci];
                let span = cell.colspan().min(widths.len() - x);
                vms.push(CellVm {
                    r: y as i32,
                    x: starts[x] as f32,
                    w: (starts[x + span] - starts[x]) as f32,
                    text: cell.body.clone().into(),
                    fill: cell.fill.as_deref().and_then(parse_hex).unwrap_or(slint::Color::from_argb_u8(0, 0, 0, 0)),
                    header: ri < t.header,
                });
                self.cell_map.push((ri, ci));
            }
        }
        let edges: Vec<ColEdge> = (0..widths.len())
            .map(|c| ColEdge { col: c as i32, x: starts[c + 1] as f32 })
            .collect();
        ui.set_table_rows(nrows.max(1) as i32);
        ui.set_table_width(starts[widths.len()] as f32);
        replace_rows(&self.cell_model, vms);
        replace_rows(&self.edge_model, edges);
    }

    fn table_col_resize_start(&mut self, ui: &MainWindow, col: usize) {
        let Some(t) = &self.edit_table else { return };
        let zoom = ui.get_zoom().max(0.01) as f64;
        let node_w = usize::try_from(ui.get_table_edit_index())
            .ok()
            .and_then(|i| self.nodes().get(i))
            .map_or(DEFAULT_WIDTH, |n| n.width);
        let widths = t.column_widths(node_w.max(TABLE_EDITOR_MIN_WIDTH / zoom), TABLE_EDITOR_MIN_COL);
        self.col_resize_orig = widths.get(col).copied();
    }

    fn table_col_resize(&mut self, ui: &MainWindow, col: usize, dw: f64) {
        let Some(orig) = self.col_resize_orig else { return };
        if let Some(t) = &mut self.edit_table {
            t.set_col_width_px(col, (orig + dw).max(24.0));
        }
        self.refresh_table_editor(ui);
    }

    /// Apply a context-menu operation to the working copy. `k` is the cell
    /// the menu was opened on.
    fn apply_table_op(&mut self, op: &str, k: usize) {
        let Some(t) = &mut self.edit_table else { return };
        let Some(&(ri, ci)) = self.cell_map.get(k) else { return };
        let (pos, _) = t.layout();
        let (x, _) = pos[ri][ci];
        let span = t.cells[ri][ci].colspan();
        match op {
            "row-above" => t.insert_row(ri),
            "row-below" => t.insert_row(ri + 1),
            "row-delete" => t.delete_row(ri),
            "col-left" => t.insert_col(x),
            "col-right" => t.insert_col(x + span),
            "col-delete" => t.delete_col(x),
            "header-toggle" => t.header = if t.header > 0 { 0 } else { 1 },
            _ => {
                if let Some(v) = op.strip_prefix("fill:") {
                    t.cells[ri][ci].fill = if v.is_empty() { None } else { Some(v.to_string()) };
                } else if let Some(v) = op.strip_prefix("row-fill:") {
                    for c in &mut t.cells[ri] {
                        c.fill = if v.is_empty() { None } else { Some(v.to_string()) };
                    }
                } else if let Some(v) = op.strip_prefix("align:") {
                    t.set_col_align(x, v);
                } else if let Some(v) = op.strip_prefix("col-track:") {
                    t.set_col_track(x, v);
                }
            }
        }
    }

    fn commit_table_edit(&mut self, ui: &MainWindow) {
        let i = ui.get_table_edit_index();
        if let (Some(t), true) = (self.edit_table.take(), i >= 0) {
            let i = i as usize;
            if i < self.nodes().len() && self.nodes()[i].table.as_ref() != Some(&t) {
                self.snapshot();
                // Widen the node if fixed column widths no longer fit.
                let needed = t.min_width_px(TABLE_EDITOR_MIN_COL) + 16.0;
                if needed > self.nodes()[i].width {
                    let w = self.doc.snap(needed).max(needed);
                    self.nodes_mut()[i].width = w;
                }
                self.nodes_mut()[i].table = Some(t);
                self.refresh_node(i);
                self.mark_dirty();
            }
        }
        ui.invoke_end_table_edit();
    }

    fn delete_selected(&mut self) {
        if let Some(i) = self.selected_arrow.take() {
            self.link_start = None;
            if i < self.doc.pages[self.active].arrows.len() {
                self.snapshot();
                self.doc.pages[self.active].arrows.remove(i);
                self.mark_dirty();
            }
            return;
        }
        let mut doomed = self.selection();
        if !doomed.is_empty() {
            self.snapshot();
            // Highest index first so the remaining indices stay valid.
            doomed.sort_unstable_by(|a, b| b.cmp(a));
            for i in doomed {
                let id = self.nodes_mut().remove(i).id;
                self.doc.pages[self.active].arrows.retain(|e| e.from != id && e.to != id);
                self.model.remove(i);
            }
            self.select(None);
            self.selected_arrow = None;
            self.arrow_start = None;
            self.link_start = None;
            self.mark_dirty();
        }
    }

    fn undo(&mut self, ui: &MainWindow) {
        if let Some(prev) = self.undo.pop() {
            let current = self.restore(ui, prev);
            self.redo.push(current);
        }
    }

    fn redo(&mut self, ui: &MainWindow) {
        if let Some(next) = self.redo.pop() {
            let current = self.restore(ui, next);
            self.undo.push(current);
        }
    }

    /// Swap in an undo/redo state and return the replaced one, tagged with the
    /// same page so stepping back again returns there. Viewports are not part
    /// of the history, so each page keeps its current one.
    fn restore(&mut self, ui: &MainWindow, snap: Snapshot) -> Snapshot {
        self.store_view(ui);
        let pages = std::mem::replace(&mut self.doc.pages, snap.pages);
        for page in &mut self.doc.pages {
            if let Some(old) = pages.iter().find(|p| p.id == page.id) {
                page.view = old.view.clone();
            }
        }
        self.active = snap.active.min(self.doc.pages.len() - 1);
        ui.set_connect_mode(0);
        self.apply_view(ui);
        self.select(None);
        self.selected_arrow = None;
        self.arrow_start = None;
        self.link_start = None;
        self.refresh_all();
        self.refresh_tabs();
        self.mark_dirty();
        Snapshot { pages, active: self.active }
    }

    /// Spatial navigation: pick the node in the given direction whose center
    /// lies within a 90 degree cone, minimizing along + 2 * across distance.
    fn navigate(&mut self, dx: f64, dy: f64) {
        self.selected_arrow = None;
        let Some(cur) = self.selected else {
            if !self.nodes().is_empty() {
                self.select(Some(0));
                self.update_selection_flags();
            }
            return;
        };
        let center = |i: usize| {
            let n = &self.nodes()[i];
            let h = self.model.row_data(i).map(|v| v.h as f64).unwrap_or(0.0);
            (n.x + n.width / 2.0, n.y + h / 2.0)
        };
        let (cx, cy) = center(cur);
        let mut best: Option<(f64, usize)> = None;
        for i in 0..self.nodes().len() {
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
            self.select(Some(i));
            self.update_selection_flags();
        }
    }
}

/// Generous 18-pixel radius independent of zoom; prefer valid targets, then
/// the nearest one. Hover and click use this exact same selection rule.
fn link_target(candidates: &[(arrows::Point,arrows::Point,bool)], p: arrows::Point, zoom:f64) -> Option<usize> {
    if !zoom.is_finite() || zoom<=0.0 { return None; }
    candidates.iter().enumerate().filter_map(|(i,(a,b,eligible))| {
        let d=arrows::distance(p,&[*a,*b])*zoom;
        (d<=18.0).then_some((i,*eligible,d))
    }).min_by(|a,b| b.1.cmp(&a.1).then_with(|| a.2.total_cmp(&b.2))).map(|(i,_,_)| i)
}

/// Vector paths avoid allocating a huge arrow image when nodes are far apart.
fn arrow_vm(points: &[arrows::Point], selected: bool, suspended: bool) -> ArrowVm {
    if points.len() < 2 || points.iter().any(|p| !p.0.is_finite() || !p.1.is_finite()) {
        return ArrowVm::default();
    }
    let min_x = points.iter().map(|p| p.0).fold(f64::INFINITY, f64::min) - 10.0;
    let min_y = points.iter().map(|p| p.1).fold(f64::INFINITY, f64::min) - 10.0;
    let w = points.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max) - min_x + 10.0;
    let h = points.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max) - min_y + 10.0;
    let mut d = String::new();
    if suspended {
        for w in points.windows(2) {
            let dx = w[1].0-w[0].0;
            let dy = w[1].1-w[0].1;
            let length = dx.hypot(dy);
            if length == 0.0 { continue; }
            // Bound path complexity even for extremely long off-screen runs.
            let step = 10.0_f64.max(length / 256.0);
            let mut along = 0.0;
            while along < length {
                let end = (along + step * 0.6).min(length);
                d.push_str(&format!("M {} {} L {} {} ",
                    w[0].0-min_x+dx*along/length,w[0].1-min_y+dy*along/length,
                    w[0].0-min_x+dx*end/length,w[0].1-min_y+dy*end/length));
                along += step;
            }
        }
    } else {
        for (j,p) in points.iter().enumerate() {
            d.push_str(&format!("{} {} {} ",if j==0 {"M"} else {"L"},p.0-min_x,p.1-min_y));
        }
    }
    let end = points[points.len() - 1];
    let before = points
        .iter()
        .rev()
        .find(|q| **q != end)
        .copied()
        .unwrap_or((end.0 - 1.0, end.1));
    let angle = (end.1 - before.1).atan2(end.0 - before.0);
    for offset in [-0.5_f64, 0.5] {
        let a = angle + offset;
        d.push_str(&format!(
            "M {} {} L {} {} ",
            end.0 - min_x - 10.0 * a.cos(),
            end.1 - min_y - 10.0 * a.sin(),
            end.0 - min_x,
            end.1 - min_y
        ));
    }
    let color = if selected { slint::Color::from_rgb_u8(37,99,235) }
        else if suspended { slint::Color::from_rgb_u8(180,83,9) }
        else { slint::Color::from_rgb_u8(82,82,91) };
    ArrowVm { x:min_x as f32,y:min_y as f32,w:w as f32,h:h as f32,commands:d.into(),color,hovered:false }

}

/// Put a copied node on the system clipboard and return the clipboard's
/// fingerprint afterwards. It is read back rather than computed from what was
/// set, since the OS may convert the data on the way in.
fn put_node_on_clipboard(node: &Node, typst: String) -> Result<u64, String> {
    let mut clip = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    match &node.data {
        Some(data) => {
            let img = picture::base64_to_rgba(data)?;
            clip.set_image(arboard::ImageData {
                width: img.width() as usize,
                height: img.height() as usize,
                bytes: img.into_raw().into(),
            })
        }
        None => clip.set_text(typst),
    }
    .map_err(|e| e.to_string())?;
    clipboard_stamp(&mut clip).ok_or_else(|| "the clipboard did not keep the copy".into())
}

/// Hash of the clipboard contents, checked in the same order `paste` reads
/// them: image first, then text.
fn clipboard_stamp(clip: &mut arboard::Clipboard) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    if let Ok(img) = clip.get_image() {
        (img.width, img.height, &img.bytes[..]).hash(&mut h);
    } else if let Ok(text) = clip.get_text() {
        text.hash(&mut h);
    } else {
        return None;
    }
    Some(h.finish())
}

/// Update a model in place when the row count matches, otherwise replace it.
/// In-place updates keep the repeated Slint elements (and any drag on them) alive.
fn replace_rows<T: Clone + PartialEq + 'static>(model: &Rc<VecModel<T>>, rows: Vec<T>) {
    if model.row_count() == rows.len() {
        for (k, row) in rows.into_iter().enumerate() {
            if model.row_data(k).as_ref() != Some(&row) {
                model.set_row_data(k, row);
            }
        }
    } else {
        model.set_vec(rows);
    }
}

/// Parse `#rgb`, `#rrggbb` or `#rrggbbaa` into a Slint color for the editor
/// preview. Other Typst color expressions are not previewed.
fn parse_hex(s: &str) -> Option<slint::Color> {
    let h = s.trim().strip_prefix('#')?;
    let v = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    let d = |i: usize| u8::from_str_radix(&h[i..i + 1], 16).ok().map(|x| x * 17);
    let (r, g, b, a) = match h.len() {
        3 => (d(0)?, d(1)?, d(2)?, 255),
        4 => (d(0)?, d(1)?, d(2)?, d(3)?),
        6 => (v(0)?, v(2)?, v(4)?, 255),
        8 => (v(0)?, v(2)?, v(4)?, v(6)?),
        _ => return None,
    };
    Some(slint::Color::from_argb_u8(a, r, g, b))
}

fn status(app: &App, ui: &MainWindow) {
    let name = app
        .path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "untitled".into());
    let dirty = if app.dirty { " [modified]" } else { "" };
    let mut left = format!("{name}{dirty}");
    if !app.also_selected.is_empty() {
        left.push_str(&format!("    {} nodes selected", app.also_selected.len() + 1));
    }
    if !app.message.is_empty() {
        left.push_str("    ");
        left.push_str(&app.message);
    }
    if app.blocked_links.get() > 0 {
        left.push_str("    Linked middles paused; intent saved and will retry when the layout allows");
    }
    ui.set_status_left(left.into());
    ui.set_status_right(
        format!(
            "{}   {} nodes   zoom {:.0}%",
            app.doc.pages[app.active].name,
            app.nodes().len(),
            ui.get_zoom() * 100.0
        )
        .into(),
    );
}

fn main() {
    let path = std::env::args().nth(1).map(PathBuf::from);
    let doc = match &path {
        Some(p) if p.exists() => match Canvas::load(p) {
            Ok(d) => {
                remember_dir(p);
                d
            }
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
    let arrow_model = Rc::new(VecModel::<ArrowVm>::default());
    ui.set_arrows(arrow_model.clone().into());
    let link_model = Rc::new(VecModel::<LinkSegmentVm>::default());
    ui.set_link_segments(link_model.clone().into());
    let cell_model = Rc::new(VecModel::<CellVm>::default());
    ui.set_edit_cells(cell_model.clone().into());
    let edge_model = Rc::new(VecModel::<ColEdge>::default());
    ui.set_edit_col_edges(edge_model.clone().into());
    let tab_model = Rc::new(VecModel::<PageTab>::default());
    ui.set_pages(tab_model.clone().into());

    let app = Rc::new(RefCell::new(App {
        active: doc.start_page(),
        doc,
        path,
        dirty: false,
        dirty_since: None,
        undo: Vec::new(),
        redo: Vec::new(),
        selected: None,
        selected_arrow: None,
        arrow_start: None,
        link_start: None,
        link_model,
        blocked_links: Cell::new(0),
        arrow_model,
        arrow_cache: RefCell::new(None),
        arrow_rows: RefCell::new(Vec::new()),
        hover_row: Cell::new(None),
        also_selected: Vec::new(),
        collapse_on_release: None,
        copied: None,
        drag_orig: Vec::new(),
        resize_orig: None,
        message: String::new(),
        renderer: Renderer::new(),
        model,
        tab_model,
        edit_table: None,
        cell_map: Vec::new(),
        cell_model,
        edge_model,
        col_resize_orig: None,
    }));
    {
        let a = app.borrow();
        a.apply_view(&ui);
        a.refresh_all();
        a.refresh_tabs();
    }
    status(&app.borrow(), &ui);

    macro_rules! hook {
        ($cb:ident, |$a:ident, $u:ident $(, $arg:ident)*| $body:block) => {{
            let app = app.clone();
            let weak = ui.as_weak();
            ui.$cb(move |$($arg),*| {
                let $u = weak.unwrap();
                let mut $a = app.borrow_mut();
                let r = (|| $body)();
                $a.refresh_arrows();
                $a.refresh_link_ui(&$u);
                status(&$a, &$u);
                r
            });
        }};
    }

    // Hover only reads the candidate model, avoiding routing on every pointer move.
    {
        let app=app.clone(); let weak=ui.as_weak();
        ui.on_link_pointer(move |x,y,inside| {
            let Some(ui)=weak.upgrade() else { return; };
            let a=app.borrow();
            let hit=if inside { a.link_segment_at(&ui,x as f64,y as f64) } else { None };
            ui.set_link_hover(hit.map(|i| i as i32).unwrap_or(-1));
            ui.set_link_hint(hit.and_then(|i| a.link_model.row_data(i)).map(|s| s.reason)
                .unwrap_or_else(|| "Move near a highlighted middle segment".into()));
        });
    }

    // Show which arrow a background click would select. Returns whether one
    // is under the pointer so the canvas can show a pointer cursor.
    {
        let app = app.clone();
        let weak = ui.as_weak();
        ui.on_canvas_pointer(move |x, y, inside| {
            let Some(ui) = weak.upgrade() else { return false; };
            let a = app.borrow();
            let row = if inside {
                a.arrow_row_at((x as f64, y as f64), ui.get_zoom() as f64)
            } else {
                None
            };
            a.set_arrow_hover(row);
            row.is_some()
        });
    }

    // Preview only touches node flags, so it skips the per-callback refreshes.
    {
        let preview = app.clone();
        ui.on_marquee_update(move |x0, y0, x1, y1| {
            let a = preview.borrow();
            a.set_marquee_preview(&a.marquee_hits(x0, y0, x1, y1));
        });
        let cancel = app.clone();
        ui.on_marquee_cancel(move || cancel.borrow().set_marquee_preview(&[]));
    }

    hook!(on_marquee_end, |a, u, x0, y0, x1, y1, add| {
        let hits = a.marquee_hits(x0, y0, x1, y1);
        a.set_marquee_preview(&[]);
        if u.get_connect_mode() != 0 {
            a.arrow_start = None;
            a.link_start = None;
            u.set_connect_mode(0);
        }
        a.selected_arrow = None;
        a.message.clear();
        if !add {
            a.select(None);
        }
        for i in hits {
            if a.selected.is_none() {
                a.selected = Some(i);
            } else if !a.is_selected(i) {
                a.also_selected.push(i);
            }
        }
        a.update_selection_flags();
    });

    hook!(on_port_clicked, |a, u, i, side| {
        let Some(node) = a.nodes().get(i as usize) else { return; };
        let id = node.id.clone();
        let side = match side {
            0 => Side::North,
            1 => Side::East,
            2 => Side::South,
            _ => Side::West,
        };
        if let Some((from, from_side)) = a.arrow_start.clone() {
            if from == id { a.message = "Choose a different destination node".into(); return; }
            a.snapshot();
            let active = a.active;
            a.doc.pages[active].arrows.push(Arrow {
                id: doc::new_id(), from, from_side, to: id, to_side: side, trunk: None,
                routing: if u.get_connect_mode() == 2 { Routing::Orthogonal } else { Routing::Straight },
            });
            a.arrow_start = None;
            a.link_start = None;
            a.select(None);
            a.selected_arrow = Some(a.doc.pages[active].arrows.len() - 1);
            a.update_selection_flags();
            a.mark_dirty();
            u.set_connect_mode(0);
            a.message = "Arrow added; Delete removes it, arrow tools change its routing".into();
        } else {
            a.arrow_start = Some((id, side));
            a.selected_arrow = None;
            a.select(Some(i as usize));
            a.update_selection_flags();
            a.message = "Click a destination attachment point (Esc cancels)".into();
        }
    });

    hook!(on_node_pressed, |a, u, i, ctrl| {
        if u.get_connect_mode() == 3 {
            a.link_start = None;
            u.set_connect_mode(0);
        }
        let i = i as usize;
        a.selected_arrow = None;
        a.message.clear();
        a.collapse_on_release = None;
        if ctrl && a.is_selected(i) {
            // Ctrl+click on a selected node removes it and starts no drag.
            a.deselect(i);
            a.update_selection_flags();
            a.drag_orig.clear();
            return;
        } else if ctrl {
            a.make_primary(i);
        } else if a.is_selected(i) && !a.also_selected.is_empty() {
            a.make_primary(i);
            a.collapse_on_release = Some(i);
        } else {
            a.select(Some(i));
        }
        a.update_selection_flags();
        a.drag_orig = a
            .selection()
            .into_iter()
            .map(|j| (j, a.nodes()[j].x, a.nodes()[j].y))
            .collect();
    });

    hook!(on_node_drag, |a, _u, _i, dx, dy| {
        for (j, ox, oy) in a.drag_orig.clone() {
            a.set_node_pos(j, ox + dx as f64, oy + dy as f64);
        }
    });

    hook!(on_node_drag_end, |a, _u, _i| {
        let orig = std::mem::take(&mut a.drag_orig);
        let collapse = a.collapse_on_release.take();
        let Some(&(lead, ox, oy)) = orig.first() else {
            return;
        };
        // Snap the pressed node and move the rest by the same amount, so a
        // group keeps its internal layout.
        let n = &a.nodes()[lead];
        let (dx, dy) = (a.doc.snap(n.x) - ox, a.doc.snap(n.y) - oy);
        if (dx, dy) == (0.0, 0.0) {
            for &(j, x, y) in &orig {
                a.set_node_pos(j, x, y);
            }
            if let Some(i) = collapse {
                a.select(Some(i));
                a.update_selection_flags();
            }
            return;
        }
        // Record the pre-drag state for undo.
        let active = a.active;
        let mut before = a.doc.pages.clone();
        for &(j, x, y) in &orig {
            before[active].nodes[j].x = x;
            before[active].nodes[j].y = y;
        }
        a.undo.push(Snapshot { pages: before, active });
        a.redo.clear();
        for &(j, x, y) in &orig {
            a.set_node_pos(j, x + dx, y + dy);
        }
        a.mark_dirty();
    });

    hook!(on_node_resize, |a, _u, i, dw| {
        let i = i as usize;
        if a.resize_orig.is_none() {
            a.resize_orig = Some(a.nodes()[i].width);
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
                a.nodes_mut()[i].width = w;
                a.refresh_node(i);
                a.mark_dirty();
            } else {
                a.refresh_node(i);
            }
        }
    });

    hook!(on_node_edit_request, |a, u, i| {
        a.open_editor(&u, i as usize);
    });

    hook!(on_paste_at, |a, u, x, y| {
        if u.get_edit_index() < 0 && u.get_table_edit_index() < 0 {
            a.paste(&u, Some((x as f64, y as f64)));
        }
        u.invoke_focus_canvas();
    });

    hook!(on_node_menu, |a, u, op, i| {
        let i = i as usize;
        if i >= a.nodes().len() {
            return;
        }
        match op.as_str() {
            "edit" => a.open_editor(&u, i),
            "copy" => a.copy_node(i),
            "copy-typst" if a.nodes()[i].data.is_some() => {
                a.message = "embedded image has no file path to reference in Typst".into();
            }
            "copy-typst" => {
                let text = a.node_as_typst(i);
                a.copy_to_clipboard(text);
            }
            "delete" => {
                a.selected_arrow = None;
                // Deleting a node in a multi-selection deletes the selection.
                if !a.is_selected(i) {
                    a.select(Some(i));
                }
                a.delete_selected();
            }
            _ => {}
        }
    });

    hook!(on_table_cell_edited, |a, _u, k, text| {
        let Some(&(ri, ci)) = a.cell_map.get(k as usize) else { return };
        if let Some(t) = &mut a.edit_table {
            t.cells[ri][ci].body = text.to_string();
        }
    });

    hook!(on_table_op, |a, u, op, k| {
        a.apply_table_op(op.as_str(), k as usize);
        a.refresh_table_editor(&u);
    });

    hook!(on_table_col_resize_start, |a, u, col| {
        a.table_col_resize_start(&u, col as usize);
    });

    hook!(on_table_col_resize, |a, u, col, dw| {
        a.table_col_resize(&u, col as usize, dw as f64);
    });

    hook!(on_table_col_resize_end, |a, _u| {
        a.col_resize_orig = None;
    });

    hook!(on_commit_table_edit, |a, u| {
        a.commit_table_edit(&u);
    });

    hook!(on_cancel_table_edit, |a, u| {
        a.edit_table = None;
        u.invoke_end_table_edit();
    });

    hook!(on_page_op, |a, u, op, i| {
        a.page_op(&u, op.as_str(), i);
    });

    hook!(on_page_renamed, |a, u, i, name| {
        u.set_rename_page(-1);
        a.rename_page(i as usize, name.as_str());
        u.invoke_focus_canvas();
    });

    hook!(on_background_clicked, |a, u, x, y| {
        if u.get_connect_mode() == 3 {
            a.link_middle_clicked(&u, x as f64, y as f64);
            return;
        }
        a.selected_arrow = a
            .arrow_row_at((x as f64, y as f64), u.get_zoom() as f64)
            .map(|row| a.arrow_rows.borrow()[row].0);
        a.arrow_start = None;
        a.link_start = None;
        u.set_connect_mode(0);
        a.select(None);
        a.update_selection_flags();
        a.message.clear();
    });

    hook!(on_background_double_clicked, |a, u, x, y| {
        u.set_connect_mode(0);
        let i = a.add_node(x as f64, y as f64);
        u.invoke_begin_edit(i as i32, SharedString::default());
    });

    hook!(on_commit_edit, |a, u, i, text| {
        let i = i as usize;
        let text = text.to_string();
        if a.nodes()[i].source != text {
            a.snapshot();
            a.nodes_mut()[i].source = text;
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
            if i < a.nodes().len() && a.nodes()[i].source.is_empty() {
                a.select(Some(i));
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
                a.redo(&u);
                return true;
            }
            // Ctrl+V is handled here rather than as a menu shortcut: menu
            // shortcuts fire before the focused widget, which would break
            // pasting text in the node and table editors.
            if !shift && (k == "v" || k == "V") {
                let at = a.pointer_pos(&u);
                a.paste(&u, at);
                return true;
            }
            // Ctrl+C likewise, so it copies text inside the editors.
            let editing = u.get_edit_index() >= 0 || u.get_table_edit_index() >= 0;
            if !shift && !editing && (k == "c" || k == "C") {
                a.copy_selected();
                return true;
            }
            return false;
        }
        if is(Key::Delete) || is(Key::Backspace) {
            a.delete_selected();
        } else if is(Key::Return) {
            if let Some(i) = a.selected {
                a.open_editor(&u, i);
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
            a.arrow_start = None;
            a.link_start = None;
            a.selected_arrow = None;
            u.set_connect_mode(0);
            a.select(None);
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
        let editing = u.get_edit_index() >= 0 || u.get_table_edit_index() >= 0;
        let end_edits = |a: &mut App, u: &MainWindow| {
            if u.get_edit_index() >= 0 {
                u.invoke_end_edit();
            }
            if u.get_table_edit_index() >= 0 {
                a.edit_table = None;
                u.invoke_end_table_edit();
            }
        };
        let zoom_step = |u: &MainWindow, f: f32| {
            let z = (u.get_zoom() * f).clamp(0.1, 8.0);
            let f = z / u.get_zoom();
            u.invoke_zoom_at(f, u.get_view_width() / 2.0, u.get_view_height() / 2.0);
        };
        match name.as_str() {
            "cancel-link" if !editing => {
                a.link_start=None;
                u.set_connect_mode(0);
                a.message.clear();
            }
            "restart-link" if !editing => {
                a.link_start=None;
                a.selected_arrow=None;
                a.message="Choose the first middle segment".into();
            }
            "link-middles" if !editing => {
                a.arrow_start = None;
                a.link_start = None;
                u.set_connect_mode(3);
                a.message = "Click the first orthogonal middle segment (Esc cancels)".into();
            }
            "unlink-middles" if !editing => {
                a.link_start = None;
                u.set_connect_mode(0);
                if let Some(i) = a.selected_arrow {
                    if a.doc.pages[a.active].arrows[i].trunk.is_some() {
                        a.snapshot();
                        a.clear_link(i);
                        a.mark_dirty();
                        a.message = "Manual link removed; automatic alignment still applies".into();
                    }
                }
            }
            "arrow-straight" | "arrow-orthogonal" if !editing => {
                a.link_start = None;
                if u.get_connect_mode() == 3 { u.set_connect_mode(0); }
                let style = if name == "arrow-straight" { Routing::Straight } else { Routing::Orthogonal };
                if let Some(i) = a.selected_arrow {
                    if a.doc.pages[a.active].arrows[i].routing != style {
                        a.snapshot();
                        let active = a.active;
                        a.doc.pages[active].arrows[i].routing = style;
                        a.mark_dirty();
                    }
                } else {
                    a.arrow_start = None;
                    a.link_start = None;
                    u.set_connect_mode(if style == Routing::Straight { 1 } else { 2 });
                    a.message = "Click a source attachment point, then a destination (Esc cancels)".into();
                }
            }
            "new" => {
                end_edits(&mut a, &u);
                if a.confirm_discard(&u) {
                    a.load_doc(Canvas::default(), None, &u);
                }
            }
            "open" => {
                end_edits(&mut a, &u);
                if !a.confirm_discard(&u) {
                    return;
                }
                let picked = rfd::FileDialog::new()
                    .add_filter("mcanvas", &["mc"])
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
            "undo" if !editing => a.undo(&u),
            "redo" if !editing => a.redo(&u),
            "add-node" if !editing => {
                u.set_connect_mode(0);
                let x = (u.get_view_width() / 2.0 - u.get_pan_x()) / u.get_zoom();
                let y = (u.get_view_height() / 2.0 - u.get_pan_y()) / u.get_zoom();
                let i = a.add_node(x as f64 - DEFAULT_WIDTH / 2.0, y as f64);
                u.invoke_begin_edit(i as i32, SharedString::default());
            }
            "edit-node" if !editing => {
                if let Some(i) = a.selected {
                    a.open_editor(&u, i);
                }
            }
            "add-table" if !editing => {
                let x = (u.get_view_width() / 2.0 - u.get_pan_x()) / u.get_zoom();
                let y = (u.get_view_height() / 2.0 - u.get_pan_y()) / u.get_zoom();
                let i = a.add_table(x as f64 - DEFAULT_WIDTH / 2.0, y as f64);
                a.open_editor(&u, i);
            }
            "delete" if !editing => a.delete_selected(),
            "copy" if !editing => a.copy_selected(),
            "paste" if !editing => a.paste(&u, None),
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
        // Return focus to the canvas unless the action opened an editor.
        if u.get_edit_index() < 0 && u.get_table_edit_index() < 0 {
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

#[cfg(test)]
mod arrow_display_tests {
    use super::*;

    #[test]
    fn linking_targets_are_generous_zoom_independent_and_prefer_valid_segments() {
        let targets=[((0.0,0.0),(100.0,0.0),true),((0.0,10.0),(100.0,10.0),false)];
        assert_eq!(link_target(&targets,(50.0,10.0),1.0),Some(0));
        assert_eq!(link_target(&targets,(50.0,-18.0),1.0),Some(0));
        assert_eq!(link_target(&targets,(50.0,-19.0),1.0),None);
        assert_eq!(link_target(&targets,(50.0,-180.0),0.1),Some(0));
        assert_eq!(link_target(&targets,(50.0,-9.0),2.0),Some(0));
        assert_eq!(link_target(&targets,(50.0,24.0),1.0),Some(1));
        assert_eq!(link_target(&targets,(0.0,0.0),0.0),None);
        assert_eq!(link_target(&[],(0.0,0.0),1.0),None);
    }

    #[test]
    fn empty_invalid_and_extreme_arrow_display_is_bounded() {
        assert!(arrow_vm(&[],false,false).commands.is_empty());
        assert!(arrow_vm(&[(0.0,0.0)],false,false).commands.is_empty());
        assert!(arrow_vm(&[(f64::NAN,0.0),(0.0,0.0)],false,false).commands.is_empty());
        let vm=arrow_vm(&[(-1_000_000.0,0.0),(1_000_000.0,0.0)],false,true);
        assert!(vm.w.is_finite() && vm.h.is_finite());
        assert!(vm.commands.len()<40_000);
        assert!(vm.commands.matches('M').count()<=258);
    }
}
