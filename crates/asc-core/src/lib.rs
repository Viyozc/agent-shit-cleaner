//! agent-shit-cleaner core：规则引擎、并行扫描器、聚合报表。
//! 只读：本 crate 不做任何写入/删除。

pub mod clean;
pub mod config;
pub mod discover;
pub mod fmt;
pub mod never;
pub mod ports;
pub mod report;
pub mod rule;
pub mod scanner;
pub mod walk;

pub use config::Config;
pub use report::{HomeReport, ScanBundle, WorkspaceReport};
pub use rule::{builtin_rules, load_rules, Category, Risk, Rule, Scope};
pub use scanner::{scan_home, scan_workspace, WorkspaceOpts};
pub use ports::{list_listeners, service_label, PortInfo};
