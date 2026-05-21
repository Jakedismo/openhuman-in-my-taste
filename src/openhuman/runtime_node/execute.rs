//! Spawn a Node.js script against the managed (or system) runtime and
//! exchange JSON over stdin/stdout.
//!
//! This is the **primitive** that the skill-invocation tool (and any
//! future Node-driven feature) builds on. It is intentionally narrow:
//!
//! - **Single-shot**: one `execute_script` call spawns one Node process,
//!   writes a single JSON blob to stdin, reads stdout to completion, and
//!   returns. No streaming. No persistent worker pool. Streaming back to
//!   the caller would let long-running skills emit incremental progress
//!   to the UI, but the agent loop doesn't have a streaming-result
//!   surface yet, so adding one here would be a feature with no consumer.
//! - **No sandbox beyond OS process isolation**: the script inherits the
//!   user's filesystem and network. Single-user local desktop trust model
//!   — the user installed the skill, the user owns the box, this is the
//!   same threat model as running any `npm script`. A proper sandbox
//!   (bubblewrap / seatbelt / JobObject) would be additive and is tracked
//!   as a follow-up.
//! - **Process timeout via `tokio::time::timeout`**: hard kill at the
//!   deadline. Memory caps require platform-specific syscalls (rlimit on
//!   Unix, JobObject on Windows) — not in this primitive.
//! - **Working directory pinned**: the caller picks the cwd (usually the
//!   skill's install directory) so relative imports + bundled resources
//!   resolve consistently regardless of where the host process was
//!   launched from.
//!
//! ## Wire contract
//!
//! ```text
//! invoker -> stdin  -> { "args": <user-args-json>, "meta": {...} }
//! script  -> stdout -> { "ok": true|false, "result"|"error": <json> }
//! script  -> stderr -> free-form diagnostics (forwarded to logs only)
//! ```
//!
//! Scripts are expected to read one JSON object from stdin (a single
//! `JSON.parse(await readAll(process.stdin))` works), do their work, and
//! print exactly one JSON object on stdout before exiting. Anything they
//! emit on stderr is captured and logged, never parsed.
//!
//! ## Log prefix
//!
//! `[runtime_node::execute]` so end-to-end greps for a single skill
//! invocation stay coherent.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time;

use super::bootstrap::ResolvedNode;

const LOG_PREFIX: &str = "[runtime_node::execute]";

/// Caller-tunable knobs for a single script invocation.
#[derive(Debug, Clone)]
pub struct ExecuteOptions {
    /// Working directory for the spawned process. Usually the skill's
    /// install directory so relative `require` / `import` and bundled
    /// resource paths resolve cleanly. Required — there's no sensible
    /// host-wide default for skill execution.
    pub cwd: PathBuf,
    /// Additional environment variables to set on the child process.
    /// Inherits the parent environment by default; entries here override
    /// (or add to) it. Use sparingly — most skills should not need any.
    pub env: HashMap<String, String>,
    /// Hard wall-clock deadline. The process is killed if it has not
    /// exited by then. `None` means "no timeout" — use sparingly, only
    /// for tests or interactive debugging where a hang is acceptable.
    pub timeout: Option<Duration>,
}

impl Default for ExecuteOptions {
    fn default() -> Self {
        Self {
            cwd: PathBuf::from("."),
            env: HashMap::new(),
            // 30s default matches the longest interactive tool turn the
            // agent loop typically waits for. Skills that need longer
            // should override explicitly.
            timeout: Some(Duration::from_secs(30)),
        }
    }
}

/// Outcome of one script invocation. The caller decides whether
/// `exit_code != 0` is fatal — some skills use a non-zero exit to signal
/// a user-visible error condition while still emitting a parseable
/// `{ "ok": false, "error": "..." }` blob on stdout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteOutcome {
    /// Raw stdout bytes as UTF-8. Scripts are expected to print a single
    /// JSON object here, but the primitive doesn't enforce that — the
    /// invocation layer (SkillInvokeTool) parses + validates.
    pub stdout: String,
    /// Captured stderr. Always returned, never parsed; the invocation
    /// layer forwards this to the agent logs for diagnostics.
    pub stderr: String,
    /// Process exit code, or `None` if the process was killed by the
    /// timeout.
    pub exit_code: Option<i32>,
    /// Wall-clock duration in milliseconds.
    pub elapsed_ms: u64,
    /// `true` when the process was killed because it exceeded the
    /// configured `timeout`. Mutually exclusive with a populated
    /// `exit_code`.
    pub timed_out: bool,
}

/// Errors that prevent a script from running at all. Distinct from a
/// script that ran and exited non-zero — that's surfaced through
/// [`ExecuteOutcome`] with the relevant fields populated.
#[derive(Debug, thiserror::Error)]
pub enum ExecuteError {
    #[error("[runtime_node::execute] script path does not exist: {0}")]
    ScriptNotFound(PathBuf),
    #[error("[runtime_node::execute] working directory does not exist: {0}")]
    CwdNotFound(PathBuf),
    #[error("[runtime_node::execute] failed to spawn node {bin}: {source}")]
    Spawn {
        bin: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("[runtime_node::execute] failed to write stdin: {0}")]
    StdinWrite(#[source] std::io::Error),
    #[error("[runtime_node::execute] failed to read child output: {0}")]
    ChildIo(#[source] std::io::Error),
}

/// Spawn `node <script_path>` with the given stdin JSON, the configured
/// cwd + env, and a wall-clock timeout.
///
/// Returns an [`ExecuteOutcome`] regardless of the script's exit code —
/// the caller decides what to do with it. The `Err` arm is reserved for
/// errors that prevent the process from running at all (script missing,
/// spawn failed, stdin write failed).
///
/// ## Cancellation
///
/// On timeout the child is killed via `tokio::process::Child::kill`
/// (SIGKILL on Unix, `TerminateProcess` on Windows). The function
/// returns immediately after the kill — it does not wait for the OS to
/// reap the process tree.
pub async fn execute_script(
    resolved: &ResolvedNode,
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
        "{LOG_PREFIX} spawn node={} script={} cwd={} stdin_bytes={} timeout_ms={:?}",
        resolved.node_bin.display(),
        script_path.display(),
        opts.cwd.display(),
        stdin_bytes.len(),
        opts.timeout.map(|d| d.as_millis()),
    );

    let mut cmd = Command::new(&resolved.node_bin);
    cmd.arg(script_path)
        .current_dir(&opts.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Prepend the resolved runtime's bin dir to PATH so child
        // processes the script may itself spawn (npm scripts, npx
        // shims) pick up the same node version.
        .env(
            "PATH",
            prepend_path(&resolved.bin_dir, std::env::var("PATH").ok().as_deref()),
        );
    for (k, v) in &opts.env {
        cmd.env(k, v);
    }
    // Kill the child if the parent dies — this matches the Tauri shell's
    // expectation that all subprocesses go away on Cmd+Q.
    cmd.kill_on_drop(true);

    let mut child = cmd.spawn().map_err(|e| ExecuteError::Spawn {
        bin: resolved.node_bin.clone(),
        source: e,
    })?;

    // Write stdin then close it so the script sees EOF.
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(&stdin_bytes)
            .await
            .map_err(ExecuteError::StdinWrite)?;
        stdin.shutdown().await.map_err(ExecuteError::StdinWrite)?;
    }

    // Apply timeout. `wait_with_output` consumes the child so the kill
    // branch has to abort it via `kill()` before awaiting.
    let result = if let Some(deadline) = opts.timeout {
        match time::timeout(deadline, child.wait_with_output()).await {
            Ok(inner) => inner.map(Some),
            Err(_) => {
                log::warn!(
                    "{LOG_PREFIX} timeout after {}ms — killing node script={}",
                    deadline.as_millis(),
                    script_path.display()
                );
                // We can't call child.kill() here because wait_with_output
                // consumed it. The kill_on_drop flag set above will fire
                // when the Future is dropped at the end of this scope.
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

/// Prepend `bin_dir` to the given PATH string. Handles the empty / None
/// case so child processes always see at least the runtime's own bin.
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
    use crate::openhuman::runtime_node::bootstrap::NodeSource;

    /// Locate a usable `node` on the host so the integration-style tests
    /// can exercise the real spawn path. Returns `None` when no system
    /// node is available — tests gate on it and skip with a log line
    /// rather than fail, so CI environments without node still pass the
    /// unit suite. Production code never reaches this fallback; it
    /// always goes through the managed `NodeBootstrap`.
    fn system_node_or_skip(test_name: &str) -> Option<ResolvedNode> {
        let which_out = std::process::Command::new("which")
            .arg("node")
            .output()
            .ok()?;
        if !which_out.status.success() {
            log::info!("{LOG_PREFIX} test={test_name} skipped: no system `node` on PATH");
            return None;
        }
        let path = String::from_utf8_lossy(&which_out.stdout)
            .trim()
            .to_string();
        if path.is_empty() {
            return None;
        }
        let node_bin = PathBuf::from(&path);
        let bin_dir = node_bin.parent()?.to_path_buf();
        Some(ResolvedNode {
            bin_dir: bin_dir.clone(),
            node_bin,
            npm_bin: bin_dir.join("npm"),
            version: "system".to_string(),
            source: NodeSource::System,
        })
    }

    fn write_fixture_script(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("fixture.js");
        std::fs::write(&path, body).expect("write fixture script");
        path
    }

    #[tokio::test]
    async fn script_not_found_returns_typed_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let resolved = ResolvedNode {
            bin_dir: tmp.path().to_path_buf(),
            node_bin: tmp.path().join("does-not-matter"),
            npm_bin: tmp.path().join("npm"),
            version: "test".to_string(),
            source: NodeSource::System,
        };
        let opts = ExecuteOptions {
            cwd: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let err = execute_script(
            &resolved,
            &tmp.path().join("missing.js"),
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
        let script = write_fixture_script(tmp.path(), "process.exit(0)");
        let resolved = ResolvedNode {
            bin_dir: tmp.path().to_path_buf(),
            node_bin: tmp.path().join("does-not-matter"),
            npm_bin: tmp.path().join("npm"),
            version: "test".to_string(),
            source: NodeSource::System,
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
        let Some(resolved) = system_node_or_skip("happy_path_echoes_stdin_json") else {
            return;
        };
        let tmp = tempfile::tempdir().expect("tempdir");
        let script = write_fixture_script(
            tmp.path(),
            r#"
                let chunks = [];
                process.stdin.on('data', c => chunks.push(c));
                process.stdin.on('end', () => {
                    const input = JSON.parse(Buffer.concat(chunks).toString('utf8'));
                    process.stdout.write(JSON.stringify({ ok: true, echo: input.args }));
                });
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
        let Some(resolved) = system_node_or_skip("timeout_kills_long_running_script") else {
            return;
        };
        let tmp = tempfile::tempdir().expect("tempdir");
        let script = write_fixture_script(
            tmp.path(),
            r#"
                // Spin for 30s; the timeout should kill us long before
                // that. Reading stdin first so the parent's write doesn't
                // race the kill.
                process.stdin.on('data', () => {});
                setTimeout(() => process.exit(0), 30000);
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
        assert!(
            outcome.exit_code.is_none(),
            "killed process has no exit code"
        );
        assert!(
            outcome.stderr.contains("timeout"),
            "stderr should mention timeout: {}",
            outcome.stderr
        );
    }

    #[tokio::test]
    async fn non_zero_exit_returns_outcome_not_error() {
        let Some(resolved) = system_node_or_skip("non_zero_exit_returns_outcome_not_error") else {
            return;
        };
        let tmp = tempfile::tempdir().expect("tempdir");
        let script = write_fixture_script(
            tmp.path(),
            r#"
                process.stdin.on('data', () => {});
                process.stdin.on('end', () => {
                    process.stdout.write(JSON.stringify({ ok: false, error: 'boom' }));
                    process.exit(2);
                });
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
        assert!(outcome.stdout.contains("\"error\":\"boom\""));
    }

    #[tokio::test]
    async fn stderr_is_captured_separately_from_stdout() {
        let Some(resolved) = system_node_or_skip("stderr_is_captured_separately_from_stdout")
        else {
            return;
        };
        let tmp = tempfile::tempdir().expect("tempdir");
        let script = write_fixture_script(
            tmp.path(),
            r#"
                process.stdin.on('data', () => {});
                process.stdin.on('end', () => {
                    process.stderr.write('diagnostic line\n');
                    process.stdout.write(JSON.stringify({ ok: true }));
                });
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

    #[test]
    fn prepend_path_unix_separator() {
        let bin = Path::new("/usr/local/node/bin");
        let joined = prepend_path(bin, Some("/usr/bin:/bin"));
        #[cfg(not(windows))]
        assert_eq!(joined, "/usr/local/node/bin:/usr/bin:/bin");
        #[cfg(windows)]
        assert_eq!(joined, "/usr/local/node/bin;/usr/bin:/bin");
    }

    #[test]
    fn prepend_path_handles_empty_current() {
        let bin = Path::new("/usr/local/node/bin");
        assert_eq!(prepend_path(bin, None), "/usr/local/node/bin");
        assert_eq!(prepend_path(bin, Some("")), "/usr/local/node/bin");
    }
}
