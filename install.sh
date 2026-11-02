#!/bin/sh
# Installs lemma from a GitHub release.
#
#   curl -fsSL https://raw.githubusercontent.com/angrybayblade/lemma/main/install.sh | sh
#
# While the repository is private, fetch the script with credentials:
#
#   gh api -H 'Accept: application/vnd.github.raw' repos/angrybayblade/lemma/contents/install.sh | sh
#
# Settings, all optional:
#   LEMMA_VERSION      release tag to install, such as v0.1.0-rc0 (default: the newest, pre-releases included)
#   LEMMA_INSTALL_DIR  where to put the binary (default: ~/.local/bin)
#   LEMMA_SKILLS       1 to also install the agent skills (default: 0)
#   LEMMA_SKILLS_DIR   where skills go (default: ~/.claude/skills)
#   GITHUB_TOKEN       token for a private repository when the GitHub CLI is not signed in
#   LEMMA_REPO         owner/name of the repository (default: angrybayblade/lemma)

set -eu

REPO="${LEMMA_REPO:-angrybayblade/lemma}"
INSTALL_DIR="${LEMMA_INSTALL_DIR:-$HOME/.local/bin}"
SKILLS_DIR="${LEMMA_SKILLS_DIR:-$HOME/.claude/skills}"

say() { printf 'lemma-install: %s\n' "$1" >&2; }
fail() { say "$1"; exit 1; }

target() {
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$os" in
    Darwin) os_part="apple-darwin" ;;
    Linux) os_part="unknown-linux-gnu" ;;
    *) fail "unsupported system: $os" ;;
  esac
  case "$arch" in
    arm64 | aarch64) arch_part="aarch64" ;;
    x86_64 | amd64) arch_part="x86_64" ;;
    *) fail "unsupported architecture: $arch" ;;
  esac
  printf '%s-%s' "$arch_part" "$os_part"
}

has_gh() {
  command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1
}

# GitHub API GET with the token if there is one.
api() {
  if [ -n "${GITHUB_TOKEN:-}" ]; then
    curl -fsSL -H "Authorization: Bearer $GITHUB_TOKEN" -H "Accept: application/vnd.github+json" "https://api.github.com/$1"
  else
    curl -fsSL -H "Accept: application/vnd.github+json" "https://api.github.com/$1"
  fi
}

latest_tag() {
  if has_gh; then
    gh release list -R "$REPO" --limit 1 --json tagName --jq '.[0].tagName'
    return
  fi
  api "repos/$REPO/releases?per_page=1" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1
}

# Downloads one release asset by name into a directory.
download() {
  tag="$1" name="$2" directory="$3"
  if has_gh; then
    gh release download "$tag" -R "$REPO" -p "$name" -D "$directory" --clobber
    return
  fi
  # Private repositories serve assets through the API, by asset id.
  id="$(api "repos/$REPO/releases/tags/$tag" |
    tr ',' '\n' |
    grep -B 3 "\"name\": *\"$name\"" |
    sed -n 's/.*"url": *"[^"]*\/assets\/\([0-9]*\)".*/\1/p' |
    head -n 1)"
  [ -n "$id" ] || fail "release $tag has no asset $name"
  if [ -n "${GITHUB_TOKEN:-}" ]; then
    curl -fsSL -H "Authorization: Bearer $GITHUB_TOKEN" -H "Accept: application/octet-stream" \
      -o "$directory/$name" "https://api.github.com/repos/$REPO/releases/assets/$id"
  else
    curl -fsSL -H "Accept: application/octet-stream" \
      -o "$directory/$name" "https://api.github.com/repos/$REPO/releases/assets/$id"
  fi
}

verify_checksum() {
  directory="$1" name="$2"
  expected="$(cut -d ' ' -f 1 "$directory/$name.sha256")"
  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$directory/$name" | cut -d ' ' -f 1)"
  else
    actual="$(shasum -a 256 "$directory/$name" | cut -d ' ' -f 1)"
  fi
  [ "$expected" = "$actual" ] || fail "checksum mismatch for $name"
}

main() {
  command -v curl >/dev/null 2>&1 || has_gh || fail "needs curl or the GitHub CLI"
  tag="${LEMMA_VERSION:-$(latest_tag)}"
  [ -n "$tag" ] || fail "no release found in $REPO (private? sign in with 'gh auth login' or set GITHUB_TOKEN)"
  platform="$(target)"
  archive="lemma-$tag-$platform.tar.gz"
  work="$(mktemp -d)"
  trap 'rm -rf "$work"' EXIT

  say "installing lemma $tag for $platform"
  download "$tag" "$archive" "$work"
  download "$tag" "$archive.sha256" "$work"
  verify_checksum "$work" "$archive"
  tar -xzf "$work/$archive" -C "$work"
  mkdir -p "$INSTALL_DIR"
  install -m 755 "$work/lemma-$tag-$platform/lemma" "$INSTALL_DIR/lemma"
  say "installed $INSTALL_DIR/lemma"

  if [ "${LEMMA_SKILLS:-0}" = "1" ]; then
    skills="lemma-skills-$tag.tar.gz"
    download "$tag" "$skills" "$work"
    mkdir -p "$SKILLS_DIR"
    tar -xzf "$work/$skills" -C "$work"
    cp -R "$work/skills/." "$SKILLS_DIR/"
    say "installed agent skills into $SKILLS_DIR"
  fi

  case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *) say "add $INSTALL_DIR to your PATH" ;;
  esac
  command -v souffle >/dev/null 2>&1 ||
    say "optional: install Souffle (brew install souffle) for faster 'lemma verify'"
  say "next: lemma providers   (shows which language servers Lemma found)"
}

main "$@"
