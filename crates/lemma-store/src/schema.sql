-- Lemma storage format. Positions are zero-based; columns are UTF-8 bytes.
-- Bump SCHEMA_VERSION in lib.rs whenever this changes.
-- Ids use AUTOINCREMENT so an id from a replaced snapshot never points at a
-- different row later.

CREATE TABLE IF NOT EXISTS schema_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- One row per indexed state of a directory.
CREATE TABLE IF NOT EXISTS snapshots (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    root              TEXT NOT NULL,
    revision          TEXT,
    is_dirty          INTEGER NOT NULL,
    fingerprint       TEXT NOT NULL,
    extractor_version TEXT NOT NULL,
    -- Wall-clock time the indexing run started. Files modified before it and
    -- unchanged on disk since can be reused without reading them again.
    indexed_at_ns     INTEGER NOT NULL
);

-- Paths, relative to the snapshot root, of source files that differ from
-- the snapshot's revision. Kept so the next index only has to ask Git about
-- files that changed since.
CREATE TABLE IF NOT EXISTS uncommitted_paths (
    snapshot_id INTEGER NOT NULL REFERENCES snapshots (id) ON DELETE CASCADE,
    path        TEXT NOT NULL,
    PRIMARY KEY (snapshot_id, path)
);

CREATE TABLE IF NOT EXISTS files (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    snapshot_id       INTEGER NOT NULL REFERENCES snapshots (id) ON DELETE CASCADE,
    path              TEXT NOT NULL,
    language          TEXT NOT NULL,
    content_hash      TEXT NOT NULL,
    has_syntax_errors INTEGER NOT NULL,
    symbol_count      INTEGER NOT NULL,
    -- File system metadata when the file was read; see FileStamp.
    size_bytes        INTEGER NOT NULL,
    modified_ns       INTEGER NOT NULL,
    changed_ns        INTEGER NOT NULL,
    inode             INTEGER NOT NULL,
    UNIQUE (snapshot_id, path)
);

CREATE TABLE IF NOT EXISTS symbols (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    file_id           INTEGER NOT NULL REFERENCES files (id) ON DELETE CASCADE,
    name              TEXT NOT NULL,
    qualified_name    TEXT NOT NULL,
    kind              TEXT NOT NULL,
    start_line        INTEGER NOT NULL,
    start_column      INTEGER NOT NULL,
    end_line          INTEGER NOT NULL,
    end_column        INTEGER NOT NULL,
    name_start_line   INTEGER NOT NULL,
    name_start_column INTEGER NOT NULL,
    name_end_line     INTEGER NOT NULL,
    name_end_column   INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS symbols_by_name ON symbols (name);
CREATE INDEX IF NOT EXISTS symbols_by_qualified_name ON symbols (qualified_name);
CREATE INDEX IF NOT EXISTS symbols_by_file ON symbols (file_id, start_line);

-- A provider's answer to one question asked at one position, keyed by
-- everything that could change the answer within one snapshot. Questions
-- about a symbol are asked at its name. Only definitive answers are stored:
-- failures say something about the provider, not the code.
CREATE TABLE IF NOT EXISTS answers (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    snapshot_id        INTEGER NOT NULL REFERENCES snapshots (id) ON DELETE CASCADE,
    -- references, definitions, implementations, or hover.
    question           TEXT NOT NULL,
    path               TEXT NOT NULL,
    line               INTEGER NOT NULL,
    column             INTEGER NOT NULL,
    provider_id        TEXT NOT NULL,
    config_fingerprint TEXT NOT NULL,
    provider_version   TEXT,
    -- found, empty, or unsupported.
    state              TEXT NOT NULL,
    -- The answer to a hover question; locations are in answer_locations.
    text               TEXT,
    UNIQUE (snapshot_id, question, path, line, column, provider_id, config_fingerprint)
);

CREATE TABLE IF NOT EXISTS answer_locations (
    answer_id    INTEGER NOT NULL REFERENCES answers (id) ON DELETE CASCADE,
    path         TEXT NOT NULL,
    is_external  INTEGER NOT NULL,
    start_line   INTEGER NOT NULL,
    start_column INTEGER NOT NULL,
    end_line     INTEGER NOT NULL,
    end_column   INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS answer_locations_by_answer ON answer_locations (answer_id);

-- What each service in lemma.yaml offers, as of a snapshot, and where in
-- the code it is implemented. Rebuilt whenever lemma.yaml or the snapshot
-- changes; `sources` records what it was built from.
CREATE TABLE IF NOT EXISTS exports (
    snapshot_id INTEGER NOT NULL REFERENCES snapshots (id) ON DELETE CASCADE,
    service     TEXT NOT NULL,
    name        TEXT NOT NULL,
    -- operation, endpoint, or symbol.
    kind        TEXT NOT NULL,
    path        TEXT,
    symbol      TEXT,
    detail      TEXT,
    PRIMARY KEY (snapshot_id, service, name, kind)
);

-- Links lemma.yaml declares between exports, by address.
CREATE TABLE IF NOT EXISTS export_links (
    snapshot_id INTEGER NOT NULL REFERENCES snapshots (id) ON DELETE CASCADE,
    caller      TEXT NOT NULL,
    provider    TEXT NOT NULL,
    PRIMARY KEY (snapshot_id, caller, provider)
);

-- What a snapshot's exports were built from: a hash of lemma.yaml and the
-- snapshot fingerprint, so an unchanged pair is not rebuilt.
CREATE TABLE IF NOT EXISTS export_sources (
    snapshot_id INTEGER PRIMARY KEY REFERENCES snapshots (id) ON DELETE CASCADE,
    fingerprint TEXT NOT NULL
);

-- A workspace index: the repositories a workspace lemma.yaml composes, each
-- with the index holding its symbols, and every member's exports and links
-- with addresses made absolute (#member:services.<service>.<name>).
CREATE TABLE IF NOT EXISTS workspace_members (
    name         TEXT PRIMARY KEY,
    root         TEXT NOT NULL,
    revision     TEXT,
    -- The directory indexed: the clone, or a worktree at the pin.
    indexed_root TEXT NOT NULL,
    database     TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS workspace_exports (
    member  TEXT NOT NULL,
    service TEXT NOT NULL,
    name    TEXT NOT NULL,
    kind    TEXT NOT NULL,
    path    TEXT,
    symbol  TEXT,
    detail  TEXT,
    PRIMARY KEY (member, service, name, kind)
);

CREATE TABLE IF NOT EXISTS workspace_links (
    member   TEXT NOT NULL,
    caller   TEXT NOT NULL,
    provider TEXT NOT NULL,
    PRIMARY KEY (member, caller, provider)
);
