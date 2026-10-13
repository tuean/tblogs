-- Track which posts are backed by a Markdown file on disk.
--
-- NULL means the post was created in the web admin and is owned by the
-- database. A non-NULL value is a path relative to the content directory, and
-- in that case the *file* is authoritative: the sync job rebuilds the row from
-- the file on every change and removes the row when the file disappears.

ALTER TABLE posts ADD COLUMN source_path TEXT;

-- Modification time of the source file, in nanoseconds since the epoch.
-- Lets a re-sync skip files that have not changed, so no file is read on the
-- common path where nothing was touched.
ALTER TABLE posts ADD COLUMN source_mtime INTEGER;

-- A partial index, so the many admin-created rows (all NULL) do not collide.
CREATE UNIQUE INDEX IF NOT EXISTS idx_posts_source_path
    ON posts (source_path)
    WHERE source_path IS NOT NULL;
