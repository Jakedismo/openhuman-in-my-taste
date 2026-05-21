//! Skill-execution tools — agent-facing wrappers around the
//! [`crate::openhuman::runtime_node::execute_script`] primitive.

pub mod invoke;

pub use invoke::SkillInvokeTool;
