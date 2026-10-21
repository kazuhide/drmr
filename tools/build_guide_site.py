"""Publish the guide's deliberately bounded Markdown subset, without dependencies.

This is not a general Markdown renderer. Unsupported or malformed input fails
instead of being passed through as HTML. Outputs are committed for static Pages.
"""
import argparse
from datetime import date
from html import escape
from pathlib import Path
import re
from urllib.parse import urlsplit


CHAPTER_IDS = (
    "introduction", "terminology", "supported-profiles", "installation",
    "key-configuration", "inspect-plan", "local-conversion", "url-input",
    "verification", "saved-hls", "prepared-dash-batch", "resources-stop",
    "docker", "windows", "web-console", "desktop", "results-errors",
    "regression-tests", "cli-reference", "references",
)
LABELS = {
    "en": {"title": "User guide", "toc": "Guide chapters", "skip": "Skip to content",
           "home": "Home", "support": "Support", "other": "日本語", "top": "Back to contents",
           "example": "Example", "table": "Table", "updated": "Updated", "version": "Version",
           "notice": "Font license", "docs": "Repository documentation (English)",
           "description": "A practical drmr guide: formats, keys, conversion, verification, Docker and local interfaces."},
    "ja": {"title": "ユーザーガイド", "toc": "目次", "skip": "本文へ移動",
           "home": "ホーム", "support": "対応表", "other": "English", "top": "目次へ戻る",
           "example": "例", "table": "表", "updated": "更新日", "version": "バージョン",
           "notice": "フォントのライセンス", "docs": "リポジトリの資料（英語）",
           "description": "drmr の対応形式、鍵の設定、変換・検証の手順、Docker・Web コンソール・デスクトップアプリの使い方を説明します。"},
}
TOKEN = re.compile(r"`([^`\n]+)`|\[([^\]\n]+)\]\(([^\s)]+)\)|\*\*([^*\n]+)\*\*")


def inline(text):
    result, position = [], 0
    for match in TOKEN.finditer(text):
        plain = text[position:match.start()]
        if any(char in plain for char in "<`[]*"):
            raise ValueError("guide-inline-syntax")
        result.append(escape(plain))
        code, label, target, strong = match.groups()
        if code is not None:
            result.append("<code>" + escape(code) + "</code>")
        elif label is not None:
            parsed = urlsplit(target)
            fragment = re.fullmatch(r"#[a-z][a-z0-9-]{0,100}", target)
            if not (fragment or (parsed.scheme == "https" and parsed.netloc and not parsed.username
                    and not parsed.password and not parsed.query and "\\" not in target)):
                raise ValueError("guide-link")
            result.append('<a href="' + escape(target, quote=True) + '">' + inline(label) + "</a>")
        else:
            result.append("<strong>" + escape(strong) + "</strong>")
        position = match.end()
    tail = text[position:]
    if any(char in tail for char in "<`[]*"):
        raise ValueError("guide-inline-syntax")
    result.append(escape(tail))
    return "".join(result)


def metadata(source):
    match = re.match(r"\A---\ntitle: ([^\n]+)\nupdated: (\d{4}-\d{2}-\d{2})\nversion: (\d+\.\d+\.\d+)\n---\n", source)
    if not match or len(source.encode("utf-8")) > 256 * 1024:
        raise ValueError("guide-metadata")
    date.fromisoformat(match[2])
    return {"title": match[1], "updated": match[2], "version": match[3]}, source[match.end():]


def chapter_titles(source):
    # A heading-looking line inside a code example is ordinary example content.
    ordinary = re.sub(r"^```[^\n]*\n.*?^```\s*$", "", source, flags=re.M | re.S)
    titles = re.findall(r"^# (\d+)\. (.+)$", ordinary, re.M)
    if [number for number, _ in titles] != [str(i) for i in range(1, 21)]:
        raise ValueError("guide-chapter-set")
    return [number + ". " + title for number, title in titles]


def body(source, locale):
    labels = LABELS[locale]
    other = "ja" if locale == "en" else "en"
    other_path = "../ja/guide/" if locale == "en" else "../../guide/"
    lines, output, i = source.splitlines(), [], 0
    chapter, example, table, heading = 0, 0, 0, labels["title"]
    while i < len(lines):
        line = lines[i]
        if not line:
            i += 1
            continue
        if line.startswith("```"):
            language = line[3:]
            if language not in ("sh", "bash", "powershell", "json", "text"):
                raise ValueError("guide-code-language")
            end = i + 1
            while end < len(lines) and lines[end] != "```":
                end += 1
            if end == len(lines) or end == i + 1:
                raise ValueError("guide-code-fence")
            example += 1
            label = f"example-{example}-title"
            code = escape("\n".join(lines[i + 1:end]) + "\n")
            output.append(f'<figure class="guide-example" aria-labelledby="{label}" data-code-language="{language}">'
                          f'<figcaption id="{label}">{labels["example"]} {example}: {language}</figcaption>'
                          f'<pre tabindex="0"><code>{code}</code></pre></figure>')
            i = end + 1
            continue
        if line.startswith("# "):
            if chapter >= len(CHAPTER_IDS) or not line.startswith(f"# {chapter + 1}. "):
                raise ValueError("guide-chapter-order")
            if chapter:
                output.append("</section>")
            name = CHAPTER_IDS[chapter]
            heading = line[2:]
            output.append(f'<section class="guide-chapter" id="{name}" data-guide-chapter="{name}" '
                          f'aria-labelledby="{name}-title"><h2 id="{name}-title">{inline(heading)}</h2>'
                          f'<a class="guide-chapter-switch" href="{other_path}#{name}" '
                          f'lang="{other}" hreflang="{other}">{labels["other"]}</a>')
            chapter += 1
            i += 1
            continue
        if line.startswith("## "):
            anchored = re.fullmatch(r"## (.+) \{#(recipe-[a-z0-9-]{1,80})\}", line)
            heading = anchored[1] if anchored else line[3:]
            attributes = f' id="{anchored[2]}" tabindex="-1"' if anchored else ""
            output.append("<h3" + attributes + ">" + inline(heading) + "</h3>")
            i += 1
            continue
        if line.startswith("|"):
            rows = []
            while i < len(lines) and lines[i].startswith("|"):
                if not lines[i].endswith("|"):
                    raise ValueError("guide-table-shape")
                rows.append([cell.strip() for cell in lines[i][1:-1].split("|")])
                i += 1
            if (len(rows) < 3 or len(rows[0]) < 2 or
                    any(len(row) != len(rows[0]) or not all(row) for row in rows) or
                    not all(re.fullmatch(r":?-{3,}:?", cell) for cell in rows[1])):
                raise ValueError("guide-table-shape")
            table += 1
            label = f"table-{table}-title"
            output.append(f'<div class="guide-table-scroll" role="region" aria-labelledby="{label}" tabindex="0">'
                          f'<table><caption id="{label}">{inline(heading)} — {labels["table"]} {table}</caption><thead><tr>'
                          + "".join('<th scope="col">' + inline(cell) + "</th>" for cell in rows[0])
                          + "</tr></thead><tbody>")
            for row in rows[2:]:
                output.append('<tr><th scope="row">' + inline(row[0]) + "</th>"
                              + "".join("<td>" + inline(cell) + "</td>" for cell in row[1:]) + "</tr>")
            output.append("</tbody></table></div>")
            continue
        if line.startswith("- ") or re.match(r"\d+\. ", line):
            ordered = not line.startswith("- ")
            tag, pattern = ("ol", r"\d+\. (.+)") if ordered else ("ul", r"- (.+)")
            if not re.fullmatch(pattern, line):
                raise ValueError("guide-list-item")
            output.append("<" + tag + ">")
            number = 1
            while i < len(lines) and re.fullmatch(pattern, lines[i]):
                if ordered and not lines[i].startswith(f"{number}. "):
                    raise ValueError("guide-list-order")
                output.append("<li>" + inline(re.fullmatch(pattern, lines[i])[1]) + "</li>")
                i += 1
                number += 1
            output.append("</" + tag + ">")
            continue
        if line.startswith(("#", ">", "---", "    ")):
            raise ValueError("guide-block-syntax")
        paragraph = []
        while i < len(lines) and lines[i] and not lines[i].startswith(("#", "|", "```", "- ")):
            paragraph.append(lines[i])
            i += 1
        output.append("<p>" + inline(" ".join(paragraph)) + "</p>")
    if chapter:
        output.append("</section>")
    return "\n".join(output)


def expand_examples(source, locale, repository_root):
    marker = "{{conversion-examples}}"
    ordinary = re.sub(r"^```[^\n]*\n.*?^```\s*$", "", source, flags=re.M | re.S)
    if (source.count(marker) != 1 or ordinary.count(marker) != 1
            or re.findall(r"^# (\d+)\. ", ordinary.split(marker)[0], re.M)[-1:] != ["8"]
            or not re.search(r"^\{\{conversion-examples\}\}$", source, re.M)):
        raise ValueError("guide-conversion-examples")
    path = Path(repository_root) / "tools/conversion_examples.py"
    # Generate from the recorded source bytes, without using a stale bytecode cache.
    examples = {"__name__": "guide_conversion_examples", "__file__": str(path)}
    exec(compile(path.read_bytes(), str(path), "exec"), examples)
    return source.replace(marker, examples["markdown"](locale))


def render(repository_root):
    root = Path(repository_root)
    template = (root / "tools/templates/guide.html").read_text(encoding="utf-8")
    if len(template.encode("utf-8")) > 32 * 1024:
        raise ValueError("guide-template-size")
    pages, versions = {}, []
    for locale in ("en", "ja"):
        text = (root / f"site-content/guide/{locale}.md").read_text(encoding="utf-8")
        if any(ord(char) < 32 and char not in "\n\t" for char in text):
            raise ValueError("guide-control-character")
        info, source = metadata(text)
        source = expand_examples(source, locale, root)
        labels = LABELS[locale]
        if info["title"] != labels["title"]:
            raise ValueError("guide-title")
        titles = chapter_titles(source)
        versions.append((info["version"], info["updated"]))
        prefix = "../" if locale == "en" else "../../"
        route = "guide/index.html" if locale == "en" else "ja/guide/index.html"
        other = "ja" if locale == "en" else "en"
        fields = {**labels, **info, "lang": locale, "other_lang": other,
                  "asset_prefix": prefix, "home_url": "../", "other_url": prefix + ("ja/guide/" if locale == "en" else "guide/"),
                  "updated_label": labels["updated"], "version_label": labels["version"],
                  "language_label": "Language" if locale == "en" else "言語",
                  "canonical": "https://kazuhide.github.io/drmr/" + ("guide/" if locale == "en" else "ja/guide/"),
                  "toc_items": "\n".join(f'<li><a href="#{name}">{inline(title)}</a></li>' for name, title in zip(CHAPTER_IDS, titles)),
                  "content": body(source, locale)}
        def substitute(match):
            name = match[1]
            if name not in fields:
                raise ValueError("guide-template-field")
            return fields[name] if name in ("content", "toc_items") else escape(fields[name], quote=True)
        page = re.sub(r"\{\{([a-z_]+)\}\}", substitute, template)
        if "{{" in page or "}}" in page:
            raise ValueError("guide-template-field")
        pages[route] = page.encode("utf-8")
    if versions[0] != versions[1]:
        raise ValueError("guide-locale-metadata")
    return pages


def outputs_match(root, pages):
    return all((Path(root) / "site" / route).is_file()
               and (Path(root) / "site" / route).read_bytes() == content for route, content in pages.items())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="Refuse stale committed HTML without writing it")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    try:
        pages = render(root)
        if args.check:
            if not outputs_match(root, pages):
                raise ValueError("guide-output-stale")
        else:
            for route, content in pages.items():
                target = root / "site" / route
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(content)
        print("Guide outputs match sources" if args.check else "Generated two guide pages")
        return 0
    except (OSError, ValueError) as error:
        print("Guide build refused: " + str(error))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
