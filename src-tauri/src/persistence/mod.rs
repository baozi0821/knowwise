//! M01 持久化、任务与恢复。
//!
//! 提供连接与 PRAGMA、模块迁移执行器、Online Backup、事务上下文、
//! 任务状态机、取消传播、幂等结果提交和启动恢复。本模块不因此获得
//! 其他模块业务表的写入权;跨模块用例通过应用服务组合。

pub mod backup;
pub mod connection;
pub mod migrations;
pub mod registration;
pub mod task;

#[cfg(feature = "desktop")]
pub mod commands;

pub use connection::{db_app_error, Db, DbError, DbResult};
pub use migrations::{Migration, MigrationReport, MigrationRunner};
pub use task::{
    AttemptRow, AttemptStatus, CommitOutcome, CreateTask, RecoveryReport, TaskRow, TaskService,
};
