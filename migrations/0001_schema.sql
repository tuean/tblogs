-- tblogs content package schema.
--
-- This database is a build artifact: `tblogs-admin build` recreates it from
-- scratch on every run, so there is a single schema definition rather than a
-- chain of migrations. The server only ever opens it read-only.

CREATE TABLE posts (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    slug         TEXT    NOT NULL UNIQUE,
    title        TEXT    NOT NULL,
    summary      TEXT    NOT NULL DEFAULT '',
    body_md      TEXT    NOT NULL DEFAULT '',
    published    INTEGER NOT NULL DEFAULT 0,
    created_at   TEXT    NOT NULL,
    updated_at   TEXT    NOT NULL,
    published_at TEXT,

    -- Path of the source Markdown file, relative to the content root, with
    -- forward slashes. Every post in a package comes from a file, and the
    -- server uses this path to resolve the post's relative image references.
    source_path  TEXT    NOT NULL UNIQUE,

    -- Source file modification time in nanoseconds since the epoch.
    source_mtime INTEGER NOT NULL,

    -- Cover image as written in front matter (`image:`), usually a remote URL.
    -- Stored verbatim; the renderer resolves it so a local path becomes a
    -- /media/... URL.
    cover_image  TEXT
);

-- The public index lists only published posts, newest first.
CREATE INDEX idx_posts_feed ON posts (published, published_at DESC);

CREATE TABLE tags (
    id   INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    slug TEXT NOT NULL UNIQUE
);

CREATE TABLE post_tags (
    post_id INTEGER NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    tag_id  INTEGER NOT NULL REFERENCES tags (id)  ON DELETE CASCADE,
    PRIMARY KEY (post_id, tag_id)
);

CREATE INDEX idx_post_tags_tag ON post_tags (tag_id);

-- Site-wide values baked in by the build, so a deployed server needs almost no
-- configuration of its own.
CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
