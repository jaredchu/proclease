#!/usr/bin/env python3
"""Check static document references and the published installer mirror."""
from html.parser import HTMLParser
from pathlib import Path

repo = Path(__file__).resolve().parent.parent
root = repo / "website/dist"
class Document(HTMLParser):
    def __init__(self):
        super().__init__()
        self.ids = set()
        self.references = []
        self.controls = []
    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if "id" in attrs:
            assert attrs["id"] not in self.ids, "duplicate id"
            self.ids.add(attrs["id"])
        for key in ["src", "href"]:
            if key in attrs:
                self.references.append(attrs[key])
        for key in ["aria-controls", "aria-labelledby", "for"]:
            if key in attrs:
                self.controls.extend(attrs[key].split())
        if tag == "img":
            assert "alt" in attrs, "missing image alternative"

document = Document()
document.feed((root / "index.html").read_text())
for ref in document.references:
    if ref.startswith("#"):
        assert len(ref) == 1 or ref[1:] in document.ids, ref
    elif ref.startswith("/"):
        assert (root / ref.lstrip("/")).is_file(), ref
for control in document.controls:
    assert control in document.ids, control
assert (root / "install.sh").read_bytes() == (repo / "install.sh").read_bytes()
assert (root / "assets/social.png").is_file()
print("Local links, assets, unique IDs, accessible references and installer mirror passed")
