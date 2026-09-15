use serde::{Deserialize, Serialize};
use std::path::Path;

pub const GRID: f64 = 8.0;

// The on-disk JSON format is described by schema/canvas.schema.json.
// When you change `Canvas`, `View`, or `Node` (fields, defaults, renames), update
// the schema in the same commit and re-run `cargo test`.

#[derive(Serialize, Deserialize, Clone, Debug)]
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
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Canvas {
    pub version: u32,
    #[serde(default = "default_grid")]
    pub grid: f64,
    #[serde(default)]
    pub view: View,
    #[serde(default)]
    pub preamble: String,
    #[serde(default)]
    pub nodes: Vec<Node>,
}

fn default_grid() -> f64 {
    GRID
}

impl Default for Canvas {
    fn default() -> Self {
        Canvas {
            version: 0,
            grid: GRID,
            view: View::default(),
            preamble: String::new(),
            nodes: Vec::new(),
        }
    }
}

impl Canvas {
    pub fn load(path: &Path) -> Result<Canvas, String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        serde_json::from_str(&text).map_err(|e| e.to_string())
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, text + "\n").map_err(|e| e.to_string())
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
        let path = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/demo.canvas.json"));
        let canvas = Canvas::load(path).expect("demo file should parse");
        assert_eq!(canvas.version, 0);
        assert!(!canvas.nodes.is_empty());
        for n in &canvas.nodes {
            assert!(matches!(n.kind.as_str(), "typst" | "image"), "unknown node type {}", n.kind);
            if n.kind == "image" {
                assert!(n.src.is_some(), "image node {} needs src", n.id);
            }
        }
        // Round trip must not lose anything.
        let text = serde_json::to_string(&canvas).unwrap();
        let back: Canvas = serde_json::from_str(&text).unwrap();
        assert_eq!(back.nodes, canvas.nodes);
    }

    #[test]
    fn schema_required_and_defaults_match_struct() {
        // Mirrors schema/canvas.schema.json: only `version` is required at the top level,
        // and a node needs id/type/x/y/width.
        let c: Canvas = serde_json::from_str(r#"{"version":0}"#).unwrap();
        assert_eq!(c.grid, GRID);
        assert_eq!(c.view.zoom, 1.0);
        let n: Node =
            serde_json::from_str(r#"{"id":"a","type":"typst","x":0,"y":0,"width":10}"#).unwrap();
        assert_eq!(n.height, None);
        assert_eq!(n.source, "");
        assert!(serde_json::from_str::<Node>(r#"{"id":"a","type":"typst"}"#).is_err());
    }
}
