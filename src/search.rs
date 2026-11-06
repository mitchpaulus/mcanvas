//! Text search across every page of a canvas.
//!
//! A node matches when every whitespace-separated term of the query occurs in
//! its searchable text, ignoring case. Terms rather than a phrase so that
//! markup between words (`*bold* text`) does not hide a match. The searchable
//! text is the raw Typst source, a table's cell bodies, or an image's path.

use std::borrow::Cow;

use crate::doc::{Canvas, Node};

/// Characters of context kept before the match in a snippet.
const BEFORE_CHARS: usize = 32;
/// Characters of context kept after the match in a snippet.
const AFTER_CHARS: usize = 120;

#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    /// Index into `canvas.pages`.
    pub page: usize,
    pub node: String,
    /// The line of the first term's first occurrence, split around it.
    pub before: String,
    pub matched: String,
    pub after: String,
}

/// Every matching node, by page order and then top-to-bottom, left-to-right.
pub fn search(canvas: &Canvas, query: &str) -> Vec<Hit> {
    let terms = terms(query);
    if terms.is_empty() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    // Reused across nodes and pages so the search allocates per hit only.
    let mut folded = String::new();
    let mut nodes: Vec<&Node> = Vec::new();
    for (p, page) in canvas.pages.iter().enumerate() {
        nodes.clear();
        nodes.extend(page.nodes.iter());
        nodes.sort_by(|a, b| a.y.total_cmp(&b.y).then(a.x.total_cmp(&b.x)));
        for node in &nodes {
            let text = node_text(node);
            let ascii = fold_into(&text, &mut folded);
            let mut first = None;
            let all = terms.iter().all(|t| match folded.find(t.as_str()) {
                Some(s) => {
                    first.get_or_insert((s, s + t.len()));
                    true
                }
                None => false,
            });
            if let (true, Some((s, e))) = (all, first) {
                let (start, end) = if ascii { (s, e) } else { original_range(&text, s, e) };
                let (before, matched, after) = snippet(&text, start, end);
                hits.push(Hit { page: p, node: node.id.clone(), before, matched, after });
            }
        }
    }
    hits
}

/// Byte ranges of every occurrence of every term of `query` in `text`,
/// ignoring case, sorted by position. Used to highlight rendered text.
pub fn find_all(text: &str, query: &str) -> Vec<(usize, usize)> {
    let mut folded = String::new();
    let ascii = fold_into(text, &mut folded);
    let mut out = Vec::new();
    for term in terms(query) {
        let mut i = 0;
        while let Some(k) = folded[i..].find(term.as_str()) {
            let s = i + k;
            out.push((s, s + term.len()));
            i = s + term.len();
        }
    }
    out.sort_unstable();
    if !ascii {
        // Original byte range of the char each lowered byte came from.
        let mut map = Vec::with_capacity(folded.len());
        for (b, c) in text.char_indices() {
            let range = (b, b + c.len_utf8());
            map.extend(std::iter::repeat(range).take(lowered_len(c)));
        }
        for r in &mut out {
            *r = (map[r.0].0, map[r.1 - 1].1);
        }
    }
    out.dedup();
    out
}

/// The lowercased, whitespace-separated terms of a query.
fn terms(query: &str) -> Vec<String> {
    query.split_whitespace().map(|t| t.chars().flat_map(char::to_lowercase).collect()).collect()
}

/// The text a node is searched by.
pub fn node_text(node: &Node) -> Cow<'_, str> {
    match node.kind.as_str() {
        "table" => Cow::Owned(
            node.table
                .as_ref()
                .map(|t| {
                    t.cells
                        .iter()
                        .map(|row| row.iter().map(|c| c.body.as_str()).collect::<Vec<_>>().join(" | "))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default(),
        ),
        "image" => Cow::Borrowed(node.src.as_deref().unwrap_or("")),
        _ => Cow::Borrowed(&node.source),
    }
}

/// Lowercase `text` into `out` (cleared first), char by char with
/// `char::to_lowercase` (so unlike `str::to_lowercase` there is no final
/// sigma rule, matching how query terms are lowercased). ASCII runs are
/// copied and lowercased in bulk, which is what keeps search fast: a single
/// `°` or `Δ` in a note must not send the whole note down the slow path.
///
/// Returns whether `text` is ASCII, in which case byte offsets in `out` are
/// the same as in `text`; otherwise map them back with `original_range`.
fn fold_into(text: &str, out: &mut String) -> bool {
    out.clear();
    if text.is_ascii() {
        out.push_str(text);
        out.make_ascii_lowercase();
        return true;
    }
    let mut rest = text;
    while !rest.is_empty() {
        let n = rest.bytes().position(|b| !b.is_ascii()).unwrap_or(rest.len());
        let start = out.len();
        out.push_str(&rest[..n]);
        out[start..].make_ascii_lowercase();
        let mut chars = rest[n..].chars();
        if let Some(c) = chars.next() {
            out.extend(c.to_lowercase());
        }
        rest = chars.as_str();
    }
    false
}

/// Bytes `c` lowercases to.
fn lowered_len(c: char) -> usize {
    if c.is_ascii() {
        1
    } else {
        c.to_lowercase().map(char::len_utf8).sum()
    }
}

/// Byte range in `text` of the whole chars that lowered bytes `s..e` (as
/// produced by `fold_into`) came from. A char can lowercase to several, so a
/// match may start or end inside one; it then covers that whole char.
fn original_range(text: &str, s: usize, e: usize) -> (usize, usize) {
    let (mut lo, mut start) = (0, None);
    for (b, c) in text.char_indices() {
        let next = lo + lowered_len(c);
        if start.is_none() && next > s {
            start = Some(b);
        }
        if next >= e {
            return (start.unwrap_or(b), b + c.len_utf8());
        }
        lo = next;
    }
    (start.unwrap_or(text.len()), text.len())
}

/// The line holding bytes `start..end`, split into the text before, the match,
/// and the text after, each trimmed to a readable length.
fn snippet(text: &str, start: usize, end: usize) -> (String, String, String) {
    let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[end..].find('\n').map_or(text.len(), |i| end + i);
    let clean = |s: &str| s.replace(['\t', '\r'], " ");
    let before = clean(text[line_start..start].trim_start());
    let n = before.chars().count();
    let before = if n > BEFORE_CHARS {
        let skip = n - BEFORE_CHARS;
        format!("…{}", before.chars().skip(skip).collect::<String>())
    } else {
        before
    };
    let after = clean(text[end..line_end].trim_end());
    let after = if after.chars().count() > AFTER_CHARS {
        format!("{}…", after.chars().take(AFTER_CHARS).collect::<String>())
    } else {
        after
    };
    (before, clean(&text[start..end]), after)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::Page;
    use crate::table::{Cell, Table};

    fn node(id: &str, x: f64, y: f64, source: &str) -> Node {
        Node {
            id: id.into(),
            kind: "typst".into(),
            x,
            y,
            width: 100.0,
            height: None,
            source: source.into(),
            src: None,
            data: None,
            table: None,
        }
    }

    fn canvas(pages: Vec<Vec<Node>>) -> Canvas {
        let mut c = Canvas::default();
        c.pages = pages
            .into_iter()
            .enumerate()
            .map(|(i, nodes)| {
                let mut p = Page::new(format!("Page {}", i + 1));
                p.nodes = nodes;
                p
            })
            .collect();
        c
    }

    fn ids(hits: &[Hit]) -> Vec<&str> {
        hits.iter().map(|h| h.node.as_str()).collect()
    }

    #[test]
    fn empty_query_finds_nothing() {
        let c = canvas(vec![vec![node("a", 0.0, 0.0, "hello")]]);
        assert!(search(&c, "").is_empty());
        assert!(search(&c, "   ").is_empty());
    }

    #[test]
    fn case_insensitive_and_all_terms_required() {
        let c = canvas(vec![vec![
            node("a", 0.0, 0.0, "= Heat Pump\nsizing *notes*"),
            node("b", 0.0, 50.0, "heat loss only"),
        ]]);
        assert_eq!(ids(&search(&c, "HEAT")), ["a", "b"]);
        // Markup between the words does not prevent a match.
        assert_eq!(ids(&search(&c, "heat notes")), ["a"]);
        assert!(search(&c, "heat missing").is_empty());
    }

    #[test]
    fn hits_are_in_page_then_reading_order() {
        let c = canvas(vec![
            vec![node("low", 0.0, 100.0, "x"), node("right", 50.0, 0.0, "x"), node("left", 0.0, 0.0, "x")],
            vec![node("p2", -500.0, -500.0, "x")],
        ]);
        let hits = search(&c, "x");
        assert_eq!(ids(&hits), ["left", "right", "low", "p2"]);
        assert_eq!(hits[3].page, 1);
    }

    #[test]
    fn tables_and_images_are_searched() {
        let mut t = Table::default();
        t.cells = vec![vec![Cell::text("Name"), Cell::text("Pressure drop")]];
        let mut table = node("t", 0.0, 0.0, "");
        table.kind = "table".into();
        table.table = Some(t);
        let mut img = node("i", 0.0, 50.0, "");
        img.kind = "image".into();
        img.src = Some("img/diagram.png".into());
        let c = canvas(vec![vec![table, img]]);
        let hits = search(&c, "pressure");
        assert_eq!(ids(&hits), ["t"]);
        assert_eq!((hits[0].before.as_str(), hits[0].matched.as_str(), hits[0].after.as_str()), ("Name | ", "Pressure", " drop"));
        assert_eq!(ids(&search(&c, "diagram")), ["i"]);
    }

    #[test]
    fn snippet_is_the_matching_line_with_original_case() {
        let c = canvas(vec![vec![node("a", 0.0, 0.0, "first line\n\tthe Quick fox\nlast")]]);
        let h = &search(&c, "quick")[0];
        assert_eq!((h.before.as_str(), h.matched.as_str(), h.after.as_str()), ("the ", "Quick", " fox"));
    }

    #[test]
    fn find_all_returns_every_occurrence_of_every_term() {
        let text = "Heat pump: the HEAT source; pumped";
        let found: Vec<&str> = find_all(text, "heat pump").iter().map(|&(a, b)| &text[a..b]).collect();
        assert_eq!(found, ["Heat", "pump", "HEAT", "pump"]);
        assert!(find_all(text, "").is_empty());
        assert_eq!(find_all("aaaa", "aa"), [(0, 2), (2, 4)]);
    }

    #[test]
    fn long_context_is_shortened() {
        let long = format!("{}needle{}", "a".repeat(100), "b".repeat(300));
        let c = canvas(vec![vec![node("a", 0.0, 0.0, &long)]]);
        let h = &search(&c, "needle")[0];
        assert_eq!(h.before.chars().count(), BEFORE_CHARS + 1);
        assert!(h.before.starts_with('…'));
        assert_eq!(h.after.chars().count(), AFTER_CHARS + 1);
        assert!(h.after.ends_with('…'));
    }

    #[test]
    fn non_ascii_case_folding_keeps_byte_ranges_valid() {
        let c = canvas(vec![vec![node("a", 0.0, 0.0, "Größe ÜBER İstanbul")]]);
        let h = &search(&c, "über")[0];
        assert_eq!(h.matched, "ÜBER");
        // İ lowercases to two characters; the match must still map back to
        // whole characters of the original.
        assert_eq!(search(&c, "İstanbul")[0].matched, "İstanbul");
        assert_eq!(ids(&search(&c, "GRÖSSE")), Vec::<&str>::new());
        assert_eq!(ids(&search(&c, "größe")), ["a"]);
    }

    #[test]
    fn match_inside_a_multi_char_lowercase_covers_the_whole_char() {
        // İ lowercases to "i\u{307}"; a match starting at the combining dot
        // still starts at İ in the original.
        let c = canvas(vec![vec![node("a", 0.0, 0.0, "xİs Δp")]]);
        assert_eq!(search(&c, "\u{307}s")[0].matched, "İs");
        assert_eq!(search(&c, "x\u{307}").len(), 0);
        let text = "xİs Δp, İ";
        let found: Vec<&str> = find_all(text, "\u{307} δp").iter().map(|&(a, b)| &text[a..b]).collect();
        assert_eq!(found, ["İ", "Δp", "İ"]);
    }

    #[test]
    fn ascii_and_mixed_notes_find_the_same_ranges() {
        let text = "Heat °C pump, HEAT";
        let found: Vec<&str> = find_all(text, "heat").iter().map(|&(a, b)| &text[a..b]).collect();
        assert_eq!(found, ["Heat", "HEAT"]);
    }
}
