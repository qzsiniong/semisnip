//! 检索：查询解析、拼音索引、打分排序。
//!
//! 这一层**不依赖 gpui**，全是纯函数，可以直接单测。三个子模块：
//! - [`query`]：把输入切成词元，并抽掉 trigger 的前缀符号；
//! - [`pinyin`]：中文标签的全拼/首字母索引（面板打不出汉字，靠它补）；
//! - [`score`]：打分与全序排序，输入相同则输出顺序相同。

pub mod pinyin;
pub mod query;
pub mod score;

pub use query::Query;
pub use score::{SearchIndex, rank};
