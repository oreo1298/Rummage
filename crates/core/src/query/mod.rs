//! The search language: parsing, compilation and evaluation.

pub mod ast;
pub mod compile;
pub mod matcher;
pub mod parse;
pub mod value;

pub use ast::{KindOnly, Mods, Node};
pub use compile::{
    CompileContext, Compiled, Macros, NumericIds, Pred, QueryError, SearchOptions, UserDb,
    compile_str,
};
pub use parse::{ParseOptions, normalize, parse};
pub use value::Clock;

#[cfg(test)]
mod tests;
