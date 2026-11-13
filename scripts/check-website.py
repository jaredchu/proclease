#!/usr/bin/env python3
"""Check static document references, metadata, assets and the installer mirror.

Every HTML page under website/dist is checked. The homepage keeps its specific
structured-data, script and installer checks; other pages are served at their
directory URL (`/name/` for `name/index.html`) and must not load app.js.
"""
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
        self.scripts = []
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
        if tag == "script" and "src" in attrs:
            self.scripts.append(attrs["src"])
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
    if path.endswith("/"):
        path += "index.html"
    return root / path.lstrip("/")


def page_url(path):
    """Directory URL under which a dist HTML file is served."""
    relative = path.relative_to(root).as_posix()
    assert relative == "index.html" or relative.endswith("/index.html"), (
        "pages must be <dir>/index.html so they are served at a directory URL", relative)
    return "/" + relative[: -len("index.html")]


def png_size(path):
    data = path.read_bytes()
    assert data[:8] == b"\x89PNG\r\n\x1a\n", path
    return struct.unpack(">II", data[16:24])


pages = sorted(root.rglob("*.html"))
home = root / "index.html"
assert home in pages
documents = {}
for path in pages:
    document = Document()
    document.feed(path.read_text())
    documents[path] = document

page_canonicals = []
for path, document in documents.items():
    for ref in document.references:
        if ref.startswith("#"):
            assert len(ref) == 1 or ref[1:] in document.ids, (path, ref)
        elif ref.startswith("/") or ref.startswith(origin + "/"):
            target = local_path(ref)
            assert target.is_file(), (path, ref)
            if "#" in ref and target.suffix == ".html":
                fragment = ref.split("#", 1)[1]
                target_doc = documents.get(target)
                assert target_doc is None or not fragment or fragment in target_doc.ids, (path, ref)
    for control in document.controls:
        assert control in document.ids, (path, control)

    # Canonical, social URL and sitemap agree on one preferred URL per page.
    expected = origin + page_url(path)
    canonicals = [l["href"] for l in document.links if l.get("rel") == "canonical"]
    assert canonicals == [expected], (path, canonicals)
    assert document.meta.get("og:url") == expected, path
    page_canonicals.append(expected)
    for key in ["description", "og:title", "og:description", "og:image:alt"]:
        assert document.meta.get(key), (path, key)

    # Social image exists locally with the declared dimensions.
    social = local_path(document.meta["og:image"])
    assert social.is_file(), social
    assert png_size(social) == (int(document.meta["og:image:width"]), int(document.meta["og:image:height"]))
    rels = {l.get("rel") for l in document.links}
    assert {"icon", "apple-touch-icon"} <= rels, (path, rels)
    if path != home:
        assert "/app.js" not in document.scripts, (path, "app.js targets homepage elements")
    for block in document.json_ld:
        json.loads(block)

sitemap = ET.parse(root / "sitemap.xml").getroot()
locs = [e.text for e in sitemap.iter("{http://www.sitemaps.org/schemas/sitemap/0.9}loc")]
assert locs[0] == canonical and len(locs) == len(set(locs)), locs
assert sorted(locs) == sorted(page_canonicals), (locs, page_canonicals)
assert "Sitemap: " + origin + "/sitemap.xml" in (root / "robots.txt").read_text()

# Icons: SVG parses; PNG fallbacks have their expected sizes.
ET.parse(root / "assets/icon.svg")
assert png_size(root / "assets/apple-touch-icon.png") == (180, 180)
assert png_size(root / "assets/icon-32.png") == (32, 32)
assert (root / "favicon.ico").read_bytes()[:4] == b"\x00\x00\x01\x00"

# Homepage structured data parses and names the visible product.
document = documents[home]
assert len(document.json_ld) == 1
graph = json.loads(document.json_ld[0])["@graph"]
apps = [n for n in graph if n["@type"] == "SoftwareApplication"]
assert len(apps) == 1 and apps[0]["name"] == "ProcLease" and apps[0]["url"] == canonical
for doc in documents.values():
    for block in doc.json_ld:
        data = json.loads(block)
        for node in data.get("@graph", [data]):
            if node.get("@type") == "SoftwareApplication":
                for banned in ["aggregateRating", "review", "offers", "softwareVersion"]:
                    assert banned not in node, banned

# No em dash, literal or encoded, in served text files.
for path in root.rglob("*"):
    if path.suffix in {".html", ".css", ".js", ".svg", ".txt", ".xml", ".sh", ".json"}:
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


# Icons: per page, one hidden same-document sprite of Tabler symbols, each used,
# each use resolved. Other pages reuse the homepage's exact symbol geometry.
app = (root / "app.js").read_text()
scripted = {"i-" + n for n in re.findall(
    r'(?:setIcon\([^,]+,\s*|showState\(|showControl\(|setFeedback\()"([a-z0-9-]+)"', app)}
assert scripted, "scripted icon names"
home_symbols = None
for path in [home] + [p for p in pages if p != home]:
    icons = Icons()
    icons.feed(path.read_text())
    if path != home and not icons.icons and not icons.sprites:
        continue
    assert len(icons.sprites) == 1, (path, icons.sprites)
    sprite = icons.sprites[0]
    assert sprite.get("aria-hidden") == "true" and sprite.get("focusable") == "false", path
    assert sprite.get("width") == "0" and sprite.get("height") == "0", path
    for sid, symbol in icons.symbols.items():
        assert sid.startswith("i-") and symbol.get("viewbox") == "0 0 24 24", (path, sid)
        style = "filled" if sid.endswith("-filled") else "outline"
        name = sid[2:].removesuffix("-filled")
        assert symbol.get("data-source") == f"icons/{style}/{name}.svg", (path, sid)
        assert symbol["paths"] and all(symbol["paths"]), (path, sid)
        if home_symbols is not None:
            assert sid in home_symbols and symbol["paths"] == home_symbols[sid]["paths"], (path, sid)
    for icon in icons.icons:
        assert "icon" in icon.get("class", "").split(), (path, icon)
        assert icon.get("aria-hidden") == "true" and icon.get("focusable") == "false", (path, icon)
    assert len(icons.uses) == len(icons.icons), (path, "each icon has exactly one <use>")
    used = {href[1:] for href in icons.uses}
    assert all(href.startswith("#i-") for href in icons.uses), (path, icons.uses)
    page_scripted = scripted if path == home else set()
    assert used | page_scripted <= set(icons.symbols), (path, (used | page_scripted) - set(icons.symbols))
    assert set(icons.symbols) <= used | page_scripted, (path, set(icons.symbols) - used - page_scripted)
    for tag, label, text, has_icon in icons.controls:
        if has_icon:
            assert label.strip() or text.strip(), f"unnamed icon {tag} in {path}"
    notice = [c for c in icons.comments if "Tabler Icons v3.50.0" in c]
    assert len(notice) == 1 and "/assets/tabler-icons/LICENSE.txt" in notice[0], (path, icons.comments)
    if path == home:
        home_symbols = icons.symbols

# Tabler attribution: pinned version, exact upstream MIT notice served locally.
license_path = root / "assets/tabler-icons/LICENSE.txt"
license_text = license_path.read_text()
assert license_text.startswith("MIT License\n\nCopyright (c) 2020-2026 Paweł Kuna\n"), license_path
assert "The above copyright notice and this permission notice shall be included" in license_text
assert local_path("/assets/tabler-icons/LICENSE.txt").is_file()
assert list((root / "assets/tabler-icons").iterdir()) == [license_path], "bundle only the notice"

# Interface glyphs are replaced by icons; textual punctuation and command syntax stay.
for path in pages + [root / "app.js", root / "style.css"]:
    text = path.read_text()
    for glyph in ["↗", "↓", "→", "↺", "↳", "●", "○", "&gt;_", "&#8599;", "&#8595;"]:
        assert glyph not in text, (path, glyph)
print(f"{len(pages)} pages: links, metadata, structured data, icons, Tabler sprite and license, "
      "em dash absence and installer mirror passed")
