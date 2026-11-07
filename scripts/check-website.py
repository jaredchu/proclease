#!/usr/bin/env python3
"""Check static document references, metadata, assets and the installer mirror."""
import json
import re
import struct
import xml.etree.ElementTree as ET
from html.parser import HTMLParser
from pathlib import Path

repo = Path(__file__).resolve().parent.parent
root = repo / "website/dist"
origin = "https://proclease.com"
canonical = origin + "/"


class Document(HTMLParser):
    def __init__(self):
        super().__init__()
        self.ids = set()
        self.references = []
        self.controls = []
        self.meta = {}
        self.links = []
        self.json_ld = []
        self._in_json = False

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
        if tag == "meta":
            key = attrs.get("property") or attrs.get("name")
            if key:
                self.meta[key] = attrs.get("content", "")
        if tag == "link":
            self.links.append(attrs)
        if tag == "script" and attrs.get("type") == "application/ld+json":
            self._in_json = True
            self.json_ld.append("")

    def handle_endtag(self, tag):
        if tag == "script":
            self._in_json = False

    def handle_data(self, data):
        if self._in_json:
            self.json_ld[-1] += data


def local_path(url):
    """Map a same-site absolute URL or root-relative path to a dist file."""
    if url.startswith(origin + "/"):
        url = url[len(origin):]
    assert url.startswith("/"), url
    path = url.split("#")[0].split("?")[0]
    return root / ("index.html" if path == "/" else path.lstrip("/"))


def png_size(path):
    data = path.read_bytes()
    assert data[:8] == b"\x89PNG\r\n\x1a\n", path
    return struct.unpack(">II", data[16:24])


document = Document()
document.feed((root / "index.html").read_text())
for ref in document.references:
    if ref.startswith("#"):
        assert len(ref) == 1 or ref[1:] in document.ids, ref
    elif ref.startswith("/"):
        assert local_path(ref).is_file(), ref
for control in document.controls:
    assert control in document.ids, control

# Canonical, social URL and sitemap agree on one preferred URL.
canonicals = [l["href"] for l in document.links if l.get("rel") == "canonical"]
assert canonicals == [canonical], canonicals
assert document.meta.get("og:url") == canonical
sitemap = ET.parse(root / "sitemap.xml").getroot()
locs = [e.text for e in sitemap.iter("{http://www.sitemaps.org/schemas/sitemap/0.9}loc")]
assert locs == [canonical], locs
assert "Sitemap: " + origin + "/sitemap.xml" in (root / "robots.txt").read_text()
for key in ["description", "og:title", "og:description", "og:image:alt"]:
    assert document.meta.get(key), key

# Social image exists locally with the declared dimensions.
social = local_path(document.meta["og:image"])
assert social.is_file(), social
assert png_size(social) == (int(document.meta["og:image:width"]), int(document.meta["og:image:height"]))

# Icons: SVG parses; PNG fallbacks have their expected sizes.
ET.parse(root / "assets/icon.svg")
assert png_size(root / "assets/apple-touch-icon.png") == (180, 180)
assert png_size(root / "assets/icon-32.png") == (32, 32)
assert (root / "favicon.ico").read_bytes()[:4] == b"\x00\x00\x01\x00"
rels = {l.get("rel") for l in document.links}
assert {"icon", "apple-touch-icon"} <= rels, rels

# Structured data parses and names the visible product.
assert len(document.json_ld) == 1
graph = json.loads(document.json_ld[0])["@graph"]
apps = [n for n in graph if n["@type"] == "SoftwareApplication"]
assert len(apps) == 1 and apps[0]["name"] == "ProcLease" and apps[0]["url"] == canonical
for banned in ["aggregateRating", "review", "offers", "softwareVersion"]:
    assert banned not in apps[0], banned

# No em dash, literal or encoded, in served text files.
for path in root.rglob("*"):
    if path.suffix in {".html", ".css", ".js", ".svg", ".txt", ".xml", ".sh"}:
        text = path.read_text()
        for token in ["—", "&mdash;", "&#8212;", "&#x2014;", "\\u2014"]:
            assert token.lower() not in text.lower(), (path, token)

assert (root / "install.sh").read_bytes() == (repo / "install.sh").read_bytes()


class Icons(HTMLParser):
    """Collect the Tabler sprite, icon uses and the names of icon-bearing controls."""

    def __init__(self):
        super().__init__()
        self.comments = []
        self.sprites = []
        self.symbols = {}
        self.uses = []
        self.icons = []
        self.controls = []  # [tag, aria-label, text, has_icon]
        self._stack = []
        self._symbol = None

    def handle_comment(self, data):
        self.comments.append(data)

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if tag in {"a", "button"}:
            self._stack.append([tag, attrs.get("aria-label", ""), "", False])
        if tag == "svg" and "icon-sprite" in attrs.get("class", "").split():
            self.sprites.append(attrs)
        elif tag == "svg":
            self.icons.append(attrs)
            if self._stack:
                self._stack[-1][3] = True
        elif tag == "symbol":
            assert attrs["id"] not in self.symbols, attrs["id"]
            self._symbol = attrs["id"]
            self.symbols[self._symbol] = dict(attrs, paths=[])
        elif tag == "path" and self._symbol:
            self.symbols[self._symbol]["paths"].append(attrs.get("d", ""))
        elif tag == "use":
            self.uses.append(attrs.get("href", ""))

    def handle_endtag(self, tag):
        if tag == "symbol":
            self._symbol = None
        if tag in {"a", "button"} and self._stack and self._stack[-1][0] == tag:
            self.controls.append(self._stack.pop())

    def handle_data(self, data):
        for control in self._stack:
            control[2] += data


# Icons: one hidden same-document sprite of Tabler symbols, each used, each use resolved.
icons = Icons()
html = (root / "index.html").read_text()
icons.feed(html)
assert len(icons.sprites) == 1, icons.sprites
assert icons.sprites[0].get("aria-hidden") == "true" and icons.sprites[0].get("focusable") == "false"
assert icons.sprites[0].get("width") == "0" and icons.sprites[0].get("height") == "0"
for sid, symbol in icons.symbols.items():
    assert sid.startswith("i-") and symbol.get("viewbox") == "0 0 24 24", sid
    style = "filled" if sid.endswith("-filled") else "outline"
    name = sid[2:].removesuffix("-filled")
    assert symbol.get("data-source") == f"icons/{style}/{name}.svg", sid
    assert symbol["paths"] and all(symbol["paths"]), sid
for icon in icons.icons:
    assert "icon" in icon.get("class", "").split(), icon
    assert icon.get("aria-hidden") == "true" and icon.get("focusable") == "false", icon
assert len(icons.uses) == len(icons.icons), "each icon has exactly one <use>"
used = {href[1:] for href in icons.uses}
assert all(href.startswith("#i-") for href in icons.uses), icons.uses
app = (root / "app.js").read_text()
scripted = {"i-" + n for n in re.findall(
    r'(?:setIcon\([^,]+,\s*|showState\(|showControl\(|setFeedback\()"([a-z0-9-]+)"', app)}
assert scripted, "scripted icon names"
assert used | scripted <= set(icons.symbols), (used | scripted) - set(icons.symbols)
assert set(icons.symbols) <= used | scripted, set(icons.symbols) - used - scripted
for tag, label, text, has_icon in icons.controls:
    if has_icon:
        assert label.strip() or text.strip(), f"unnamed icon {tag}"

# Tabler attribution: pinned version, exact upstream MIT notice served locally.
license_path = root / "assets/tabler-icons/LICENSE.txt"
license_text = license_path.read_text()
assert license_text.startswith("MIT License\n\nCopyright (c) 2020-2026 Paweł Kuna\n"), license_path
assert "The above copyright notice and this permission notice shall be included" in license_text
notice = [c for c in icons.comments if "Tabler Icons v3.50.0" in c]
assert len(notice) == 1 and "/assets/tabler-icons/LICENSE.txt" in notice[0], icons.comments
assert local_path("/assets/tabler-icons/LICENSE.txt").is_file()
assert list((root / "assets/tabler-icons").iterdir()) == [license_path], "bundle only the notice"

# Interface glyphs are replaced by icons; textual punctuation and command syntax stay.
for path in [root / "index.html", root / "app.js", root / "style.css"]:
    text = path.read_text()
    for glyph in ["↗", "↓", "→", "↺", "↳", "●", "○", "&gt;_", "&#8599;", "&#8595;"]:
        assert glyph not in text, (path, glyph)
print("Links, metadata, structured data, icons, Tabler sprite and license, em dash absence and installer mirror passed")
