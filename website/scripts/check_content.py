"""Check that website/content, the docs snapshot the site builds from, matches the docs.

The build copies `docs/`, `skills/README.md` and `editors/README.md` into
`website/content` so the site can be built outside this repository. A docs
change committed without running the build leaves the site showing the old
text; this lists every file that differs. `source.json` is left out: it names
the last commit that changed the docs, which a commit cannot name for itself.

Run from the repository root, before the build, which would refresh the copy:

    python3 website/scripts/check_content.py
"""
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[2]
CONTENT = ROOT / "website" / "content"


def sources():
    """Each documentation source, by its path in the snapshot."""
    found = {
        Path("docs") / path.relative_to(ROOT / "docs"): path
        for path in (ROOT / "docs").rglob("*.md")
    }
    for name in ("skills", "editors"):
        found[Path(name) / "README.md"] = ROOT / name / "README.md"
    return found


def copies():
    """Each Markdown file in the snapshot, by its path there."""
    return {path.relative_to(CONTENT): path for path in CONTENT.rglob("*.md")}


def main():
    expected, kept = sources(), copies()
    problems = []
    for relative in sorted(expected.keys() | kept.keys()):
        if relative not in kept:
            problems.append(f"missing from the snapshot: {relative}")
        elif relative not in expected:
            problems.append(f"in the snapshot but no longer a source: {relative}")
        elif kept[relative].read_bytes() != expected[relative].read_bytes():
            problems.append(f"differs from its source: {relative}")
    if problems:
        print("website/content is out of date; run `node website/scripts/build.mjs` and commit it:")
        print("\n".join(f"  {problem}" for problem in problems))
        return 1
    print(f"website/content matches its {len(expected)} sources.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
