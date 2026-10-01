use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

use crate::table::Table;

pub const GRID: f64 = 8.0;
/// File format version written by this build. Version 0 files kept a single
/// canvas in top-level `view` and `nodes`; they load as one page.
pub const VERSION: u32 = 1;

// The on-disk JSON format is described by schema/canvas.schema.json.
// When you change `Canvas`, `Page`, `View`, or `Node` (fields, defaults, renames),
// update the schema in the same commit and re-run `cargo test`.

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct View {
    pub x: f64,
    pub y: f64,
    pub zoom: f64,
}

impl Default for View {
    fn default() -> Self {
        View { x: 0.0, y: 0.0, zoom: 1.0 }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Node {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    #[serde(default)]
    pub height: Option<f64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub src: Option<String>,
    /// Embedded base64 PNG for "image" nodes, used instead of `src` (pasted
    /// images). Arc so that undo snapshots do not copy the pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Arc<str>>,
    /// Table model for "table" nodes. See table.rs for the Typst mapping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table: Option<Table>,
}

/// One tab of the canvas: its own nodes and viewport. The grid and preamble
/// are shared by every page.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Page {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub view: View,
    #[serde(default)]
    pub nodes: Vec<Node>,
}

impl Page {
    pub fn new(name: String) -> Page {
        Page { id: new_id(), name, view: View::default(), nodes: Vec::new() }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(from = "CanvasFile")]
pub struct Canvas {
    pub version: u32,
    pub grid: f64,
    pub preamble: String,
    /// Id of the page shown when the file is opened.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub page: String,
    /// Never empty after loading.
    pub pages: Vec<Page>,
}

/// What is accepted on disk, including the version 0 layout.
#[derive(Deserialize)]
struct CanvasFile {
    version: u32,
    #[serde(default = "default_grid")]
    grid: f64,
    #[serde(default)]
    preamble: String,
    #[serde(default)]
    page: String,
    #[serde(default)]
    pages: Vec<Page>,
    #[serde(default)]
    view: Option<View>,
    #[serde(default)]
    nodes: Vec<Node>,
}

impl From<CanvasFile> for Canvas {
    fn from(f: CanvasFile) -> Canvas {
        let mut pages = f.pages;
        // A version 0 canvas (or top-level nodes next to pages) becomes the first page.
        if pages.is_empty() || !f.nodes.is_empty() {
            let mut first = Page::new("Page 1".into());
            first.view = f.view.unwrap_or_default();
            first.nodes = f.nodes;
            pages.insert(0, first);
        }
        Canvas { version: f.version, grid: f.grid, preamble: f.preamble, page: f.page, pages }
    }
}

fn default_grid() -> f64 {
    GRID
}

impl Default for Canvas {
    fn default() -> Self {
        Canvas {
            version: VERSION,
            grid: GRID,
            preamble: String::new(),
            page: String::new(),
            pages: vec![Page::new("Page 1".into())],
        }
    }
}

impl Canvas {
    pub fn load(path: &Path) -> Result<Canvas, String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let canvas: Canvas = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        if canvas.version > VERSION {
            return Err(format!(
                "file format version {} is newer than this build supports ({VERSION})",
                canvas.version
            ));
        }
        Ok(canvas)
    }

    /// Saving always writes the current format version.
    pub fn save(&mut self, path: &Path) -> Result<(), String> {
        self.version = VERSION;
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, text + "\n").map_err(|e| e.to_string())
    }

    /// Index of the page named by `page`, or the first page.
    pub fn start_page(&self) -> usize {
        self.pages.iter().position(|p| p.id == self.page).unwrap_or(0)
    }

    /// "Page N" with the smallest N at or above the page count that is not taken.
    pub fn next_page_name(&self) -> String {
        (self.pages.len() + 1..)
            .map(|n| format!("Page {n}"))
            .find(|name| !self.pages.iter().any(|p| &p.name == name))
            .unwrap()
    }

    pub fn snap(&self, v: f64) -> f64 {
        if self.grid <= 0.0 {
            v
        } else {
            (v / self.grid).round() * self.grid
        }
    }
}

pub fn new_id() -> String {
    use rand::Rng;
    const CHARS: &[u8] = b"0123456789abcdef";
    let mut rng = rand::thread_rng();
    (0..8)
        .map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_file_matches_document_model() {
        let path = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/demo.mc"));
        let canvas = Canvas::load(path).expect("demo file should parse");
        assert_eq!(canvas.version, VERSION);
        assert!(canvas.pages.len() > 1, "demo should show off pages");
        assert_eq!(canvas.pages[canvas.start_page()].id, canvas.page);
        let nodes: Vec<&Node> = canvas.pages.iter().flat_map(|p| &p.nodes).collect();
        assert!(!nodes.is_empty());
        let mut ids: Vec<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), nodes.len(), "node ids must be unique across pages");
        for n in nodes {
            assert!(matches!(n.kind.as_str(), "typst" | "image" | "table"), "unknown node type {}", n.kind);
            if n.kind == "image" {
                assert!(n.src.is_some() || n.data.is_some(), "image node {} needs src or data", n.id);
            }
            if n.kind == "table" {
                assert!(n.table.is_some(), "table node {} needs table", n.id);
            }
        }
        // Round trip must not lose anything.
        let text = serde_json::to_string(&canvas).unwrap();
        let back: Canvas = serde_json::from_str(&text).unwrap();
        assert_eq!(back.pages, canvas.pages);
        assert_eq!(back.page, canvas.page);
    }

    #[test]
    fn version_0_file_loads_as_one_page() {
        let c: Canvas = serde_json::from_str(
            r#"{"version":0,"view":{"x":5,"y":6,"zoom":2},
                "nodes":[{"id":"a","type":"typst","x":0,"y":0,"width":10}]}"#,
        )
        .unwrap();
        assert_eq!(c.pages.len(), 1);
        assert_eq!(c.pages[0].name, "Page 1");
        assert_eq!(c.pages[0].view, View { x: 5.0, y: 6.0, zoom: 2.0 });
        assert_eq!(c.pages[0].nodes[0].id, "a");
        // Written back in the paged layout only.
        let text = serde_json::to_string(&c).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(v.get("nodes").is_none() && v.get("view").is_none());
        assert_eq!(v["pages"][0]["nodes"][0]["id"], "a");
    }

    #[test]
    fn page_names_and_start_page() {
        let mut c: Canvas = serde_json::from_str(
            r#"{"version":1,"page":"q","pages":[
                {"id":"p","name":"Page 2"},{"id":"q","name":"Notes"}]}"#,
        )
        .unwrap();
        assert_eq!(c.start_page(), 1);
        assert_eq!(c.next_page_name(), "Page 3");
        c.page = "missing".into();
        assert_eq!(c.start_page(), 0);
        c.pages[1].name = "Page 3".into();
        assert_eq!(c.next_page_name(), "Page 4");
    }

    #[test]
    fn schema_required_and_defaults_match_struct() {
        // Mirrors schema/canvas.schema.json: only `version` is required at the top level,
        // a page needs id/name, and a node needs id/type/x/y/width.
        let c: Canvas = serde_json::from_str(r#"{"version":1}"#).unwrap();
        assert_eq!(c.grid, GRID);
        assert_eq!(c.pages.len(), 1);
        assert_eq!(c.pages[0].view.zoom, 1.0);
        let p: Page = serde_json::from_str(r#"{"id":"p","name":"A"}"#).unwrap();
        assert!(p.nodes.is_empty());
        assert!(serde_json::from_str::<Page>(r#"{"id":"p"}"#).is_err());
        let n: Node =
            serde_json::from_str(r#"{"id":"a","type":"typst","x":0,"y":0,"width":10}"#).unwrap();
        assert_eq!(n.height, None);
        assert_eq!(n.source, "");
        assert!(serde_json::from_str::<Node>(r#"{"id":"a","type":"typst"}"#).is_err());
        let img: Node = serde_json::from_str(
            r#"{"id":"b","type":"image","x":0,"y":0,"width":10,"data":"iVBORw0KGgo="}"#,
        )
        .unwrap();
        assert_eq!(img.data.as_deref(), Some("iVBORw0KGgo="));
        assert!(serde_json::to_string(&img).unwrap().contains(r#""data":"iVBORw0KGgo=""#));
    }
}
