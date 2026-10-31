//! 数据库连接(M01,见《M01》§2)。
//!
//! 一个专用写线程串行执行全部写入;读池只读查询。Tauri 异步命令必须
//! 经 `Db::write` / `Db::read`,不得直接持有连接。事务边界由业务服务
//! 在写闭包内用 `conn.transaction()` 显式开启。

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use rusqlite::Connection;
use thiserror::Error;

use crate::shared::{AppError, CoreErrorCode};

#[derive(Debug, Error)]
pub enum DbError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("write queue closed")]
    QueueClosed,
}

pub type DbResult<T> = Result<T, DbError>;

/// rusqlite 错误 → AppError。只保留稳定错误码,不外泄 SQL 原文。
pub fn db_app_error(err: &rusqlite::Error) -> AppError {
    let code = if is_corruption(err) {
        CoreErrorCode::DbCorrupt
    } else {
        CoreErrorCode::Internal
    };
    AppError::new(code, crate::shared::new_trace_id()).with_detail(
        "sqliteResultCode",
        err.sqlite_error_code()
            .map(|c| c as i64)
            .unwrap_or(0)
            .into(),
    )
}

fn is_corruption(err: &rusqlite::Error) -> bool {
    use rusqlite::ErrorCode::*;
    matches!(
        err,
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: DatabaseCorrupt | NotADatabase,
                ..
            },
            _,
        )
    )
}

type WriteJob = Box<dyn FnOnce(&mut Connection) + Send + 'static>;

/// 打开写连接并应用 M01 PRAGMA 基线。
pub fn open_write_connection(path: &Path) -> rusqlite::Result<Connection> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(path)?;
    apply_pragmas(&conn)?;
    Ok(conn)
}

fn apply_pragmas(conn: &Connection) -> rusqlite::Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.pragma_update(None, "cache_size", -8000)?;
    Ok(())
}

/// 数据库句柄:写线程 + 有界只读连接池。
pub struct Db {
    tx: Option<mpsc::SyncSender<WriteJob>>,
    writer: Option<std::thread::JoinHandle<()>>,
    readers: Vec<Arc<Mutex<Connection>>>,
    next_reader: AtomicUsize,
}

impl Db {
    /// 打开数据库:1 个写连接(独立线程)+ `read_pool_size` 个只读连接。
    pub fn open(path: &Path, read_pool_size: usize) -> DbResult<Self> {
        let writer_conn = open_write_connection(path)?;
        let (tx, rx) = mpsc::sync_channel::<WriteJob>(256);
        let writer = std::thread::Builder::new()
            .name("kw-db-writer".into())
            .spawn(move || {
                let mut conn = writer_conn;
                while let Ok(job) = rx.recv() {
                    job(&mut conn);
                }
            })
            .map_err(DbError::Io)?;

        let mut readers = Vec::with_capacity(read_pool_size.max(1));
        for _ in 0..read_pool_size.max(1) {
            let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
            let conn = Connection::open_with_flags(path, flags)?;
            conn.pragma_update(None, "foreign_keys", "ON")?;
            conn.pragma_update(None, "busy_timeout", 5000)?;
            readers.push(Arc::new(Mutex::new(conn)));
        }

        Ok(Self {
            tx: Some(tx),
            writer: Some(writer),
            readers,
            next_reader: AtomicUsize::new(0),
        })
    }

    /// 读池句柄(供异步上下文在 `spawn_blocking` 中使用)。
    pub fn read_handle(&self) -> ReadHandle {
        let idx = self.next_reader.fetch_add(1, Ordering::Relaxed) % self.readers.len();
        ReadHandle {
            conn: Arc::clone(&self.readers[idx]),
        }
    }

    /// 在写线程上执行闭包。闭包内自行决定事务边界;长任务必须拆批。
    /// 闭包错误原样返回;队列/线程错误映射为 INTERNAL(不应出现在正常路径)。
    pub fn write<T, F>(&self, f: F) -> Result<T, AppError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, AppError> + Send + 'static,
    {
        let Some(tx) = &self.tx else {
            return Err(AppError::internal("db-write-queue-closed"));
        };
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        let job: WriteJob = Box::new(move |conn| {
            let out = f(conn);
            let _ = ack_tx.send(out);
        });
        tx.send(job)
            .map_err(|_| AppError::internal("db-write-queue-closed"))?;
        ack_rx
            .recv()
            .map_err(|_| AppError::internal("db-write-queue-closed"))?
    }

    /// 在读池连接上执行只读查询。连接以只读方式打开,写入会被 SQLite 拒绝。
    pub fn read<T, F>(&self, f: F) -> Result<T, AppError>
    where
        F: FnOnce(&Connection) -> Result<T, AppError>,
    {
        self.read_handle().with(f)
    }
}

/// 只读连接租约。
pub struct ReadHandle {
    conn: Arc<Mutex<Connection>>,
}

impl ReadHandle {
    pub fn with<T, F>(&self, f: F) -> Result<T, AppError>
    where
        F: FnOnce(&Connection) -> Result<T, AppError>,
    {
        let conn = self
            .conn
            .lock()
            .map_err(|_| AppError::internal("db-read-lock-poisoned"))?;
        f(&conn)
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        // 先关闭发送端(写队列排空后线程退出),再回收线程。
        self.tx.take();
        if let Some(handle) = self.writer.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app<E: std::fmt::Display>(e: E) -> AppError {
        AppError::new(CoreErrorCode::Internal, crate::shared::new_trace_id())
            .with_detail("testError", e.to_string().into())
    }

    fn temp_db_path(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("kw-db-{}-{}", tag, std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("t.sqlite3")
    }

    #[test]
    fn write_and_read_roundtrip_with_wal() {
        let path = temp_db_path("rw");
        let _ = std::fs::remove_file(&path);
        let db = Db::open(&path, 2).unwrap();

        db.write(|conn| {
            conn.execute_batch("CREATE TABLE t(a INTEGER); INSERT INTO t VALUES (42);")
                .map_err(app)
        })
        .unwrap();

        let v = db
            .read(|conn| {
                conn.query_row("SELECT a FROM t", [], |r| r.get::<_, i64>(0))
                    .map_err(app)
            })
            .unwrap();
        assert_eq!(v, 42);

        let mode = db
            .read(|conn| {
                conn.query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
                    .map_err(app)
            })
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");

        // 外键约束生效。
        db.write(|conn| {
            conn.execute_batch("CREATE TABLE p(id INTEGER PRIMARY KEY); CREATE TABLE c(pid INTEGER REFERENCES p(id));")
                .map_err(app)
        })
        .unwrap();
        let fk_err = db.write(|conn| {
            conn.execute("INSERT INTO c(pid) VALUES (999)", [])
                .map_err(app)
        });
        assert!(fk_err.is_err(), "外键必须拦截悬空引用");
        drop(db);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn concurrent_writers_are_serialized() {
        let path = temp_db_path("conc");
        let _ = std::fs::remove_file(&path);
        let db = Arc::new(Db::open(&path, 2).unwrap());
        db.write(|conn| {
            conn.execute_batch("CREATE TABLE c(n INTEGER);")
                .map_err(app)
        })
        .unwrap();

        let mut handles = Vec::new();
        for i in 0..8 {
            let db = Arc::clone(&db);
            handles.push(std::thread::spawn(move || {
                db.write(move |conn| {
                    conn.execute("INSERT INTO c(n) VALUES (?1)", [i])
                        .map_err(app)
                })
                .unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let count = db
            .read(|conn| {
                conn.query_row("SELECT COUNT(*) FROM c", [], |r| r.get::<_, i64>(0))
                    .map_err(app)
            })
            .unwrap();
        assert_eq!(count, 8);
        drop(db);
        let _ = std::fs::remove_file(&path);
    }
}
