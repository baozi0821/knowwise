//! SQLite Online Backup(M01,见《M01》§3、《01》§12)。
//! 结构迁移前生成可恢复副本;不直接复制仍在 WAL 写入中的数据库文件。

use std::path::{Path, PathBuf};

use rusqlite::backup::{Backup, StepResult};
use rusqlite::Connection;

use crate::shared::textnorm::sha256_hex;

#[derive(Debug, Clone)]
pub struct BackupInfo {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub checksum: String,
}

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// 用 Online Backup API 备份到 `backups_dir/knowwise-<now_ms>.sqlite3`。
pub fn backup_database(
    conn: &Connection,
    backups_dir: &Path,
    now_ms: i64,
) -> Result<BackupInfo, BackupError> {
    std::fs::create_dir_all(backups_dir)?;
    let path = backups_dir.join(format!("knowwise-{now_ms}.sqlite3"));
    let mut dst = Connection::open(&path)?;
    let backup = Backup::new(conn, &mut dst)?;
    loop {
        match backup.step(256)? {
            StepResult::Done => break,
            StepResult::More | StepResult::Busy | StepResult::Locked => continue,
            _ => continue, // StepResult 为 non_exhaustive,防御未来新增变体。
        }
    }
    // 校验和基于文件字节;备份必须能被独立打开(M01-A8 在测试中断言)。
    let bytes = std::fs::read(&path)?;
    let checksum = sha256_hex(&bytes);
    Ok(BackupInfo {
        path,
        size_bytes: bytes.len() as u64,
        checksum,
    })
}

/// 只保留最近 `keep` 份备份(按文件名时间戳排序)。
pub fn prune_old_backups(backups_dir: &Path, keep: usize) -> std::io::Result<()> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(backups_dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().starts_with("knowwise-"))
                .unwrap_or(false)
        })
        .collect();
    files.sort(); // 文件名含时间戳,字典序即时间序。
    if files.len() > keep {
        for old in &files[..files.len() - keep] {
            let _ = std::fs::remove_file(old);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_is_openable_and_passes_integrity_check() {
        // M01-A8:备份可被独立打开并通过完整性检查。
        let dir = std::env::temp_dir().join(format!("kw-backup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src_path = dir.join("src.sqlite3");
        let conn = Connection::open(&src_path).unwrap();
        conn.execute_batch("CREATE TABLE x(a INTEGER); INSERT INTO x VALUES (7);")
            .unwrap();

        let info = backup_database(&conn, &dir.join("backups"), 12345).unwrap();
        assert!(info.path.exists());
        assert!(info.size_bytes > 0);
        assert_eq!(info.checksum.len(), 64);

        let restored = Connection::open(&info.path).unwrap();
        let v: i64 = restored
            .query_row("SELECT a FROM x", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 7);
        let ok: String = restored
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ok, "ok");
        drop(restored);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_keeps_recent_only() {
        let dir = std::env::temp_dir().join(format!("kw-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for ts in [1000u64, 2000, 3000, 4000] {
            std::fs::write(dir.join(format!("knowwise-{ts}.sqlite3")), b"x").unwrap();
        }
        std::fs::write(dir.join("other.txt"), b"y").unwrap();
        prune_old_backups(&dir, 2).unwrap();
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "knowwise-3000.sqlite3",
                "knowwise-4000.sqlite3",
                "other.txt"
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
