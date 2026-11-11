//! HTTP contract facts: operations from `OpenAPI` documents and route code,
//! and the calls consumers make, from TypeScript and Python source.
//!
//! Extractors report what the sources say; mapping those reports to a
//! grammar's relations, by each repository's role, is the caller's job.

mod aliases;
mod code;
mod openapi;
mod paths;
mod source;
mod tsconfig;

pub use aliases::implied_aliases;
pub use code::{
    ClientCall, CodeError, CodeFacts, CodeOptions, ImportBinding, Location, MemberCall, Mention,
    MethodPath, Mount, PathReference, Reexport, SchemaUnion, SchemaUse, extract, facts_language,
    facts_of,
};
pub use openapi::{Field, Operation, Parameter, is_yaml_document, operations};
pub use paths::{
    canonical_method, canonical_path, canonical_route, has_marker, is_test_file, path_markers,
};
pub use source::{
    ListedFile, Listings, SourceError, SourceFile, contents_key, is_under_any, list_files,
    list_with_blobs, read_file, read_files, read_files_where, read_from_disk, read_listed,
    read_paths,
};
pub use tsconfig::PathAliases;
