//! 取手の解釈（parse）と、現物との照合（resolve）。
//!
//! parseは文字列だけを見る。resolveはworkspaceのファイルとgitを読むが、何も書かない。

pub mod capture;
pub mod proc;
pub mod parse;
pub mod resolve;

pub use parse::parse;
pub use resolve::Resolver;
