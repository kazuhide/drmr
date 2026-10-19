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
            "pending_docs": sorted(pending),
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
