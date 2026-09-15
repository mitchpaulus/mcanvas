I want my own personal free-form canvas tool.

The file format should be JSON, inspired by JSON canvas from Obsidian.

Features:

- Basic markdown functionality for prose
- Code with syntax highlighting
- Mathematics typesetting -> Probably something that can compile to SVG for presentation
- Images (be able to choose by ref (absolute or relative) or inline directly, like HTML
- Draggable components

Technologies

- Rust with Slint
- Cross Platform

I'm imagining each component has a "edit" mode and "presentation".
The "edit" mode can literally be basic text box for most things.
