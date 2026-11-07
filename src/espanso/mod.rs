//! espanso CLI 封装。
//!
//! semisnip 不自己展开片段：求值（日期、表单、shell）、注入、消歧都交给 espanso，
//! 我们只负责"选"。这里只有三个能力——`match list`、`match exec`、`--version`。

pub mod cli;
