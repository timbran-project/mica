use crate::index::SemanticIndexProvider;
use crate::provider::{
    ListRequest, LocalSourceProvider, ProviderResult, ReadRequest, SourceBounds,
    SourceCapabilities, SourceConfig, SourceEntry, SourceProvider, SourceProviderKey,
    SourceProviderRegistry,
};
use crate::rust_analyzer::RustAnalyzerProvider;
use crate::syntax::SyntaxDocument;
use crate::util::{
    content_hash, expect_single_value, invalid_relation, relation_id, validate_relative_path,
};
use crate::vcs::VcsProvider;
use mica_relation_kernel::{ComputedRelationRead, KernelError, RelationId};
use mica_var::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

const SOURCE_DOCUMENT_CACHE_LIMIT: usize = 64;

/// Owns the configured source providers and routes public source relations to
/// them. It performs repository-root admission, caches documents and VCS
/// handles, and translates the four-way provider result into relation rows or
/// kernel errors.
pub(crate) struct SourceDispatcher {
    registry: SourceProviderRegistry,
    allowed_roots: Vec<PathBuf>,
    bounds: SourceBounds,
    semantic_index: SemanticIndexProvider,
    rust_analyzer: Option<RustAnalyzerProvider>,
    source_document_cache: Mutex<HashMap<SourceDocumentCacheKey, Arc<CachedSourceDocument>>>,
    vcs_providers: Mutex<HashMap<PathBuf, Arc<VcsProvider>>>,
}

impl SourceDispatcher {
    pub(crate) fn new(mut config: SourceConfig) -> Self {
        let configured_providers = config.take_providers();
        let allowed_roots: Vec<PathBuf> = config
            .roots
            .into_iter()
            .filter_map(|root| root.canonicalize().ok())
            .collect();
        let mut registry = SourceProviderRegistry::default();
        if !allowed_roots.is_empty() {
            registry.insert(Arc::new(LocalSourceProvider::new(config.bounds.clone())));
        }
        for provider in configured_providers.into_values() {
            registry.insert(provider);
        }
        let semantic_index = SemanticIndexProvider::new(config.semantic_index_path);
        let rust_analyzer = config.rust_analyzer_binary.map(RustAnalyzerProvider::new);
        Self {
            registry,
            allowed_roots,
            bounds: config.bounds,
            semantic_index,
            rust_analyzer,
            source_document_cache: Mutex::new(HashMap::new()),
            vcs_providers: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn providers(
        &self,
    ) -> impl Iterator<Item = (&SourceProviderKey, &Arc<dyn SourceProvider>)> {
        self.registry.iter()
    }

    pub(crate) fn rust_analyzer(&self) -> Option<&RustAnalyzerProvider> {
        self.rust_analyzer.as_ref()
    }

    pub(crate) fn semantic_index(
        &self,
        relation: RelationId,
    ) -> Result<Arc<crate::index::PersistentSemanticIndex>, KernelError> {
        self.semantic_index.load(relation)
    }

    pub(crate) fn semantic_index_text_search(
        &self,
        relation: RelationId,
        query: &str,
        limit: usize,
        scope: &str,
    ) -> Result<Vec<crate::provider::TextSearchHit>, KernelError> {
        self.semantic_index
            .text_search(relation, query, limit, scope)
    }

    pub(crate) fn repository_root(
        &self,
        reader: &dyn ComputedRelationRead,
        relation: RelationId,
        repository: &Value,
        revision: &Value,
    ) -> Result<PathBuf, KernelError> {
        let root_relation = relation_id(reader, "source/RepositoryRoot", 2).ok_or_else(|| {
            invalid_relation(relation, "missing relation source/RepositoryRoot/2")
        })?;
        let revision_of = relation_id(reader, "source/RevisionOf", 2)
            .ok_or_else(|| invalid_relation(relation, "missing relation source/RevisionOf/2"))?;

        if reader
            .scan_relation(
                revision_of,
                &[Some(revision.clone()), Some(repository.clone())],
            )?
            .is_empty()
        {
            return Err(invalid_relation(
                relation,
                "revision does not belong to repository",
            ));
        }

        let root = expect_single_value(
            reader,
            root_relation,
            &[Some(repository.clone()), None],
            relation,
            "expected source/RepositoryRoot(repository, root)",
        )?
        .with_str(str::to_owned)
        .ok_or_else(|| invalid_relation(relation, "repository root must be a string"))?;
        let configured_root = PathBuf::from(root);
        let root = configured_root.canonicalize().map_err(|error| {
            invalid_relation(
                relation,
                format!(
                    "invalid repository root {}: {error}",
                    configured_root.display()
                ),
            )
        })?;
        if self
            .allowed_roots
            .iter()
            .any(|allowed| root.starts_with(allowed))
        {
            Ok(root)
        } else {
            Err(invalid_relation(
                relation,
                format!(
                    "repository root {} is not under an allowed source root",
                    root.display()
                ),
            ))
        }
    }

    pub(crate) fn resolve_path(
        &self,
        reader: &dyn ComputedRelationRead,
        relation: RelationId,
        repository: &Value,
        revision: &Value,
        relative_path: &str,
    ) -> Result<(PathBuf, String), KernelError> {
        validate_relative_path(relation, relative_path)?;
        let root = self.repository_root(reader, relation, repository, revision)?;
        let safe_path = if relative_path == "." || relative_path == "./" {
            ""
        } else {
            relative_path
        };
        Ok((root, safe_path.to_owned()))
    }

    pub(crate) fn source_document(
        &self,
        reader: &dyn ComputedRelationRead,
        relation: RelationId,
        repository: &Value,
        revision: &Value,
        root: &Path,
        relative_path: &str,
    ) -> Result<Option<Arc<CachedSourceDocument>>, KernelError> {
        let revision_kind = self.revision_kind(reader, relation, revision)?;
        let providers = self.eligible_providers(
            reader,
            relation,
            repository,
            SourceCapabilities::READ,
            "read",
            &revision_kind,
        )?;
        for selected in providers {
            let document = match selected.provider.read(&ReadRequest {
                repository: repository.clone(),
                revision: revision.clone(),
                revision_kind: revision_kind.clone(),
                root: root.to_path_buf(),
                relative_path: relative_path.to_owned(),
                max_bytes: self.bounds.max_document_bytes,
            }) {
                ProviderResult::Found(document) => document,
                ProviderResult::Absent => continue,
                ProviderResult::Denied(denial) => {
                    return Err(invalid_relation(
                        relation,
                        format!(
                            "source provider {} denied request: {}",
                            selected.key, denial.reason
                        ),
                    ));
                }
                ProviderResult::Failed(failure) => {
                    return Err(invalid_relation(
                        relation,
                        format!(
                            "source provider {} failed: {}",
                            selected.key, failure.message
                        ),
                    ));
                }
            };
            if document.text.len() > self.bounds.max_document_bytes {
                return Err(invalid_relation(
                    relation,
                    format!(
                        "source provider {} returned a document exceeding the {} byte bound",
                        selected.key, self.bounds.max_document_bytes
                    ),
                ));
            }
            if content_hash(document.text.as_bytes()) != document.content_hash {
                return Err(invalid_relation(
                    relation,
                    format!(
                        "source provider {} returned a content hash that does not describe its text",
                        selected.key
                    ),
                ));
            }
            let cache_key = SourceDocumentCacheKey {
                repository: repository.clone(),
                revision: revision.clone(),
                provider: selected.key.clone(),
                root: root.to_path_buf(),
                relative_path: relative_path.to_owned(),
                source_version: document.source_version.clone(),
                content_hash: document.content_hash.clone(),
            };
            if let Some(cached) = self.source_document_cache.lock().unwrap().get(&cache_key) {
                return Ok(Some(cached.clone()));
            }
            let document = Arc::new(CachedSourceDocument {
                provider: selected.key,
                source_version: document.source_version,
                hash: document.content_hash,
                line_count: document.text.lines().count().max(1),
                text: document.text,
                syntax: OnceLock::new(),
            });
            let mut cache = self.source_document_cache.lock().unwrap();
            if cache.len() >= SOURCE_DOCUMENT_CACHE_LIMIT {
                cache.clear();
            }
            cache.insert(cache_key, document.clone());
            return Ok(Some(document));
        }
        Ok(None)
    }

    pub(crate) fn list_entries(
        &self,
        reader: &dyn ComputedRelationRead,
        relation: RelationId,
        repository: &Value,
        revision: &Value,
        root: &Path,
        relative_path: &str,
    ) -> Result<Vec<ProvidedSourceEntry>, KernelError> {
        let revision_kind = self.revision_kind(reader, relation, revision)?;
        let providers = self.eligible_providers(
            reader,
            relation,
            repository,
            SourceCapabilities::LIST,
            "list",
            &revision_kind,
        )?;
        let mut merged = BTreeMap::new();
        for selected in providers {
            let remaining = self
                .bounds
                .max_directory_entries
                .saturating_sub(merged.len());
            if remaining == 0 {
                break;
            }
            let entries = match selected.provider.list(&ListRequest {
                repository: repository.clone(),
                revision: revision.clone(),
                revision_kind: revision_kind.clone(),
                root: root.to_path_buf(),
                relative_path: relative_path.to_owned(),
                limit: remaining,
            }) {
                ProviderResult::Found(entries) => entries,
                ProviderResult::Absent => continue,
                ProviderResult::Denied(denial) => {
                    return Err(invalid_relation(
                        relation,
                        format!(
                            "source provider {} denied request: {}",
                            selected.key, denial.reason
                        ),
                    ));
                }
                ProviderResult::Failed(failure) => {
                    return Err(invalid_relation(
                        relation,
                        format!(
                            "source provider {} failed: {}",
                            selected.key, failure.message
                        ),
                    ));
                }
            };
            if entries.len() > remaining {
                return Err(invalid_relation(
                    relation,
                    format!(
                        "source provider {} returned more than its {remaining} row budget",
                        selected.key
                    ),
                ));
            }
            for entry in entries {
                validate_relative_path(relation, &entry.relative_path)?;
                merged
                    .entry(entry.relative_path.clone())
                    .or_insert(ProvidedSourceEntry {
                        provider: selected.key.clone(),
                        entry,
                    });
            }
        }
        Ok(merged.into_values().collect())
    }

    fn revision_kind(
        &self,
        reader: &dyn ComputedRelationRead,
        relation: RelationId,
        revision: &Value,
    ) -> Result<String, KernelError> {
        let revision_kind = relation_id(reader, "source/RevisionKind", 2)
            .ok_or_else(|| invalid_relation(relation, "missing relation source/RevisionKind/2"))?;
        expect_single_value(
            reader,
            revision_kind,
            &[Some(revision.clone()), None],
            relation,
            "expected source/RevisionKind(revision, kind)",
        )?
        .with_str(str::to_owned)
        .ok_or_else(|| invalid_relation(relation, "revision kind must be a string"))
    }

    fn eligible_providers(
        &self,
        reader: &dyn ComputedRelationRead,
        relation: RelationId,
        repository: &Value,
        capability: SourceCapabilities,
        capability_name: &str,
        revision_kind: &str,
    ) -> Result<Vec<SelectedProvider>, KernelError> {
        let repository_provider =
            relation_id(reader, "source/RepositoryProvider", 4).ok_or_else(|| {
                invalid_relation(relation, "missing relation source/RepositoryProvider/4")
            })?;
        let provider_enabled =
            relation_id(reader, "source/ProviderEnabled", 2).ok_or_else(|| {
                invalid_relation(relation, "missing relation source/ProviderEnabled/2")
            })?;
        let rows = reader.scan_relation(
            repository_provider,
            &[
                Some(repository.clone()),
                None,
                Some(Value::string(capability_name)),
                None,
            ],
        )?;
        let mut selected = Vec::new();
        for row in rows {
            let values = row.values();
            let key = values[1].with_str(str::to_owned).ok_or_else(|| {
                invalid_relation(relation, "source provider key must be a string")
            })?;
            let precedence = values[3].as_int().ok_or_else(|| {
                invalid_relation(relation, "source provider precedence must be an integer")
            })?;
            if reader
                .scan_relation(
                    provider_enabled,
                    &[Some(values[1].clone()), Some(Value::bool(true))],
                )?
                .is_empty()
            {
                continue;
            }
            let provider = self.registry.get(&key).ok_or_else(|| {
                invalid_relation(
                    relation,
                    format!("source provider {key} is not registered in this runtime"),
                )
            })?;
            if !provider.capabilities().contains(capability) {
                return Err(invalid_relation(
                    relation,
                    format!("source provider {key} does not declare {capability_name}"),
                ));
            }
            if !provider.is_available() {
                continue;
            }
            if !provider.supports_revision_kind(revision_kind) {
                continue;
            }
            selected.push(SelectedProvider {
                key: SourceProviderKey::new(key),
                precedence,
                provider: provider.clone(),
            });
        }
        selected.sort_by(|left, right| {
            right
                .precedence
                .cmp(&left.precedence)
                .then_with(|| left.key.cmp(&right.key))
        });
        if selected.len() > self.bounds.max_provider_fanout {
            return Err(invalid_relation(
                relation,
                format!(
                    "source operation exceeds the {} provider fan-out bound",
                    self.bounds.max_provider_fanout
                ),
            ));
        }
        if selected
            .windows(2)
            .any(|pair| pair[0].precedence == pair[1].precedence)
        {
            return Err(invalid_relation(
                relation,
                format!("ambiguous {capability_name} providers have equal precedence"),
            ));
        }
        Ok(selected)
    }

    pub(crate) fn vcs_provider_for(
        &self,
        reader: &dyn ComputedRelationRead,
        relation: RelationId,
        repository: &Value,
    ) -> Result<Arc<VcsProvider>, KernelError> {
        let root_relation = relation_id(reader, "source/RepositoryRoot", 2).ok_or_else(|| {
            invalid_relation(relation, "missing relation source/RepositoryRoot/2")
        })?;
        let root = expect_single_value(
            reader,
            root_relation,
            &[Some(repository.clone()), None],
            relation,
            "expected source/RepositoryRoot(repository, root)",
        )?
        .with_str(str::to_owned)
        .ok_or_else(|| invalid_relation(relation, "repository root must be a string"))?;
        let root_path = PathBuf::from(root);
        let allowed_root = root_path.canonicalize().map_err(|error| {
            invalid_relation(
                relation,
                format!("invalid repository root {}: {error}", root_path.display()),
            )
        })?;
        if !self
            .allowed_roots
            .iter()
            .any(|allowed| allowed_root.starts_with(allowed))
        {
            return Err(invalid_relation(
                relation,
                format!(
                    "repository root {} is not under an allowed source root",
                    allowed_root.display()
                ),
            ));
        }
        let git_file = allowed_root.join(".git");
        {
            let cache = self.vcs_providers.lock().unwrap();
            if let Some(provider) = cache.get(&git_file) {
                return Ok(provider.clone());
            }
        }
        let provider = VcsProvider::open(&git_file).map_err(|error| {
            invalid_relation(
                relation,
                format!("failed to open vcs for {}: {error}", allowed_root.display()),
            )
        })?;
        let provider = Arc::new(provider);
        self.vcs_providers
            .lock()
            .unwrap()
            .insert(git_file, provider.clone());
        Ok(provider)
    }

    pub(crate) fn allowed_git_dir(
        &self,
        relation: RelationId,
        git_dir: &str,
    ) -> Result<PathBuf, KernelError> {
        let path = PathBuf::from(git_dir);
        let canonical = path.canonicalize().map_err(|error| {
            invalid_relation(
                relation,
                format!("invalid git dir {}: {error}", path.display()),
            )
        })?;
        if self
            .allowed_roots
            .iter()
            .any(|allowed| canonical.starts_with(allowed))
        {
            Ok(canonical)
        } else {
            Err(invalid_relation(
                relation,
                format!(
                    "git dir {} is not under an allowed source root",
                    canonical.display()
                ),
            ))
        }
    }
}

struct SelectedProvider {
    key: SourceProviderKey,
    precedence: i64,
    provider: Arc<dyn SourceProvider>,
}

pub(crate) struct ProvidedSourceEntry {
    pub(crate) provider: SourceProviderKey,
    pub(crate) entry: SourceEntry,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct SourceDocumentCacheKey {
    repository: Value,
    revision: Value,
    provider: SourceProviderKey,
    root: PathBuf,
    relative_path: String,
    source_version: String,
    content_hash: String,
}

#[derive(Debug)]
pub(crate) struct CachedSourceDocument {
    pub(crate) provider: SourceProviderKey,
    pub(crate) source_version: String,
    pub(crate) text: String,
    pub(crate) hash: String,
    pub(crate) line_count: usize,
    syntax: OnceLock<SyntaxDocument>,
}

impl CachedSourceDocument {
    pub(crate) fn syntax(&self, path: &str) -> &SyntaxDocument {
        self.syntax
            .get_or_init(|| SyntaxDocument::parse(path, &self.text))
    }
}
