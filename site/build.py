#!/usr/bin/env python3
"""Build lean-runtime's architecture site (site/*.html) from site/src/.

    python3 site/build.py           # write the pages
    python3 site/build.py --check   # write nothing; fail if a page is out of
                                    # date or a check fails

Python 3's standard library only. The pages work offline (file://): no
network, no scripts, no external fonts or images.

What it does:
- wraps every src/pages/NAME.html (the body of a page) in the shared
  template: the header, the navigation and the footer;
- fills the placeholders of the bodies:
    {{svg:NAME}}  the hand-drawn diagram src/diagrams/NAME.svg
    {{v:NAME}}    a short value: the Lean version or a count (values())
- turns links written as href="repo:PATH" into relative links to the
  repository's files;
- refuses to write anything when a source or a page holds a forbidden
  string (FORBIDDEN: the repository is public), when a diagram is not
  well-formed XML, when a link or a fragment is broken, or when a page's
  tags do not balance;
- warns when a sentence is longer than MAX_WORDS, when a diagram is
  unused or a page is not in PAGES, and when the sources have moved on
  from the pages (a Lean-bug entry, a module or a feature that its page
  does not name; a branch the status page calls in progress whose files
  are now in the checkout). --check fails on a warning.

The output depends only on the repository's files, not on the date, the
commit or the machine: building twice gives the same pages, so the built
pages are committed and open directly after a clone.
"""

import argparse
import glob
import html
import os
import re
import subprocess
import sys
import xml.etree.ElementTree as ET
from html.parser import HTMLParser

SITE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(SITE)
SRC = os.path.join(SITE, "src")

# (file name, navigation label). The page title is "<label> · lean-runtime".
PAGES = [
    ("index", "Overview"),
    ("architecture", "Architecture"),
    ("semantics", "semantics"),
    ("io", "io"),
    ("sched", "sched"),
    ("net", "net"),
    ("testing", "Testing"),
    ("lean-bugs", "Lean bugs"),
    ("status", "Status"),
    ("glossary", "Glossary"),
]

# Strings that must not appear in the site's sources or pages: local paths.
# The repository is public. More strings (names of private directories or
# projects, which this file must not spell out) come from the environment:
# LEAN_RUNTIME_SITE_FORBIDDEN, comma-separated.
FORBIDDEN = ["/home/", "/Users/", "~/"] + [
    s.strip() for s in os.environ.get("LEAN_RUNTIME_SITE_FORBIDDEN", "").split(",") if s.strip()]

# The only external site links may point to: the repository itself.
EXTERNAL_OK = "https://github.com/QueClr/lean-runtime-rs"

# Sentences longer than this are reported (about 80% of ASD-STE100: 20 words
# for steps, 25 for descriptions).
MAX_WORDS = 25

# Lean bugs that lean-bugs.html describes before docs/lean-bugs.md has their
# entry. When the entry lands, the page's "not in docs/lean-bugs.md yet"
# becomes stale: update the page and remove the id here.
PENDING_LB = set()

# Branches that status.html (and other pages) call in progress, each with a
# file that only that branch adds. When the file is in the checkout, the
# branch is merged and the pages are stale. This needs no git: it works in a
# fresh clone and after the branch is deleted.
IN_FLIGHT = {}

VOID = {"area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta",
        "source", "track", "wbr"}

ERRORS = []
WARNINGS = []


def error(msg):
    ERRORS.append(msg)


def warn(msg):
    WARNINGS.append(msg)


def read(path):
    with open(os.path.join(REPO, path), encoding="utf-8") as f:
        return f.read()


def forbidden_in(text):
    low = text.lower()
    return [f for f in FORBIDDEN if f.lower() in low]


# ---------------------------------------------------------------------------
# Short values, counted from the repository

def values():
    m = re.search(r'LEAN_VERSION: &str = "([^"]+)"', read("src/lib.rs"))
    rows_files = sorted(glob.glob(os.path.join(REPO, "tests/cases/*/*.rows.toml")))
    rows = sum(len(re.findall(r"^\[\[row\]\]", open(f, encoding="utf-8").read(), re.M))
               for f in rows_files)
    cases = sorted(c for c in glob.glob(os.path.join(REPO, "tests/cases/**/*.lean"), recursive=True)
                   if not any(part.endswith(".files") for part in c.split(os.sep)))
    per_area = {}
    for c in cases:
        area = os.path.relpath(c, os.path.join(REPO, "tests/cases")).split(os.sep)[0]
        per_area[area] = per_area.get(area, 0) + 1
    # one list item an area ("<code>io/</code> 20"), for a list in columns
    case_areas = "".join(f"<li><code>{html.escape(a)}/</code> {n}</li>" for a, n in sorted(per_area.items()))
    benches = read("benches/benches.toml")
    io_benches = read("benches/io/benches.toml")
    return {
        "lean": m.group(1) if m else "?",
        "rows": str(rows),
        "row_areas": str(len(rows_files)),
        "cases": str(len(cases)),
        "case_areas": case_areas,
        "bench_pairs": str(len(re.findall(r"^\[\[bench\]\]", benches, re.M))),
        "bench_not_comparable": str(len(re.findall(r"^comparable = false", benches, re.M))),
        "io_bench_pairs": str(len(re.findall(r"^\[\[bench\]\]", io_benches, re.M))),
    }


# ---------------------------------------------------------------------------
# Rendering

TEMPLATE = """<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} · lean-runtime</title>
<link rel="stylesheet" href="style.css">
</head>
<body>
<header class="top">
  <div class="brand"><a href="index.html">lean-runtime</a><span>Lean {lean}'s runtime in safe Rust</span></div>
  <nav class="main" aria-label="Pages">
{nav}
  </nav>
</header>
<main>
{body}
</main>
<footer>Generated by <code>site/build.py</code> from <code>site/src/</code> and the repository's files.
Written about 80% to ASD-STE100 (Simplified Technical English).
The markdown docs and the module docs are the authority; this site summarizes them.</footer>
</body>
</html>
"""


def diagram(name, page):
    path = os.path.join(SRC, "diagrams", name + ".svg")
    if not os.path.exists(path):
        error(f"{page}: unknown diagram {name}")
        return ""
    text = open(path, encoding="utf-8").read().strip()
    check_svg(name, text)
    return text


def render(name, label, vals):
    body = open(os.path.join(SRC, "pages", name + ".html"), encoding="utf-8").read().strip()
    body = re.sub(r"\{\{svg:([\w-]+)\}\}", lambda m: diagram(m.group(1), name), body)

    def value(m):
        if m.group(1) not in vals:
            error(f"{name}: unknown value {m.group(1)}")
            return "?"
        return vals[m.group(1)]

    body = re.sub(r"\{\{v:([\w-]+)\}\}", value, body)
    if "{{" in body:
        error(f"{name}: unknown placeholder near {body[body.index('{{'):][:40]!r}")
    body = re.sub(r'href="repo:([^"]+)"', r'href="../\1"', body)
    nav = "\n".join(
        f'    <a href="{p}.html"{" aria-current=\"page\"" if p == name else ""}>{html.escape(t)}</a>'
        for p, t in PAGES)
    return TEMPLATE.format(title=html.escape(label), lean=vals["lean"], nav=nav, body=body)


# ---------------------------------------------------------------------------
# Checks on the pages

def check_svg(name, text):
    try:
        root = ET.fromstring(text)
    except ET.ParseError as e:
        error(f"diagram {name}: not well-formed XML: {e}")
        return
    ns = "{http://www.w3.org/2000/svg}"
    if root.tag != ns + "svg":
        error(f"diagram {name}: the root is not an <svg> in the SVG namespace")
    if root.find(ns + "title") is None:
        error(f"diagram {name}: no <title>")
    ids = {e.get("id") for e in root.iter() if e.get("id")}
    for e in root.iter():
        for attr in ("marker-start", "marker-end"):
            v = e.get(attr)
            if v:
                m = re.match(r"url\(#([^)]+)\)", v)
                if not m or m.group(1) not in ids:
                    error(f"diagram {name}: {attr} {v} is not defined")
    for ref in root.get("aria-labelledby", "").split():
        if ref not in ids:
            error(f"diagram {name}: aria-labelledby {ref} is not defined")


class PageScan(HTMLParser):
    """Collects ids, links, tag balance and the text of prose elements."""

    PROSE = {"p", "li", "dd", "td", "th", "figcaption", "summary"}
    SKIP = {"svg", "pre", "style", "script"}

    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.ids, self.links, self.stack, self.problems = [], [], [], []
        self.prose = []      # stack of [element, text] being collected
        self.texts = []      # finished prose texts
        self.skip = 0

    def handle_starttag(self, tag, attrs):
        a = dict(attrs)
        if a.get("id"):
            self.ids.append(a["id"])
        for k in ("href", "src"):
            if a.get(k) is not None:
                self.links.append((tag, a[k]))
        if tag in ("img", "script", "iframe", "object"):
            self.problems.append(f"<{tag}> is not allowed")
        if tag in VOID:
            return
        self.stack.append(tag)
        if tag in self.SKIP:
            self.skip += 1
        elif not self.skip and tag in self.PROSE:
            self.prose.append([tag, ""])
        elif not self.skip and tag in ("ul", "ol", "table", "div") and self.prose:
            # a nested block ends the sentence of its parent's text so far
            self.prose[-1][1] += " "

    def handle_startendtag(self, tag, attrs):
        a = dict(attrs)
        if a.get("id"):
            self.ids.append(a["id"])
        for k in ("href", "src"):
            if a.get(k) is not None:
                self.links.append((tag, a[k]))
        if tag in ("img", "script", "iframe", "object"):
            self.problems.append(f"<{tag}> is not allowed")

    def handle_endtag(self, tag):
        if tag in VOID:
            return
        if not self.stack or self.stack[-1] != tag:
            self.problems.append(f"</{tag}> does not close <{self.stack[-1] if self.stack else 'nothing'}>"
                                 f" (line {self.getpos()[0]})")
            if tag in self.stack:
                while self.stack and self.stack[-1] != tag:
                    self.stack.pop()
            else:
                return
        self.stack.pop()
        if tag in self.SKIP:
            self.skip -= 1
        elif not self.skip and tag in self.PROSE and self.prose and self.prose[-1][0] == tag:
            self.texts.append(self.prose.pop()[1])

    def handle_data(self, data):
        if not self.skip and self.prose:
            self.prose[-1][1] += data


def scan(page, text):
    s = PageScan()
    s.feed(text)
    s.close()
    for p in s.problems:
        error(f"{page}.html: {p}")
    if s.stack:
        error(f"{page}.html: unclosed tags {s.stack}")
    dup = sorted({i for i in s.ids if s.ids.count(i) > 1})
    if dup:
        error(f"{page}.html: duplicate ids {dup}")
    for t in s.texts:
        for sentence in re.split(r"(?<=[.!?])\s+", re.sub(r"\s+", " ", t).strip()):
            words = [w for w in sentence.split() if re.search(r"[A-Za-z0-9]", w)]
            if len(words) > MAX_WORDS:
                warn(f"{page}.html: a sentence of {len(words)} words: {sentence}")
    return s


def tracked_files():
    """The repository's tracked files (relative paths), or None without git.
    A link from the site to a repository file must point to a tracked file,
    or it is broken in a clone. The site's own files are exempt: they are
    committed together with the pages."""
    try:
        r = subprocess.run(["git", "-C", REPO, "ls-files", "-z"], capture_output=True)
    except OSError:
        return None
    if r.returncode != 0:
        return None
    return set(p for p in r.stdout.decode("utf-8").split("\0") if p)


def check_links(scans):
    ids = {name: set(s.ids) for name, s in scans.items()}
    tracked = tracked_files()
    for name, s in scans.items():
        for tag, url in s.links:
            if re.match(r"^[a-z][a-z0-9+.-]*:", url):
                if not (url == EXTERNAL_OK or url.startswith((EXTERNAL_OK + "/", EXTERNAL_OK + "#"))):
                    error(f"{name}.html: external link not allowed: {url}")
                continue
            path, _, frag = url.partition("#")
            target = os.path.normpath(os.path.join(SITE, path)) if path else os.path.join(SITE, name + ".html")
            rel = os.path.relpath(target, REPO)
            if rel.split(os.sep)[0] == "..":
                error(f"{name}.html: link leaves the repository: {url}")
                continue
            page = os.path.basename(target)[:-5] if target.endswith(".html") and os.path.dirname(target) == SITE else None
            if page is None and not os.path.exists(target):
                error(f"{name}.html: broken link: {url}")
            elif page is None and tracked is not None and not rel.startswith("site" + os.sep) \
                    and rel not in tracked and not any(t.startswith(rel + "/") for t in tracked):
                error(f"{name}.html: link to a file git does not track (broken in a clone): {url}")
            elif page is not None and page not in ids:
                error(f"{name}.html: broken link: {url}")
            if frag:
                if page is None:
                    error(f"{name}.html: a fragment on a file that is not a page: {url}")
                elif page in ids and frag not in ids[page]:
                    error(f"{name}.html: missing fragment: {url}")


# ---------------------------------------------------------------------------
# Checks of the pages against the sources

def check_sources(pages):
    lb_doc = set(re.findall(r"\bLB-\d\d\b", read("docs/lean-bugs.md")))
    lb_page = set(re.findall(r"\bLB-\d\d\b", pages["lean-bugs"]))
    for i in sorted(lb_doc - lb_page):
        warn(f"docs/lean-bugs.md has {i}; lean-bugs.html does not mention it")
    for i in sorted(PENDING_LB & lb_doc):
        warn(f"{i} is in docs/lean-bugs.md now: update lean-bugs.html and remove it from PENDING_LB")
    for i in sorted(lb_page - lb_doc - PENDING_LB):
        warn(f"lean-bugs.html mentions {i}, which docs/lean-bugs.md does not have")
    for module, page in (("semantics", "semantics"), ("io", "io"), ("sched", "sched"), ("net", "net")):
        mod_rs = os.path.join(REPO, "src", module, "mod.rs")
        if not os.path.exists(mod_rs):
            continue
        for sub in re.findall(r"^pub mod (\w+);", read(f"src/{module}/mod.rs"), re.M):
            if f'href="../src/{module}/{sub}.rs"' not in pages[page]:
                warn(f"src/{module}/mod.rs has the module {sub}; {page}.html does not link "
                     f"src/{module}/{sub}.rs")
    feats = re.search(r"^\[features\]\n(.*?)(?:^\[|\Z)", read("Cargo.toml"), re.M | re.S)
    for f in re.findall(r"^([\w-]+) =", feats.group(1) if feats else "", re.M):
        if f != "default" and f"<code>{f}</code>" not in pages["architecture"]:
            warn(f"Cargo.toml has the feature {f}; architecture.html does not name it")
    for branch, marker in IN_FLIGHT.items():
        if os.path.exists(os.path.join(REPO, marker)):
            warn(f"{marker} is in the checkout, so branch {branch} is merged: update status.html "
                 f"and the pages that call it a branch, then remove it from IN_FLIGHT")


# ---------------------------------------------------------------------------

def main():
    ap = argparse.ArgumentParser(description="Build lean-runtime's architecture site.")
    ap.add_argument("--check", action="store_true",
                    help="write nothing; fail if a page would change or a check fails")
    args = ap.parse_args()

    sources = [os.path.join(SITE, f) for f in ("README.md", "style.css")]
    sources += sorted(glob.glob(os.path.join(SRC, "**", "*"), recursive=True))
    for path in sources:
        if os.path.isfile(path):
            bad = forbidden_in(open(path, encoding="utf-8").read())
            if bad:
                error(f"{os.path.relpath(path, SITE)}: forbidden {bad}")
    if re.search(r"@import|url\(", open(os.path.join(SITE, "style.css"), encoding="utf-8").read()):
        error("style.css: @import or url() would load something from outside")

    vals = values()
    pages = {name: render(name, label, vals) for name, label in PAGES}
    for name, out in pages.items():
        bad = forbidden_in(out)
        if bad:
            error(f"{name}.html: forbidden {bad}")
    scans = {name: scan(name, out) for name, out in pages.items()}
    check_links(scans)
    check_sources(pages)
    bodies = "".join(open(os.path.join(SRC, "pages", n + ".html"), encoding="utf-8").read() for n, _ in PAGES)
    for path in sorted(glob.glob(os.path.join(SRC, "diagrams", "*.svg"))):
        d = os.path.basename(path)[:-4]
        if "{{svg:" + d + "}}" not in bodies:
            check_svg(d, open(path, encoding="utf-8").read())
            warn(f"diagram {d} is not used by any page")
    for n, _ in PAGES:
        body = open(os.path.join(SRC, "pages", n + ".html"), encoding="utf-8").read()
        for k, svg in enumerate(re.findall(r"<svg\b.*?</svg>", body, re.S)):
            check_svg(f"{n} (inline #{k})", svg)
    for path in sorted(glob.glob(os.path.join(SITE, "*.html"))):
        if os.path.basename(path)[:-5] not in pages:
            warn(f"{os.path.basename(path)} is not in PAGES: remove it, or add it to PAGES")

    for w in WARNINGS:
        print("warning:", w, file=sys.stderr)
    if ERRORS:
        for e in ERRORS:
            print("error:", e, file=sys.stderr)
        print("nothing written", file=sys.stderr)
        return 1
    changed = []
    for name, out in pages.items():
        path = os.path.join(SITE, name + ".html")
        old = open(path, encoding="utf-8").read() if os.path.exists(path) else None
        if old != out:
            changed.append(name + ".html")
            if not args.check:
                with open(path, "w", encoding="utf-8") as f:
                    f.write(out)
    if args.check:
        if changed:
            print("out of date: " + ", ".join(changed) + " (run site/build.py)", file=sys.stderr)
        ok = not changed and not WARNINGS
        print("site: up to date, no warnings" if ok else "site: check failed", file=sys.stderr)
        return 0 if ok else 1
    print(f"{len(pages)} pages, {len(changed)} changed, {len(WARNINGS)} warning(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
