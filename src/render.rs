use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime};
use typst::syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World};
use typst_kit::fonts::FontStore;
use typst_layout::PagedDocument;

pub struct Renderer {
    library: LazyHash<Library>,
    fonts: FontStore,
}

#[derive(Debug)]
pub struct Rendered {
    pub svg: String,
    pub width: f64,
    pub height: f64,
}

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
        let prefix = format!(
            "#set page(width: {width}pt, height: auto, margin: 8pt, fill: none)\n\
             #set text(size: 12pt)\n{preamble}\n"
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
                Ok(Rendered {
                    svg,
                    width: page.frame.width().to_pt(),
                    height: page.frame.height().to_pt(),
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
    fn reports_errors_with_line() {
        let r = Renderer::new();
        let err = r
            .render(Path::new("."), "", "ok\n#nosuchvar\n", 200.0)
            .unwrap_err();
        assert!(err.contains("line 2"), "{err}");
    }
}
