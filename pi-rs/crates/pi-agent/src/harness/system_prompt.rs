//! Port of `packages/agent/src/harness/system-prompt.ts`.

use crate::harness::types::Skill;

/// Render the model-visible skills block for the system prompt.
///
/// Returns an empty string when no skill is model-visible. Field values are XML
/// escaped; the exact layout is asserted by upstream's tests.
pub fn format_skills_for_system_prompt(skills: &[Skill]) -> String {
    let visible: Vec<&Skill> = skills
        .iter()
        .filter(|s| s.disable_model_invocation != Some(true))
        .collect();
    if visible.is_empty() {
        return String::new();
    }

    let mut lines = vec![
        "The following skills provide specialized instructions for specific tasks.".to_string(),
        "Read the full skill file when the task matches its description.".to_string(),
        "When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.".to_string(),
        String::new(),
        "<available_skills>".to_string(),
    ];

    for skill in visible {
        lines.push("  <skill>".to_string());
        lines.push(format!("    <name>{}</name>", escape_xml(&skill.name)));
        lines.push(format!(
            "    <description>{}</description>",
            escape_xml(&skill.description)
        ));
        lines.push(format!(
            "    <location>{}</location>",
            escape_xml(&skill.file_path)
        ));
        lines.push("  </skill>".to_string());
    }

    lines.push("</available_skills>".to_string());
    lines.join("\n")
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(name: &str, description: &str, path: &str) -> Skill {
        Skill {
            name: name.into(),
            description: description.into(),
            content: "content".into(),
            file_path: path.into(),
            disable_model_invocation: None,
        }
    }

    #[test]
    fn formats_visible_skills_in_order_and_skips_disabled() {
        let visible = skill("visible", "Use <this> & that", "/skills/visible/SKILL.md");
        let mut disabled = skill("hidden", "Hidden", "/skills/hidden/SKILL.md");
        disabled.disable_model_invocation = Some(true);
        let second = skill("second", "Second skill", "/skills/second/SKILL.md");

        let out = format_skills_for_system_prompt(&[visible, disabled, second]);
        let expected = r#"The following skills provide specialized instructions for specific tasks.
Read the full skill file when the task matches its description.
When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.

<available_skills>
  <skill>
    <name>visible</name>
    <description>Use &lt;this&gt; &amp; that</description>
    <location>/skills/visible/SKILL.md</location>
  </skill>
  <skill>
    <name>second</name>
    <description>Second skill</description>
    <location>/skills/second/SKILL.md</location>
  </skill>
</available_skills>"#;
        assert_eq!(out, expected);
    }

    #[test]
    fn returns_empty_string_when_nothing_is_visible() {
        let mut disabled = skill("hidden", "Hidden", "/skills/hidden/SKILL.md");
        disabled.disable_model_invocation = Some(true);
        assert_eq!(format_skills_for_system_prompt(&[disabled]), "");
    }

    #[test]
    fn escapes_xml_in_all_visible_fields() {
        let out = format_skills_for_system_prompt(&[skill(
            "a&b",
            "Quote \"double\" and 'single'",
            "/skills/<bad>&\"quote\"/SKILL.md",
        )]);
        assert!(out.contains(
            "<name>a&amp;b</name>\n    <description>Quote &quot;double&quot; and &apos;single&apos;</description>\n    <location>/skills/&lt;bad&gt;&amp;&quot;quote&quot;/SKILL.md</location>"
        ));
    }
}
