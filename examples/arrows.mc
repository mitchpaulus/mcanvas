{
  "version": 1,
  "grid": 8,
  "preamble": "",
  "page": "arrows",
  "pages": [{
    "id": "arrows",
    "name": "Arrows",
    "view": {"x": 40, "y": 80, "zoom": 1},
    "nodes": [
      {"id": "source", "type": "typst", "x": 0, "y": 0, "width": 160, "source": "= Source\nShared orthogonal runs"},
      {"id": "one", "type": "typst", "x": 400, "y": 240, "width": 180, "source": "= Destination A"},
      {"id": "two", "type": "typst", "x": 408, "y": 344, "width": 180, "source": "= Destination B"},
      {"id": "three", "type": "typst", "x": 8, "y": 104, "width": 160, "source": "= Source B"}
    ],
    "arrows": [
      {"id": "a", "from": "source", "from_side": "east", "to": "one", "to_side": "west", "routing": "orthogonal"},
      {"id": "b", "from": "three", "from_side": "east", "to": "two", "to_side": "west", "routing": "orthogonal"},
      {"id": "c", "from": "source", "from_side": "south", "to": "three", "to_side": "north", "routing": "straight"}
    ]
  }]
}
