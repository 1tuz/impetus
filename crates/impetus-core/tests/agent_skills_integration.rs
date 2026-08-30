use impetus_core::{
    AgentSkillsAdapter, ExtensionAdapter, ExtensionRegistry, ExtensionSource, ImportCapability,
};
use std::path::PathBuf;

#[tokio::test]
async fn import_brainstorming_skill_from_home() {
    let home = std::env::var("HOME").expect("HOME not set");
    let skill_dir = PathBuf::from(home).join(".agents/skills/brainstorming");

    if !skill_dir.exists() {
        eprintln!(
            "Skipping test: ~/.agents/skills/brainstorming not found (expected in user environment)"
        );
        return;
    }

    let adapter = ExtensionAdapter::new();
    let result = adapter
        .import(ExtensionSource::AgentSkills, &skill_dir)
        .await
        .expect("Import should succeed");

    assert_eq!(result.source, ExtensionSource::AgentSkills);
    assert_eq!(result.capability, ImportCapability::Supported);
    assert!(result.canonical.is_some());
    assert!(result.errors.is_empty());

    let spec = result.canonical.unwrap();
    assert_eq!(spec.id, "brainstorming");
    assert_eq!(spec.name, "brainstorming");
    assert!(spec.capabilities.contains(&"instructions".to_string()));
    assert!(spec.capabilities.contains(&"triggers".to_string()));
}

#[tokio::test]
async fn parse_brainstorming_skill_directly() {
    let home = std::env::var("HOME").expect("HOME not set");
    let skill_path = PathBuf::from(home)
        .join(".agents/skills/brainstorming")
        .join("SKILL.md");

    if !skill_path.exists() {
        eprintln!("Skipping test: {:?} not found", skill_path);
        return;
    }

    let (skill, spec) = AgentSkillsAdapter::import(&skill_path)
        .await
        .expect("Parse should succeed");

    assert_eq!(skill.id, "brainstorming");
    assert_eq!(skill.name, "brainstorming");
    assert!(!skill.instructions.is_empty());
    assert!(skill.instructions[0].content.len() > 100);
    assert!(
        skill.instructions[0]
            .content
            .contains("Brainstorming Ideas Into Designs")
    );

    assert_eq!(spec.source, ExtensionSource::AgentSkills);
    assert_eq!(spec.kind, impetus_core::CanonicalModuleKind::Skill);
}

#[tokio::test]
async fn register_imported_skill() {
    let home = std::env::var("HOME").expect("HOME not set");
    let skill_dir = PathBuf::from(home).join(".agents/skills/brainstorming");

    if !skill_dir.exists() {
        eprintln!("Skipping test: brainstorming skill not found");
        return;
    }

    let adapter = ExtensionAdapter::new();
    let result = adapter
        .import(ExtensionSource::AgentSkills, &skill_dir)
        .await
        .expect("Import should succeed");

    assert!(result.canonical.is_some());
    let spec = result.canonical.unwrap();

    let registry = ExtensionRegistry::new();
    registry
        .register(spec.clone())
        .expect("Registration should succeed");

    let retrieved = registry.get("brainstorming");
    assert!(retrieved.is_some());
    assert_eq!(retrieved.unwrap().id, "brainstorming");

    let by_source = registry.list_by_source(&ExtensionSource::AgentSkills);
    assert_eq!(by_source.len(), 1);
    assert_eq!(by_source[0].id, "brainstorming");
}

#[tokio::test]
async fn import_missing_skill_warns() {
    let adapter = ExtensionAdapter::new();
    let result = adapter
        .import(
            ExtensionSource::AgentSkills,
            std::path::Path::new("/tmp/nonexistent-skill"),
        )
        .await
        .expect("Import should return result");

    assert_eq!(result.capability, ImportCapability::Unsupported);
    assert!(result.canonical.is_none());
    assert!(!result.warnings.is_empty());
    assert!(result.warnings[0].contains("SKILL.md not found"));
}

#[tokio::test]
async fn import_multiple_skills() {
    let home = std::env::var("HOME").expect("HOME not set");
    let skills_root = PathBuf::from(home).join(".agents/skills");

    if !skills_root.exists() {
        eprintln!("Skipping test: ~/.agents/skills not found");
        return;
    }

    let adapter = ExtensionAdapter::new();
    let registry = ExtensionRegistry::new();

    let mut imported_count = 0;
    let skill_names = ["brainstorming", "writing-plans"];

    for skill_name in skill_names {
        let skill_dir = skills_root.join(skill_name);
        if !skill_dir.exists() {
            continue;
        }

        let result = adapter
            .import(ExtensionSource::AgentSkills, &skill_dir)
            .await
            .expect("Import should succeed");

        if let (ImportCapability::Supported, Some(spec)) = (result.capability, result.canonical) {
            registry
                .register(spec)
                .expect("Registration should succeed");
            imported_count += 1;
        }
    }

    if imported_count > 0 {
        let all_skills = registry.list_by_source(&ExtensionSource::AgentSkills);
        assert_eq!(all_skills.len(), imported_count);
    } else {
        eprintln!("No skills found for multi-import test");
    }
}
