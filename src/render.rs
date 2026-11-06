use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime};
use typst::layout::{Frame, FrameItem, Point, Transform};
use typst::model::Destination;
use typst::syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst::text::{Font, FontBook};
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
                Ok(Rendered {
                    svg,
                    links,
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
    fn reports_errors_with_line() {
        let r = Renderer::new();
        let err = r
            .render(Path::new("."), "", "ok\n#nosuchvar\n", 200.0)
            .unwrap_err();
        assert!(err.contains("line 2"), "{err}");
    }
}
