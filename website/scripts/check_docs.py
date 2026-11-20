"""Check the static documentation's routes, anchors, labels, and search targets, and the Markdown
agents read: each page's Markdown, /llms.txt and /llms-full.txt."""
from html.parser import HTMLParser
import json
from pathlib import Path
import re
import sys
from urllib.parse import unquote, urljoin, urlsplit


class Document(HTMLParser):
    def __init__(self, source):
        super().__init__()
        self.ids = []
        self.references = []
        self.links = []
        self.h1_count = 0
        self.feed(source)

    def handle_starttag(self, tag, attributes):
        attributes = dict(attributes)
        if "id" in attributes:
            self.ids.append(attributes["id"])
        if tag == "h1":
            self.h1_count += 1
        for name in ("aria-controls", "aria-labelledby", "aria-describedby"):
            self.references.extend(attributes.get(name, "").split())
        if tag in ("a", "use") and "href" in attributes:
            self.links.append(attributes["href"])
        if tag in ("script", "img") and "src" in attributes:
            self.links.append(attributes["src"])
        if tag == "link" and attributes.get("rel") in ("stylesheet", "icon", "alternate"):
            self.links.append(attributes["href"])


def main():
    root = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).resolve().parents[1] / "dist"
    documents = {}
    failures = []

    def read(target):
        if target not in documents:
            documents[target] = Document(target.read_text())
        return documents[target]

    def check_link(route, href):
        url = urlsplit(href)
        if url.scheme or url.netloc:
            return
        location = urljoin(route, url.path or "")
        target = root / unquote(location).lstrip("/")
        if location.endswith("/") or target.is_dir():
            target /= "index.html"
        if not target.is_file():
            failures.append(f"{route}: missing {href}")
            return
        if url.fragment and unquote(url.fragment) not in read(target).ids:
            failures.append(f"{route}: missing anchor {href}")

    pages = json.loads((root / "docs/pages.json").read_text())
    for page in pages:
        route = page["href"]
        document = read(root / route.lstrip("/") / "index.html")
        if document.h1_count != 1:
            failures.append(f"{route}: expected exactly one h1")
        if len(document.ids) != len(set(document.ids)):
            failures.append(f"{route}: duplicate IDs")
        for reference in document.references:
            if reference not in document.ids:
                failures.append(f"{route}: missing accessible target {reference}")
        for href in document.links:
            check_link(route, href)
    index = json.loads((root / "docs/search-index.json").read_text())
    for record in index:
        check_link("/docs/", record["href"])
    for href in read(root / "index.html").links:
        if href.startswith("/docs/"):
            check_link("/", href)
    check_agent_markdown(root, pages, read, check_link, failures)
    if failures:
        raise SystemExit("\n".join(failures))
    print(f"Checked {len(pages)} docs pages and {len(index)} search targets: routes, anchors, assets, IDs, and accessible labels are valid.")
    print(f"Checked {len(pages)} Markdown pages, /llms.txt and /llms-full.txt: every page is listed, and every link is absolute and resolves.")


SITE_URL = "https://lemma.systems"
FENCE = re.compile(r"(?ms)^\s*(`{3,}|~{3,}).*?^\s*\1\s*$")
INLINE_CODE = re.compile(r"(`+).*?\1")
LINK = re.compile(r"\]\(([^)\s]+)")


def prose(markdown):
    """The Markdown outside code, where links and comments are written."""
    return INLINE_CODE.sub("", FENCE.sub("", markdown))


def check_agent_markdown(root, pages, read, check_link, failures):
    """An agent reading the Markdown follows every link without the repository beside it, so each
    link is absolute (or a fragment of its own page) and a link into the site resolves, as the HTML's do."""
    html_route = {page["markdown"]: page["href"] for page in pages}

    def check_markdown_link(route, href):
        if href.startswith("#"):
            fragment = href[1:]
            if fragment not in read(root / route.lstrip("/") / "index.html").ids:
                failures.append(f"{route} (Markdown): missing anchor {href}")
            return
        if href.startswith(SITE_URL + "/"):
            location, _, fragment = href[len(SITE_URL):].partition("#")
            if location in html_route:
                if not (root / location.lstrip("/")).is_file():
                    failures.append(f"{route} (Markdown): missing {href}")
                    return
                location = html_route[location]
            check_link(route, location + (f"#{fragment}" if fragment else ""))
            return
        if not re.match(r"(https?:|mailto:)", href):
            failures.append(f"{route} (Markdown): relative link {href}")

    llms_index = (root / "llms.txt").read_text() if (root / "llms.txt").is_file() else ""
    llms_full = (root / "llms-full.txt").read_text() if (root / "llms-full.txt").is_file() else ""
    if not llms_index or not llms_full:
        failures.append("missing /llms.txt or /llms-full.txt")
    for href in LINK.findall(prose(llms_index)):
        check_markdown_link("/", href)
    for page in pages:
        route, markdown_file = page["href"], root / page["markdown"].lstrip("/")
        if not markdown_file.is_file():
            failures.append(f"{route}: missing Markdown {page['markdown']}")
            continue
        if f"]({SITE_URL}{page['markdown']})" not in llms_index:
            failures.append(f"{route}: not listed in /llms.txt")
        if f"Source: {SITE_URL}{route}\n" not in llms_full:
            failures.append(f"{route}: not in /llms-full.txt")
        text = prose(markdown_file.read_text())
        if "<!--" in text:
            failures.append(f"{route} (Markdown): an HTML comment is left in the prose")
        for href in LINK.findall(text):
            check_markdown_link(route, href)


if __name__ == "__main__":
    main()
