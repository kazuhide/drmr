"""Validate the small, self-contained Pages payload before artifact upload.

Python 3.9+, standard library only. This is a static gate, not a browser or
accessibility audit. Exit 0 means passed; 1 means invalid; 2 means explicitly
pending repository documents. Pending reports never authorize deployment.
"""
import argparse
import hashlib
from html.parser import HTMLParser
import json
from pathlib import Path, PurePosixPath
import re
import stat
from urllib.parse import unquote, urljoin, urlsplit

FILES = frozenset((
    "index.html", "ja/index.html", "assets/site.css", "assets/drmr.svg",
    "assets/fonts/ibm-plex-sans-latin-400-normal.woff2",
    "assets/fonts/ibm-plex-sans-latin-600-normal.woff2",
    "assets/fonts/ibm-plex-mono-latin-400-normal.woff2",
    "assets/fonts/LICENSE-ibm-plex.txt", "assets/fonts/README.txt",
))
DIRECTORIES = frozenset(("ja", "assets", "assets/fonts"))
MAX_BYTES = 180 * 1024
PAGES = {"index.html": ("en", "https://kazuhide.github.io/drmr/"),
         "ja/index.html": ("ja", "https://kazuhide.github.io/drmr/ja/")}
ALTERNATES = {"en": PAGES["index.html"][1], "ja": PAGES["ja/index.html"][1],
              "x-default": PAGES["index.html"][1]}


class Failure(ValueError):
    """A fixed validation token suitable for CI output."""


def require(value, token):
    if not value:
        raise Failure(token)


def linked(info):
    # lstat exposes junctions and other Windows name-surrogate reparse points
    # through file attributes, even when their mode is an ordinary directory.
    return stat.S_ISLNK(info.st_mode) or bool(
        getattr(info, "st_file_attributes", 0) & stat.FILE_ATTRIBUTE_REPARSE_POINT)


def inventory(root):
    root_info = root.lstat()
    require(not linked(root_info), "site-link-or-special-file")
    require(stat.S_ISDIR(root_info.st_mode), "site-root")
    files = {}
    pending = [root]
    while pending:
        directory = pending.pop()
        for path in sorted(directory.iterdir()):
            info = path.lstat()
            relative = path.relative_to(root).as_posix()
            require(not linked(info), "site-link-or-special-file")
            if stat.S_ISDIR(info.st_mode):
                require(relative in DIRECTORIES, "site-file-set")
                pending.append(path)
            else:
                require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1, "site-link-or-special-file")
                require(relative in FILES, "site-file-set")
                files[relative] = info.st_size
    require(set(files) == FILES, "site-file-set")
    require(sum(files.values()) <= MAX_BYTES, "site-byte-budget")
    return files


def local_reference(reference, origin, files):
    require(isinstance(reference, str) and not any(ord(c) <= 32 or c == "\\" for c in reference), "site-reference")
    parsed = urlsplit(reference)
    require(not parsed.scheme and not parsed.netloc and not parsed.query, "site-reference")
    decoded = unquote(parsed.path, errors="strict")
    require(not decoded.startswith("/") and "\\" not in decoded, "site-reference")
    parts = PurePosixPath(decoded).parts
    if ".." in parts:
        # The Japanese document may reach the shared assets or English root
        # through exactly one literal parent. Encoded or extra parents fail.
        allowed_parent = origin == "ja/index.html" and parsed.path == decoded and (
            decoded in ("../", "../index.html") or
            decoded.startswith("../assets/") and ".." not in PurePosixPath(decoded[3:]).parts)
        require(allowed_parent, "site-reference")
    if not decoded:
        return origin, parsed.fragment
    stack = list(PurePosixPath(origin).parent.parts)
    for part in decoded.split("/"):
        if part in ("", "."):
            continue
        if part == "..":
            require(stack, "site-reference")
            stack.pop()
        else:
            stack.append(part)
    resolved = "/".join(stack)
    if decoded.endswith("/") or not resolved:
        resolved = (resolved + "/" if resolved else "") + "index.html"
    require(resolved in files, "site-reference")
    for mount in ("/", "/drmr/"):
        url = urljoin("https://pages.example" + mount + origin, reference)
        expected = mount + resolved
        if decoded.endswith("/"):
            expected = expected[:-len("index.html")]
        require(urlsplit(url).path == expected, "site-reference-mount")
    return resolved, parsed.fragment


class Page(HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.ids = set()
        self.references = []
        self.headings = []
        self.main_count = 0
        self.language = None
        self.has_title = False
        self.has_viewport = False
        self.canonicals = []
        self.alternates = []
        self.language_links = []

    def handle_starttag(self, tag, attrs):
        require(tag not in {"script", "base", "iframe", "object", "embed", "form", "input", "textarea", "video", "audio", "source"}, "site-active-content")
        require(len(dict(attrs)) == len(attrs), "site-duplicate-attribute")
        data = dict(attrs)
        require(not any(name.startswith("on") or name in {"style", "srcset", "ping", "srcdoc"} for name in data), "site-active-content")
        if "id" in data:
            require(data["id"] and data["id"] not in self.ids, "site-duplicate-id")
            self.ids.add(data["id"])
        if tag == "html":
            self.language = data.get("lang")
        if tag == "title":
            self.has_title = True
        if tag == "meta" and data.get("name") == "viewport":
            self.has_viewport = data.get("content") == "width=device-width, initial-scale=1"
        if tag == "main":
            self.main_count += 1
        if re.fullmatch("h[1-6]", tag):
            self.headings.append(int(tag[1]))
        if tag == "img":
            require("alt" in data and "width" in data and "height" in data, "site-image-semantics")
        if tag == "link" and data.get("rel") in ("canonical", "alternate"):
            if data["rel"] == "canonical":
                self.canonicals.append(data.get("href"))
            else:
                self.alternates.append((data.get("hreflang"), data.get("href")))
            return
        if tag == "a" and "hreflang" in data:
            self.language_links.append((data.get("hreflang"), data.get("lang"), data.get("href")))
        for attribute in ("href", "src"):
            if attribute in data:
                self.references.append((tag, attribute, data[attribute]))

    def handle_startendtag(self, tag, attrs):
        self.handle_starttag(tag, attrs)


MATRIX_ROWS = (
    ("mp4-standard", ("Clear", "cenc", "cbcs")),
    ("mp4-audio-edit", ("Clear", "cenc")),
    ("dash", ("Clear", "cenc", "cbcs")),
    ("hls-fmp4", ("Clear", "cbcs", "AES-128")),
    ("hls-ts-video", ("Clear", "AES-128", "SAMPLE-AES")),
    ("hls-ts-audio", ("Clear", "AES-128", "SAMPLE-AES")),
)
STANDARD_OUTPUTS = (("Clear", "cenc", "cbcs"), ("Clear", "cenc", "cbcs"),
                    ("Clear", "cbcs", "AES-128"), ())
MATRIX_CELLS = (STANDARD_OUTPUTS, (("Clear",), (), (), ()), STANDARD_OUTPUTS,
                STANDARD_OUTPUTS, ((), (), ("Clear", "cbcs", "AES-128"),
                                  ("Clear", "AES-128", "SAMPLE-AES")),
                ((), (), (), ("Clear", "AES-128", "SAMPLE-AES")))
MATRIX_LOCALES = {
    "en": {"heading": "Input and output support", "region": "Conversion support matrix",
           "caption": "Supported conversion profiles", "corner": "Input ↓ / Output →",
           "outside": "Outside profile", "qualified": "(qualified)",
           "profiles": ("Standard profile", "Finite audio edits", "Fragmented MP4 tracks",
                        "Fragmented MP4 (fMP4)", "Video with optional audio", "Audio only")},
    "ja": {"heading": "入力と出力の対応表", "region": "変換対応表",
           "caption": "対応する変換プロファイル", "corner": "入力 ↓ / 出力 →",
           "outside": "対象外", "qualified": "（限定条件）",
           "profiles": ("通常プロファイル", "再生時間を指定した音声編集",
                        "フラグメント化MP4トラック", "フラグメント化MP4（fMP4）",
                        "映像（音声は省略可）", "音声のみ")},
}


class Element:
    def __init__(self, tag, attrs=()):
        self.tag, self.attrs, self.children = tag, dict(attrs), []

    def descendants(self, tag=None):
        for child in self.children:
            if isinstance(child, Element):
                if tag is None or child.tag == tag:
                    yield child
                yield from child.descendants(tag)

    def text(self, omit=()):
        return "".join(child if isinstance(child, str) else "" if child.tag in omit
                       else child.text(omit) for child in self.children)

    def one(self, tag=None, **attrs):
        matches = [node for node in self.descendants(tag)
                   if all(node.attrs.get(key) == value for key, value in attrs.items())]
        require(len(matches) == 1, "site-conversion-matrix")
        return matches[0]


class MatrixMarkup(HTMLParser):
    VOID = frozenset(("area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr"))

    def __init__(self, text):
        super().__init__(convert_charrefs=True)
        self.root = Element("document")
        self.stack = [self.root]
        self.feed(text)
        self.close()
        require(len(self.stack) == 1, "site-conversion-matrix")

    def handle_starttag(self, tag, attrs):
        node = Element(tag, attrs)
        self.stack[-1].children.append(node)
        if tag not in self.VOID:
            self.stack.append(node)

    def handle_startendtag(self, tag, attrs):
        self.handle_starttag(tag, attrs)
        if tag not in self.VOID:
            self.handle_endtag(tag)

    def handle_endtag(self, tag):
        require(len(self.stack) > 1 and self.stack[-1].tag == tag, "site-conversion-matrix")
        self.stack.pop()

    def handle_data(self, data):
        self.stack[-1].children.append(data)


def matrix_text(node, omit=()):
    return " ".join(node.text(omit).split())


def matrix_protections(node, expected, qualified=""):
    require([matrix_text(code) for code in node.descendants("code")] == list(expected[1:]),
            "site-matrix-protections")
    require(any(matrix_text(span) == "Clear" for span in node.descendants("span")),
            "site-matrix-protections")
    actual = matrix_text(node, omit=("sup",))
    if qualified:
        require(actual.count(qualified) == 1, "site-matrix-protections")
        actual = actual.replace(qualified, "")
    require(re.sub(r"[\s/]", "", actual) == "".join(expected), "site-matrix-protections")


def validate_matrix(text, language):
    document = MatrixMarkup(text).root
    locale = MATRIX_LOCALES[language]
    require(len(list(document.descendants("table"))) == 1, "site-conversion-matrix")
    main = document.one("main")
    sections = [node for node in main.children if isinstance(node, Element) and node.tag == "section"]
    require(len(sections) >= 3 and sections[0].attrs.get("class") == "hero"
            and sections[1].attrs.get("id") == "support"
            and sections[2].attrs.get("id") == "workflow", "site-matrix-priority")
    require(not any("workbench" in node.attrs.get("class", "").split()
                    for node in document.descendants()), "site-matrix-priority")
    section = sections[1]
    require(matrix_text(section.one("h2")) == locale["heading"], "site-conversion-matrix")
    wrapper = section.one("div", **{"class": "matrix-scroll"})
    require(wrapper.attrs.get("role") == "region" and wrapper.attrs.get("tabindex") == "0"
            and wrapper.attrs.get("aria-label") == locale["region"]
            and wrapper.attrs.get("aria-describedby", "").split() == ["matrix-guide", "matrix-scroll-hint"],
            "site-conversion-matrix")
    for name in ("matrix-guide", "matrix-scroll-hint", "matrix-limits", "matrix-definitions"):
        require(matrix_text(section.one(id=name)), "site-conversion-matrix")
    table = wrapper.one("table", **{"class": "conversion-matrix"})
    require(matrix_text(table.one("caption")) == locale["caption"]
            and table.attrs.get("aria-describedby", "").split() == ["matrix-limits", "matrix-definitions"], "site-conversion-matrix")
    require(all(node.attrs.get("colspan", "1") == "1" and node.attrs.get("rowspan", "1") == "1"
                for node in table.descendants() if node.tag in ("th", "td")), "site-matrix-headers")
    head = table.one("thead").one("tr")
    columns = [node for node in head.children if isinstance(node, Element)]
    require(len(columns) == 5 and all(node.tag == "th" and node.attrs.get("scope") == "col"
                                     for node in columns), "site-matrix-headers")
    require(columns[0].attrs.get("aria-label") == locale["corner"]
            and matrix_text(columns[0].one("span", **{"class": "axis-input"})) == locale["corner"].split(" / ")[0]
            and matrix_text(columns[0].one("span", **{"class": "axis-output"})) == locale["corner"].split(" / ")[1]
            and [matrix_text(node) for node in columns[1:]] == ["MP4", "DASH", "HLS fMP4", "HLS MPEG-TS"],
            "site-matrix-headers")
    rows = [node for node in table.one("tbody").children if isinstance(node, Element)]
    require(len(rows) == 6 and [node.attrs.get("id") for node in rows] == [row[0] for row in MATRIX_ROWS],
            "site-matrix-rows")
    for index, (row, (_, inputs), outputs) in enumerate(zip(rows, MATRIX_ROWS, MATRIX_CELLS)):
        cells = [node for node in row.children if isinstance(node, Element)]
        require(row.tag == "tr" and len(cells) == 5 and cells[0].tag == "th"
                and cells[0].attrs.get("scope") == "row"
                and all(cell.tag == "td" for cell in cells[1:]), "site-matrix-headers")
        header = cells[0]
        require(matrix_text(header.one("strong", **{"class": "input-format"}))
                == ("MP4", "MP4", "DASH", "HLS", "HLS / MPEG-TS", "HLS / MPEG-TS")[index], "site-matrix-headers")
        profile = header.one("span", **{"class": "input-profile"})
        require(matrix_text(profile, omit=("sup",)) == locale["profiles"][index], "site-matrix-headers")
        require([matrix_text(node) for node in profile.descendants("sup")]
                == (["2"] if index == 1 else ["3"] if index >= 4 else []), "site-matrix-notes")
        matrix_protections(header.one("span", **{"class": "input-protections"}), inputs,
                           locale["qualified"] if index == 1 else "")
        for column, (cell, protections) in enumerate(zip(cells[1:], outputs)):
            if protections:
                matrix_protections(cell, protections)
            else:
                require(matrix_text(cell) == locale["outside"] and not list(cell.descendants("code")),
                        "site-matrix-protections")
            require([matrix_text(node) for node in cell.descendants("sup")]
                    == (["1"] if column == 0 and index in (0, 2, 3) else []), "site-matrix-notes")
    require(len(list(section.one(id="matrix-limits").descendants("li"))) == 3, "site-matrix-notes")
    return {"rows": 6, "output_columns": 4, "cells": 24, "caption": locale["caption"]}


def document(reference, repository, pending_docs):
    parsed = urlsplit(reference)
    require(parsed.scheme == "https" and parsed.netloc == "github.com" and not parsed.query, "site-external-link")
    path = parsed.path
    if path in ("/kazuhide/drmr", "/kazuhide/drmr/"):
        relative = "README.md"
    else:
        require(path.startswith("/kazuhide/drmr/blob/main/"), "site-external-link")
        relative = unquote(path[len("/kazuhide/drmr/blob/main/"):], errors="strict")
    require(not relative.startswith("/") and ".." not in PurePosixPath(relative).parts and "\\" not in relative, "site-document-path")
    target = repository / relative
    if not target.is_file():
        require(relative in pending_docs, "missing-repository-document")
        return relative
    if parsed.fragment:
        headings = re.findall(r"^#{1,6} (.+)$", target.read_text(encoding="utf-8"), re.MULTILINE)
        anchors = {re.sub(r"[^\w -]", "", heading.lower()).replace(" ", "-") for heading in headings}
        require(parsed.fragment in anchors, "site-document-anchor")
    return None


def validate(root, repository, pending_docs=()):
    root, repository = Path(root), Path(repository)
    files = inventory(root)
    pages = {}
    for origin, (language, canonical) in PAGES.items():
        page = Page()
        page.feed((root / origin).read_text(encoding="utf-8"))
        page.close()
        pages[origin] = page
        require(page.language == language and page.has_title and page.has_viewport and page.main_count == 1, "site-document-semantics")
        require(page.headings and page.headings[0] == 1 and page.headings.count(1) == 1, "site-heading-semantics")
        require(all(b <= a + 1 for a, b in zip(page.headings, page.headings[1:])), "site-heading-semantics")
        require(page.canonicals == [canonical] and len(page.alternates) == len(ALTERNATES)
                and dict(page.alternates) == ALTERNATES, "site-language-links")
        other = "ja" if language == "en" else "en"
        require(len(page.language_links) == 1 and page.language_links[0][:2] == (other, other), "site-language-links")
        target, fragment = local_reference(page.language_links[0][2], origin, files)
        require(target == ("ja/index.html" if other == "ja" else "index.html") and not fragment, "site-language-links")
    matrices = {origin: validate_matrix((root / origin).read_text(encoding="utf-8"), language)
                for origin, (language, _) in PAGES.items()}
    pending = set()
    for origin, page in pages.items():
        notice_link = False
        for tag, attribute, reference in page.references:
            require(isinstance(reference, str) and not any(ord(c) <= 32 or c == "\\" for c in reference), "site-reference")
            if urlsplit(reference).scheme:
                require(tag == "a" and attribute == "href", "site-reference")
                missing = document(reference, repository, pending_docs)
                if missing:
                    pending.add(missing)
            else:
                target, fragment = local_reference(reference, origin, files)
                if tag == "a" and target == "assets/fonts/LICENSE-ibm-plex.txt":
                    notice_link = True
                if fragment:
                    require(target in pages and fragment in pages[target].ids, "site-anchor")
        require(notice_link, "site-font-license-link")
    css = (root / "assets/site.css").read_text(encoding="utf-8")
    require(not re.search(r"@import|expression\s*\(|javascript:", css, re.IGNORECASE), "site-active-content")
    for match in re.finditer(r"url\(\s*(['\"]?)(.*?)\1\s*\)", css, re.IGNORECASE):
        _, fragment = local_reference(match[2], "assets/site.css", files)
        require(not fragment, "site-reference")
    for relative in FILES:
        if relative.startswith("assets/fonts/") and not relative.endswith("README.txt"):
            original = repository / "crates/drmr-server/static/vendor/fonts" / Path(relative).name
            require((root / relative).read_bytes() == original.read_bytes(), "site-vendored-bytes")
    require((root / "assets/drmr.svg").read_bytes() == (repository / "crates/drmr-desktop/icons/icon.svg").read_bytes(), "site-vendored-bytes")
    return {"schema_version": 1, "success": not pending, "status": "pending-documents" if pending else "passed",
            "files": len(files), "bytes": sum(files.values()), "scripts": 0,
            "mounts": ["/", "/drmr/"], "languages": ["en", "ja"],
            "pages": {path: {"language": value[0], "canonical": value[1]} for path, value in PAGES.items()},
            "pending_docs": sorted(pending), "conversion_matrices": matrices,
            "sha256": {name: hashlib.sha256((root / name).read_bytes()).hexdigest() for name in sorted(files)},
            "limits": ["static checks only; no browser, keyboard, visual or screen-reader qualification", "repository document existence checked locally; no remote HTTP requests"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path("site"))
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--repository-root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--allow-pending-doc", action="append", default=[], help="development only; returns exit 2, never a deployment pass")
    args = parser.parse_args()
    try:
        report = validate(args.root, args.repository_root, args.allow_pending_doc)
        code = 0 if report["success"] else 2
    except (Failure, OSError, UnicodeError, ValueError) as error:
        token = str(error) if isinstance(error, Failure) else "site-unreadable-or-invalid"
        report = {"schema_version": 1, "success": False, "status": "invalid", "error": token}
        code = 1
    args.report.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(json.dumps({key: value for key, value in report.items() if key != "sha256"}, sort_keys=True))
    return code


if __name__ == "__main__":
    raise SystemExit(main())
