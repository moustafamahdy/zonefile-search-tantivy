use rusqlite::{Connection, OpenFlags};
use std::collections::HashMap;
use std::sync::Mutex;

/// Read-only SQLite connection for precheck lookups.
pub struct PrecheckDb {
    conn: Mutex<Connection>,
}

impl PrecheckDb {
    pub fn open(path: &str) -> anyhow::Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.execute_batch("PRAGMA cache_size = -8192;")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Batch lookup reachability for a list of domains.
    /// Returns a map of domain -> is_reachable.
    /// Domains found in the table are reachable (true).
    /// Domains NOT in the table are assumed unreachable (false).
    pub fn batch_lookup(&self, domains: &[&str]) -> HashMap<String, bool> {
        let mut result = HashMap::new();

        // Default all to false (unreachable)
        for domain in domains {
            result.insert(domain.to_string(), false);
        }

        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return result,
        };

        let mut stmt = match conn.prepare_cached(
            "SELECT reachable FROM domain_precheck WHERE domain = ?1",
        ) {
            Ok(s) => s,
            Err(_) => return result,
        };

        for domain in domains {
            if let Ok(reachable) = stmt.query_row(rusqlite::params![domain], |row| {
                row.get::<_, i64>(0)
            }) {
                result.insert(domain.to_string(), reachable == 1);
            }
        }

        result
    }
}
