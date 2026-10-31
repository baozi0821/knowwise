//! 模块迁移执行器(M01,见《M01》§3、《01》§12)。
//!
//! 每个模块在自己的 `src-tauri/migrations/mXX/` 下拥有 SQL 文件,迁移身份
//! 为 `(module, version)`。执行器内嵌 SQL、计算 SHA-256 校验和并写入
//! `schema_migration`;已应用迁移的校验和变化直接阻止启动。
//! 只前滚,不提供 down migration。

use std::path::Path;

use rusqlite::Connection;
use thiserror::Error;

use super::backup;
use crate::shared::textnorm::sha256_hex;

/// 单个迁移。SQL 由模块 registration.rs `include_str!` 提供。
#[derive(Debug, Clone)]
pub struct Migration {
    pub module: &'static str,
    pub version: i64,
    pub name: &'static str,
    pub sql: &'static str,
}

impl Migration {
    /// LF 规范化后的 SQL;校验和只基于这一形式,不受 checkout 行尾影响。
    fn normalized_sql(&self) -> String {
        self.sql.replace("\r\n", "\n")
    }

    pub fn checksum(&self) -> String {
        sha256_hex(self.normalized_sql().as_bytes())
    }

    /// 兼容历史数据库中的校验和:
    /// 1) 规范化 LF(本版本起写入的新格式);
    /// 2) 原始嵌入文本(旧 LF 检出构建);
    /// 3) 全部转成 CRLF(旧 Windows CI 检出构建)。
    pub fn accepts_checksum(&self, applied: &str) -> bool {
        if applied == self.checksum() {
            return true;
        }
        if applied == sha256_hex(self.sql.as_bytes()) {
            return true;
        }
        let crlf = self.normalized_sql().replace('\n', "\r\n");
        applied == sha256_hex(crlf.as_bytes())
    }
}

#[derive(Debug, Error)]
pub enum MigrationError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("module {module} v{version} checksum mismatch: applied={applied}, current={current}")]
    ChecksumMismatch {
        module: String,
        version: i64,
        applied: String,
        current: String,
    },
    #[error("database has applied migrations unknown to this build: {0:?}")]
    UnknownAppliedMigration(Vec<String>),
    #[error("backup failed: {0}")]
    Backup(String),
    #[error("foreign key check failed after {module} v{version}: {details}")]
    ForeignKeyCheck {
        module: String,
        version: i64,
        details: String,
    },
}

/// schema 版本高于应用:任何已应用但未注册的迁移都拒绝继续。
fn ensure_no_unknown_applied(conn: &Connection, sets: &[Migration]) -> Result<(), MigrationError> {
    use std::collections::HashSet;
    let known: HashSet<(String, i64)> = sets
        .iter()
        .map(|m| (m.module.to_string(), m.version))
        .collect();
    let mut stmt = conn.prepare("SELECT module, version FROM schema_migration")?;
    let unknown: Vec<String> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
        .filter_map(|row| row.ok())
        .filter(|(module, version)| !known.contains(&(module.clone(), *version)))
        .map(|(module, version)| format!("{module} v{version}"))
        .collect();
    drop(stmt);
    if unknown.is_empty() {
        Ok(())
    } else {
        Err(MigrationError::UnknownAppliedMigration(unknown))
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct MigrationReport {
    pub applied: Vec<(String, i64)>,
    pub skipped_already_applied: usize,
    pub backup_path: Option<std::path::PathBuf>,
}

/// 迁移运行器:先备份(若有待应用迁移),再逐条在事务中执行。
pub struct MigrationRunner {
    pub backups_dir: std::path::PathBuf,
    pub keep_backups: usize,
}

impl MigrationRunner {
    pub fn new(backups_dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            backups_dir: backups_dir.into(),
            keep_backups: 8,
        }
    }

    /// 执行迁移(幂等)。`sets` 必须由 M13 按依赖顺序注册。
    pub fn run(
        &self,
        conn: &mut Connection,
        sets: &[Migration],
        // 备份目标由 backups_dir 决定;db_path 仅保留给调用方日志对齐,不参与写入。
        _db_path: &Path,
        now_ms: i64,
    ) -> Result<MigrationReport, MigrationError> {
        ensure_schema_migration_table(conn)?;
        let mut report = MigrationReport::default();

        // 数据库存在应用未注册的迁移(schema 版本高于应用)→ 拒绝写入,
        // 绝不降级破坏数据(《M01》§8)。
        ensure_no_unknown_applied(conn, sets)?;

        // 校验已应用迁移的校验和;收集待应用项。
        let mut pending: Vec<&Migration> = Vec::new();
        for m in sets {
            match applied_checksum(conn, m)? {
                Some(checksum) => {
                    if !m.accepts_checksum(&checksum) {
                        return Err(MigrationError::ChecksumMismatch {
                            module: m.module.to_string(),
                            version: m.version,
                            applied: checksum,
                            current: m.checksum(),
                        });
                    }
                    report.skipped_already_applied += 1;
                }
                None => pending.push(m),
            }
        }

        if pending.is_empty() {
            return Ok(report);
        }

        // 结构迁移前必须 Online Backup,不能复制 WAL 写入中的文件。
        let backup_info = backup::backup_database(conn, &self.backups_dir, now_ms)
            .map_err(|e| MigrationError::Backup(e.to_string()))?;
        record_backup_meta(conn, &backup_info, now_ms)?;
        backup::prune_old_backups(&self.backups_dir, self.keep_backups)?;
        report.backup_path = Some(backup_info.path.clone());

        for m in pending {
            apply_one(conn, m, now_ms)?;
            report.applied.push((m.module.to_string(), m.version));
        }
        Ok(report)
    }
}

fn ensure_schema_migration_table(conn: &Connection) -> rusqlite::Result<()> {
    // 两张执行器元数据表在首个迁移前创建;m01 的 0001_init.sql 以
    // IF NOT EXISTS 重复声明,保证从任意历史形态启动都一致。
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migration (
            module TEXT NOT NULL,
            version INTEGER NOT NULL,
            checksum TEXT NOT NULL,
            applied_at INTEGER NOT NULL,
            PRIMARY KEY(module, version)
        ) WITHOUT ROWID;
        CREATE TABLE IF NOT EXISTS migration_backup (
            id TEXT PRIMARY KEY,
            file_name TEXT NOT NULL,
            size_bytes INTEGER NOT NULL,
            checksum TEXT NOT NULL,
            created_at INTEGER NOT NULL
        );",
    )
}

fn applied_checksum(conn: &Connection, m: &Migration) -> rusqlite::Result<Option<String>> {
    let mut stmt =
        conn.prepare("SELECT checksum FROM schema_migration WHERE module=?1 AND version=?2")?;
    let mut rows = stmt.query(rusqlite::params![m.module, m.version])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

fn apply_one(conn: &mut Connection, m: &Migration, now_ms: i64) -> Result<(), MigrationError> {
    // 少数迁移必须重建被外键引用的表(SQLite 无法修改 CHECK)。标记只在
    // 首行注释声明;事务外关闭外键,提交后立即 foreign_key_check,失败即报错。
    let fk_off = m
        .sql
        .lines()
        .take(3)
        .any(|line| line.trim() == "--! foreign_keys_off");
    if fk_off {
        conn.pragma_update(None, "foreign_keys", "OFF")?;
    }
    let result = apply_in_tx(conn, m, now_ms);
    if fk_off {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        if result.is_ok() {
            let violations = foreign_key_violations(conn)?;
            if !violations.is_empty() {
                return Err(MigrationError::ForeignKeyCheck {
                    module: m.module.to_string(),
                    version: m.version,
                    details: violations.join("; "),
                });
            }
        }
    }
    result
}

fn apply_in_tx(conn: &mut Connection, m: &Migration, now_ms: i64) -> Result<(), MigrationError> {
    let tx = conn.transaction()?;
    tx.execute_batch(m.sql)?;
    tx.execute(
        "INSERT INTO schema_migration(module, version, checksum, applied_at) VALUES (?1,?2,?3,?4)",
        rusqlite::params![m.module, m.version, m.checksum(), now_ms],
    )?;
    tx.commit()?;
    Ok(())
}

fn foreign_key_violations(conn: &Connection) -> Result<Vec<String>, MigrationError> {
    let mut stmt =
        conn.prepare("SELECT \"table\", rowid, \"parent\", fkid FROM pragma_foreign_key_check")?;
    let rows = stmt
        .query_map([], |r| {
            Ok(format!(
                "{} rowid={:?} -> {} fk={}",
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn record_backup_meta(
    conn: &mut Connection,
    info: &backup::BackupInfo,
    now_ms: i64,
) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    tx.execute(
        "INSERT INTO migration_backup(id, file_name, size_bytes, checksum, created_at)
         VALUES (?1,?2,?3,?4,?5)",
        rusqlite::params![
            uuid::Uuid::now_v7().to_string(),
            info.path.file_name().unwrap_or_default().to_string_lossy(),
            info.size_bytes,
            info.checksum,
            now_ms
        ],
    )?;
    tx.commit()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m01() -> Vec<Migration> {
        vec![Migration {
            module: "m01",
            version: 1,
            name: "init",
            sql: include_str!("../../migrations/m01/0001_init.sql"),
        }]
    }

    fn temp_path(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("kw-mig-{}-{}", tag, std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn fresh_db_applies_once_and_is_idempotent() {
        let dir = temp_path("fresh");
        let db_path = dir.join("a.sqlite3");
        let _ = std::fs::remove_file(&db_path);
        let mut conn = Connection::open(&db_path).unwrap();
        let runner = MigrationRunner::new(dir.join("backups"));

        let r1 = runner.run(&mut conn, &m01(), &db_path, 1000).unwrap();
        assert_eq!(r1.applied.len(), 1);
        assert!(r1.backup_path.is_some(), "首次迁移前必须备份");

        let r2 = runner.run(&mut conn, &m01(), &db_path, 2000).unwrap();
        assert!(r2.applied.is_empty());
        assert!(r2.backup_path.is_none(), "无待应用迁移不备份");
        assert_eq!(r2.skipped_already_applied, 1);

        // 表已存在。
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='task'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn checksum_tamper_blocks_startup() {
        // M01-A1:已应用迁移的校验和被篡改时必须拒绝。
        let dir = temp_path("tamper");
        let db_path = dir.join("b.sqlite3");
        let _ = std::fs::remove_file(&db_path);
        let mut conn = Connection::open(&db_path).unwrap();
        let runner = MigrationRunner::new(dir.join("backups"));
        runner.run(&mut conn, &m01(), &db_path, 1000).unwrap();

        let mutated = vec![Migration {
            module: "m01",
            version: 1,
            name: "init",
            sql: "CREATE TABLE extra(x INTEGER);",
        }];
        let err = runner.run(&mut conn, &mutated, &db_path, 2000).unwrap_err();
        assert!(
            matches!(err, MigrationError::ChecksumMismatch { .. }),
            "got {err:?}"
        );
        // 篡改的 SQL 未被执行。
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='extra'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_migration_leaves_old_state_recoverable() {
        // M01-A1:迁移 SQL 中途失败时整体回滚,不产生半套结构。
        let dir = temp_path("fail");
        let db_path = dir.join("c.sqlite3");
        let _ = std::fs::remove_file(&db_path);
        let mut conn = Connection::open(&db_path).unwrap();
        let runner = MigrationRunner::new(dir.join("backups"));
        runner.run(&mut conn, &m01(), &db_path, 1000).unwrap();

        let bad = vec![Migration {
            module: "m02",
            version: 1,
            name: "bad",
            // 第二条语句违反约束:验证迁移在事务中整体回滚。
            sql: "CREATE TABLE half(x INTEGER UNIQUE); INSERT INTO half VALUES (1); INSERT INTO half VALUES (1);",
        }];
        assert!(runner.run(&mut conn, &bad, &db_path, 2000).is_err());
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='half'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "失败迁移必须整体回滚");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn foreign_keys_off_migration_can_rebuild_referenced_table() {
        // 2D:m21 需要重建被外键引用的 content_block;提交后必须无悬挂引用。
        let dir = temp_path("fk-rebuild");
        let db_path = dir.join("e.sqlite3");
        let _ = std::fs::remove_file(&db_path);
        let mut conn = Connection::open(&db_path).unwrap();
        let runner = MigrationRunner::new(dir.join("backups"));
        let base = vec![Migration {
            module: "t0",
            version: 1,
            name: "base",
            sql: "CREATE TABLE parent(id TEXT PRIMARY KEY);\
                  CREATE TABLE content_block(id TEXT PRIMARY KEY, \
                    parent_id TEXT NOT NULL REFERENCES parent(id), \
                    origin TEXT NOT NULL CHECK(origin IN ('parser','on_demand')), \
                    text TEXT);\
                  CREATE TABLE child(id TEXT PRIMARY KEY, \
                    block_id TEXT NOT NULL REFERENCES content_block(id));\
                  INSERT INTO parent VALUES('p1');\
                  INSERT INTO content_block VALUES('b1','p1','parser','one');\
                  INSERT INTO child VALUES('c1','b1');",
        }];
        runner.run(&mut conn, &base, &db_path, 1000).unwrap();

        let mut sets = base.clone();
        sets.push(Migration {
            module: "t1",
            version: 1,
            name: "rebuild",
            sql: "--! foreign_keys_off\n\
                  CREATE TABLE content_block_new(id TEXT PRIMARY KEY, \
                    parent_id TEXT NOT NULL REFERENCES parent(id), \
                    origin TEXT NOT NULL CHECK(origin IN ('parser','on_demand','ocr')), \
                    text TEXT, confidence REAL);\
                  INSERT INTO content_block_new(id,parent_id,origin,text,confidence) \
                    SELECT id,parent_id,origin,text,NULL FROM content_block;\
                  DROP TABLE content_block;\
                  ALTER TABLE content_block_new RENAME TO content_block;",
        });
        runner.run(&mut conn, &sets, &db_path, 2000).unwrap();

        conn.execute(
            "INSERT INTO content_block(id,parent_id,origin,text,confidence) \
             VALUES('b2','p1','ocr','two',0.9)",
            [],
        )
        .unwrap();
        let child_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM child", [], |r| r.get(0))
            .unwrap();
        assert_eq!(child_count, 1, "重建后既有引用必须保留");
        let fk_on: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk_on, 1, "迁移结束后必须恢复外键约束");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn schema_higher_than_build_is_rejected() {
        // 《M01》§8:数据库存在本构建未注册的迁移 → 拒绝,绝不降级。
        let dir = temp_path("higher");
        let db_path = dir.join("d.sqlite3");
        let _ = std::fs::remove_file(&db_path);
        let mut conn = Connection::open(&db_path).unwrap();
        let runner = MigrationRunner::new(dir.join("backups"));
        runner.run(&mut conn, &m01(), &db_path, 1000).unwrap();

        // 模拟未来版本写入的迁移记录。
        conn.execute(
            "INSERT INTO schema_migration(module, version, checksum, applied_at) VALUES ('m99', 1, 'x', 1)",
            [],
        )
        .unwrap();

        let err = runner.run(&mut conn, &m01(), &db_path, 2000).unwrap_err();
        assert!(
            matches!(err, MigrationError::UnknownAppliedMigration(ref list) if list.len() == 1),
            "got {err:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    const LF_SQL: &str = "CREATE TABLE a(x INTEGER);\nCREATE TABLE b(y TEXT);\n";
    const CRLF_SQL: &str = "CREATE TABLE a(x INTEGER);\r\nCREATE TABLE b(y TEXT);\r\n";

    fn migration_with(sql: &'static str) -> Migration {
        Migration {
            module: "t",
            version: 1,
            name: "line-endings",
            sql,
        }
    }

    #[test]
    fn checksum_is_line_ending_independent() {
        // 同一份 SQL 以 LF/CRLF 嵌入时必须得到同一个校验和。
        assert_eq!(
            migration_with(LF_SQL).checksum(),
            migration_with(CRLF_SQL).checksum()
        );
    }

    #[test]
    fn accepts_legacy_lf_and_crlf_checksums() {
        let lf = migration_with(LF_SQL);
        let crlf = migration_with(CRLF_SQL);
        let legacy_lf = sha256_hex(LF_SQL.as_bytes());
        let legacy_crlf = sha256_hex(CRLF_SQL.as_bytes());

        for m in [&lf, &crlf] {
            assert!(m.accepts_checksum(&legacy_lf), "旧 LF 校验和必须被接受");
            assert!(m.accepts_checksum(&legacy_crlf), "旧 CRLF 校验和必须被接受");
            assert!(m.accepts_checksum(&m.checksum()));
        }
        assert!(
            !lf.accepts_checksum(&sha256_hex(
                b"CREATE TABLE a(x INTEGER);\nCREATE TABLE c(z BLOB);\n"
            )),
            "SQL 内容真正变化时不得接受"
        );
    }

    #[test]
    fn runner_accepts_databases_with_legacy_checksums() {
        // v0.1.0 的 LF 构建与 v0.1.1 Windows CRLF 构建写下的记录都要能继续启动。
        let lf_sql = m01()[0].sql.replace("\r\n", "\n");
        let lf_checksum = sha256_hex(lf_sql.as_bytes());
        let crlf_checksum = sha256_hex(lf_sql.replace('\n', "\r\n").as_bytes());
        assert_ne!(lf_checksum, crlf_checksum);

        for (tag, legacy) in [("legacy-lf", &lf_checksum), ("legacy-crlf", &crlf_checksum)] {
            let dir = temp_path(tag);
            let db_path = dir.join("legacy.sqlite3");
            let _ = std::fs::remove_file(&db_path);
            let mut conn = Connection::open(&db_path).unwrap();
            let runner = MigrationRunner::new(dir.join("backups"));
            runner.run(&mut conn, &m01(), &db_path, 1000).unwrap();

            conn.execute(
                "UPDATE schema_migration SET checksum=?1 WHERE module='m01' AND version=1",
                rusqlite::params![legacy.as_str()],
            )
            .unwrap();

            let report = runner.run(&mut conn, &m01(), &db_path, 2000).unwrap();
            assert!(report.applied.is_empty(), "{tag}: 不得重复执行迁移");
            assert_eq!(report.skipped_already_applied, 1, "{tag}");
            let rows: i64 = conn
                .query_row("SELECT COUNT(*) FROM schema_migration", [], |r| r.get(0))
                .unwrap();
            assert_eq!(rows, 1, "{tag}: 不得重复写入迁移记录");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn new_migrations_record_normalized_checksum() {
        // 即使构建时嵌入的是 CRLF,写库的也必须是稳定的 LF 校验和。
        const APPLY_SQL: &str =
            "CREATE TABLE line_end_test(x INTEGER);\r\nINSERT INTO line_end_test VALUES (1);\r\n";
        let dir = temp_path("record-normalized");
        let db_path = dir.join("record.sqlite3");
        let _ = std::fs::remove_file(&db_path);
        let mut conn = Connection::open(&db_path).unwrap();
        let runner = MigrationRunner::new(dir.join("backups"));
        let sets = vec![migration_with(APPLY_SQL)];
        runner.run(&mut conn, &sets, &db_path, 1000).unwrap();

        let recorded: String = conn
            .query_row(
                "SELECT checksum FROM schema_migration WHERE module='t' AND version=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(recorded, sets[0].checksum());
        assert_eq!(
            recorded,
            sha256_hex(APPLY_SQL.replace("\r\n", "\n").as_bytes()),
            "新写入的校验和必须是规范化 LF 值"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fresh_db_applies_all_registered_migrations() {
        let sets = crate::app_shell::all_migrations();
        assert_eq!(sets.len(), 27, "迁移文件与注册数量必须一致");

        let dir = temp_path("all-migrations");
        let db_path = dir.join("all.sqlite3");
        let _ = std::fs::remove_file(&db_path);
        let mut conn = Connection::open(&db_path).unwrap();
        let runner = MigrationRunner::new(dir.join("backups"));
        let report = runner.run(&mut conn, &sets, &db_path, 1000).unwrap();
        assert_eq!(report.applied.len(), 27);
        assert!(report.backup_path.is_some());

        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migration", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 27);

        let again = runner.run(&mut conn, &sets, &db_path, 2000).unwrap();
        assert!(again.applied.is_empty());
        assert_eq!(again.skipped_already_applied, 27);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
