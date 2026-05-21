//! Single-shot Python script execution with JSON over stdin/stdout.
//!
//! Mirror of [`crate::openhuman::runtime_node::execute`]. See that
//! module's header for the design rationale; this file applies the same
//! contract to the managed (or system) Python runtime so skills with a
//! `.py` entrypoint can be invoked through [`crate::openhuman::tools`]
//! alongside their JS counterparts.
//!
//! ## Wire contract
//!
//! ```text
//! invoker -> stdin  -> { "args": <user-args-json>, "meta": {...} }
//! script  -> stdout -> { "ok": true|false, "result"|"error": <json> }
//! script  -> stderr -> free-form diagnostics (forwarded to logs only)
//! ```
//!
//! A minimal Python entrypoint looks like:
//!
//! ```python
//! import json, sys
//! payload = json.loads(sys.stdin.read())
//! print(json.dumps({"ok": True, "result": {"echo": payload["args"]}}))
//! ```
//!
//! ## What's NOT here
//!
//! - Stronger sandbox than OS process isolation — same trust model as
//!   the Node primitive.
//! - Streaming results — single stdout JSON object then exit.
//! - Per-call PYTHONPATH manipulation beyond inheriting the parent. If
//!   a skill needs vendored libs it ships them in its `scripts/`
//!   directory and uses relative imports rooted at the cwd we pin.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time;

use super::bootstrap::ResolvedPython;

const LOG_PREFIX: &str = "[runtime_python::execute]";

/// Caller-tunable knobs for a single Python script invocation.
#[derive(Debug, Clone)]
pub struct ExecuteOptions {
    pub cwd: PathBuf,
    pub env: HashMap<String, String>,
    pub timeout: Option<Duration>,
    /// Optional address-space cap, applied via `setrlimit(RLIMIT_AS)`
    /// on Unix. Ignored on Windows for now — JobObject support is a
    /// follow-up. `None` means "no cap" (process inherits the user's
    /// limit). Set to `Some(N)` to enforce a hard upper bound on the
    /// child's virtual memory; the kernel kills the process when it
    /// allocates past `N` bytes, which surfaces as a non-zero exit
    /// code (typically `137` / SIGKILL).
    pub memory_limit_bytes: Option<u64>,
}

impl Default for ExecuteOptions {
    fn default() -> Self {
        Self {
            cwd: PathBuf::from("."),
            env: HashMap::new(),
            timeout: Some(Duration::from_secs(30)),
            memory_limit_bytes: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteOutcome {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub elapsed_ms: u64,
    pub timed_out: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ExecuteError {
    #[error("[runtime_python::execute] script path does not exist: {0}")]
    ScriptNotFound(PathBuf),
    #[error("[runtime_python::execute] working directory does not exist: {0}")]
    CwdNotFound(PathBuf),
    #[error("[runtime_python::execute] failed to spawn python {bin}: {source}")]
    Spawn {
        bin: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("[runtime_python::execute] failed to write stdin: {0}")]
    StdinWrite(#[source] std::io::Error),
    #[error("[runtime_python::execute] failed to read child output: {0}")]
    ChildIo(#[source] std::io::Error),
}

/// Spawn `python -u <script_path>` with the given stdin JSON, cwd, env,
/// and timeout. Returns an [`ExecuteOutcome`] regardless of the script's
/// exit code — the caller decides what to do with it.
pub async fn execute_script(
    resolved: &ResolvedPython,
    script_path: &Path,
    stdin_payload: &serde_json::Value,
    opts: &ExecuteOptions,
) -> Result<ExecuteOutcome, ExecuteError> {
    if !script_path.exists() {
        return Err(ExecuteError::ScriptNotFound(script_path.to_path_buf()));
    }
    if !opts.cwd.exists() {
        return Err(ExecuteError::CwdNotFound(opts.cwd.clone()));
    }

    let started = Instant::now();
    let stdin_bytes = serde_json::to_vec(stdin_payload).unwrap_or_else(|_| b"{}".to_vec());

    log::debug!(
        "{LOG_PREFIX} spawn python={} script={} cwd={} stdin_bytes={} timeout_ms={:?} memory_limit_bytes={:?}",
        resolved.python_bin.display(),
        script_path.display(),
        opts.cwd.display(),
        stdin_bytes.len(),
        opts.timeout.map(|d| d.as_millis()),
        opts.memory_limit_bytes,
    );

    // ResolvedPython doesn't carry a `bin_dir` field (Node's does);
    // derive it from `python_bin.parent()`. Falls back to "." if the
    // binary somehow has no parent path — shouldn't happen in practice
    // since python_bin is always absolute.
    let bin_dir = resolved
        .python_bin
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    let mut cmd = Command::new(&resolved.python_bin);
    cmd.arg("-u")
        .arg(script_path)
        .current_dir(&opts.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Match runtime_node: prepend the runtime's bin dir to PATH so
        // any subprocess the script spawns picks up the same Python.
        .env(
            "PATH",
            prepend_path(&bin_dir, std::env::var("PATH").ok().as_deref()),
        );
    for (k, v) in &opts.env {
        cmd.env(k, v);
    }
    cmd.kill_on_drop(true);

    // Memory cap via setrlimit(RLIMIT_AS) — Unix only. The pre_exec
    // closure runs after fork() but before exec(), so the limit
    // applies to the child without affecting the parent.
    #[cfg(unix)]
    if let Some(limit) = opts.memory_limit_bytes {
        // Unsafe wrapper around libc::setrlimit. The closure is `Send +
        // Sync + 'static` because it captures only the limit by copy.
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(move || apply_rlimit_as(limit));
        }
    }

    let mut child = cmd.spawn().map_err(|e| ExecuteError::Spawn {
        bin: resolved.python_bin.clone(),
        source: e,
    })?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(&stdin_bytes)
            .await
            .map_err(ExecuteError::StdinWrite)?;
        stdin.shutdown().await.map_err(ExecuteError::StdinWrite)?;
    }

    let result = if let Some(deadline) = opts.timeout {
        match time::timeout(deadline, child.wait_with_output()).await {
            Ok(inner) => inner.map(Some),
            Err(_) => {
                log::warn!(
                    "{LOG_PREFIX} timeout after {}ms — killing python script={}",
                    deadline.as_millis(),
                    script_path.display()
                );
                return Ok(ExecuteOutcome {
                    stdout: String::new(),
                    stderr: format!(
                        "{LOG_PREFIX} killed: exceeded {}ms timeout",
                        deadline.as_millis()
                    ),
                    exit_code: None,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                    timed_out: true,
                });
            }
        }
    } else {
        child.wait_with_output().await.map(Some)
    };

    let output = result.map_err(ExecuteError::ChildIo)?.expect(
        "wait_with_output returned Ok(None) — shouldn't happen, we map success to Some above",
    );
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let exit_code = output.status.code();

    log::debug!(
        "{LOG_PREFIX} done script={} elapsed_ms={} exit_code={:?} stdout_bytes={} stderr_bytes={}",
        script_path.display(),
        elapsed_ms,
        exit_code,
        stdout.len(),
        stderr.len(),
    );

    Ok(ExecuteOutcome {
        stdout,
        stderr,
        exit_code,
        elapsed_ms,
        timed_out: false,
    })
}

/// `setrlimit(RLIMIT_AS, { rlim_cur: limit, rlim_max: limit })`. Called
/// from `pre_exec` so the limit applies to the child after `fork()` and
/// before `execve()`. Returns `io::Result<()>` so a failure aborts the
/// spawn cleanly instead of running the script with the parent's limit.
#[cfg(unix)]
fn apply_rlimit_as(limit_bytes: u64) -> std::io::Result<()> {
    let rl = libc::rlimit {
        rlim_cur: limit_bytes as libc::rlim_t,
        rlim_max: limit_bytes as libc::rlim_t,
    };
    let rc = unsafe { libc::setrlimit(libc::RLIMIT_AS, &rl) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn prepend_path(bin_dir: &Path, current: Option<&str>) -> String {
    let bin = bin_dir.to_string_lossy().to_string();
    match current {
        Some(p) if !p.is_empty() => {
            #[cfg(windows)]
            let sep = ";";
            #[cfg(not(windows))]
            let sep = ":";
            format!("{bin}{sep}{p}")
        }
        _ => bin,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openhuman::runtime_python::bootstrap::PythonSource;

    fn system_python_or_skip(test_name: &str) -> Option<ResolvedPython> {
        for candidate in ["python3", "python"] {
            let which_out = std::process::Command::new("which")
                .arg(candidate)
                .output()
                .ok()?;
            if !which_out.status.success() {
                continue;
            }
            let path = String::from_utf8_lossy(&which_out.stdout)
                .trim()
                .to_string();
            if path.is_empty() {
                continue;
            }
            let python_bin = PathBuf::from(&path);
            return Some(ResolvedPython {
                python_bin,
                version: "system".to_string(),
                source: PythonSource::System,
            });
        }
        log::info!("{LOG_PREFIX} test={test_name} skipped: no system python on PATH");
        None
    }

    fn write_fixture_script(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("fixture.py");
        std::fs::write(&path, body).expect("write fixture script");
        path
    }

    #[tokio::test]
    async fn script_not_found_returns_typed_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let resolved = ResolvedPython {
            python_bin: tmp.path().join("does-not-matter"),
            version: "test".to_string(),
            source: PythonSource::System,
        };
        let opts = ExecuteOptions {
            cwd: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let err = execute_script(
            &resolved,
            &tmp.path().join("missing.py"),
            &serde_json::json!({}),
            &opts,
        )
        .await
        .err()
        .expect("missing script must error");
        assert!(matches!(err, ExecuteError::ScriptNotFound(_)));
    }

    #[tokio::test]
    async fn cwd_not_found_returns_typed_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let script = write_fixture_script(tmp.path(), "import sys; sys.exit(0)");
        let resolved = ResolvedPython {
            python_bin: tmp.path().join("does-not-matter"),
            version: "test".to_string(),
            source: PythonSource::System,
        };
        let opts = ExecuteOptions {
            cwd: tmp.path().join("nope"),
            ..Default::default()
        };
        let err = execute_script(&resolved, &script, &serde_json::json!({}), &opts)
            .await
            .err()
            .expect("missing cwd must error");
        assert!(matches!(err, ExecuteError::CwdNotFound(_)));
    }

    #[tokio::test]
    async fn happy_path_echoes_stdin_json() {
        let Some(resolved) = system_python_or_skip("happy_path_echoes_stdin_json") else {
            return;
        };
        let tmp = tempfile::tempdir().expect("tempdir");
        let script = write_fixture_script(
            tmp.path(),
            r#"
import json, sys
payload = json.loads(sys.stdin.read())
print(json.dumps({"ok": True, "echo": payload["args"]}))
"#,
        );
        let opts = ExecuteOptions {
            cwd: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let payload = serde_json::json!({ "args": { "hello": "world" } });
        let outcome = execute_script(&resolved, &script, &payload, &opts)
            .await
            .expect("execute should succeed");
        assert!(!outcome.timed_out);
        assert_eq!(outcome.exit_code, Some(0));
        let parsed: serde_json::Value = serde_json::from_str(outcome.stdout.trim())
            .expect("script must emit valid JSON on stdout");
        assert_eq!(parsed["ok"], serde_json::json!(true));
        assert_eq!(parsed["echo"], serde_json::json!({"hello": "world"}));
    }

    #[tokio::test]
    async fn timeout_kills_long_running_script() {
        let Some(resolved) = system_python_or_skip("timeout_kills_long_running_script") else {
            return;
        };
        let tmp = tempfile::tempdir().expect("tempdir");
        let script = write_fixture_script(
            tmp.path(),
            r#"
import sys, time
sys.stdin.read()
time.sleep(30)
"#,
        );
        let opts = ExecuteOptions {
            cwd: tmp.path().to_path_buf(),
            timeout: Some(Duration::from_millis(200)),
            ..Default::default()
        };
        let outcome = execute_script(&resolved, &script, &serde_json::json!({}), &opts)
            .await
            .expect("execute should succeed even on timeout");
        assert!(outcome.timed_out, "should have timed out");
        assert!(outcome.exit_code.is_none());
        assert!(outcome.stderr.contains("timeout"));
    }

    #[tokio::test]
    async fn non_zero_exit_returns_outcome_not_error() {
        let Some(resolved) = system_python_or_skip("non_zero_exit_returns_outcome_not_error")
        else {
            return;
        };
        let tmp = tempfile::tempdir().expect("tempdir");
        let script = write_fixture_script(
            tmp.path(),
            r#"
import json, sys
sys.stdin.read()
print(json.dumps({"ok": False, "error": "boom"}))
sys.exit(2)
"#,
        );
        let opts = ExecuteOptions {
            cwd: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let outcome = execute_script(&resolved, &script, &serde_json::json!({}), &opts)
            .await
            .expect("non-zero exit is not a primitive-level error");
        assert!(!outcome.timed_out);
        assert_eq!(outcome.exit_code, Some(2));
        assert!(outcome.stdout.contains("\"error\":") && outcome.stdout.contains("boom"));
    }

    #[tokio::test]
    async fn stderr_is_captured_separately_from_stdout() {
        let Some(resolved) = system_python_or_skip("stderr_is_captured_separately_from_stdout")
        else {
            return;
        };
        let tmp = tempfile::tempdir().expect("tempdir");
        let script = write_fixture_script(
            tmp.path(),
            r#"
import json, sys
sys.stdin.read()
sys.stderr.write("diagnostic line\n")
print(json.dumps({"ok": True}))
"#,
        );
        let opts = ExecuteOptions {
            cwd: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let outcome = execute_script(&resolved, &script, &serde_json::json!({}), &opts)
            .await
            .expect("execute should succeed");
        assert!(outcome.stderr.contains("diagnostic line"));
        assert!(!outcome.stdout.contains("diagnostic line"));
    }
}
