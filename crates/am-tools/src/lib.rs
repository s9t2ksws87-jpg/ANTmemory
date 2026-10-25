//! ANTmemory v4 のtool層。MCPの1行を受け取り、storeへの命令と表示を作る。
//!
//! 通信（stdio・named pipe）とプロセスの管理は `am-cli` が持つ。ここはI/Oの経路を知らない。

pub mod anchorview;
pub mod ctx;
pub mod goals;
pub mod hooks;
pub mod mcp;
pub mod render;
pub mod search;
pub mod shared_context;
pub mod terms;
pub mod tools;
pub mod useful;
pub mod utter;
pub mod workspace;

pub use ctx::{Level, Session, Workspace};
