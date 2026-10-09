use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use parking_lot::RwLock;

const DEFAULT_CAPACITY: usize = 500;

#[derive(Clone)]
pub struct LogEntry {
    pub level: u8,
    pub message: String,
    pub timestamp_ms: u64,
}

/// Per-session ring buffer for console log entries.
pub struct ConsoleLogBuffer {
    entries: Vec<LogEntry>,
    /// Next write position (wraps around).
    write_pos: usize,
    /// Total entries ever written (monotonic).
    total_written: u64,
    capacity: usize,
}

impl ConsoleLogBuffer {
    fn new(capacity: usize) -> Self {
        Self {
            entries: Vec::with_capacity(capacity),
            write_pos: 0,
            total_written: 0,
            capacity,
        }
    }

    pub fn push(&mut self, level: u8, message: String) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let entry = LogEntry {
            level,
            message,
            timestamp_ms: ts,
        };

        if self.entries.len() < self.capacity {
            self.entries.push(entry);
        } else {
            self.entries[self.write_pos] = entry;
        }
        self.write_pos = (self.write_pos + 1) % self.capacity;
        self.total_written += 1;
    }

    /// Read entries written after `since_index` (exclusive).
    /// Returns (entries, new_cursor) where new_cursor should be passed as
    /// `since_index` on the next call.
    pub fn read_since(&self, since_index: u64) -> (Vec<LogEntry>, u64) {
        if self.total_written <= since_index {
            return (Vec::new(), self.total_written);
        }

        let available = self.entries.len() as u64;
        // How many new entries since the cursor?
        let new_count = self.total_written - since_index;
        // Clamp to available (if cursor is too old, some entries were overwritten)
        let actual_count = new_count.min(available) as usize;

        let mut result = Vec::with_capacity(actual_count);

        // Read in chronological order starting from oldest unread
        let start = if self.entries.len() < self.capacity {
            // Buffer not yet full: entries are 0..len, oldest unread is at (total - actual_count)
            (self.total_written - actual_count as u64) as usize
        } else {
            // Buffer full and wrapping: write_pos points to oldest.
            // We want the last `actual_count` entries before write_pos.
            (self.write_pos + self.capacity - actual_count) % self.capacity
        };

        for i in 0..actual_count {
            let idx = (start + i) % self.capacity;
            result.push(self.entries[idx].clone());
        }

        (result, self.total_written)
    }
}

/// Global per-session console log storage.
///
/// # Lock ordering (must be followed to prevent deadlocks)
///
/// **Outer lock**: `parking_lot::RwLock` on the `HashMap<session_id, Arc<Mutex<Buffer>>>`.
///   - Acquired briefly to look up or insert/remove a session's buffer.
///   - Read-locked for lookups (`get_console_log`, `push_console_log`).
///   - Write-locked only for session registration/unregistration.
///
/// **Inner lock**: `std::sync::Mutex` on individual `ConsoleLogBuffer`.
///   - Acquired after the outer RwLock is released (the `Arc` clone is taken
///     under the read lock, then the RwLock is dropped before locking the Mutex).
///   - Protects per-session buffer reads and writes.
///
/// The two locks are never held simultaneously: callers always clone the
/// `Arc<Mutex<..>>` while holding the outer RwLock, release the RwLock, and
/// only then lock the inner Mutex.  This makes deadlocks impossible as long as
/// this ordering is preserved.  Do NOT lock the inner Mutex while holding the
/// outer RwLock.
static CONSOLE_LOGS: OnceLock<RwLock<HashMap<i32, Arc<Mutex<ConsoleLogBuffer>>>>> = OnceLock::new();

fn log_map() -> &'static RwLock<HashMap<i32, Arc<Mutex<ConsoleLogBuffer>>>> {
    CONSOLE_LOGS.get_or_init(|| RwLock::new(HashMap::new()))
}

pub fn register_console_log(id: i32) -> Arc<Mutex<ConsoleLogBuffer>> {
    let buf = Arc::new(Mutex::new(ConsoleLogBuffer::new(DEFAULT_CAPACITY)));
    log_map().write().insert(id, buf.clone());
    buf
}

pub fn unregister_console_log(id: i32) {
    log_map().write().remove(&id);
}

pub fn get_console_log(id: i32) -> Option<Arc<Mutex<ConsoleLogBuffer>>> {
    log_map().read().get(&id).cloned()
}

/// Hand the registry's own lock to the cross-session contention gate.
///
/// The op that writes console output must hold the buffer it resolved at isolate
/// bring-up, never look the session up again -- this map is shared with every
/// other Session, and its writers are their bring-up and teardown. Behind a
/// feature no shipped build enables, because the per-event path lives in another
/// crate and cannot hold a private lock.
#[cfg(any(test, feature = "contention-probe"))]
pub fn console_registry_lock_for_contention_probe()
-> &'static RwLock<HashMap<i32, Arc<Mutex<ConsoleLogBuffer>>>> {
    log_map()
}

/// Push a console log entry for the given session.
/// Returns false if the session is not registered (non-debug mode).
pub fn push_console_log(id: i32, level: u8, message: String) -> bool {
    if let Some(buf) = get_console_log(id) {
        if let Ok(mut b) = buf.lock() {
            b.push(level, message);
            return true;
        }
    }
    false
}

/// Read console log entries since the given cursor.
/// Returns JSON string: `{"logs":[{"l":level,"m":"msg","t":timestamp},...], "cursor": N}`
pub fn read_console_logs_json(id: i32, since_cursor: u64) -> Option<String> {
    let buf = get_console_log(id)?;
    let b = buf.lock().ok()?;
    let (entries, new_cursor) = b.read_since(since_cursor);

    let mut json = String::with_capacity(entries.len() * 80 + 32);
    json.push_str("{\"logs\":[");
    for (i, entry) in entries.iter().enumerate() {
        if i > 0 {
            json.push(',');
        }
        json.push_str("{\"l\":");
        json.push_str(&entry.level.to_string());
        json.push_str(",\"t\":");
        json.push_str(&entry.timestamp_ms.to_string());
        json.push_str(",\"m\":");
        // JSON-escape the message
        json.push('"');
        for c in entry.message.chars() {
            match c {
                '"' => json.push_str("\\\""),
                '\\' => json.push_str("\\\\"),
                '\n' => json.push_str("\\n"),
                '\r' => json.push_str("\\r"),
                '\t' => json.push_str("\\t"),
                c if (c as u32) < 0x20 => {
                    json.push_str(&format!("\\u{:04x}", c as u32));
                }
                c => json.push(c),
            }
        }
        json.push('"');
        json.push('}');
    }
    json.push_str("],\"cursor\":");
    json.push_str(&new_cursor.to_string());
    json.push('}');

    Some(json)
}
