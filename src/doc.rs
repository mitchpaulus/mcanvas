use serde::{Deserialize, Serialize};
use std::path::Path;

pub const GRID: f64 = 8.0;

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
