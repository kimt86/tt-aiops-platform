//! Isolated Oracle gateway (mirrors `extractor::runner::Toolbox`). scengen keeps its OWN
//! copy so it shares ZERO Rust code with the critical extractor.
//!
//! The default timeout is SHORTER than the critical path (45s vs 90s): scenario work must
//! never hold Oracle for long.
//!
//! SERIALIZATION — what is and is not guaranteed (this used to be documented incorrectly):
//!  * in-process `ORACLE_LOCK` covers concurrent calls inside ONE scengen process;
//!  * scengen's collectors run as SEPARATE systemd oneshot processes (collect/yard/enrich), whose
//!    timers overlap, so the in-process mutex alone does NOT serialize them — hence the `flock(1)`
//!    wrapper below, which does;
//!  * the `remote-toolbox-sql` script itself does NOT lock (verified: no flock/lockfile in it), so
//!    scengen is NOT yet serialized against the critical extractor. Closing that would require the
//!    extractor to take the same lock file — a critical-path change deliberately not made here.
//!    Impact is small while every query is an index seek, but do not assume mutual exclusion.

use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use tokio::sync::Mutex;

/// Serializes all Oracle access for the lifetime of this (non-critical) process.
static ORACLE_LOCK: Mutex<()> = Mutex::const_new(());

pub struct Toolbox {
    skill_dir: PathBuf,
    target: String,
    timeout_secs: u64,
}

impl Toolbox {
    /// `target` e.g. "oracle-prod"; `timeout_secs` should stay small (~45s) for isolation.
    pub fn from_env(target: &str, timeout_secs: u64) -> Result<Self> {
        let skill_dir = std::env::var("SKILL_DIR")
            .unwrap_or_else(|_| "/home/aiadmin/.codex/skills/yard-db-ops".to_string());
        let skill_dir = PathBuf::from(skill_dir);
        let script = skill_dir.join("scripts/remote-toolbox-sql");
        if !script.exists() {
            bail!("remote-toolbox-sql not found at {}", script.display());
        }
        Ok(Self {
            skill_dir,
            target: target.to_string(),
            timeout_secs,
        })
    }

    fn script(&self) -> PathBuf {
        self.skill_dir.join("scripts/remote-toolbox-sql")
    }

    /// Execute `sql` and return raw stdout (the `{"result":"..."}` envelope). SQL is passed
    /// via a temp file (`--file`) to avoid shell-escape damage.
    pub async fn run_sql(&self, sql: &str) -> Result<String> {
        let _guard = ORACLE_LOCK.lock().await; // serialize Oracle access

        let dir = std::env::temp_dir();
        let path = dir.join(format!("scengen-{}.sql", std::process::id()));
        tokio::fs::write(&path, sql)
            .await
            .with_context(|| format!("writing temp SQL to {}", path.display()))?;

        // Cross-PROCESS serialization: scengen's collectors are separate processes with overlapping
        // timers, so run the script under flock(1) on a shared lock file. Waiting a little longer
        // than our own query timeout lets an in-flight sibling finish rather than failing early; if
        // the wait elapses flock exits non-zero and the tick is recorded as failed (isolated).
        let lock_file = std::env::var("ORACLE_LOCK_FILE")
            .unwrap_or_else(|_| "/tmp/wp-oracle-toolbox.lock".to_string());
        let lock_wait = self.timeout_secs + 15;

        let out = tokio::process::Command::new("flock")
            .arg("-w")
            .arg(lock_wait.to_string())
            .arg(&lock_file)
            .arg(self.script())
            .arg(&self.target)
            .arg("--file")
            .arg(&path)
            .arg("--timeout")
            .arg(self.timeout_secs.to_string())
            .output()
            .await
            .context("spawning remote-toolbox-sql under flock")?;

        let _ = tokio::fs::remove_file(&path).await;

        if !out.status.success() {
            bail!(
                "remote-toolbox-sql failed (status {:?}): {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(String::from_utf8(out.stdout).context("toolbox stdout was not UTF-8")?)
    }
}
