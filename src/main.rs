#![windows_subsystem = "windows"]

mod doc;
mod picture;
mod render;
mod table;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, SharedString, VecModel};

use doc::{Canvas, Node, Page};
use render::Renderer;
use table::Table;

slint::include_modules!();

const UNSAVED_WARN_AFTER: Duration = Duration::from_secs(10 * 60);
const DEFAULT_WIDTH: f64 = 320.0;
/// Pasted images wider than this (canvas units) are scaled down to fit.
const MAX_PASTE_WIDTH: f64 = 960.0;

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
    drag_orig: Option<(f64, f64)>,
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

impl App {
    fn root_dir(&self) -> PathBuf {
        self.path
            .as_ref()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .filter(|d| !d.as_os_str().is_empty())
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
            selected: self.selected == Some(i),
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
        self.path = Some(path);
        self.dirty = false;
        self.dirty_since = None;
        Ok(())
    }

    /// Replace the document and reset all editing state.
    fn load_doc(&mut self, doc: Canvas, path: Option<PathBuf>, ui: &MainWindow) {
        self.active = doc.start_page();
        self.doc = doc;
        self.apply_view(ui);
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
            self.apply_view(ui);
            self.selected = None;
            self.drag_orig = None;
            self.resize_orig = None;
            self.refresh_all();
        }
        self.refresh_tabs();
    }

    /// Page tab actions. `i` is the tab the action targets, or -1 for the
    /// active page. Open editors were committed by the UI beforehand.
    fn page_op(&mut self, ui: &MainWindow, op: &str, i: i32) {
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
                for node in &mut page.nodes {
                    node.id = doc::new_id();
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
                    self.apply_view(ui);
                    self.selected = None;
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
        self.nodes_mut().push(node);
        let i = self.nodes().len() - 1;
        self.selected = Some(i);
        self.model.push(self.build_vm(i));
        self.update_selection_flags();
        self.mark_dirty();
        i
    }

    /// Paste the clipboard as a new node centered in the view: an image
    /// (e.g. a screen snip) becomes an embedded image node, text becomes a
    /// Typst node.
    fn paste(&mut self, ui: &MainWindow) {
        let mut clip = match arboard::Clipboard::new() {
            Ok(c) => c,
            Err(e) => {
                self.message = format!("clipboard error: {e}");
                return;
            }
        };
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
            self.snapshot();
            self.push_node(Node {
                id: doc::new_id(),
                kind: "image".into(),
                x: self.doc.snap(cx - width / 2.0),
                y: self.doc.snap(cy - height / 2.0),
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
            self.snapshot();
            self.push_node(Node {
                id: doc::new_id(),
                kind: "typst".into(),
                x: self.doc.snap(cx - DEFAULT_WIDTH / 2.0),
                y: self.doc.snap(cy),
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
        match arboard::Clipboard::new().and_then(|mut c| c.set_text(text)) {
            Ok(()) => self.message = "copied as Typst".into(),
            Err(e) => self.message = format!("clipboard error: {e}"),
        }
    }

    /// Open the editor appropriate for the node's kind.
    fn open_editor(&mut self, ui: &MainWindow, i: usize) {
        self.selected = Some(i);
        self.update_selection_flags();
        self.drag_orig = None;
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
        if let Some(i) = self.selected {
            self.snapshot();
            self.nodes_mut().remove(i);
            self.model.remove(i);
            self.selected = None;
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
        self.apply_view(ui);
        self.selected = None;
        self.refresh_all();
        self.refresh_tabs();
        self.mark_dirty();
        Snapshot { pages, active: self.active }
    }

    /// Spatial navigation: pick the node in the given direction whose center
    /// lies within a 90 degree cone, minimizing along + 2 * across distance.
    fn navigate(&mut self, dx: f64, dy: f64) {
        let Some(cur) = self.selected else {
            if !self.nodes().is_empty() {
                self.selected = Some(0);
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
            self.selected = Some(i);
            self.update_selection_flags();
        }
    }
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
    if !app.message.is_empty() {
        left.push_str("    ");
        left.push_str(&app.message);
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
        drag_orig: None,
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
                status(&$a, &$u);
                r
            });
        }};
    }

    hook!(on_node_pressed, |a, _u, i| {
        let i = i as usize;
        a.selected = Some(i);
        a.update_selection_flags();
        a.drag_orig = Some((a.nodes()[i].x, a.nodes()[i].y));
        a.message.clear();
    });

    hook!(on_node_drag, |a, _u, i, dx, dy| {
        let i = i as usize;
        if let Some((ox, oy)) = a.drag_orig {
            a.nodes_mut()[i].x = ox + dx as f64;
            a.nodes_mut()[i].y = oy + dy as f64;
            if let Some(mut vm) = a.model.row_data(i) {
                vm.x = a.nodes()[i].x as f32;
                vm.y = a.nodes()[i].y as f32;
                a.model.set_row_data(i, vm);
            }
        }
    });

    hook!(on_node_drag_end, |a, _u, i| {
        let i = i as usize;
        if let Some((ox, oy)) = a.drag_orig.take() {
            let n = &a.nodes()[i];
            let (nx, ny) = (a.doc.snap(n.x), a.doc.snap(n.y));
            if (nx, ny) != (ox, oy) {
                // Record the pre-drag state for undo.
                let mut before = a.doc.pages.clone();
                before[a.active].nodes[i].x = ox;
                before[a.active].nodes[i].y = oy;
                let active = a.active;
                a.undo.push(Snapshot { pages: before, active });
                a.redo.clear();
                a.nodes_mut()[i].x = nx;
                a.nodes_mut()[i].y = ny;
                if let Some(mut vm) = a.model.row_data(i) {
                    vm.x = nx as f32;
                    vm.y = ny as f32;
                    a.model.set_row_data(i, vm);
                }
                a.mark_dirty();
            } else {
                a.nodes_mut()[i].x = ox;
                a.nodes_mut()[i].y = oy;
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

    hook!(on_node_menu, |a, u, op, i| {
        let i = i as usize;
        if i >= a.nodes().len() {
            return;
        }
        match op.as_str() {
            "edit" => a.open_editor(&u, i),
            "copy-typst" if a.nodes()[i].data.is_some() => {
                a.message = "embedded image has no file path to reference in Typst".into();
            }
            "copy-typst" => {
                let text = a.node_as_typst(i);
                a.copy_to_clipboard(text);
            }
            "delete" => {
                a.selected = Some(i);
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
                a.redo(&u);
                return true;
            }
            // Ctrl+V is handled here rather than as a menu shortcut: menu
            // shortcuts fire before the focused widget, which would break
            // pasting text in the node and table editors.
            if !shift && (k == "v" || k == "V") {
                a.paste(&u);
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
            "paste" if !editing => a.paste(&u),
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
