//! System prompt builder for the `integrations_agent` built-in agent.
//!
//! `integrations_agent` is the one sub-agent that executes Composio actions
//! directly — every other agent delegates to it via `spawn_subagent`.
//! That means the prompt owns two blocks nobody else renders:
//!
//! * `## Available Skills` — the SKILL.md catalogue the agent can invoke
//!   via the `skill_invoke` tool. Each skill declares an entrypoint
//!   (`.js` / `.mjs` / `.cjs` for the Node runtime, `.py` for Python) and
//!   the agent calls `skill_invoke({ skill_id, args })` to run it.
//!   Replaces the upstream QuickJS catalogue that ran in-process; the
//!   replacement runs as an out-of-process Node/Python subprocess via
//!   [`crate::openhuman::runtime_node::execute_script`] /
//!   [`crate::openhuman::runtime_python::execute_script`].
//! * `## Connected Integrations` — the list of Composio toolkits the
//!   user has connected, framed as "you have direct access to the
//!   action tools in your tool list" rather than "delegate to integrations_agent".
//!
//! Both blocks live here (not in the shared prompts module) so the
//! delegator agents stay lean and the integrations_agent-specific wording
//! isn't a branch on `agent_id` somewhere else.

use crate::openhuman::context::prompt::{
    render_safety, render_tools, render_user_files, render_workspace, ConnectedIntegration,
    PromptContext,
};
use crate::openhuman::skills::Skill;
use anyhow::Result;
use std::fmt::Write;
use std::path::Path;

const ARCHETYPE: &str = include_str!("prompt.md");

pub fn build(ctx: &PromptContext<'_>) -> Result<String> {
    let mut out = String::with_capacity(8192);
    out.push_str(ARCHETYPE.trim_end());
    out.push_str("\n\n");

    let user_files = render_user_files(ctx)?;
    if !user_files.trim().is_empty() {
        out.push_str(user_files.trim_end());
        out.push_str("\n\n");
    }

    let identities = ctx.connected_identities_md.as_str();
    if !identities.trim().is_empty() {
        out.push_str(identities.trim_end());
        out.push_str("\n\n");
    }

    let skills = render_available_skills(ctx.skills, ctx.workspace_dir);
    if !skills.trim().is_empty() {
        out.push_str(skills.trim_end());
        out.push_str("\n\n");
    }

    let integrations = render_connected_integrations(ctx.connected_integrations);
    if !integrations.trim().is_empty() {
        out.push_str(integrations.trim_end());
        out.push_str("\n\n");
    }

    let tools = render_tools(ctx)?;
    if !tools.trim().is_empty() {
        out.push_str(tools.trim_end());
        out.push_str("\n\n");
    }

    let safety = render_safety();
    out.push_str(safety.trim_end());
    out.push_str("\n\n");

    let workspace = render_workspace(ctx)?;
    if !workspace.trim().is_empty() {
        out.push_str(workspace.trim_end());
        out.push('\n');
    }

    Ok(out)
}

/// Render the `## Available Skills` XML catalogue of SKILL.md packages
/// this agent can invoke via the `skill_invoke` tool. Empty when no
/// skills are registered.
///
/// Each `<skill>` entry includes `<dir_name>` (the slug to pass as
/// `skill_id`) and an `<entrypoint>` when the skill declares one in its
/// frontmatter `metadata.entrypoint`. Skills without an entrypoint are
/// metadata-only — the agent reads their SKILL.md body for instructions
/// but cannot call them directly.
fn render_available_skills(skills: &[Skill], workspace_dir: &Path) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "## Available Skills\n\n\
         The skills below are SKILL.md packages the user installed. \
         Each one with an `<entrypoint>` element can be executed directly by \
         calling the `skill_invoke` tool: `skill_invoke({ skill_id: \"<dir_name>\", args: {...} })`. \
         The script reads `{ args, meta }` from stdin and prints \
         `{ ok: bool, result|error }` to stdout. \
         Skills without an entrypoint are metadata-only — read their \
         SKILL.md body for instructions but do not try to `skill_invoke` them.\n\n\
         <available_skills>\n",
    );
    for skill in skills {
        let location = skill.location.clone().unwrap_or_else(|| {
            workspace_dir
                .join("skills")
                .join(&skill.name)
                .join("SKILL.md")
        });
        let entrypoint = skill
            .frontmatter
            .metadata
            .get("entrypoint")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let _ = writeln!(
            out,
            "  <skill>\n    <name>{}</name>\n    <dir_name>{}</dir_name>\n    <description>{}</description>\n    <location>{}</location>",
            xml_escape(&skill.name),
            xml_escape(&skill.dir_name),
            xml_escape(&skill.description),
            xml_escape(&location.display().to_string()),
        );
        if let Some(ep) = entrypoint {
            let _ = writeln!(out, "    <entrypoint>{}</entrypoint>", xml_escape(ep));
        }
        out.push_str("  </skill>\n");
    }
    out.push_str("</available_skills>");
    out
}

/// Escape XML-sensitive characters so skill metadata can't break the
/// surrounding `<available_skills>` block if a name or description
/// contains `<`, `>`, or `&`.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Render the skill-executor-flavoured `## Connected Integrations`
/// block. Tells the model that the action tools for each toolkit are
/// already in its tool list and to call them directly — no delegation
/// wording, because `integrations_agent` IS the delegation target.
fn render_connected_integrations(integrations: &[ConnectedIntegration]) -> String {
    let connected: Vec<&ConnectedIntegration> =
        integrations.iter().filter(|ci| ci.connected).collect();
    if connected.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "## Connected Integrations\n\n\
         You have direct access to the following external services. \
         The corresponding action tools are in your tool list with \
         their typed parameter schemas — call them by name.\n\n",
    );
    for ci in connected {
        let _ = writeln!(out, "- **{}** — {}", ci.toolkit, ci.description);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openhuman::context::prompt::{LearnedContextData, ToolCallFormat};
    use std::collections::HashSet;

    fn ctx_with<'a>(
        integrations: &'a [ConnectedIntegration],
        skills: &'a [Skill],
    ) -> PromptContext<'a> {
        // Leak a HashSet so the returned context borrows a 'static-ish
        // reference — the test owns the value for its lifetime.
        use std::sync::OnceLock;
        static EMPTY_VISIBLE: OnceLock<HashSet<String>> = OnceLock::new();
        PromptContext {
            workspace_dir: std::path::Path::new("."),
            model_name: "test",
            agent_id: "integrations_agent",
            tools: &[],
            skills,
            dispatcher_instructions: "",
            learned: LearnedContextData::default(),
            visible_tool_names: EMPTY_VISIBLE.get_or_init(HashSet::new),
            tool_call_format: ToolCallFormat::PFormat,
            connected_integrations: integrations,
            connected_identities_md: String::new(),
            include_profile: false,
            include_memory_md: false,
            curated_snapshot: None,
            user_identity: None,
        }
    }

    #[test]
    fn build_returns_nonempty_body() {
        let body = build(&ctx_with(&[], &[])).unwrap();
        assert!(!body.is_empty());
        assert!(!body.contains("## Connected Integrations"));
        assert!(!body.contains("## Available Skills"));
    }

    #[test]
    fn build_includes_connected_integrations_in_executor_voice() {
        let integrations = vec![ConnectedIntegration {
            toolkit: "gmail".into(),
            description: "Email access.".into(),
            tools: Vec::new(),
            connected: true,
        }];
        let body = build(&ctx_with(&integrations, &[])).unwrap();
        assert!(body.contains("## Connected Integrations"));
        assert!(body.contains("You have direct access"));
        assert!(body.contains("- **gmail** — Email access."));
        // `integrations_agent` must NOT render the delegator spawn snippet —
        // that belongs on the orchestrator/welcome side.
        assert!(!body.contains("Delegation Guide"));
        assert!(!body.contains("spawn_subagent"));
    }

    #[test]
    fn build_skips_unconnected_integrations() {
        let integrations = vec![ConnectedIntegration {
            toolkit: "notion".into(),
            description: "Pages.".into(),
            tools: Vec::new(),
            connected: false,
        }];
        let body = build(&ctx_with(&integrations, &[])).unwrap();
        assert!(!body.contains("## Connected Integrations"));
    }

    fn make_skill(name: &str, dir_name: &str, entrypoint: Option<&str>) -> Skill {
        let mut fm = crate::openhuman::skills::ops_types::SkillFrontmatter::default();
        fm.name = name.to_string();
        fm.description = format!("{name} description");
        if let Some(ep) = entrypoint {
            fm.metadata.insert(
                "entrypoint".to_string(),
                serde_yaml::Value::String(ep.to_string()),
            );
        }
        Skill {
            name: name.to_string(),
            dir_name: dir_name.to_string(),
            description: format!("{name} description"),
            frontmatter: fm,
            ..Default::default()
        }
    }

    #[test]
    fn available_skills_block_mentions_skill_invoke_when_skills_present() {
        let skills = vec![make_skill(
            "image-resize",
            "image-resize",
            Some("scripts/main.js"),
        )];
        let body = build(&ctx_with(&[], &skills)).unwrap();
        assert!(body.contains("## Available Skills"));
        assert!(
            body.contains("skill_invoke"),
            "agent should be told to invoke skills via skill_invoke; got: {body}"
        );
        assert!(body.contains("<dir_name>image-resize</dir_name>"));
        assert!(body.contains("<entrypoint>scripts/main.js</entrypoint>"));
    }

    #[test]
    fn available_skills_block_omits_entrypoint_element_for_metadata_only_skills() {
        // No metadata.entrypoint → the rendered <skill> block must not
        // include the closing `</entrypoint>` tag so the agent doesn't
        // try to skill_invoke a non-callable package. The opening
        // `<entrypoint>` literal appears once in the header paragraph
        // (explaining the convention) — that's expected and isn't what
        // we're checking here.
        let skills = vec![make_skill("docs-only", "docs-only", None)];
        let body = build(&ctx_with(&[], &skills)).unwrap();
        assert!(body.contains("## Available Skills"));
        assert!(body.contains("<dir_name>docs-only</dir_name>"));
        assert!(
            !body.contains("</entrypoint>"),
            "metadata-only skill should not render an <entrypoint>…</entrypoint> pair"
        );
    }

    #[test]
    fn available_skills_block_xml_escapes_user_data() {
        let skills = vec![make_skill(
            "naughty<&\">",
            "naughty-slug",
            Some("scripts/<main>.js"),
        )];
        let body = build(&ctx_with(&[], &skills)).unwrap();
        // Raw `<`/`&`/`>` from user data must be escaped so they don't
        // close the <available_skills> block early.
        assert!(body.contains("naughty&lt;&amp;&quot;&gt;"));
        assert!(body.contains("<entrypoint>scripts/&lt;main&gt;.js</entrypoint>"));
    }
}
