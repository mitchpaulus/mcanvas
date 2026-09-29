# mcanvas: working notes

Companion to `design.md`. Captures decisions made so far and a proposed plan.
`design.md` is the owner's original statement and is not edited here.

## Decisions (2026-09-15)

| Topic | Decision |
|---|---|
| File format | JSON, *inspired by* Obsidian JSON Canvas. No compatibility requirement. |
| Zoom/pan | Yes, infinite canvas. |
| Edges / groups | Out of scope for now. Keep the schema open for them later. |
| Markup language | Typst for everything: prose, lists, math, code. No markdown. |
| Edit vs view | Every node has a raw-source edit mode (plain text box) and a rendered view mode. Only one node is edited at a time; the rest stay rendered. |
| Node sizing | Fixed width with auto height, plus manual resize. |
| Platforms | Windows, Linux, macOS desktops. No mobile. |
| Persistence | Explicit save. Undo history. Warn on exit with unsaved changes, and after a long unsaved interval. |
| Interaction | Mouse and keyboard. Heavy keyboard-shortcut coverage is a first-class goal. |
| Export | Not in v1. |
| Image paths | Relative paths resolve relative to the canvas file. |
| Stack | Rust + Slint. |

## Proposed architecture

```
+------------------+     +-----------------+     +------------------+
| canvas document  | --> | render pipeline | --> | Slint UI         |
| (JSON, in-memory | <-- | typst -> SVG    |     | pan/zoom viewport|
|  model + undo)   |     | (cached per     |     | draggable nodes  |
+------------------+     |  node + scale)  |     | text editor      |
                         +-----------------+     +------------------+
```

- **Document model**: `Canvas { nodes: Vec<Node>, view: ViewState }`. Undo is a stack of
  reversible commands (move, resize, edit source, add, delete) rather than snapshots,
  so large canvases stay cheap.
- **Rendering**: each node's Typst source is compiled independently with the `typst`
  crate into SVG (`typst-svg`). Slint shows SVG via its `resvg`-backed `Image`.
  Compile is done off the UI thread; results cached by (source hash, width, zoom bucket).
- **Zoom**: two options, decide by measurement.
  1. Rasterize SVG once at 1x and let Slint scale the bitmap. Cheap, blurry when zoomed in.
  2. Re-rasterize at discrete zoom buckets (0.5x, 1x, 2x, 4x). Sharp, more cache churn.
  Start with 1, upgrade to 2 if it looks bad.
- **Typst world**: each node compiles as a small standalone document with a shared
  preamble (page width = node width, no margins, auto height). A canvas-level preamble
  file can hold user fonts, colors, and `#let` helpers shared by all nodes.
- **Images**: node type `image` with `src` (path relative to canvas file, or absolute)
  or `data` (base64). Inside prose, Typst `#image()` calls resolve through the same
  path rule via a custom `World` implementation.

## File format sketch (v0)

The authoritative description is `schema/canvas.schema.json` (JSON Schema 2020-12).
It mirrors the serde structs in `src/doc.rs`; change both together. Validate a file with
`check-jsonschema --schemafile schema/canvas.schema.json file.mc` or any
2020-12 validator.

```json
{
  "version": 0,
  "view": { "x": 0, "y": 0, "zoom": 1.0 },
  "preamble": "#set text(font: \"Inter\")",
  "nodes": [
    { "id": "a1b2", "type": "typst", "x": 0, "y": 0, "width": 400,
      "height": null, "source": "= Title\n- one\n- two\n$ x^2 $" },
    { "id": "c3d4", "type": "image", "x": 500, "y": 0, "width": 300,
      "height": 200, "src": "img/diagram.png" }
  ]
}
```

- `height: null` means auto (from render). A number means the user resized it.
- `id` is a short random string like JSON Canvas uses.
- `edges` and `groups` can be added later as top-level arrays without breaking v0 files.
- Code blocks and math are just Typst source, so `typst` is the only prose node type.
- `table` nodes carry a `table` object that mirrors Typst's `table()` call (see
  `src/table.rs`): `columns`/`rows` track sizes, `cells` as a 2D array in emission
  order, `header`/`footer` row counts, `align`/`fill`/`stroke`/`inset`/`gutter`, and
  `hlines`/`vlines`. A cell is a markup string or `{ "body", "colspan", "rowspan",
  "fill", "align", "stroke", "inset" }` which becomes `table.cell(..)[body]`. Style
  values are Typst expressions copied verbatim; a bare hex color is wrapped in `rgb()`.
  The app edits tables in a grid editor (double-click) with a right-click menu for
  rows, columns, alignment, and fills, and compiles to Typst for rendering.

## Custom syntax highlighting

Typst highlights fenced code with Sublime Text syntax definitions, and the renderer
resolves file paths relative to the canvas file's directory. So for your own language:

1. Put a `name.sublime-syntax` file next to the canvas (see
   `examples/syntaxes/mini.sublime-syntax` for a minimal one).
2. Add `#set raw(syntaxes: "name.sublime-syntax")` to the canvas `preamble`
   (a list works for several: `syntaxes: ("a.sublime-syntax", "b.sublime-syntax")`).
3. Fence code with the syntax's `name` or one of its `file_extensions`.

Colors come from the raw theme; `#set raw(theme: "my.tmTheme")` swaps it. Both
settings are per canvas because the preamble lives in the file.

## Risks to verify early

1. **Typst crate weight and API churn.** Big dependency, frequent minor releases,
   custom `World` needed for fonts and file access. Spike this first.
2. **Slint SVG quality and performance.** Confirm `resvg` feature renders Typst SVG
   correctly (fonts are outlined by `typst-svg`, so it should).
3. **Slint on WSL.** Needs WSLg or a Windows-side build for testing.
4. **Text editing in Slint.** Built-in `TextEdit` is basic. Fine for v1; a custom
   editor may be needed later for shortcuts inside the editor.

## MVP milestones

1. **Skeleton**: Slint window, pan with drag or space+drag, zoom with wheel or ctrl+/-.
   Static rectangles as placeholder nodes.
2. **Document**: load/save JSON, node add/delete/move, dirty flag, warn on close.
3. **Typst render**: compile node source to SVG, display in node. Auto height.
4. **Edit mode**: double-click or Enter opens text box over the node; Esc or ctrl+Enter
   commits and re-renders.
5. **Undo/redo** across move, resize, edit, add, delete.
6. **Resize** handle and fixed-width behavior.
7. **Keyboard layer**: command palette or leader-key bindings for everything above.
8. **Images** by reference and inline.

## Resolved follow-ups

- **Preamble.** "Preamble" here means the Typst setup lines (fonts, colors, `#let`
  helpers) prepended to every node before compiling. Stored in the canvas file only.
  No config-directory preamble and no global config at all: a canvas is fully
  self-contained in one JSON file, and external CLI tooling can edit that JSON directly.
- **Grid snapping.** Yes. Proposed: positions and widths snap to a base grid (say 8 px
  in canvas units). Snap on drop and on resize release, not during the drag, so the
  motion stays smooth. Hold a modifier (Alt) to bypass. Grid size stored in the file.
- **Spatial navigation.** Move focus with a direction key. Candidate nodes are those
  whose center lies in the 90 degree cone opening from the current node's center in
  that direction. Pick the one minimizing `d_along + 2 * d_across`, where `d_along`
  is distance in the movement direction and `d_across` is perpendicular offset. The
  weight favors nodes that are aligned over nodes that are merely near. No candidate
  means focus stays put.
- **Unsaved warning.** Timer based. After 10 minutes with unsaved changes, show a
  non-blocking notice; still warn on exit regardless of the timer.

## Status (2026-09-15): MVP built

Run with `cargo run -- examples/demo.mc` (or any path; a missing file
starts an empty canvas that saves to that path).

Working:
- Typst rendering per node to SVG, auto height, error text shown in the node
  with the source line number.
- Pan (drag background or wheel), zoom (Ctrl+wheel, Ctrl+plus/minus/0, Home
  resets), node drag with grid snap on drop, width resize via bottom-right handle.
- Edit mode: double-click or Enter opens a raw-source box; Ctrl+Enter commits,
  Esc cancels (an empty new node is discarded on cancel).
- Ctrl+Shift+N or double-click empty space creates a node. Delete removes the selection.
- Undo/redo (Ctrl+Z, Ctrl+Shift+Z or Ctrl+Y). Snapshot based for now, not
  command based as the architecture section proposed; fine at this scale.
- Spatial navigation with arrow keys using the cone-and-score rule above.
- Menu bar (File / Edit / View) with shortcuts declared on the menu items, and a
  toolbar row of icon buttons (`ui/icons/*.svg`). Both route through one
  `command(string)` callback handled in `main.rs`.
- File: New (Ctrl+N), Open (Ctrl+O), Save (Ctrl+S), Save As (Ctrl+Shift+S), Quit
  (Ctrl+Q). Native file and "save changes?" dialogs via `rfd`. Quit and the close
  button ask Yes/No/Cancel when dirty. A reminder appears in the status bar after
  10 minutes unsaved.
- View: zoom in/out (Ctrl+= / Ctrl+-), actual size (Ctrl+0), zoom to fit (Ctrl+F),
  reset view.
- Node drag and resize measure the mouse in canvas coordinates (`node.x +
  mouse-x`) rather than TouchArea-local ones, since the TouchArea moves with the
  node; local deltas fed back into the position and made it jump.
- `#![windows_subsystem = "windows"]` so no console window opens on Windows
  (stderr output is therefore invisible there).
- Image nodes (`type: "image"`, `src` relative to the canvas file) load via Slint.
- Paste (Ctrl+V on the canvas, or Edit > Paste): a clipboard image (a Windows
  snip, macOS screenshot, etc.) becomes an image node with the pixels embedded as
  base64 PNG in `data`, sized to its on-screen size (physical px / scale factor,
  capped at 960). Clipboard text becomes a Typst node. Ctrl+V is handled in the
  canvas key handler, not declared on the menu, because Slint runs menu
  shortcuts before the focused widget and would swallow text paste in editors.
  "Copy as Typst" does nothing useful for embedded images (there is no path).

Not yet:
- Manual height (`height` is respected if set in JSON, but there is no UI for it).
- Off-thread rendering; compiles run on the UI thread and are fast enough so far.
