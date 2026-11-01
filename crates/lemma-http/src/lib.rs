//! HTTP contract facts: operations from `OpenAPI` documents and route code,
//! and the calls consumers make, from TypeScript and Python source.
//!
//! Extractors report what the sources say; mapping those reports to a
//! grammar's relations, by each repository's role, is the caller's job.

mod code;
mod openapi;
mod paths;
mod source;

pub use code::{
    ClientCall, CodeError, CodeFacts, CodeOptions, Location, Mention, MethodPath, PathReference,
    extract,
};
pub use openapi::{Field, Operation, Parameter, operations};
pub use paths::{canonical_method, canonical_path};
pub use source::{SourceError, SourceFile, read_file, read_files};
