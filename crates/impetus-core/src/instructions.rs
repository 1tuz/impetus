use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const CACHE_LIMIT: usize = 128;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum InstructionKind {
    Soul,
    ProjectRules,
    Convention,
    Guide,
    Skill,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum InstructionScope {
    Global,
    Workspace,
    Path(String),
    Ecosystem(String),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InstructionReference {
    pub id: String,
    pub kind: InstructionKind,
    pub scope: InstructionScope,
    pub relative_path: PathBuf,
    pub content_hash: String,
    pub text: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct InstructionTokenEstimate {
    pub project_rules: usize,
    pub conventions: usize,
    pub guides: usize,
    pub skills: usize,
}

impl InstructionTokenEstimate {
    pub fn total(&self) -> usize {
        self.project_rules + self.conventions + self.guides + self.skills
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResolvedInstructions {
    pub references: Vec<InstructionReference>,
    pub estimated_tokens: InstructionTokenEstimate,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResolveRequest {
    pub task_path: Option<PathBuf>,
    pub ecosystems: BTreeSet<String>,
}

impl ResolveRequest {
    pub fn new(
        task_path: impl Into<PathBuf>,
        ecosystems: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            task_path: Some(task_path.into()),
            ecosystems: ecosystems.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug)]
pub enum InstructionResolveError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    DuplicateId {
        kind: InstructionKind,
        id: String,
        first_path: PathBuf,
        second_path: PathBuf,
    },
    UnmatchedReference {
        kind: InstructionKind,
        id: String,
        skill_path: PathBuf,
    },
}

impl fmt::Display for InstructionResolveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::DuplicateId {
                kind,
                id,
                first_path,
                second_path,
            } => write!(
                formatter,
                "duplicate {kind:?} ID {id:?} in {} and {}",
                first_path.display(),
                second_path.display()
            ),
            Self::UnmatchedReference {
                kind,
                id,
                skill_path,
            } => write!(
                formatter,
                "{} references missing {kind:?} ID {id:?}",
                skill_path.display()
            ),
        }
    }
}

impl std::error::Error for InstructionResolveError {}

#[derive(Clone, Debug)]
struct InstructionMetadata {
    id: Option<String>,
    scope: InstructionScope,
    conventions: Vec<String>,
    guides: Vec<String>,
}

#[derive(Clone, Debug)]
struct CachedInstruction {
    fingerprint: FileFingerprint,
    reference: InstructionReference,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FileFingerprint {
    length: u64,
    modified: Option<std::time::SystemTime>,
}

#[derive(Clone, Debug)]
struct CatalogInstruction {
    relative_path: PathBuf,
    kind: InstructionKind,
    metadata: InstructionMetadata,
    fingerprint: FileFingerprint,
}

/// Filesystem-only workspace instruction catalog with a bounded per-file cache.
#[derive(Debug)]
pub struct InstructionResolver {
    workspace: PathBuf,
    cache: BTreeMap<PathBuf, CachedInstruction>,
    catalog: BTreeMap<PathBuf, CatalogInstruction>,
}

impl InstructionResolver {
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        Self {
            workspace: workspace.into(),
            cache: BTreeMap::new(),
            catalog: BTreeMap::new(),
        }
    }

    pub fn resolve(
        &mut self,
        request: &ResolveRequest,
    ) -> Result<ResolvedInstructions, InstructionResolveError> {
        let discovered = self.discover_catalog()?;
        let mut seen = BTreeSet::new();

        let selected_skills: Vec<_> = discovered
            .iter()
            .filter(|entry| {
                entry.kind == InstructionKind::Skill
                    && matches_scope(&entry.metadata.scope, request)
            })
            .collect();
        let requested_conventions: BTreeSet<_> = selected_skills
            .iter()
            .flat_map(|skill| skill.metadata.conventions.iter())
            .collect();
        let requested_guides: BTreeSet<_> = selected_skills
            .iter()
            .flat_map(|skill| skill.metadata.guides.iter())
            .collect();
        for kind in [InstructionKind::Soul, InstructionKind::ProjectRules] {
            for entry in discovered
                .iter()
                .filter(|entry| entry.kind == kind && matches_scope(&entry.metadata.scope, request))
            {
                seen.insert(entry.relative_path.clone());
            }
        }
        for entry in discovered.iter().filter(|entry| {
            entry.kind == InstructionKind::Convention
                && (matches_scope(&entry.metadata.scope, request)
                    || requested_conventions.contains(&instruction_id(entry)))
        }) {
            seen.insert(entry.relative_path.clone());
        }
        for entry in discovered.iter().filter(|entry| {
            entry.kind == InstructionKind::Guide
                && (matches_scope(&entry.metadata.scope, request)
                    || requested_guides.contains(&instruction_id(entry)))
        }) {
            seen.insert(entry.relative_path.clone());
        }
        for skill in selected_skills {
            seen.insert(skill.relative_path.clone());
        }

        for skill in discovered.iter().filter(|entry| {
            entry.kind == InstructionKind::Skill && matches_scope(&entry.metadata.scope, request)
        }) {
            for (kind, ids) in [
                (InstructionKind::Convention, &skill.metadata.conventions),
                (InstructionKind::Guide, &skill.metadata.guides),
            ] {
                for id in ids {
                    if !discovered
                        .iter()
                        .any(|entry| entry.kind == kind && instruction_id(entry) == *id)
                    {
                        return Err(InstructionResolveError::UnmatchedReference {
                            kind,
                            id: id.clone(),
                            skill_path: skill.relative_path.clone(),
                        });
                    }
                }
            }
        }

        let mut references = Vec::new();
        for entry in discovered
            .iter()
            .filter(|entry| seen.contains(&entry.relative_path))
        {
            references.push(self.load(entry)?);
        }

        let mut estimated_tokens = InstructionTokenEstimate::default();
        for reference in &references {
            let estimate = estimate_tokens(&reference.text);
            match reference.kind {
                InstructionKind::ProjectRules => estimated_tokens.project_rules += estimate,
                InstructionKind::Convention => estimated_tokens.conventions += estimate,
                InstructionKind::Guide => estimated_tokens.guides += estimate,
                InstructionKind::Skill => estimated_tokens.skills += estimate,
                InstructionKind::Soul => {}
            }
        }
        Ok(ResolvedInstructions {
            references,
            estimated_tokens,
        })
    }

    pub fn cache_len(&self) -> usize {
        self.cache.len()
    }

    fn discover_catalog(&mut self) -> Result<Vec<CatalogInstruction>, InstructionResolveError> {
        let mut candidates = Vec::new();
        for (relative, kind) in [
            (PathBuf::from(".impetus/SOUL.md"), InstructionKind::Soul),
            (PathBuf::from("AGENTS.md"), InstructionKind::ProjectRules),
            (PathBuf::from("SKILL.md"), InstructionKind::Skill),
        ] {
            if self.workspace.join(&relative).is_file() {
                candidates.push((relative, kind));
            }
        }
        candidates.extend(self.files_in(".impetus/conventions", InstructionKind::Convention)?);
        candidates.extend(self.files_in(".impetus/guides", InstructionKind::Guide)?);
        candidates.extend(
            self.files_in(".impetus/skills", InstructionKind::Skill)?
                .into_iter()
                .filter(|(path, _)| path.file_name().is_some_and(|name| name == "SKILL.md")),
        );
        candidates.sort_by(|left, right| left.0.cmp(&right.0));

        let paths: BTreeSet<_> = candidates.iter().map(|(path, _)| path.clone()).collect();
        self.cache.retain(|path, _| paths.contains(path));
        self.catalog.retain(|path, _| paths.contains(path));
        let mut entries = Vec::with_capacity(candidates.len());
        for (relative, kind) in candidates {
            entries.push(self.catalog_entry(relative, kind)?);
        }
        entries.sort_by(|left, right| {
            left.kind
                .cmp(&right.kind)
                .then_with(|| left.relative_path.cmp(&right.relative_path))
        });
        validate_unique_ids(&entries)?;
        Ok(entries)
    }

    fn files_in(
        &self,
        directory: impl AsRef<Path>,
        kind: InstructionKind,
    ) -> Result<Vec<(PathBuf, InstructionKind)>, InstructionResolveError> {
        let relative_directory = directory.as_ref();
        let absolute_directory = self.workspace.join(relative_directory);
        if !absolute_directory.is_dir() {
            return Ok(Vec::new());
        }
        let mut files = Vec::new();
        collect_markdown_files(&self.workspace, relative_directory, &mut files)?;
        Ok(files.into_iter().map(|path| (path, kind)).collect())
    }

    fn catalog_entry(
        &mut self,
        relative: PathBuf,
        kind: InstructionKind,
    ) -> Result<CatalogInstruction, InstructionResolveError> {
        let absolute = self.workspace.join(&relative);
        let fingerprint = fingerprint(&absolute)?;
        if let Some(cached) = self.catalog.get(&relative)
            && cached.fingerprint == fingerprint
            && cached.kind == kind
        {
            return Ok(cached.clone());
        }
        let metadata = read_metadata(&absolute)?;
        let entry = CatalogInstruction {
            relative_path: relative.clone(),
            kind,
            metadata,
            fingerprint,
        };
        self.catalog.insert(relative, entry.clone());
        Ok(entry)
    }

    fn load(
        &mut self,
        catalog: &CatalogInstruction,
    ) -> Result<InstructionReference, InstructionResolveError> {
        if let Some(cached) = self.cache.get(&catalog.relative_path)
            && cached.fingerprint == catalog.fingerprint
            && cached.reference.kind == catalog.kind
        {
            return Ok(cached.reference.clone());
        }
        let absolute = self.workspace.join(&catalog.relative_path);
        let text = fs::read_to_string(&absolute).map_err(|source| InstructionResolveError::Io {
            path: absolute,
            source,
        })?;
        let hash = format!("{:x}", Sha256::digest(text.as_bytes()));
        let relative = &catalog.relative_path;
        let (_, body) = parse_front_matter(&text);
        let id = instruction_id(catalog);
        let entry = CachedInstruction {
            fingerprint: catalog.fingerprint.clone(),
            reference: InstructionReference {
                id,
                kind: catalog.kind,
                scope: catalog.metadata.scope.clone(),
                relative_path: relative.clone(),
                content_hash: hash,
                text: body.to_owned(),
            },
        };
        if self.cache.len() >= CACHE_LIMIT
            && !self.cache.contains_key(relative)
            && let Some(oldest) = self.cache.keys().next().cloned()
        {
            self.cache.remove(&oldest);
        }
        self.cache.insert(relative.clone(), entry.clone());
        Ok(entry.reference)
    }
}

fn fingerprint(path: &Path) -> Result<FileFingerprint, InstructionResolveError> {
    let metadata = fs::metadata(path).map_err(|source| InstructionResolveError::Io {
        path: path.to_owned(),
        source,
    })?;
    let modified = metadata.modified().ok();
    Ok(FileFingerprint {
        length: metadata.len(),
        modified,
    })
}

fn validate_unique_ids(entries: &[CatalogInstruction]) -> Result<(), InstructionResolveError> {
    let mut ids = BTreeMap::new();
    for entry in entries {
        if !matches!(
            entry.kind,
            InstructionKind::Convention | InstructionKind::Guide
        ) {
            continue;
        }
        let key = (entry.kind, instruction_id(entry));
        if let Some(first_path) = ids.insert(key.clone(), entry.relative_path.clone()) {
            return Err(InstructionResolveError::DuplicateId {
                kind: key.0,
                id: key.1,
                first_path,
                second_path: entry.relative_path.clone(),
            });
        }
    }
    Ok(())
}

fn instruction_id(entry: &CatalogInstruction) -> String {
    entry
        .metadata
        .id
        .clone()
        .unwrap_or_else(|| default_id(&entry.relative_path, entry.kind))
}

fn collect_markdown_files(
    workspace: &Path,
    directory: &Path,
    files: &mut Vec<PathBuf>,
) -> Result<(), InstructionResolveError> {
    let absolute = workspace.join(directory);
    for entry in fs::read_dir(&absolute).map_err(|source| InstructionResolveError::Io {
        path: absolute.clone(),
        source,
    })? {
        let entry = entry.map_err(|source| InstructionResolveError::Io {
            path: absolute.clone(),
            source,
        })?;
        let relative = directory.join(entry.file_name());
        if entry
            .file_type()
            .map_err(|source| InstructionResolveError::Io {
                path: entry.path(),
                source,
            })?
            .is_dir()
        {
            collect_markdown_files(workspace, &relative, files)?;
        } else if relative
            .extension()
            .is_some_and(|extension| extension == "md")
        {
            files.push(relative);
        }
    }
    Ok(())
}

fn parse_front_matter(text: &str) -> (InstructionMetadata, &str) {
    let Some(rest) = text.strip_prefix("---\n") else {
        return (default_metadata(), text);
    };
    let Some((header, body)) = rest.split_once("\n---\n") else {
        return (default_metadata(), text);
    };
    (parse_metadata(header), body)
}

fn read_metadata(path: &Path) -> Result<InstructionMetadata, InstructionResolveError> {
    let file = fs::File::open(path).map_err(|source| InstructionResolveError::Io {
        path: path.to_owned(),
        source,
    })?;
    let mut lines = BufReader::new(file).lines();
    let first = lines
        .next()
        .transpose()
        .map_err(|source| InstructionResolveError::Io {
            path: path.to_owned(),
            source,
        })?;
    if first.as_deref() != Some("---") {
        return Ok(default_metadata());
    }
    let mut header = String::new();
    for line in lines {
        let line = line.map_err(|source| InstructionResolveError::Io {
            path: path.to_owned(),
            source,
        })?;
        if line == "---" {
            return Ok(parse_metadata(&header));
        }
        header.push_str(&line);
        header.push('\n');
    }
    Ok(default_metadata())
}

fn default_metadata() -> InstructionMetadata {
    InstructionMetadata {
        id: None,
        scope: InstructionScope::Workspace,
        conventions: Vec::new(),
        guides: Vec::new(),
    }
}

fn parse_metadata(header: &str) -> InstructionMetadata {
    let mut metadata = default_metadata();
    for line in header.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'');
        match key.trim() {
            "id" => metadata.id = (!value.is_empty()).then(|| value.to_owned()),
            "scope" => metadata.scope = parse_scope(value),
            "path" => metadata.scope = InstructionScope::Path(value.to_owned()),
            "ecosystem" => metadata.scope = InstructionScope::Ecosystem(value.to_owned()),
            "conventions" => metadata.conventions = parse_list(value),
            "guides" => metadata.guides = parse_list(value),
            _ => {}
        }
    }
    metadata
}

fn parse_scope(value: &str) -> InstructionScope {
    if let Some(path) = value.strip_prefix("path:") {
        InstructionScope::Path(path.trim().to_owned())
    } else if let Some(ecosystem) = value.strip_prefix("ecosystem:") {
        InstructionScope::Ecosystem(ecosystem.trim().to_owned())
    } else if value == "global" {
        InstructionScope::Global
    } else {
        InstructionScope::Workspace
    }
}

fn parse_list(value: &str) -> Vec<String> {
    value
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|item| item.trim().trim_matches('"').trim_matches('\'').to_owned())
        .filter(|item| !item.is_empty())
        .collect()
}

fn default_id(relative: &Path, kind: InstructionKind) -> String {
    match kind {
        InstructionKind::Soul => "soul".to_owned(),
        InstructionKind::ProjectRules => "project-rules".to_owned(),
        InstructionKind::Skill if relative == Path::new("SKILL.md") => "skill".to_owned(),
        InstructionKind::Skill => relative
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or("instruction")
            .to_owned(),
        _ => relative
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("instruction")
            .to_owned(),
    }
}

fn matches_scope(scope: &InstructionScope, request: &ResolveRequest) -> bool {
    match scope {
        InstructionScope::Global | InstructionScope::Workspace => true,
        InstructionScope::Path(prefix) => request.task_path.as_ref().is_some_and(|path| {
            path.components()
                .map(|component| component.as_os_str())
                .collect::<PathBuf>()
                .starts_with(prefix)
        }),
        InstructionScope::Ecosystem(ecosystem) => request.ecosystems.contains(ecosystem),
    }
}

fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{InstructionKind, InstructionResolveError, InstructionResolver, ResolveRequest};

    fn write(root: &std::path::Path, relative: &str, text: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().expect("fixture parent")).expect("create fixture parent");
        fs::write(path, text).expect("write fixture");
    }

    #[test]
    fn resolves_roots_bare_skill_and_scoped_catalog_in_stable_order() {
        let fixture = tempdir().expect("fixture");
        let root = fixture.path();
        write(root, "AGENTS.md", "project");
        write(root, ".impetus/SOUL.md", "soul");
        write(root, "SKILL.md", "legacy skill");
        write(
            root,
            ".impetus/conventions/rust.md",
            "---\nid: rust\necosystem: rust\n---\nrust convention",
        );
        write(
            root,
            ".impetus/conventions/web.md",
            "---\nid: web\necosystem: web\n---\nweb convention",
        );
        write(
            root,
            ".impetus/guides/api.md",
            "---\nid: api\npath: services/api\n---\napi guide",
        );
        write(
            root,
            ".impetus/guides/other.md",
            "---\nid: other\npath: services/other\n---\nother guide",
        );
        write(
            root,
            ".impetus/skills/deploy/SKILL.md",
            "---\nid: deploy\nconventions: [rust]\nguides: [api]\npath: services/api\n---\ndeploy skill",
        );

        let mut resolver = InstructionResolver::new(root);
        let resolved = resolver
            .resolve(&ResolveRequest::new("services/api/src/main.rs", ["rust"]))
            .expect("resolve catalog");

        assert_eq!(
            resolved
                .references
                .iter()
                .map(|reference| reference.kind)
                .collect::<Vec<_>>(),
            vec![
                InstructionKind::Soul,
                InstructionKind::ProjectRules,
                InstructionKind::Convention,
                InstructionKind::Guide,
                InstructionKind::Skill,
                InstructionKind::Skill,
            ]
        );
        assert_eq!(resolved.references[2].id, "rust");
        assert_eq!(resolved.references[3].id, "api");
        assert_eq!(resolved.references[4].id, "deploy");
        assert_eq!(resolved.references[5].id, "skill");
        assert_eq!(resolved.estimated_tokens.conventions, 4);
        assert_eq!(resolved.estimated_tokens.guides, 3);
        assert_eq!(resolved.estimated_tokens.skills, 6);
    }

    #[test]
    fn includes_explicit_skill_references_once_and_refreshes_only_changed_file() {
        let fixture = tempdir().expect("fixture");
        let root = fixture.path();
        write(
            root,
            ".impetus/conventions/base.md",
            "---\nid: base\n---\nbase",
        );
        write(
            root,
            ".impetus/guides/guide.md",
            "---\nid: guide\n---\nguide",
        );
        write(
            root,
            ".impetus/skills/run/SKILL.md",
            "---\nid: run\nconventions: [base, base]\nguides: [guide, guide]\n---\nrun",
        );

        let mut resolver = InstructionResolver::new(root);
        let request = ResolveRequest::default();
        let first = resolver.resolve(&request).expect("first resolve");
        write(
            root,
            ".impetus/guides/guide.md",
            "---\nid: guide\n---\nchanged guide",
        );
        let second = resolver.resolve(&request).expect("second resolve");

        assert_eq!(
            second
                .references
                .iter()
                .filter(|reference| reference.id == "base")
                .count(),
            1
        );
        assert_eq!(
            second
                .references
                .iter()
                .filter(|reference| reference.id == "guide")
                .count(),
            1
        );
        assert_ne!(
            first.references[1].content_hash,
            second.references[1].content_hash
        );
        assert_eq!(resolver.cache_len(), 3);
    }

    #[test]
    fn keeps_selected_cache_entries_when_large_scoped_catalog_is_unchanged() {
        let fixture = tempdir().expect("fixture");
        let root = fixture.path();
        for number in 0..=128 {
            write(
                root,
                &format!(".impetus/conventions/excluded-{number:03}.md"),
                "---\necosystem: other\n---\nexcluded",
            );
        }
        write(
            root,
            ".impetus/conventions/selected.md",
            "---\nid: selected\necosystem: rust\n---\nfirst selected",
        );

        let mut resolver = InstructionResolver::new(root);
        let request = ResolveRequest::new("src/main.rs", ["rust"]);
        let first = resolver.resolve(&request).expect("first resolve");
        write(
            root,
            ".impetus/conventions/selected.md",
            "---\nid: selected\necosystem: rust\n---\nchanged selected",
        );
        let second = resolver.resolve(&request).expect("second resolve");

        assert_eq!(first.references.len(), 1);
        assert_eq!(second.references.len(), 1);
        assert_ne!(
            first.references[0].content_hash,
            second.references[0].content_hash
        );
        assert_eq!(resolver.cache_len(), 1);
    }

    #[test]
    fn rejects_duplicate_ids_and_unmatched_explicit_references_deterministically() {
        let duplicate_fixture = tempdir().expect("duplicate fixture");
        write(
            duplicate_fixture.path(),
            ".impetus/conventions/a.md",
            "---\nid: duplicate\n---\na",
        );
        write(
            duplicate_fixture.path(),
            ".impetus/conventions/b.md",
            "---\nid: duplicate\n---\nb",
        );
        let duplicate_error = InstructionResolver::new(duplicate_fixture.path())
            .resolve(&ResolveRequest::default())
            .expect_err("duplicate IDs must fail");
        assert!(matches!(
            duplicate_error,
            InstructionResolveError::DuplicateId {
                kind: InstructionKind::Convention,
                ref id,
                ..
            } if id == "duplicate"
        ));

        let missing_fixture = tempdir().expect("missing fixture");
        write(
            missing_fixture.path(),
            ".impetus/skills/run/SKILL.md",
            "---\nguides: [missing]\n---\nrun",
        );
        let missing_error = InstructionResolver::new(missing_fixture.path())
            .resolve(&ResolveRequest::default())
            .expect_err("unmatched explicit guide must fail");
        assert!(matches!(
            missing_error,
            InstructionResolveError::UnmatchedReference {
                kind: InstructionKind::Guide,
                ref id,
                ..
            } if id == "missing"
        ));
    }
}
