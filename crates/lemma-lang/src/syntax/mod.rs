//! The syntax of `.lm` modules: tokens, the tree, and the parser that
//! builds one from the other.

pub mod ast;
pub mod lexer;
pub mod parser;

pub use parser::{Parsed, parse};
