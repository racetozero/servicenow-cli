//! Instructions for AI agents.
//!
//! The skill is in `.agents/skills`, where `npx skills add
//! racetozero/servicenow-cli` finds it; the binary includes the same file.

use crate::ctx::Ctx;
use clap::Args;
use serde_json::json;
use sn_core::{Error, Result};
use std::path::PathBuf;

const SKILL: &str = include_str!("../../../../.agents/skills/sn/SKILL.md");

#[derive(Args, Debug)]
pub struct SkillArgs {
    /// Write sn/SKILL.md into this directory (default .claude/skills).
    #[arg(long, num_args = 0..=1, default_missing_value = ".claude/skills", value_name = "DIR")]
    pub install: Option<PathBuf>,
}

pub fn run(ctx: &Ctx, args: SkillArgs) -> Result<()> {
    let Some(dir) = args.install else {
        return ctx.out.text(SKILL);
    };
    let path = dir.join("sn").join("SKILL.md");
    let failed = |err: std::io::Error| {
        Error::invalid(format!(
            "cannot write {} ({err}); pass --install DIR with a directory you can write",
            path.display()
        ))
    };
    std::fs::create_dir_all(dir.join("sn")).map_err(failed)?;
    std::fs::write(&path, SKILL).map_err(failed)?;
    ctx.out.done(
        json!({ "path": path.display().to_string() }),
        &format!("Wrote {}", path.display()),
    )
}

#[cfg(test)]
mod tests {
    use super::SKILL;

    /// The skills CLI skips a skill whose front matter it cannot read.
    #[test]
    fn front_matter_is_valid() {
        let front = SKILL
            .strip_prefix("---\nname: sn\ndescription: ")
            .expect("SKILL.md starts with its name and description");
        let (description, _) = front
            .split_once("\n---\n")
            .expect("the front matter ends after the description");
        assert!(!description.trim().is_empty());
        // YAML reads `: ` inside a plain value as a nested mapping.
        assert!(!description.contains(": "), "{description}");
    }
}
