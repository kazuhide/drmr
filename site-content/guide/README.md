# Guide maintenance

The English and Japanese Markdown files are the user-facing source for the
twenty-chapter web guide. Keep commands, key roles, format restrictions and links
aligned across languages. Record the content update date in both frontmatter
blocks.

Generate the two committed static pages with Python 3.9 or later:

```sh
python3 tools/build_guide_site.py
python3 tools/build_guide_site.py --check
python3 -m unittest discover -s tools/tests -p test_build_guide_site.py -v
```

The dependency-free publisher supports only the constructs used here:
ordered numbered chapter headings, second-level topic headings, paragraphs,
flat lists, pipe tables, fenced `sh`, `bash`, `powershell`, `json` or `text`
examples, and inline code, HTTPS links and bold text. It rejects unsupported or
malformed syntax instead of passing HTML through. Chapter order and IDs are
shared across locales; change them together with the static and browser gates.

`--check` never writes files. CI also validates the explicit Pages payload,
source-to-output identity, local references and semantic HTML. Browser checks
separately cover responsive reading, native keyboard scrolling, locale links,
print styles and accessibility. Passing generation alone is not rendered QA.

Keep examples illustrative. Do not add real keys, private paths, internal
evidence or claims of playback-device/DRM-license qualification. Link to public
documentation and identifiable test runs when describing observed test results;
do not imply that every future `main` commit has the same runtime evidence.
