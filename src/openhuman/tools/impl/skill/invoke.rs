//! Tool: `skill_invoke` — runs the JS entrypoint of an installed skill
//! against the managed (or system) Node.js runtime, exchanging JSON
//! over stdin/stdout.
//!
//! This is the agent-facing layer on top of
//! [`crate::openhuman::runtime_node::execute_script`]. The primitive
//! handles the process spawn + I/O + timeout; this tool handles skill
//! lookup, entrypoint resolution, and JSON marshalling between the
//! agent's tool-call schema and the script's wire contract.
//!
//! ## Skill metadata contract
//!
//! A skill is invocable when its `SKILL.md` frontmatter declares an
//! entrypoint:
//!
//! ```yaml
//! ---
//! name: my-skill
//! description: …
//! metadata:
//!   entrypoint: scripts/main.js
//! ---
//! ```
//!
//! The path is **relative to the skill directory** and must point at a
//! `.js` or `.mjs` file under `scripts/` (one of the conventional
//! `RESOURCE_DIRS`). Anything else is rejected so a malicious or buggy
//! skill can't escape its install root via `..` traversal.
//!
//! ## Script wire contract
//!
//! The primitive's contract: stdin gets `{ "args": <user>, "meta":
//! {...} }`, stdout should be `{ "ok": bool, "result"|"error": <json> }`.
//! This tool wraps the user's `args` payload, parses stdout as JSON
//! into a `ToolResult`, and surfaces the script's `error` field directly
//! to the agent when `ok: false`.
//!
//! ## Out of scope (v1)
//!
//! - Python skills (entrypoint must be `.js` / `.mjs`).
//! - Sandbox beyond OS process isolation.
//! - Resource limits beyond a wall-clock timeout.
//! - Streaming results; one stdout JSON object, then exit.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;

use crate::openhuman::javascript::NodeBootstrap;
use crate::openhuman::runtime_node::{execute_script, ExecuteOptions};
use crate::openhuman::skills::ops_discover::load_skills;
use crate::openhuman::skills::ops_types::Skill;
use crate::openhuman::tools::traits::{
    PermissionLevel, Tool, ToolCallOptions, ToolCategory, ToolResult,
};

const LOG_PREFIX: &str = "[skill_invoke]";

/// Tool implementation. Constructed once per agent build with the
/// workspace dir (for skill lookup) and a shared `NodeBootstrap` (for
/// runtime resolution).
pub struct SkillInvokeTool {
    workspace_dir: PathBuf,
    node_bootstrap: Arc<NodeBootstrap>,
}

impl SkillInvokeTool {
    pub fn new(workspace_dir: PathBuf, node_bootstrap: Arc<NodeBootstrap>) -> Self {
        Self {
            workspace_dir,
            node_bootstrap,
        }
    }

    /// Look up the skill by `dir_name` (the on-disk slug under
    /// `~/.openhuman/skills/` or the workspace skills dir). Returns
    /// `None` when no skill matches.
    fn find_skill(&self, skill_id: &str) -> Option<Skill> {
        let skills = load_skills(&self.workspace_dir);
        skills.into_iter().find(|s| s.dir_name == skill_id)
    }
}

#[async_trait]
impl Tool for SkillInvokeTool {
    fn name(&self) -> &str {
        "skill_invoke"
    }

    fn description(&self) -> &str {
        "Run an installed skill's JavaScript entrypoint. Pass the skill's \
         directory slug as `skill_id` and a JSON `args` object that the \
         script will read from stdin. Returns the JSON the script writes \
         to stdout. Use this when a `SKILL.md` declares a `metadata.entrypoint` \
         and the skill's instructions tell the agent to invoke it directly. \
         Single-shot only — long-running or streaming skills are not supported."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "required": ["skill_id"],
            "properties": {
                "skill_id": {
                    "type": "string",
                    "description": "The skill's on-disk directory slug (e.g. 'image-resize'). \
                                    Must match an installed skill — use the skills catalog if \
                                    you're not sure of the exact slug."
                },
                "args": {
                    "type": "object",
                    "description": "JSON object passed to the script on stdin as `{ args: <here> }`. \
                                    Optional. The script defines its own arg shape — read the \
                                    skill's SKILL.md before invoking."
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Optional wall-clock timeout in seconds. Defaults to 30. \
                                    Capped at 300 — skills that need longer should be redesigned.",
                    "minimum": 1,
                    "maximum": 300
                }
            }
        })
    }

    fn permission_level(&self) -> PermissionLevel {
        // Script execution is Execute (same level as ShellTool). The
        // OS-process isolation is the only sandbox; this tool inherits
        // the user's filesystem + network access.
        PermissionLevel::Execute
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Skill
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        self.execute_with_options(args, ToolCallOptions::default())
            .await
    }

    async fn execute_with_options(
        &self,
        args: serde_json::Value,
        _options: ToolCallOptions,
    ) -> anyhow::Result<ToolResult> {
        let skill_id = args
            .get("skill_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let Some(skill_id) = skill_id else {
            return Ok(ToolResult::error(format!(
                "{LOG_PREFIX} `skill_id` is required"
            )));
        };

        let user_args = args.get("args").cloned().unwrap_or(json!({}));
        let timeout = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .map(|s| s.clamp(1, 300))
            .map(Duration::from_secs)
            .unwrap_or_else(|| Duration::from_secs(30));

        log::debug!(
            "{LOG_PREFIX} invoke skill_id={skill_id} timeout_secs={}s",
            timeout.as_secs()
        );

        // ── 1. Resolve the skill ───────────────────────────────────────
        let Some(skill) = self.find_skill(skill_id) else {
            return Ok(ToolResult::error(format!(
                "{LOG_PREFIX} unknown skill '{skill_id}' — not installed in this workspace"
            )));
        };

        // ── 2. Find the entrypoint in the skill's frontmatter ──────────
        let entrypoint = skill
            .frontmatter
            .metadata
            .get("entrypoint")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let Some(entrypoint_rel) = entrypoint else {
            return Ok(ToolResult::error(format!(
                "{LOG_PREFIX} skill '{skill_id}' has no `metadata.entrypoint` — \
                 not invocable. Skills must declare a script path under \
                 `scripts/` (e.g. `metadata.entrypoint: scripts/main.js`) \
                 to be callable from the agent."
            )));
        };

        // ── 3. Resolve the entrypoint to an absolute path safely ───────
        let skill_dir = match skill.location.as_ref().and_then(|p| p.parent()) {
            Some(dir) => dir.to_path_buf(),
            None => {
                return Ok(ToolResult::error(format!(
                    "{LOG_PREFIX} skill '{skill_id}' has no on-disk location — \
                     can't resolve entrypoint"
                )));
            }
        };
        let entrypoint_abs = match resolve_entrypoint(&skill_dir, entrypoint_rel) {
            Ok(p) => p,
            Err(reason) => {
                return Ok(ToolResult::error(format!(
                    "{LOG_PREFIX} skill '{skill_id}': {reason}"
                )));
            }
        };

        // ── 4. Resolve (and possibly download) the managed Node runtime
        log::debug!("{LOG_PREFIX} resolving node runtime via NodeBootstrap");
        let resolved = match self.node_bootstrap.resolve().await {
            Ok(r) => r,
            Err(e) => {
                return Ok(ToolResult::error(format!(
                    "{LOG_PREFIX} node runtime resolution failed: {e:#}"
                )));
            }
        };

        // ── 5. Spawn the script ────────────────────────────────────────
        let payload = json!({
            "args": user_args,
            "meta": {
                "skill_id": skill_id,
                "skill_dir": skill_dir.display().to_string(),
                "host": "closedhuman",
                "tool": "skill_invoke",
            }
        });
        let opts = ExecuteOptions {
            cwd: skill_dir.clone(),
            env: Default::default(),
            timeout: Some(timeout),
        };
        let outcome = match execute_script(&resolved, &entrypoint_abs, &payload, &opts).await {
            Ok(o) => o,
            Err(e) => {
                return Ok(ToolResult::error(format!(
                    "{LOG_PREFIX} script spawn failed: {e}"
                )));
            }
        };

        log::debug!(
            "{LOG_PREFIX} script done skill_id={skill_id} exit_code={:?} timed_out={} elapsed_ms={} stdout_bytes={} stderr_bytes={}",
            outcome.exit_code,
            outcome.timed_out,
            outcome.elapsed_ms,
            outcome.stdout.len(),
            outcome.stderr.len(),
        );

        // ── 6. Marshal the result ──────────────────────────────────────
        if outcome.timed_out {
            return Ok(ToolResult::error(format!(
                "{LOG_PREFIX} skill '{skill_id}' timed out after {}s — \
                 stderr: {}",
                timeout.as_secs(),
                truncate(&outcome.stderr, 800)
            )));
        }

        // Try to parse stdout as JSON. Non-JSON output is surfaced as a
        // diagnostic error — the wire contract requires a JSON object.
        let parsed: serde_json::Value = match serde_json::from_str(outcome.stdout.trim()) {
            Ok(v) => v,
            Err(e) => {
                return Ok(ToolResult::error(format!(
                    "{LOG_PREFIX} skill '{skill_id}' did not emit valid JSON on stdout \
                     (exit_code={:?}, parse_error={e}, stderr={}): {}",
                    outcome.exit_code,
                    truncate(&outcome.stderr, 400),
                    truncate(&outcome.stdout, 400)
                )));
            }
        };

        // Wire-contract path: { ok: bool, result|error: <json> }.
        let ok = parsed.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
        if !ok {
            let err_field = parsed
                .get("error")
                .map(|v| {
                    v.as_str()
                        .map(String::from)
                        .unwrap_or_else(|| v.to_string())
                })
                .unwrap_or_else(|| "(no `error` field in script output)".to_string());
            return Ok(ToolResult::error(format!(
                "{LOG_PREFIX} skill '{skill_id}' reported failure: {err_field}"
            )));
        }

        let result = parsed.get("result").cloned().unwrap_or(json!(null));
        let body = serde_json::to_string(&result).unwrap_or_else(|_| "null".to_string());
        Ok(ToolResult::success(body))
    }
}

/// Resolve `entrypoint` (declared in frontmatter, relative to the
/// skill's directory) to an absolute, canonicalised path under
/// `skill_dir`. Rejects:
///
/// - absolute paths (must be relative)
/// - extensions other than `.js` / `.mjs`
/// - traversal outside `skill_dir` (after canonicalisation)
/// - missing files
/// - directories
///
/// Returns `Err` with a user-facing reason string.
fn resolve_entrypoint(skill_dir: &Path, entrypoint: &str) -> Result<PathBuf, String> {
    let rel = Path::new(entrypoint);
    if rel.is_absolute() {
        return Err(format!(
            "entrypoint '{entrypoint}' must be relative to the skill directory"
        ));
    }

    let ext = rel
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    if !matches!(ext.as_deref(), Some("js") | Some("mjs")) {
        return Err(format!(
            "entrypoint '{entrypoint}' must end with .js or .mjs (got {:?})",
            ext.as_deref().unwrap_or("<none>")
        ));
    }

    let joined = skill_dir.join(rel);
    let canon_skill = skill_dir.canonicalize().map_err(|e| {
        format!(
            "could not canonicalise skill dir {}: {e}",
            skill_dir.display()
        )
    })?;
    let canon_entry = joined
        .canonicalize()
        .map_err(|e| format!("entrypoint '{entrypoint}' missing or unreadable: {e}"))?;
    if !canon_entry.starts_with(&canon_skill) {
        return Err(format!(
            "entrypoint '{entrypoint}' resolves outside the skill directory — \
             ../-style traversal is rejected"
        ));
    }
    if !canon_entry.is_file() {
        return Err(format!("entrypoint '{entrypoint}' is not a regular file"));
    }
    Ok(canon_entry)
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…(truncated to {max} chars)", &s[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn write_fixture_skill(
        skills_root: &Path,
        slug: &str,
        frontmatter_yaml: &str,
        script: Option<&str>,
    ) -> PathBuf {
        let skill_dir = skills_root.join(slug);
        fs::create_dir_all(skill_dir.join("scripts")).expect("mkdir scripts");
        let body = format!("---\n{frontmatter_yaml}---\n\nSkill body for {slug}.\n");
        fs::write(skill_dir.join("SKILL.md"), body).expect("write SKILL.md");
        if let Some(js) = script {
            fs::write(skill_dir.join("scripts").join("main.js"), js).expect("write script");
        }
        skill_dir
    }

    /// Build a SkillInvokeTool against a temp workspace. The
    /// NodeBootstrap is constructed against the temp workspace too, so
    /// resolution will go to a workspace-local cache that's torn down
    /// with the tempdir.
    fn tool_for_workspace(workspace: &Path) -> SkillInvokeTool {
        let node_config = crate::openhuman::config::schema::NodeConfig::default();
        let bootstrap = Arc::new(NodeBootstrap::new(
            node_config,
            workspace.to_path_buf(),
            reqwest::Client::new(),
        ));
        SkillInvokeTool::new(workspace.to_path_buf(), bootstrap)
    }

    /// Detect the host's `node` major version so the test's NodeConfig
    /// can target it explicitly. Without this the bootstrap rejects the
    /// system node (default config pins `v22.11.0`) and tries to
    /// download a managed distribution into the test tempdir — which
    /// either fails on permissions, blows past the test timeout, or
    /// races a sibling test on the same cache.
    fn host_node_version_or_skip(test_name: &str) -> Option<String> {
        let out = std::process::Command::new("node")
            .arg("--version")
            .output()
            .ok()?;
        if !out.status.success() {
            log::info!("{LOG_PREFIX} test={test_name} skipped: `node --version` failed");
            return None;
        }
        let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if raw.is_empty() {
            return None;
        }
        // `parse_node_version` only cares about the major; round-trip
        // through a canonical "vMAJOR.0.0" string so the bootstrap
        // matcher accepts any patch/minor of the host major.
        let stripped = raw.strip_prefix('v').unwrap_or(&raw);
        let major = stripped.split('.').next().unwrap_or("");
        if major.is_empty() {
            return None;
        }
        Some(format!("v{major}.0.0"))
    }

    #[tokio::test]
    async fn rejects_missing_skill_id() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tool = tool_for_workspace(tmp.path());
        let result = tool
            .execute(json!({}))
            .await
            .expect("execute should not panic");
        assert!(result.is_error);
        assert!(
            result.text().contains("skill_id"),
            "error should mention skill_id: {}",
            result.text()
        );
    }

    #[tokio::test]
    async fn rejects_unknown_skill() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Initialise the skills dir so load_skills doesn't bail on a
        // missing root.
        let _ = crate::openhuman::skills::ops_discover::init_skills_dir(tmp.path());
        let tool = tool_for_workspace(tmp.path());
        let result = tool
            .execute(json!({ "skill_id": "does-not-exist" }))
            .await
            .expect("execute should not panic");
        assert!(result.is_error);
        assert!(
            result.text().contains("unknown skill"),
            "error should mention unknown skill: {}",
            result.text()
        );
    }

    #[tokio::test]
    async fn rejects_skill_without_entrypoint() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _ = crate::openhuman::skills::ops_discover::init_skills_dir(tmp.path());
        let workspace_skills = tmp.path().join("skills");
        write_fixture_skill(
            &workspace_skills,
            "no-entrypoint",
            "name: no-entrypoint\ndescription: a skill without an entrypoint\n",
            None,
        );
        let tool = tool_for_workspace(tmp.path());
        let result = tool
            .execute(json!({ "skill_id": "no-entrypoint" }))
            .await
            .expect("execute should not panic");
        assert!(result.is_error);
        assert!(
            result.text().contains("metadata.entrypoint"),
            "error should explain the missing field: {}",
            result.text()
        );
    }

    #[tokio::test]
    async fn rejects_entrypoint_outside_skill_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _ = crate::openhuman::skills::ops_discover::init_skills_dir(tmp.path());
        let workspace_skills = tmp.path().join("skills");
        write_fixture_skill(
            &workspace_skills,
            "evil",
            "name: evil\ndescription: tries to escape via ../\nmetadata:\n  entrypoint: ../../../etc/passwd\n",
            Some("console.log('ok')"),
        );
        let tool = tool_for_workspace(tmp.path());
        let result = tool
            .execute(json!({ "skill_id": "evil" }))
            .await
            .expect("execute should not panic");
        assert!(result.is_error);
        // The reject can come from "must end with .js" OR the traversal
        // check OR a missing file — all are acceptable rejection paths.
        assert!(
            result.text().contains("entrypoint"),
            "error should reference entrypoint validation: {}",
            result.text()
        );
    }

    #[tokio::test]
    async fn rejects_non_js_entrypoint_extension() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _ = crate::openhuman::skills::ops_discover::init_skills_dir(tmp.path());
        let workspace_skills = tmp.path().join("skills");
        let skill_dir = write_fixture_skill(
            &workspace_skills,
            "py-skill",
            "name: py-skill\ndescription: declares a python entrypoint\nmetadata:\n  entrypoint: scripts/main.py\n",
            None,
        );
        fs::write(skill_dir.join("scripts").join("main.py"), "print('hi')").expect("write py file");
        let tool = tool_for_workspace(tmp.path());
        let result = tool
            .execute(json!({ "skill_id": "py-skill" }))
            .await
            .expect("execute should not panic");
        assert!(result.is_error);
        assert!(
            result.text().contains(".js") && result.text().contains(".mjs"),
            "error should explain the allowed extensions: {}",
            result.text()
        );
    }

    /// Build a NodeConfig that pins to the host's actual node major so
    /// the bootstrap reuses the system binary instead of trying to
    /// download a managed distribution into the test cache. Each test
    /// gets its own cache_dir under the tempdir so siblings can't race
    /// on the user's `~/Library/Caches/openhuman/node-runtime/`.
    fn host_node_config(
        target_version: String,
        cache_dir: &Path,
    ) -> crate::openhuman::config::schema::NodeConfig {
        let mut cfg = crate::openhuman::config::schema::NodeConfig::default();
        cfg.version = target_version;
        cfg.prefer_system = true;
        cfg.cache_dir = cache_dir.to_string_lossy().to_string();
        cfg
    }

    #[tokio::test]
    async fn happy_path_returns_result_object() {
        // Full path through NodeBootstrap.resolve() against the host
        // node. Skipped when no `node --version` works on the box.
        let Some(host_version) = host_node_version_or_skip("happy_path_returns_result_object")
        else {
            return;
        };

        let tmp = tempfile::tempdir().expect("tempdir");
        let _ = crate::openhuman::skills::ops_discover::init_skills_dir(tmp.path());
        let workspace_skills = tmp.path().join("skills");
        let script = r#"
            let chunks = [];
            process.stdin.on('data', c => chunks.push(c));
            process.stdin.on('end', () => {
                const input = JSON.parse(Buffer.concat(chunks).toString('utf8'));
                process.stdout.write(JSON.stringify({
                    ok: true,
                    result: { echo: input.args, skill_id: input.meta.skill_id }
                }));
            });
        "#;
        write_fixture_skill(
            &workspace_skills,
            "echo",
            "name: echo\ndescription: echoes its args\nmetadata:\n  entrypoint: scripts/main.js\n",
            Some(script),
        );

        let cache_dir = tmp.path().join("node-cache");
        let bootstrap = Arc::new(NodeBootstrap::new(
            host_node_config(host_version, &cache_dir),
            tmp.path().to_path_buf(),
            reqwest::Client::new(),
        ));
        let tool = SkillInvokeTool::new(tmp.path().to_path_buf(), bootstrap);

        let result = tool
            .execute(json!({
                "skill_id": "echo",
                "args": { "hello": "world" }
            }))
            .await
            .expect("execute should succeed");
        assert!(
            !result.is_error,
            "expected success but got error: {}",
            result.text()
        );
        let parsed: serde_json::Value = serde_json::from_str(&result.text()).expect("valid JSON");
        assert_eq!(parsed["echo"], json!({"hello": "world"}));
        assert_eq!(parsed["skill_id"], json!("echo"));
    }

    #[tokio::test]
    async fn script_error_path_surfaces_message() {
        let Some(host_version) = host_node_version_or_skip("script_error_path_surfaces_message")
        else {
            return;
        };

        let tmp = tempfile::tempdir().expect("tempdir");
        let _ = crate::openhuman::skills::ops_discover::init_skills_dir(tmp.path());
        let workspace_skills = tmp.path().join("skills");
        let script = r#"
            process.stdin.on('data', () => {});
            process.stdin.on('end', () => {
                process.stdout.write(JSON.stringify({ ok: false, error: "boom-from-script" }));
            });
        "#;
        write_fixture_skill(
            &workspace_skills,
            "fails",
            "name: fails\ndescription: returns ok=false\nmetadata:\n  entrypoint: scripts/main.js\n",
            Some(script),
        );

        let cache_dir = tmp.path().join("node-cache");
        let bootstrap = Arc::new(NodeBootstrap::new(
            host_node_config(host_version, &cache_dir),
            tmp.path().to_path_buf(),
            reqwest::Client::new(),
        ));
        let tool = SkillInvokeTool::new(tmp.path().to_path_buf(), bootstrap);

        let result = tool
            .execute(json!({ "skill_id": "fails" }))
            .await
            .expect("execute should not panic");
        assert!(result.is_error);
        assert!(
            result.text().contains("boom-from-script"),
            "should surface the script's error field: {}",
            result.text()
        );
    }

    #[test]
    fn truncate_short_string_is_unchanged() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_long_string_is_clipped_with_marker() {
        let long = "a".repeat(20);
        let out = truncate(&long, 10);
        assert!(out.starts_with(&"a".repeat(10)));
        assert!(out.contains("truncated"));
    }

    #[test]
    fn resolve_entrypoint_rejects_absolute_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let err = resolve_entrypoint(tmp.path(), "/etc/passwd")
            .err()
            .expect("absolute path must be rejected");
        assert!(err.contains("relative"));
    }

    #[test]
    fn resolve_entrypoint_rejects_wrong_extension() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let err = resolve_entrypoint(tmp.path(), "scripts/main.sh")
            .err()
            .expect("wrong extension must be rejected");
        assert!(err.contains(".js") && err.contains(".mjs"));
    }

    #[test]
    fn resolve_entrypoint_accepts_relative_js_under_skill_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir(tmp.path().join("scripts")).unwrap();
        fs::write(tmp.path().join("scripts").join("main.js"), "// noop").unwrap();
        let resolved = resolve_entrypoint(tmp.path(), "scripts/main.js")
            .expect("relative .js under skill dir should resolve");
        assert!(resolved.ends_with("scripts/main.js"));
    }

    #[test]
    fn resolve_entrypoint_rejects_traversal_via_dotdot() {
        let outer = tempfile::tempdir().expect("outer tempdir");
        let skill_dir = outer.path().join("inner");
        fs::create_dir(&skill_dir).unwrap();
        // Create a real .js file outside the skill dir so the .canonicalize()
        // call succeeds and we end up exercising the starts_with() check.
        let outside = outer.path().join("outside.js");
        fs::write(&outside, "// noop").unwrap();
        let err = resolve_entrypoint(&skill_dir, "../outside.js")
            .err()
            .expect("traversal must be rejected");
        assert!(err.contains("outside"), "{err}");
    }
}
