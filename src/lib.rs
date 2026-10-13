//! tblogs — a small blog split into two programs.
//!
//! * `tblogs-admin` is the **local authoring client**. It reads a directory of
//!   Markdown files, builds a self-contained content package (a SQLite database
//!   plus a mirror of the referenced assets), generates a deployable server
//!   directory, and publishes the result over SSH.
//!
//! * `tblogs-server` is the **remote server**. It serves that package over HTTP
//!   as a read-only site: no authoring, no write path, no accounts. When a new
//!   package lands it reloads it without a restart.
//!
//! Everything shared between them lives in this library: the schema and
//! queries, the Markdown and asset handling, and the templates.

pub mod assets;
pub mod bundle;
pub mod config;
pub mod content;
pub mod db;
pub mod error;
pub mod export;
pub mod frontmatter;
pub mod markdown;
pub mod models;
pub mod package;
pub mod pages;
pub mod publish;
pub mod routes;
pub mod server;
pub mod slug;
pub mod state;
pub mod templates;
pub mod util;
