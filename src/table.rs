//! Table node model. The JSON shape mirrors Typst's `table()` function so that
//! compiling to Typst is a direct, lossless mapping:
//!
//! - `columns` / `rows` are track sizes (`auto`, `1fr`, `80pt`) → `columns:` / `rows:`
//! - `cells` is the grid in emission order; each inner array is one row.
//!   A cell is a markup string, or an object with `body` and any of
//!   `colspan`, `rowspan`, `fill`, `align`, `stroke`, `inset` → `table.cell(..)[body]`.
//! - `header` / `footer` are counts of leading / trailing rows wrapped in
//!   `table.header(..)` / `table.footer(..)`.
//! - `align`, `fill`, `stroke`, `inset`, `gutter` map to the same-named arguments.
//! - `hlines` / `vlines` → `table.hline(y: ..)` / `table.vline(x: ..)`.
//!
//! Style values are Typst expressions copied verbatim (`"0.5pt + gray"`,
//! `"center + horizon"`). The one convenience is that a bare hex color such as
//! `"#dbeafe"` is wrapped as `rgb("#dbeafe")`, so GUI palettes can store plain hex.
//!
//! The schema in schema/canvas.schema.json describes the same shape; keep them in sync.

use serde::{Deserialize, Serialize};
use std::fmt::Write;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Table {
    /// Column track sizes. Empty means `auto` for every column.
    #[serde(default)]
    pub columns: Vec<String>,
    /// Row track sizes. Empty means `auto`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rows: Vec<String>,
    /// Number of leading rows that form the repeating header.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub header: usize,
    /// Number of trailing rows that form the footer.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub footer: usize,
    /// Render header rows in bold via a show rule.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub bold_header: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<OneOrMany>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<OneOrMany>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gutter: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hlines: Vec<Line>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vlines: Vec<Line>,
    /// Rows of cells in emission order.
    #[serde(default)]
    pub cells: Vec<Vec<Cell>>,
}

impl Default for Table {
    fn default() -> Self {
        Table {
            columns: Vec::new(),
            rows: Vec::new(),
            header: 0,
            footer: 0,
            bold_header: true,
            align: None,
            fill: None,
            stroke: None,
            inset: None,
            gutter: None,
            hlines: Vec::new(),
            vlines: Vec::new(),
            cells: Vec::new(),
        }
    }
}

/// A single expression or one per column, like Typst's `align: (left, right)`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Line {
    /// Grid line index: `y` for hlines, `x` for vlines.
    pub at: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(from = "CellRepr", into = "CellRepr")]
pub struct Cell {
    /// Typst markup.
    pub body: String,
    pub colspan: Option<usize>,
    pub rowspan: Option<usize>,
    pub fill: Option<String>,
    pub align: Option<String>,
    pub stroke: Option<String>,
    pub inset: Option<String>,
}

/// On disk a cell without attributes is just its body string.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum CellRepr {
    Text(String),
    Full(CellFull),
}

#[derive(Serialize, Deserialize)]
struct CellFull {
    #[serde(default)]
    body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    colspan: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rowspan: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fill: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    align: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stroke: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    inset: Option<String>,
}

impl From<CellRepr> for Cell {
    fn from(r: CellRepr) -> Self {
        match r {
            CellRepr::Text(body) => Cell { body, ..Default::default() },
            CellRepr::Full(f) => Cell {
                body: f.body,
                colspan: f.colspan,
                rowspan: f.rowspan,
                fill: f.fill,
                align: f.align,
                stroke: f.stroke,
                inset: f.inset,
            },
        }
    }
}

impl From<Cell> for CellRepr {
    fn from(c: Cell) -> Self {
        if c.is_plain() {
            CellRepr::Text(c.body)
        } else {
            CellRepr::Full(CellFull {
                body: c.body,
                colspan: c.colspan,
                rowspan: c.rowspan,
                fill: c.fill,
                align: c.align,
                stroke: c.stroke,
                inset: c.inset,
            })
        }
    }
}

impl Cell {
    pub fn text(s: &str) -> Cell {
        Cell { body: s.to_string(), ..Default::default() }
    }

    fn is_plain(&self) -> bool {
        self.colspan.map_or(true, |n| n <= 1)
            && self.rowspan.map_or(true, |n| n <= 1)
            && self.fill.is_none()
            && self.align.is_none()
            && self.stroke.is_none()
            && self.inset.is_none()
    }

    pub fn colspan(&self) -> usize {
        self.colspan.unwrap_or(1).max(1)
    }

    pub fn rowspan(&self) -> usize {
        self.rowspan.unwrap_or(1).max(1)
    }
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}
fn yes() -> bool {
    true
}
fn is_true(b: &bool) -> bool {
    *b
}

/// Turn a stored style value into a Typst expression. Bare hex colors become
/// `rgb("..")`; anything else is already Typst and passes through.
pub fn expr(s: &str) -> String {
    let t = s.trim();
    let hex = t.strip_prefix('#').unwrap_or("");
    let is_hex = matches!(hex.len(), 3 | 4 | 6 | 8) && hex.chars().all(|c| c.is_ascii_hexdigit());
    if is_hex {
        format!("rgb(\"{t}\")")
    } else {
        t.to_string()
    }
}

fn tuple(items: &[String]) -> String {
    let mut s = String::from("(");
    for it in items {
        s.push_str(&expr(it));
        s.push_str(", ");
    }
    s.push(')');
    s
}

impl OneOrMany {
    fn to_typst(&self) -> String {
        match self {
            OneOrMany::One(s) => expr(s),
            OneOrMany::Many(v) => tuple(v),
        }
    }
}

impl Table {
    /// A 3x3 starter table with a header row that fills the node width.
    pub fn starter() -> Table {
        Table {
            columns: vec!["1fr".into(); 3],
            header: 1,
            cells: vec![
                vec![Cell::text("Column 1"), Cell::text("Column 2"), Cell::text("Column 3")],
                vec![Cell::default(); 3],
                vec![Cell::default(); 3],
            ],
            ..Default::default()
        }
    }

    /// Pixel widths (canvas units, 1px = 0.75pt) for the grid editor. Fixed
    /// `Npt` tracks keep their size; `auto` and `Nfr` tracks share what is left
    /// of `total`, weighted by fraction. Each flexible column gets at least
    /// `min_flex`, growing the table beyond `total` if needed.
    pub fn column_widths(&self, total: f64, min_flex: f64) -> Vec<f64> {
        let n = self.ncols();
        let spec: Vec<Option<f64>> = (0..n)
            .map(|i| self.columns.get(i).and_then(|c| parse_pt(c)).map(|pt| pt / 0.75))
            .collect();
        let weight = |i: usize| -> f64 {
            self.columns
                .get(i)
                .and_then(|c| c.trim().strip_suffix("fr"))
                .and_then(|f| f.trim().parse::<f64>().ok())
                .unwrap_or(1.0)
                .max(0.01)
        };
        let fixed: f64 = spec.iter().flatten().sum();
        let weights: f64 = (0..n).filter(|&i| spec[i].is_none()).map(weight).sum();
        let mut unit = if weights > 0.0 { (total - fixed) / weights } else { 0.0 };
        if weights > 0.0 {
            let min_unit = (0..n)
                .filter(|&i| spec[i].is_none())
                .map(|i| min_flex / weight(i))
                .fold(0.0, f64::max);
            unit = unit.max(min_unit);
        }
        (0..n)
            .map(|i| spec[i].unwrap_or_else(|| unit * weight(i)))
            .collect()
    }

    /// Canvas width needed to show every column: fixed tracks at their size,
    /// flexible ones at `min_flex`.
    pub fn min_width_px(&self, min_flex: f64) -> f64 {
        (0..self.ncols())
            .map(|i| {
                self.columns
                    .get(i)
                    .and_then(|c| parse_pt(c))
                    .map_or(min_flex, |pt| pt / 0.75)
            })
            .sum()
    }

    /// Set one column's track to a fixed width given in canvas pixels.
    pub fn set_col_width_px(&mut self, x: usize, px: f64) {
        let n = self.ncols();
        if self.columns.len() < n {
            self.columns.resize(n, "auto".into());
        }
        if x < n {
            self.columns[x] = format!("{}pt", (px * 0.75).round().max(12.0));
        }
    }

    /// Set one column's track to a Typst track expression such as `auto` or `1fr`.
    pub fn set_col_track(&mut self, x: usize, track: &str) {
        let n = self.ncols();
        if self.columns.len() < n {
            self.columns.resize(n, "auto".into());
        }
        if x < n {
            self.columns[x] = track.to_string();
        }
    }

    /// Number of grid columns: the declared tracks, or the widest row.
    pub fn ncols(&self) -> usize {
        let widest = self
            .cells
            .iter()
            .map(|r| r.iter().map(Cell::colspan).sum::<usize>())
            .max()
            .unwrap_or(0);
        self.columns.len().max(widest).max(1)
    }

    /// Grid position (x, y) of every cell following Typst's automatic
    /// placement: cells fill left to right, top to bottom, skipping slots
    /// already covered by a span. Also returns the number of grid rows.
    pub fn layout(&self) -> (Vec<Vec<(usize, usize)>>, usize) {
        let cols = self.ncols();
        let mut occupied: Vec<Vec<bool>> = Vec::new();
        let mut pos = Vec::with_capacity(self.cells.len());
        let (mut cx, mut cy) = (0usize, 0usize);
        for row in &self.cells {
            let mut row_pos = Vec::with_capacity(row.len());
            for cell in row {
                let (cs, rs) = (cell.colspan().min(cols), cell.rowspan());
                // Find the next free slot where the span fits on this row.
                loop {
                    while occupied.len() <= cy {
                        occupied.push(vec![false; cols]);
                    }
                    if cx + cs > cols {
                        cx = 0;
                        cy += 1;
                        continue;
                    }
                    if occupied[cy][cx] {
                        cx += 1;
                        continue;
                    }
                    break;
                }
                for dy in 0..rs {
                    while occupied.len() <= cy + dy {
                        occupied.push(vec![false; cols]);
                    }
                    for dx in 0..cs {
                        occupied[cy + dy][cx + dx] = true;
                    }
                }
                row_pos.push((cx, cy));
                cx += cs;
            }
            // Each JSON row starts a new grid row.
            cx = 0;
            cy += 1;
            pos.push(row_pos);
        }
        let nrows = occupied.len().max(cy);
        (pos, nrows)
    }

    /// Compile to Typst markup (a `#table(..)` call, wrapped in a block when a
    /// header show rule is needed).
    pub fn to_typst(&self) -> String {
        let mut out = String::new();
        let bold = self.bold_header && self.header > 0;
        let ind = if bold {
            out.push_str("#{\n");
            let _ = writeln!(
                out,
                "  show table.cell: it => if it.y < {} {{ strong(it) }} else {{ it }}",
                self.header
            );
            out.push_str("  table(\n");
            "    "
        } else {
            out.push_str("#table(\n");
            "  "
        };
        let cols = self.ncols();
        if self.columns.is_empty() {
            let _ = writeln!(out, "{ind}columns: {cols},");
        } else {
            let _ = writeln!(out, "{ind}columns: {},", tuple(&self.columns));
        }
        if !self.rows.is_empty() {
            let _ = writeln!(out, "{ind}rows: {},", tuple(&self.rows));
        }
        if let Some(a) = &self.align {
            let _ = writeln!(out, "{ind}align: {},", a.to_typst());
        }
        if let Some(f) = &self.fill {
            let _ = writeln!(out, "{ind}fill: {},", f.to_typst());
        }
        for (name, v) in [("stroke", &self.stroke), ("inset", &self.inset), ("gutter", &self.gutter)] {
            if let Some(v) = v {
                let _ = writeln!(out, "{ind}{name}: {},", expr(v));
            }
        }
        let n = self.cells.len();
        let header_end = self.header.min(n);
        let footer_start = n.saturating_sub(self.footer).max(header_end);
        let emit_rows = |out: &mut String, range: std::ops::Range<usize>, wrap: Option<&str>| {
            if range.is_empty() {
                return;
            }
            let ind2 = if let Some(w) = wrap {
                let _ = writeln!(out, "{ind}table.{w}(");
                format!("{ind}  ")
            } else {
                ind.to_string()
            };
            for row in &self.cells[range] {
                out.push_str(&ind2);
                for cell in row {
                    out.push_str(&cell.to_typst());
                    out.push_str(", ");
                }
                out.push('\n');
            }
            if wrap.is_some() {
                let _ = writeln!(out, "{ind}),");
            }
        };
        emit_rows(&mut out, 0..header_end, Some("header"));
        emit_rows(&mut out, header_end..footer_start, None);
        emit_rows(&mut out, footer_start..n, Some("footer"));
        for (kind, axis, lines) in [("hline", "y", &self.hlines), ("vline", "x", &self.vlines)] {
            for l in lines {
                let _ = write!(out, "{ind}table.{kind}({axis}: {}", l.at);
                if let Some(s) = l.start {
                    let _ = write!(out, ", start: {s}");
                }
                if let Some(e) = l.end {
                    let _ = write!(out, ", end: {e}");
                }
                if let Some(s) = &l.stroke {
                    let _ = write!(out, ", stroke: {}", expr(s));
                }
                out.push_str("),\n");
            }
        }
        if bold {
            out.push_str("  )\n}\n");
        } else {
            out.push_str(")\n");
        }
        out
    }

    // ---- Editing operations used by the GUI. Rows are indexed by JSON row
    // (emission order); columns by grid x from `layout()`.

    pub fn insert_row(&mut self, ri: usize) {
        let len = self.cells.len();
        let ri = ri.min(len);
        let n = self.ncols();
        if ri < self.header {
            self.header += 1;
        } else if self.footer > 0 && ri > len - self.footer.min(len) {
            self.footer += 1;
        }
        self.cells.insert(ri, vec![Cell::default(); n]);
        if !self.rows.is_empty() && ri <= self.rows.len() {
            self.rows.insert(ri, "auto".into());
        }
        for l in &mut self.hlines {
            if l.at > ri {
                l.at += 1;
            }
        }
    }

    pub fn delete_row(&mut self, ri: usize) {
        let len = self.cells.len();
        if len <= 1 || ri >= len {
            return;
        }
        if ri < self.header {
            self.header -= 1;
        } else if ri >= len - self.footer.min(len) {
            self.footer -= 1;
        }
        self.cells.remove(ri);
        if ri < self.rows.len() {
            self.rows.remove(ri);
        }
        for l in &mut self.hlines {
            if l.at > ri {
                l.at -= 1;
            }
        }
    }

    pub fn insert_col(&mut self, x: usize) {
        let (pos, _) = self.layout();
        let n = self.ncols();
        let x = x.min(n);
        if self.columns.is_empty() {
            self.columns = vec!["auto".into(); n];
        }
        self.columns.insert(x, "auto".into());
        if let Some(OneOrMany::Many(a)) = &mut self.align {
            if x <= a.len() {
                a.insert(x, "auto".into());
            }
        }
        if let Some(OneOrMany::Many(f)) = &mut self.fill {
            if x <= f.len() {
                f.insert(x, "none".into());
            }
        }
        for (row, rp) in self.cells.iter_mut().zip(&pos) {
            // A cell spanning across x grows; otherwise insert before the first
            // cell at or beyond x.
            if let Some(j) = rp.iter().position(|&(cx, _)| cx < x && cx + row_span(row, rp, cx) > x) {
                row[j].colspan = Some(row[j].colspan() + 1);
                continue;
            }
            let j = rp.iter().position(|&(cx, _)| cx >= x).unwrap_or(row.len());
            row.insert(j, Cell::default());
        }
        for l in &mut self.vlines {
            if l.at > x {
                l.at += 1;
            }
        }
    }

    pub fn delete_col(&mut self, x: usize) {
        let n = self.ncols();
        if n <= 1 || x >= n {
            return;
        }
        let (pos, _) = self.layout();
        if self.columns.is_empty() {
            self.columns = vec!["auto".into(); n];
        }
        self.columns.remove(x);
        if let Some(OneOrMany::Many(a)) = &mut self.align {
            if x < a.len() {
                a.remove(x);
            }
        }
        if let Some(OneOrMany::Many(f)) = &mut self.fill {
            if x < f.len() {
                f.remove(x);
            }
        }
        for (row, rp) in self.cells.iter_mut().zip(&pos) {
            let mut j = 0;
            while j < row.len() {
                let (cx, _) = rp[j];
                let span = row[j].colspan();
                if cx <= x && x < cx + span {
                    if span > 1 {
                        row[j].colspan = Some(span - 1);
                        j += 1;
                    } else {
                        row.remove(j);
                        // rp is now misaligned with row; only one cell per row
                        // can start at x, so we're done with this row.
                        break;
                    }
                } else {
                    j += 1;
                }
            }
        }
        for l in &mut self.vlines {
            if l.at > x {
                l.at -= 1;
            }
        }
    }

    /// Set the alignment of one column, expanding `align` to a per-column list.
    pub fn set_col_align(&mut self, x: usize, align: &str) {
        let n = self.ncols();
        let mut list = match self.align.take() {
            Some(OneOrMany::Many(v)) => v,
            Some(OneOrMany::One(s)) => vec![s; n],
            None => vec!["auto".into(); n],
        };
        list.resize(n, "auto".into());
        if x < n {
            list[x] = align.to_string();
        }
        self.align = Some(OneOrMany::Many(list));
    }
}

/// Parse a fixed length track into points. Supports pt, mm, cm, in, em (at 12pt).
fn parse_pt(s: &str) -> Option<f64> {
    let s = s.trim();
    for (suffix, factor) in [("pt", 1.0), ("mm", 72.0 / 25.4), ("cm", 72.0 / 2.54), ("in", 72.0), ("em", 12.0)] {
        if let Some(num) = s.strip_suffix(suffix) {
            return num.trim().parse::<f64>().ok().map(|v| v * factor);
        }
    }
    None
}

fn row_span(row: &[Cell], rp: &[(usize, usize)], cx: usize) -> usize {
    rp.iter()
        .position(|&(px, _)| px == cx)
        .map(|j| row[j].colspan())
        .unwrap_or(1)
}

impl Cell {
    fn to_typst(&self) -> String {
        if self.is_plain() {
            return format!("[{}]", self.body);
        }
        let mut args: Vec<String> = Vec::new();
        if self.colspan() > 1 {
            args.push(format!("colspan: {}", self.colspan()));
        }
        if self.rowspan() > 1 {
            args.push(format!("rowspan: {}", self.rowspan()));
        }
        for (name, v) in [
            ("fill", &self.fill),
            ("align", &self.align),
            ("stroke", &self.stroke),
            ("inset", &self.inset),
        ] {
            if let Some(v) = v {
                args.push(format!("{name}: {}", expr(v)));
            }
        }
        format!("table.cell({})[{}]", args.join(", "), self.body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(json: &str) -> Table {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn plain_cells_round_trip_as_strings() {
        let table = t(r#"{"columns":["auto","1fr"],"cells":[["a","b"],[{"body":"c","colspan":2}]]}"#);
        assert_eq!(table.cells[0][0], Cell::text("a"));
        assert_eq!(table.cells[1][0].colspan, Some(2));
        let json = serde_json::to_value(&table).unwrap();
        assert_eq!(json["cells"][0][0], "a");
        assert_eq!(json["cells"][1][0]["colspan"], 2);
        assert!(json.get("header").is_none(), "defaults are omitted");
        let back: Table = serde_json::from_value(json).unwrap();
        assert_eq!(back, table);
    }

    #[test]
    fn hex_colors_become_rgb_and_typst_passes_through() {
        assert_eq!(expr("#dbeafe"), "rgb(\"#dbeafe\")");
        assert_eq!(expr("#fff"), "rgb(\"#fff\")");
        assert_eq!(expr("0.5pt + gray"), "0.5pt + gray");
        assert_eq!(expr("none"), "none");
    }

    #[test]
    fn compiles_to_typst() {
        let table = t(r##"{
            "columns":["auto","1fr"],"header":1,"stroke":"0.5pt + gray",
            "align":["left","right"],
            "cells":[["Name","Qty"],["a",{"body":"1","fill":"#eee"}],[{"body":"total","colspan":2,"align":"center"}]],
            "hlines":[{"at":1,"stroke":"2pt"}]
        }"##);
        let src = table.to_typst();
        assert!(src.starts_with("#{\n  show table.cell: it => if it.y < 1 { strong(it) } else { it }\n  table(\n"), "{src}");
        assert!(src.contains("columns: (auto, 1fr, ),"), "{src}");
        assert!(src.contains("align: (left, right, ),"), "{src}");
        assert!(src.contains("stroke: 0.5pt + gray,"), "{src}");
        assert!(src.contains("table.header(\n      [Name], [Qty], \n    ),"), "{src}");
        assert!(src.contains("[a], table.cell(fill: rgb(\"#eee\"))[1], "), "{src}");
        assert!(src.contains("table.cell(colspan: 2, align: center)[total], "), "{src}");
        assert!(src.contains("table.hline(y: 1, stroke: 2pt),"), "{src}");
    }

    #[test]
    fn no_header_is_a_bare_call() {
        let src = t(r#"{"cells":[["a","b"]]}"#).to_typst();
        assert!(src.starts_with("#table(\n  columns: 2,\n"), "{src}");
        assert!(src.ends_with(")\n"));
        assert!(!src.contains("show"));
    }

    #[test]
    fn starter_renders_with_bold_header() {
        let src = Table::starter().to_typst();
        assert!(src.contains("show table.cell: it => if it.y < 1"), "{src}");
        let r = crate::render::Renderer::new();
        let render = |s: &str| r.render(std::path::Path::new("."), "", s, 320.0).unwrap();
        assert!(render(&src).height > 40.0);
        // The show rule must produce the same output as explicit bold markup.
        let explicit = render(
            "#table(columns: (1fr, 1fr, 1fr), table.header([*Column 1*], [*Column 2*], [*Column 3*]), [], [], [], [], [], [])",
        );
        assert_eq!(render(&src).svg, explicit.svg);
    }

    #[test]
    fn demo_table_compiles_under_typst() {
        let canvas = crate::doc::Canvas::load(std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/examples/demo.mc"
        )))
        .unwrap();
        let node = canvas.nodes.iter().find(|n| n.kind == "table").expect("demo has a table node");
        let src = node.table.as_ref().unwrap().to_typst();
        let r = crate::render::Renderer::new();
        let root = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/examples"));
        let out = r.render(root, &canvas.preamble, &src, node.width).unwrap();
        assert!(out.height > 60.0, "{src}");
    }

    #[test]
    fn column_widths_split_flex_space() {
        let t = t(r#"{"columns":["60pt","1fr","2fr"],"cells":[["a","b","c"]]}"#);
        let w = t.column_widths(380.0, 40.0);
        assert_eq!(w[0], 80.0, "60pt is 80px");
        assert!((w[1] - 100.0).abs() < 1e-9 && (w[2] - 200.0).abs() < 1e-9, "{w:?}");
        // Flexible columns never drop below the minimum.
        let w = t.column_widths(100.0, 40.0);
        assert!(w[1] >= 40.0 && w[2] >= 80.0, "{w:?}");
        let mut t = t;
        t.set_col_width_px(1, 133.3);
        assert_eq!(t.columns[1], "100pt");
        t.set_col_track(1, "1fr");
        assert_eq!(t.columns[1], "1fr");
    }

    #[test]
    fn layout_follows_spans() {
        let table = t(r#"{"columns":["auto","auto","auto"],
            "cells":[[{"body":"a","rowspan":2},"b","c"],["d","e"],[{"body":"f","colspan":3}]]}"#);
        let (pos, nrows) = table.layout();
        assert_eq!(pos[0], vec![(0, 0), (1, 0), (2, 0)]);
        assert_eq!(pos[1], vec![(1, 1), (2, 1)], "row 1 skips the rowspan slot");
        assert_eq!(pos[2], vec![(0, 2)]);
        assert_eq!(nrows, 3);
    }

    #[test]
    fn row_and_column_ops() {
        let mut table = Table::starter();
        table.insert_col(1);
        assert_eq!(table.columns, vec!["1fr", "auto", "1fr", "1fr"]);
        assert_eq!(table.cells[0].len(), 4);
        assert_eq!(table.cells[0][1], Cell::default());
        table.delete_col(1);
        assert_eq!(table.columns.len(), 3);
        assert_eq!(table.cells[0][1].body, "Column 2");

        table.insert_row(1);
        assert_eq!(table.cells.len(), 4);
        assert_eq!(table.header, 1);
        table.insert_row(0);
        assert_eq!(table.header, 2, "inserting above the header extends it");
        table.delete_row(0);
        assert_eq!(table.header, 1);
        table.delete_row(1);
        assert_eq!(table.cells.len(), 3);

        // Deleting a column under a colspan shrinks the span instead.
        let mut spanned = t(r#"{"columns":["a","b","c"],"cells":[["x",{"body":"y","colspan":2}]]}"#);
        spanned.delete_col(2);
        assert_eq!(spanned.cells[0][1].colspan, Some(1));
        assert_eq!(spanned.columns.len(), 2);

        table.set_col_align(2, "right");
        assert_eq!(table.align, Some(OneOrMany::Many(vec!["auto".into(), "auto".into(), "right".into()])));
    }
}
