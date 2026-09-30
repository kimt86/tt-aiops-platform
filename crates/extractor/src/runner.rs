//! Runs a SQL statement against Oracle via the `remote-toolbox-sql` skill.
//! This module is the ONLY path to production Oracle. Calls are serialized by a
//! process-wide async lock so two queries never hit Oracle concurrently.

use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use tokio::sync::Mutex;

/// Serializes all Oracle access for the lifetime of the process.
static ORACLE_LOCK: Mutex<()> = Mutex::const_new(());

pub struct Toolbox {
    skill_dir: PathBuf,
    target: String,
    timeout_secs: u64,
}

impl Toolbox {
    /// `target` is e.g. "oracle-prod" / "oracle-uat".
    ///
    /// The script lives in THIS repo (`tools/oracle-toolbox/scripts/remote-toolbox-sql`), found
    /// relative to the binary (`<repo>/target/release/extractor`). Until 2026-09-30 the default was
    /// another person's Codex skill file (`/home/aiadmin/.codex/skills/yard-db-ops`) — every Oracle
    /// poll depended on a file we did not own, pointed at a relay host that was being retired.
    /// `SKILL_DIR` still overrides (any dir holding `scripts/remote-toolbox-sql`).
    pub fn from_env(target: &str) -> Result<Self> {
        let skill_dir = match std::env::var("SKILL_DIR") {
            Ok(d) => PathBuf::from(d),
            Err(_) => std::env::current_exe()
                .context("locating extractor binary")?
                .ancestors()
                .nth(3) // extractor -> release -> target -> <repo>
                .context("extractor binary is not under <repo>/target/<profile>/")?
                .join("tools/oracle-toolbox"),
        };
        let script = skill_dir.join("scripts/remote-toolbox-sql");
        if !script.exists() {
            bail!("remote-toolbox-sql not found at {}", script.display());
        }
        Ok(Self {
            skill_dir,
            target: target.to_string(),
            timeout_secs: 90,
        })
    }

    fn script(&self) -> PathBuf {
        self.skill_dir.join("scripts/remote-toolbox-sql")
    }

    /// Execute `sql` and return raw stdout (the `{"result":"..."}` envelope).
    /// The SQL is passed via a temp file (`--file`) to avoid shell-escape damage.
    pub async fn run_sql(&self, sql: &str) -> Result<String> {
        let _guard = ORACLE_LOCK.lock().await; // serialize Oracle access

        // Write SQL to a scratch file the script can read — on DISK, not std::env::temp_dir().
        // That default is /tmp, which on this host is a 124GB tmpfs: RAM. On 2026-08-02 it filled
        // and every Oracle poll failed on this exact write, taking QC_MOVE, RTG_MOVE and
        // HANDOVER_LABEL down together — the critical move streams, killed by a few-KB file that had
        // no business competing for the scarcest resource on the box. Commit 5a24c7e moved the
        // roadgraph dumps off the same tmpfs (ROADSCRATCH -> /var/tmp/roadscratch) and missed this.
        // Growth is bounded by construction: one file per pid, overwritten on reuse, removed below.
        let dir = std::path::PathBuf::from(
            std::env::var("TT_SQL_SCRATCH").unwrap_or_else(|_| "/var/tmp/tt-sql".into()),
        );
        let _ = tokio::fs::create_dir_all(&dir).await;
        let path = dir.join(format!("wp-extract-{}.sql", std::process::id()));
        tokio::fs::write(&path, sql)
            .await
            .with_context(|| format!("writing temp SQL to {}", path.display()))?;

        let out = tokio::process::Command::new(self.script())
            .arg(&self.target)
            .arg("--file")
            .arg(&path)
            .arg("--timeout")
            .arg(self.timeout_secs.to_string())
            .output()
            .await
            .context("spawning remote-toolbox-sql")?;

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
