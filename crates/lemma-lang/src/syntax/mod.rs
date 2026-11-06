//! The syntax of `.lm` modules: tokens, the tree, the parser that builds
//! one from the other, and the formatter that lays a module out.

pub mod ast;
pub mod format;
pub mod lexer;
pub mod parser;

pub use format::{FormatError, format};
pub use parser::{Parsed, parse};
