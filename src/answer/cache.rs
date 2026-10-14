//! Answer cache: an in-memory LRU, optionally persisted as an append-only JSON Lines file.
//!
//! Courses repeat questions heavily (retakes, several accounts, chapter quizzes reused in exams),
//! so caching avoids paying for the same question twice.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use lru::LruCache;
use serde::{Deserialize, Serialize};

use crate::config::CacheConfig;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CachedAnswer {
    pub key: String,
    pub question: String,
    pub qtype: String,
    pub answer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    pub model: String,
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    pub created_at: u64,
}

pub struct AnswerCache {
    entries: Mutex<LruCache<String, CachedAnswer>>,
    ttl_secs: u64,
    file: Option<Mutex<File>>,
    path: Option<PathBuf>,
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl AnswerCache {
    /// Loads the persisted entries (later lines win) and opens the file for appending.
    pub fn open(cfg: &CacheConfig) -> std::io::Result<Self> {
        let capacity = NonZeroUsize::new(cfg.capacity.max(1)).expect("capacity >= 1");
        let mut entries = LruCache::new(capacity);
        let path = (!cfg.file.trim().is_empty()).then(|| PathBuf::from(cfg.file.trim()));
        let mut file = None;
        if let Some(path) = &path {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent)?;
            }
            let loaded = load_file(path, &mut entries, cfg.ttl_secs)?;
            if loaded > 0 {
                tracing::info!(path = %path.display(), entries = loaded, "已加载答案缓存");
            }
            file = Some(Mutex::new(
                OpenOptions::new().create(true).append(true).open(path)?,
            ));
        }
        Ok(Self {
            entries: Mutex::new(entries),
            ttl_secs: cfg.ttl_secs,
            file,
            path,
        })
    }

    pub fn get(&self, key: &str) -> Option<CachedAnswer> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let hit = entries.get(key)?.clone();
        if self.expired(&hit) {
            entries.pop(key);
            return None;
        }
        Some(hit)
    }

    pub fn insert(&self, entry: CachedAnswer) {
        if let Some(file) = &self.file
            && let Ok(line) = serde_json::to_string(&entry)
        {
            let mut f = file.lock().unwrap_or_else(|e| e.into_inner());
            if let Err(e) = writeln!(f, "{line}") {
                tracing::warn!(error = %e, "写入答案缓存文件失败");
            }
        }
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .put(entry.key.clone(), entry);
    }

    pub fn remove(&self, key: &str) -> bool {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop(key)
            .is_some()
    }

    pub fn len(&self) -> usize {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Clears memory and truncates the persisted file.
    pub fn clear(&self) -> std::io::Result<()> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        if let (Some(path), Some(file)) = (&self.path, &self.file) {
            let mut f = file.lock().unwrap_or_else(|e| e.into_inner());
            *f = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(path)?;
            *f = OpenOptions::new().append(true).open(path)?;
        }
        Ok(())
    }

    fn expired(&self, e: &CachedAnswer) -> bool {
        self.ttl_secs > 0 && now_secs().saturating_sub(e.created_at) > self.ttl_secs
    }
}

fn load_file(
    path: &Path,
    entries: &mut LruCache<String, CachedAnswer>,
    ttl_secs: u64,
) -> std::io::Result<usize> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let now = now_secs();
    let mut loaded = 0;
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        // A torn last line after a crash must not prevent startup.
        let Ok(entry) = serde_json::from_str::<CachedAnswer>(&line) else {
            continue;
        };
        if ttl_secs > 0 && now.saturating_sub(entry.created_at) > ttl_secs {
            continue;
        }
        entries.put(entry.key.clone(), entry);
        loaded += 1;
    }
    Ok(loaded)
}
