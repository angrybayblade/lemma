"""Check the static documentation's routes, anchors, labels, and search targets."""
from html.parser import HTMLParser
import json
from pathlib import Path
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
        if tag == "link" and attributes.get("rel") in ("stylesheet", "icon"):
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
    if failures:
        raise SystemExit("\n".join(failures))
    print(f"Checked {len(pages)} docs pages and {len(index)} search targets: routes, anchors, assets, IDs, and accessible labels are valid.")


if __name__ == "__main__":
    main()
