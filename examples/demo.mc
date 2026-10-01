{
  "$schema": "../schema/canvas.schema.json",
  "version": 1,
  "grid": 8,
  "preamble": "#set text(font: \"Libertinus Serif\")\n#set raw(syntaxes: \"syntaxes/mini.sublime-syntax\")",
  "page": "0d1e2f30",
  "pages": [
    {
      "id": "0d1e2f30",
      "name": "Overview",
      "view": {
        "x": 40,
        "y": 40,
        "zoom": 1.0
      },
      "nodes": [
        {
          "id": "a1b2c3d4",
          "type": "typst",
          "x": 0,
          "y": 0,
          "width": 360,
          "height": null,
          "source": "= mcanvas demo\n\nA free-form canvas where every block is Typst.\n\n- bullet one\n- bullet two\n  - nested\n- *bold* and _italic_"
        },
        {
          "id": "e5f60718",
          "type": "typst",
          "x": 400,
          "y": 0,
          "width": 320,
          "height": null,
          "source": "== Math\n\n$ integral_0^1 x^2 dif x = 1/3 $\n\nInline math like $a^2 + b^2 = c^2$ works too."
        },
        {
          "id": "9a0b1c2d",
          "type": "typst",
          "x": 0,
          "y": 240,
          "width": 360,
          "height": null,
          "source": "== Code\n\n```rust\nfn main() {\n    println!(\"hello\");\n}\n```"
        },
        {
          "id": "c4d5e6f7",
          "type": "typst",
          "x": 760,
          "y": 0,
          "width": 300,
          "height": null,
          "source": "== Custom syntax\n\nA `.sublime-syntax` file next to the canvas, loaded from the preamble.\n\n```mini\n# comment\nlet x = 42\nfn area(r) {\n  return 3.14 * r * r\n}\nprint(\"done\")\n```"
        },
        {
          "id": "7b8c9d0e",
          "type": "table",
          "x": 0,
          "y": 480,
          "width": 720,
          "height": null,
          "table": {
            "columns": [
              "auto",
              "1fr",
              "auto",
              "auto"
            ],
            "header": 1,
            "stroke": "0.5pt + luma(60%)",
            "align": [
              "left",
              "left",
              "right",
              "right"
            ],
            "cells": [
              [
                "Item",
                "Description",
                "Qty",
                "Cost"
              ],
              [
                "Widget",
                "Standard *bolted* flange",
                "12",
                "\\$48.00"
              ],
              [
                "Gasket",
                {
                  "body": "Nitrile, 3 mm",
                  "fill": "#fef3c7"
                },
                "24",
                "\\$6.00"
              ],
              [
                {
                  "body": "Total",
                  "colspan": 3,
                  "align": "right"
                },
                {
                  "body": "\\$54.00",
                  "fill": "#dbeafe"
                }
              ]
            ],
            "hlines": [
              {
                "at": 3,
                "stroke": "1.5pt"
              }
            ]
          }
        }
      ]
    },
    {
      "id": "4a5b6c7d",
      "name": "Reference",
      "view": {
        "x": 40,
        "y": 40,
        "zoom": 1.0
      },
      "nodes": [
        {
          "id": "3e4f5061",
          "type": "typst",
          "x": 0,
          "y": 0,
          "width": 320,
          "height": null,
          "source": "== Shortcuts\n\n/ Ctrl+Shift+N: new block\n/ Enter: edit selected\n/ Ctrl+Enter: commit edit\n/ Esc: cancel edit\n/ Arrows: move selection spatially\n/ Ctrl+Z, Ctrl+Shift+Z: undo, redo\n/ Ctrl+S: save\n/ Ctrl+wheel: zoom\n/ Del: delete block\n/ Ctrl+T: new page\n/ Ctrl+PgUp, Ctrl+PgDn: switch page\n/ F2 or double-click tab: rename page"
        }
      ]
    }
  ]
}
