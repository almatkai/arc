//! The pinned Arc extension reads Chromium history and downloads from Arc's User Data.
use rusqlite::{params_from_iter, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tezbar_extension_sdk::ExtensionContext;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct History {
    pub id: i64,
    pub title: String,
    pub url: String,
    pub time: String,
    pub profile: Option<String>,
    #[serde(skip)]
    timestamp: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Download {
    pub id: i64,
    pub path: String,
    pub url: String,
    pub time: String,
}

#[derive(Debug, Default)]
pub struct HistoryResults {
    pub rows: Vec<History>,
    pub warnings: Vec<String>,
}

fn entry(path: &Path, directory: bool) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(meta) if (directory && meta.is_dir()) || (!directory && meta.is_file()) => Ok(true),
        Ok(_) => Err("Arc data must be a regular file or directory".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err("Arc data could not be accessed. Check macOS file permissions.".into()),
    }
}

pub fn read_file(path: &Path, cap: usize) -> Result<Vec<u8>, String> {
    if !entry(path, false)? {
        return Err("Arc data file is missing".into());
    }
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(cap as u64 + 1).read_to_end(&mut bytes))
        .map_err(|_| "Arc data file could not be read")?;
    if bytes.len() > cap {
        return Err("Arc data file exceeds its size limit".into());
    }
    Ok(bytes)
}

fn user_data(base: &Path) -> Result<PathBuf, String> {
    if !entry(base, true)? || !entry(&base.join("User Data"), true)? {
        return Err(
            "Arc User Data is unavailable. Open Arc before searching its local records.".into(),
        );
    }
    Ok(base.join("User Data"))
}

fn profile_name(profile: &Path) -> Option<String> {
    let bytes = read_file(&profile.join("Preferences"), 2 * 1024 * 1024).ok()?;
    let settings: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    settings["profile"]["name"]
        .as_str()
        .filter(|name| !name.is_empty())
        .map(|name| name.chars().take(150).collect())
}

fn open_database(
    profile: &Path,
    cancel: Arc<AtomicBool>,
    deadline: Instant,
) -> Result<Connection, String> {
    let path = profile.join("History");
    if !entry(profile, true)? || !entry(&path, false)? {
        return Err("Arc History database is unavailable".into());
    }
    for suffix in ["History-wal", "History-shm"] {
        entry(&profile.join(suffix), false)?;
    }
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| "Arc History could not be opened read-only. Check macOS file permissions.")?;
    db.busy_timeout(Duration::from_millis(150))
        .map_err(|_| "Arc History could not be configured")?;
    db.progress_handler(
        2000,
        Some(move || cancel.load(Ordering::Relaxed) || Instant::now() >= deadline),
    );
    Ok(db)
}

fn predicate(query: &str, columns: [&str; 2]) -> (String, Vec<String>) {
    let values: Vec<String> = query
        .split_whitespace()
        .take(24)
        .map(|term| {
            format!(
                "%{}%",
                term.replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            )
        })
        .collect();
    let filter = values
        .iter()
        .enumerate()
        .map(|(index, _)| {
            let n = index + 1;
            format!(
                "({} LIKE ?{n} ESCAPE '\\' OR {} LIKE ?{n} ESCAPE '\\')",
                columns[0], columns[1]
            )
        })
        .collect::<Vec<_>>()
        .join(" AND ");
    (
        if filter.is_empty() {
            String::new()
        } else {
            format!("WHERE {filter}")
        },
        values,
    )
}

fn history_profile(
    db: &Connection,
    query: &str,
    limit: usize,
    profile: Option<&str>,
) -> Result<Vec<History>, String> {
    let (where_clause, terms) = predicate(query, ["url", "title"]);
    let sql = format!("SELECT id, substr(coalesce(title,''),1,750), CASE WHEN length(CAST(url AS BLOB)) <= 8192 THEN url ELSE '' END,
        coalesce(datetime(last_visit_time / 1000000 - 11644473600, 'unixepoch', 'localtime'), ''), last_visit_time
        FROM urls {where_clause} GROUP BY url HAVING last_visit_time = max(last_visit_time)
        ORDER BY last_visit_time DESC LIMIT {limit}");
    let mut statement = db
        .prepare(&sql)
        .map_err(|_| "Arc history schema is unavailable")?;
    let rows = statement
        .query_map(params_from_iter(terms), |row| {
            Ok(History {
                id: row.get(0)?,
                title: row.get(1)?,
                url: row.get(2)?,
                time: row.get(3)?,
                timestamp: row.get(4)?,
                profile: profile.map(str::to_owned),
            })
        })
        .map_err(|_| "Arc history query failed")?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|_| "Arc history is unavailable or busy. Try refreshing.".into())
}

fn history(
    base: &Path,
    query: &str,
    per_profile: usize,
    cancel: Arc<AtomicBool>,
) -> Result<HistoryResults, String> {
    let data = user_data(base)?;
    let mut profiles = Vec::new();
    for number in 0..5 {
        let id = if number == 0 {
            "Default".into()
        } else {
            format!("Profile {number}")
        };
        let path = data.join(&id);
        if number > 0 && (!entry(&path, true)? || !entry(&path.join("History"), false)?) {
            break;
        }
        let label = if entry(&path, true)? {
            profile_name(&path).unwrap_or(id)
        } else {
            id
        };
        if label != "__ARC_SYSTEM_PROFILE" {
            profiles.push((path, label));
        }
    }
    let mut result = HistoryResults::default();
    let deadline = Instant::now() + Duration::from_secs(3);
    for (path, name) in &profiles {
        if cancel.load(Ordering::Relaxed) {
            return Err("Operation cancelled".into());
        }
        let label = (profiles.len() > 1).then_some(name.as_str());
        match open_database(path, cancel.clone(), deadline)
            .and_then(|db| history_profile(&db, query, per_profile, label))
        {
            Ok(rows) => result.rows.extend(rows),
            Err(error) => result.warnings.push(format!("{name}: {error}")),
        }
    }
    result.rows.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    Ok(result)
}

fn downloads(base: &Path, query: &str, cancel: Arc<AtomicBool>) -> Result<Vec<Download>, String> {
    let profile = user_data(base)?.join("Default");
    let db = open_database(&profile, cancel, Instant::now() + Duration::from_secs(3))?;
    let (where_clause, terms) = predicate(query, ["current_path", "tab_url"]);
    let sql = format!(
        "SELECT id, CASE WHEN length(CAST(current_path AS BLOB)) <= 4096 THEN current_path ELSE '' END,
        CASE WHEN length(CAST(tab_url AS BLOB)) <= 8192 THEN tab_url ELSE '' END,
        coalesce(datetime(end_time / 1000000 - 11644473600, 'unixepoch', 'localtime'), '')
        FROM downloads {where_clause} ORDER BY id DESC LIMIT 100"
    );
    let mut statement = db
        .prepare(&sql)
        .map_err(|_| "Arc download schema is unavailable")?;
    let rows = statement
        .query_map(params_from_iter(terms), |row| {
            Ok(Download {
                id: row.get(0)?,
                path: row.get(1)?,
                url: row.get(2)?,
                time: row.get(3)?,
            })
        })
        .map_err(|_| "Arc downloads query failed")?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|_| "Arc downloads are unavailable or busy. Try refreshing.".into())
}

struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

pub async fn query_db<T: Send + 'static>(
    ctx: &ExtensionContext,
    query: impl FnOnce(Arc<AtomicBool>) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let token = Arc::new(AtomicBool::new(false));
    let _cancel = CancelOnDrop(token.clone());
    let task = tokio::task::spawn_blocking(move || query(token));
    tokio::select! {
        biased;
        _ = ctx.cancelled() => Err("Operation cancelled".into()),
        result = task => result.map_err(|_| "Arc data worker exited unexpectedly")?,
    }
}

pub async fn query_history(
    ctx: &ExtensionContext,
    path: PathBuf,
    query: String,
    per_profile: usize,
) -> Result<HistoryResults, String> {
    query_db(ctx, move |cancel| {
        history(&path, &query, per_profile, cancel)
    })
    .await
}

pub async fn query_downloads(
    ctx: &ExtensionContext,
    path: PathBuf,
    query: String,
) -> Result<Vec<Download>, String> {
    query_db(ctx, move |cancel| downloads(&path, &query, cancel)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_percent_underscore_quote_and_backslash() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE urls (id INTEGER, title TEXT, url TEXT, last_visit_time INTEGER);
          INSERT INTO urls VALUES (1, '100% O''Reilly', 'https://a.example/a_b', 11644473601000000);
          INSERT INTO urls VALUES (2, '1000 O''Reilly', 'https://a.example/axb', 11644473602000000)").unwrap();
        assert_eq!(
            history_profile(&db, "100% O'Reilly a_b", 20, None).unwrap()[0].id,
            1
        );
        assert!(history_profile(&db, "' OR 1=1 --", 20, None)
            .unwrap()
            .is_empty());
        assert!(history_profile(&db, r"a\b", 20, None).unwrap().is_empty());
        assert_eq!(history_profile(&db, "", 20, None).unwrap().len(), 2);
    }
}
