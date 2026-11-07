#!/usr/bin/env python3
"""Check static document references, metadata, assets and the installer mirror."""
import json
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
print("Links, metadata, structured data, icons, em dash absence and installer mirror passed")
