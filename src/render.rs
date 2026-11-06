use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime};
use typst::layout::{Abs, Frame, FrameItem, Point, Transform};
use typst::model::Destination;
use typst::syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst::text::{Font, FontBook, TextItem};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World};
use typst_kit::fonts::FontStore;
use typst_layout::PagedDocument;

const PT_PER_PX: f64 = 0.75;

pub struct Renderer {
    library: LazyHash<Library>,
    fonts: FontStore,
}

#[derive(Debug)]
pub struct Rendered {
    pub svg: String,
    pub width: f64,
    pub height: f64,
    /// External (URL) links, in logical pixels from the top-left of the page.
    pub links: Vec<LinkRect>,
    /// The visible text and where each glyph sits, for search highlights.
    pub text: TextLayout,
}

/// Plain text of a rendered page with the box of every glyph. Text runs that
/// do not continue each other on the same line are separated by a newline so
/// that a search term never matches across them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TextLayout {
    pub text: String,
    pub glyphs: Vec<GlyphBox>,
}

/// A glyph's byte range in `TextLayout::text` and its box in logical pixels.
/// Kept for every rendered node, so compact: 24 bytes rather than 48 with
/// `usize` and `f64` (`f32` is what the highlight boxes end up as anyway).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlyphBox {
    pub start: u32,
    pub end: u32,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl TextLayout {
    /// Append to `out` the boxes covering bytes `start..end` of the text, one
    /// per line it spans.
    pub fn boxes(&self, start: usize, end: usize, out: &mut Vec<(f32, f32, f32, f32)>) {
        let (start, end) = (start as u32, end as u32);
        let base = out.len();
        // Glyphs are pushed in text order (`TextCollector` appends each
        // glyph's chars to the text), so both `start` and `end` are sorted.
        let first = self.glyphs.partition_point(|g| g.end <= start);
        for g in self.glyphs[first..].iter().take_while(|g| g.start < end) {
            let n = out.len();
            match out.last_mut().filter(|_| n > base) {
                // Same line: grow the box.
                Some(b) if (g.y - b.1).abs() < b.3.min(g.h) / 2.0 => {
                    let x0 = b.0.min(g.x);
                    let y0 = b.1.min(g.y);
                    b.2 = (b.0 + b.2).max(g.x + g.w) - x0;
                    b.3 = (b.1 + b.3).max(g.y + g.h) - y0;
                    b.0 = x0;
                    b.1 = y0;
                }
                _ => out.push((g.x, g.y, g.w, g.h)),
            }
        }
    }

    /// The same layout with every box scaled.
    pub fn scaled(mut self, sx: f64, sy: f64) -> TextLayout {
        let (sx, sy) = (sx as f32, sy as f32);
        for g in &mut self.glyphs {
            g.x *= sx;
            g.w *= sx;
            g.y *= sy;
            g.h *= sy;
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LinkRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub url: String,
}

/// Styles URL links like a browser: blue and underlined. Internal links
/// (to labels or positions) keep their normal look. It comes before the canvas
/// preamble so a canvas can restyle links with its own `show link` rule.
const LINK_STYLE: &str = "#show link: it => if type(it.dest) == str {\n  \
     set text(fill: rgb(\"#0b57d0\"))\n  underline(it)\n} else { it }\n";

impl Renderer {
    pub fn new() -> Self {
        let mut fonts = FontStore::new();
        fonts.extend(typst_kit::fonts::embedded());
        fonts.extend(typst_kit::fonts::system());
        Renderer {
            library: LazyHash::new(Library::builder().build()),
            fonts,
        }
    }

    pub fn render(
        &self,
        root: &Path,
        preamble: &str,
        source: &str,
        width: f64,
    ) -> Result<Rendered, String> {
        // Page size in points such that 1 logical pixel = 0.75pt, matching how
        // usvg converts pt to px, so the SVG maps 1:1 on screen at zoom 1.
        let width_pt = width * PT_PER_PX;
        let margin_pt = 8.0 * PT_PER_PX;
        let prefix = format!(
            "#set page(width: {width_pt}pt, height: auto, margin: {margin_pt}pt, fill: none)\n\
             #set text(size: 12pt)\n{LINK_STYLE}{preamble}\n"
        );
        let prefix_lines = prefix.matches('\n').count();
        let text = format!("{prefix}{source}\n");
        let main_id = RootedPath::new(
            VirtualRoot::Project,
            VirtualPath::new("/__mcanvas_node__.typ").map_err(|e| e.to_string())?,
        )
        .intern();
        let world = NodeWorld {
            renderer: self,
            root: root.to_path_buf(),
            main: Source::new(main_id, text),
            cache: Mutex::new(HashMap::new()),
        };
        let warned = typst::compile::<PagedDocument>(&world);
        match warned.output {
            Ok(doc) => {
                let page = doc
                    .pages()
                    .first()
                    .ok_or_else(|| "no pages produced".to_string())?;
                let svg = typst_svg::svg(page, &typst_svg::SvgOptions::default());
                let mut links = Vec::new();
                collect_links(&page.frame, Transform::identity(), &mut links);
                let mut text = TextCollector::default();
                text.collect(&page.frame, Transform::identity());
                // The layout is cached for every node; drop the spare
                // capacity left by growth (up to half of it).
                let mut layout = text.layout;
                layout.text.shrink_to_fit();
                layout.glyphs.shrink_to_fit();
                Ok(Rendered {
                    svg,
                    links,
                    text: layout,
                    width: page.frame.width().to_pt() / PT_PER_PX,
                    height: page.frame.height().to_pt() / PT_PER_PX,
                })
            }
            Err(errors) => {
                let msgs: Vec<String> = errors
                    .iter()
                    .map(|e| {
                        let mut m = e.message.to_string();
                        if let Some(r) = typst::WorldExt::range(&world, e.span) {
                            let line = world.main.lines().byte_to_line(r.start).unwrap_or(0);
                            let line = line.saturating_sub(prefix_lines) + 1;
                            m = format!("line {line}: {m}");
                        }
                        m
                    })
                    .collect();
                Err(msgs.join("\n"))
            }
        }
    }
}

/// Walk the frame tree and record the bounding box of every URL link,
/// converted from points to logical pixels.
fn collect_links(frame: &Frame, ts: Transform, out: &mut Vec<LinkRect>) {
    for (pos, item) in frame.items() {
        let ts = ts.pre_concat(Transform::translate(pos.x, pos.y));
        match item {
            FrameItem::Group(group) => {
                collect_links(&group.frame, ts.pre_concat(group.transform), out);
            }
            FrameItem::Link(Destination::Url(url), size) => {
                let corners = [
                    Point::zero(),
                    Point::with_x(size.x),
                    Point::with_y(size.y),
                    Point::new(size.x, size.y),
                ]
                .map(|p| p.transform(ts));
                let xs = corners.map(|p| p.x.to_pt());
                let ys = corners.map(|p| p.y.to_pt());
                let min = |v: [f64; 4]| v.into_iter().fold(f64::INFINITY, f64::min);
                let max = |v: [f64; 4]| v.into_iter().fold(f64::NEG_INFINITY, f64::max);
                out.push(LinkRect {
                    x: min(xs) / PT_PER_PX,
                    y: min(ys) / PT_PER_PX,
                    w: (max(xs) - min(xs)) / PT_PER_PX,
                    h: (max(ys) - min(ys)) / PT_PER_PX,
                    url: url.to_string(),
                });
            }
            _ => {}
        }
    }
}

/// Bounding box in logical pixels of a rectangle in frame points under `ts`.
fn px_box(ts: Transform, x0: Abs, y0: Abs, x1: Abs, y1: Abs) -> (f64, f64, f64, f64) {
    let corners = [Point::new(x0, y0), Point::new(x1, y0), Point::new(x0, y1), Point::new(x1, y1)]
        .map(|p| p.transform(ts));
    let xs = corners.map(|p| p.x.to_pt());
    let ys = corners.map(|p| p.y.to_pt());
    let min = |v: [f64; 4]| v.into_iter().fold(f64::INFINITY, f64::min);
    let max = |v: [f64; 4]| v.into_iter().fold(f64::NEG_INFINITY, f64::max);
    (
        min(xs) / PT_PER_PX,
        min(ys) / PT_PER_PX,
        (max(xs) - min(xs)) / PT_PER_PX,
        (max(ys) - min(ys)) / PT_PER_PX,
    )
}

#[derive(Default)]
struct TextCollector {
    layout: TextLayout,
    /// Where the previous text run ended: baseline point (page coordinates,
    /// points) and font size, to tell whether the next run continues it.
    last_end: Option<(f64, f64, f64)>,
}

impl TextCollector {
    fn collect(&mut self, frame: &Frame, ts: Transform) {
        for (pos, item) in frame.items() {
            let ts = ts.pre_concat(Transform::translate(pos.x, pos.y));
            match item {
                FrameItem::Group(group) => self.collect(&group.frame, ts.pre_concat(group.transform)),
                FrameItem::Text(t) => self.text(t, ts),
                _ => {}
            }
        }
    }

    /// Add one run of shaped text whose baseline starts at the origin of `ts`.
    fn text(&mut self, item: &TextItem, ts: Transform) {
        let size = item.size;
        let metrics = item.font.metrics();
        let top = -metrics.ascender.at(size);
        let bottom = metrics.descender.at(size).abs();
        let origin = Point::zero().transform(ts);
        let continues = self.last_end.is_some_and(|(x, y, s)| {
            let tol = 0.3 * s.max(size.to_pt());
            (origin.y.to_pt() - y).abs() < 0.5 && (origin.x.to_pt() - x).abs() < tol
        });
        if !continues && !self.layout.text.is_empty() {
            self.layout.text.push('\n');
        }
        let mut cursor = Abs::zero();
        let mut prev: Option<std::ops::Range<usize>> = None;
        for g in &item.glyphs {
            let x0 = cursor + g.x_offset.at(size);
            let x1 = x0 + g.x_advance.at(size);
            let dy = -g.y_offset.at(size);
            let (x, y, w, h) = px_box(ts, x0, top + dy, x1, bottom + dy);
            let (x, y, w, h) = (x as f32, y as f32, w as f32, h as f32);
            cursor += g.x_advance.at(size);
            let range = g.range();
            // Several glyphs for one cluster (combining marks): widen the box.
            if prev.as_ref() == Some(&range) {
                if let Some(last) = self.layout.glyphs.last_mut() {
                    let right = (last.x + last.w).max(x + w);
                    last.x = last.x.min(x);
                    last.w = right - last.x;
                }
                continue;
            }
            let Some(chars) = item.text.get(range.clone()) else { continue };
            if chars.is_empty() {
                continue;
            }
            // A node's rendered text is far below 4 GiB, so u32 offsets fit.
            let start = self.layout.text.len() as u32;
            self.layout.text.push_str(chars);
            let end = self.layout.text.len() as u32;
            self.layout.glyphs.push(GlyphBox { start, end, x, y, w, h });
            prev = Some(range);
        }
        let end = Point::with_x(cursor).transform(ts);
        self.last_end = Some((end.x.to_pt(), end.y.to_pt(), size.to_pt()));
    }
}

struct NodeWorld<'a> {
    renderer: &'a Renderer,
    root: PathBuf,
    main: Source,
    cache: Mutex<HashMap<FileId, Bytes>>,
}

impl NodeWorld<'_> {
    fn read(&self, id: FileId) -> FileResult<Bytes> {
        if let Some(b) = self.cache.lock().unwrap().get(&id) {
            return Ok(b.clone());
        }
        if matches!(id.root(), VirtualRoot::Package(_)) {
            return Err(FileError::Other(Some("packages are not supported".into())));
        }
        let path = id
            .vpath()
            .realize(&self.root)
            .map_err(|_| FileError::AccessDenied)?;
        let data = std::fs::read(&path).map_err(|e| FileError::from_io(e, &path))?;
        let bytes = Bytes::new(data);
        self.cache.lock().unwrap().insert(id, bytes.clone());
        Ok(bytes)
    }
}

impl World for NodeWorld<'_> {
    fn library(&self) -> &LazyHash<Library> {
        &self.renderer.library
    }
    fn book(&self) -> &LazyHash<FontBook> {
        self.renderer.fonts.book()
    }
    fn main(&self) -> FileId {
        self.main.id()
    }
    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.main.id() {
            return Ok(self.main.clone());
        }
        let bytes = self.read(id)?;
        let text = std::str::from_utf8(bytes.as_slice())
            .map_err(|_| FileError::InvalidUtf8)?
            .to_string();
        Ok(Source::new(id, text))
    }
    fn file(&self, id: FileId) -> FileResult<Bytes> {
        self.read(id)
    }
    fn font(&self, index: usize) -> Option<Font> {
        self.renderer.fonts.font(index)
    }
    fn today(&self, _offset: Option<typst::foundations::Duration>) -> Option<Datetime> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_prose_math_and_code() {
        let r = Renderer::new();
        let out = r
            .render(
                Path::new("."),
                "",
                "= Title\n- a\n- b\n$ x^2 $\n```rust\nfn f() {}\n```",
                320.0,
            )
            .unwrap();
        assert!(out.svg.contains("<svg"));
        assert!(out.height > 40.0);
        assert!((out.width - 320.0).abs() < 0.5);
    }

    #[test]
    fn custom_syntax_loads_relative_to_root() {
        let r = Renderer::new();
        let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/examples"));
        let pre = "#set raw(syntaxes: \"syntaxes/mini.sublime-syntax\")";
        let code = "```mini\nlet x = 42 # note\n```";
        let with = r.render(root, pre, code, 300.0).unwrap();
        let without = r.render(root, "", code, 300.0).unwrap();
        // Highlighting changes fill colors in the SVG; plain text has none.
        assert_ne!(with.svg, without.svg);
        let err = r.render(root, "#set raw(syntaxes: \"nope.sublime-syntax\")", code, 300.0).unwrap_err();
        assert!(err.contains("nope"), "{err}");
    }

    #[test]
    fn finds_url_links() {
        let r = Renderer::new();
        let out = r
            .render(
                Path::new("."),
                "",
                "Intro line\n\nSee https://example.com and #link(\"https://typst.app\")[Typst].\n\n= Head <h>\n#link(<h>)[internal]",
                400.0,
            )
            .unwrap();
        let urls: Vec<&str> = out.links.iter().map(|l| l.url.as_str()).collect();
        assert_eq!(urls, ["https://example.com", "https://typst.app"]);
        for l in &out.links {
            assert!(l.w > 10.0 && l.h > 5.0, "{l:?}");
            assert!(l.y > 10.0 && l.y + l.h < out.height, "{l:?}");
            assert!(l.x >= 0.0 && l.x + l.w <= out.width, "{l:?}");
        }
        // Styled blue (#0b57d0) in the SVG.
        assert!(out.svg.to_lowercase().contains("#0b57d0"), "link color missing");
    }

    #[test]
    fn collects_text_with_glyph_boxes() {
        let r = Renderer::new();
        let out = r
            .render(Path::new("."), "", "First line here\n\nHello *bold* world, again", 400.0)
            .unwrap();
        let t = &out.text;
        // Style changes continue a run; separate paragraphs do not.
        assert!(t.text.contains("Hello bold world"), "{:?}", t.text);
        assert!(!t.text.contains("hereHello"), "{:?}", t.text);
        let s = t.text.find("bold").unwrap();
        let mut boxes = Vec::new();
        t.boxes(s, s + 4, &mut boxes);
        assert_eq!(boxes.len(), 1, "{boxes:?}");
        let (x, y, w, h) = boxes[0];
        assert!(x > 20.0 && w > 15.0 && w < 60.0, "{boxes:?}");
        assert!(h > 8.0 && h < 30.0, "{boxes:?}");
        // Below the first line, inside the page.
        let mut first = Vec::new();
        t.boxes(0, 5, &mut first);
        let first = first[0];
        assert!(y > first.1 + first.3 - 1.0 && ((y + h) as f64) < out.height, "{boxes:?} {first:?}");
        assert!(((x + w) as f64) < out.width);
        // Appending a second match on the same line adds its own box rather
        // than growing the first one.
        let wd = t.text.find("world").unwrap();
        t.boxes(wd, wd + 5, &mut boxes);
        assert_eq!(boxes.len(), 2, "{boxes:?}");
        assert!(boxes[1].0 > x + w, "{boxes:?}");
    }

    #[test]
    fn reports_errors_with_line() {
        let r = Renderer::new();
        let err = r
            .render(Path::new("."), "", "ok\n#nosuchvar\n", 200.0)
            .unwrap_err();
        assert!(err.contains("line 2"), "{err}");
    }
}
