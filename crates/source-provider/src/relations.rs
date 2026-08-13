use crate::index::{IndexedTextUnit, PersistentSemanticIndex};
use crate::navigation::{
    SemanticSymbol, SemanticSymbolProvider, byte_offset_to_lsp_position, semantic_location,
    semantic_symbol,
};
use crate::receive::GitReceiveRecorder;
use crate::rust_analyzer::RustAnalyzerProvider;
use crate::syntax::{SourceLanguage, SyntaxDocument, syntax_lines};
use crate::util::*;
use crate::vcs::{BlameRow, VcsProvider};
use jj_lib::object_id::ObjectId;
use mica_relation_kernel::{
    ComputedRelation, ComputedRelationRead, KernelError, RelationId, RelationMetadata, Tuple,
};
use mica_var::{Symbol, Value};
use std::collections::{BTreeMap, HashMap};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

const REPOSITORY_ENTRY_BOUND: &[u16] = &[0, 1, 2];
const FILE_TEXT_BOUND: &[u16] = &[0, 1, 2];
const FILE_LINES_BOUND: &[u16] = &[0, 1, 2, 3, 4];
const FILE_LINE_COUNT_BOUND: &[u16] = &[0, 1, 2];
const FILE_CONTENT_HASH_BOUND: &[u16] = &[0, 1, 2];
const SYNTAX_LINE_BOUND: &[u16] = &[0, 1, 2, 3, 4];
const SYNTAX_OUTLINE_BOUND: &[u16] = &[0, 1, 2];
const SYNTAX_NODE_AT_BOUND: &[u16] = &[0, 1, 2, 3];
const DEFINITION_AT_BOUND: &[u16] = &[0, 1, 2, 3];
const REFERENCES_OF_BOUND: &[u16] = &[0, 1, 2];
const SYMBOL_SEARCH_BOUND: &[u16] = &[0, 1, 2, 3];
const INDEXED_TEXT_UNIT_BOUND: &[u16] = &[];
const INDEXED_FILE_BOUND: &[u16] = &[];
const TEXT_SEARCH_BOUND: &[u16] = &[0, 1, 2];
const INDEX_VALUE_BOUND: &[u16] = &[];
const VCS_COMMIT_KEY_BOUND: &[u16] = &[0, 1];
const VCS_REF_TARGET_BOUND: &[u16] = &[0, 1];
const GIT_REF_TARGET_BOUND: &[u16] = &[0, 1];
const VCS_REPOSITORY_BOUND: &[u16] = &[0];
const VCS_TWO_COMMIT_BOUND: &[u16] = &[0, 1, 2];
const VCS_TWO_COMMIT_PATH_BOUND: &[u16] = &[0, 1, 2, 3];
const VCS_TWO_COMMIT_PATH_RANGE_BOUND: &[u16] = &[0, 1, 2, 3, 4, 5];
const VCS_COMMIT_PATH_BOUND: &[u16] = &[0, 1];
const VCS_BLAME_BOUND: &[u16] = &[0, 1, 2];
const VCS_SEARCH_BOUND: &[u16] = &[0, 1];
const GIT_RECEIVED_REF_UPDATE_BOUND: &[u16] = &[0];
const SEMANTIC_INDEX_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const SOURCE_DOCUMENT_CACHE_LIMIT: usize = 64;

pub fn default_computed_relations() -> Vec<Arc<dyn ComputedRelation>> {
    let provider = Arc::new(LocalSourceProvider::from_env());
    vec![
        Arc::new(RepositoryEntryRelation {
            provider: provider.clone(),
        }),
        Arc::new(FileTextRelation {
            provider: provider.clone(),
        }),
        Arc::new(FileLinesRelation {
            provider: provider.clone(),
        }),
        Arc::new(FileLineCountRelation {
            provider: provider.clone(),
        }),
        Arc::new(FileContentHashRelation {
            provider: provider.clone(),
        }),
        Arc::new(SyntaxLineRelation {
            provider: provider.clone(),
        }),
        Arc::new(SyntaxOutlineRelation {
            provider: provider.clone(),
        }),
        Arc::new(SyntaxNodeAtRelation {
            provider: provider.clone(),
        }),
        Arc::new(DefinitionAtRelation {
            provider: provider.clone(),
        }),
        Arc::new(ReferencesOfRelation {
            provider: provider.clone(),
        }),
        Arc::new(SymbolSearchRelation {
            provider: provider.clone(),
        }),
        Arc::new(IndexedTextUnitRelation {
            provider: provider.clone(),
        }),
        Arc::new(IndexedFileRelation {
            provider: provider.clone(),
        }),
        Arc::new(TextSearchRelation {
            provider: provider.clone(),
        }),
        Arc::new(SourceIndexRelation {
            provider: provider.clone(),
        }),
        Arc::new(IndexRepositoryRelation {
            provider: provider.clone(),
        }),
        Arc::new(IndexRevisionRelation {
            provider: provider.clone(),
        }),
        Arc::new(IndexProviderRelation {
            provider: provider.clone(),
        }),
        Arc::new(IndexStatusRelation {
            provider: provider.clone(),
        }),
        Arc::new(IndexVersionRelation {
            provider: provider.clone(),
        }),
        Arc::new(IndexBuildErrorRelation {
            provider: provider.clone(),
        }),
        Arc::new(RepositoryVcsRelation {
            provider: provider.clone(),
        }),
        Arc::new(RefTargetRelation {
            provider: provider.clone(),
        }),
        Arc::new(GitRefTargetRelation {
            provider: provider.clone(),
        }),
        Arc::new(GitReceivedRefUpdateRelation {
            provider: provider.clone(),
        }),
        Arc::new(CommitExistsRelation {
            provider: provider.clone(),
        }),
        Arc::new(CommitTreeRelation {
            provider: provider.clone(),
        }),
        Arc::new(CommitAuthorRelation {
            provider: provider.clone(),
        }),
        Arc::new(CommitMessageRelation {
            provider: provider.clone(),
        }),
        Arc::new(CommitParentsRelation {
            provider: provider.clone(),
        }),
        Arc::new(ChangedFilesRelation {
            provider: provider.clone(),
        }),
        Arc::new(FileDiffRelation {
            provider: provider.clone(),
        }),
        Arc::new(FileDiffLineRelation {
            provider: provider.clone(),
        }),
        Arc::new(FileLineProjectionRelation {
            provider: provider.clone(),
        }),
        Arc::new(CommitLogRelation {
            provider: provider.clone(),
        }),
        Arc::new(CommitSearchRelation {
            provider: provider.clone(),
        }),
        Arc::new(FileHistoryRelation {
            provider: provider.clone(),
        }),
        Arc::new(FileBlameRelation {
            provider: provider.clone(),
        }),
        Arc::new(FileBlameHunkRelation {
            provider: provider.clone(),
        }),
    ]
}

#[derive(Debug)]
struct LocalSourceProvider {
    allowed_roots: Vec<PathBuf>,
    semantic_index_path: PathBuf,
    semantic_index_cache: Mutex<Option<CachedSemanticIndex>>,
    source_document_cache: Mutex<HashMap<PathBuf, Arc<CachedSourceDocument>>>,
    rust_analyzer: RustAnalyzerProvider,
    vcs_providers: Mutex<HashMap<PathBuf, Arc<VcsProvider>>>,
}

impl LocalSourceProvider {
    fn from_env() -> Self {
        let configured_roots = env::var_os("MICA_SOURCE_ROOTS")
            .map(|roots| env::split_paths(&roots).collect::<Vec<_>>())
            .or_else(|| env::var_os("MICA_SOURCE_ROOT").map(|root| vec![PathBuf::from(root)]))
            .unwrap_or_else(|| vec![env::current_dir().unwrap_or_else(|_| PathBuf::from("."))]);
        let allowed_roots = configured_roots
            .into_iter()
            .filter_map(|root| root.canonicalize().ok())
            .collect();
        let semantic_index_path = env::var_os("MICA_SOURCE_INDEX")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(".cache/source-index/mica-worktree.json"));
        Self {
            allowed_roots,
            semantic_index_path,
            semantic_index_cache: Mutex::new(None),
            source_document_cache: Mutex::new(HashMap::new()),
            rust_analyzer: RustAnalyzerProvider::from_env(),
            vcs_providers: Mutex::new(HashMap::new()),
        }
    }

    fn repository_root(
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

    fn vcs_provider_for(
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

    fn allowed_git_dir(&self, relation: RelationId, git_dir: &str) -> Result<PathBuf, KernelError> {
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

    fn resolve_path(
        &self,
        reader: &dyn ComputedRelationRead,
        relation: RelationId,
        repository: &Value,
        revision: &Value,
        relative_path: &str,
    ) -> Result<(PathBuf, PathBuf), KernelError> {
        validate_relative_path(relation, relative_path)?;
        let root = self.repository_root(reader, relation, repository, revision)?;
        let safe_path = if relative_path == "." || relative_path == "./" {
            ""
        } else {
            relative_path
        };
        let candidate = root.join(safe_path);
        let absolute = candidate.canonicalize().map_err(|error| {
            invalid_relation(
                relation,
                format!(
                    "failed to resolve path {} from repository root {} and relative path {}: {error}",
                    candidate.display(),
                    root.display(),
                    relative_path
                ),
            )
        })?;
        if !absolute.starts_with(&root) {
            return Err(invalid_relation(
                relation,
                "source path escapes repository root",
            ));
        }
        Ok((root, absolute))
    }

    fn source_document(
        &self,
        relation: RelationId,
        path: &Path,
    ) -> Result<Arc<CachedSourceDocument>, KernelError> {
        let key = source_document_key(relation, path)?;
        if let Some(cached) = self.source_document_cache.lock().unwrap().get(path)
            && cached.key == key
        {
            return Ok(cached.clone());
        }

        let bytes = read_file_bytes(relation, path)?;
        let text = String::from_utf8(bytes).map_err(|error| {
            invalid_relation(relation, format!("source file is not utf-8: {error}"))
        })?;
        let document = Arc::new(CachedSourceDocument {
            key,
            hash: content_hash(text.as_bytes()),
            line_count: text.lines().count().max(1),
            text,
            syntax: OnceLock::new(),
        });

        let mut cache = self.source_document_cache.lock().unwrap();
        if cache.len() >= SOURCE_DOCUMENT_CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(path.to_path_buf(), document.clone());
        Ok(document)
    }

    fn semantic_index(
        &self,
        relation: RelationId,
    ) -> Result<Arc<PersistentSemanticIndex>, KernelError> {
        let mut cache = self.semantic_index_cache.lock().unwrap();
        if let Some(cached) = cache.as_ref()
            && cached.last_checked.elapsed() < SEMANTIC_INDEX_REFRESH_INTERVAL
        {
            return Ok(cached.index.clone());
        }

        let key = semantic_index_key(relation, &self.semantic_index_path)?;
        if let Some(cached) = cache.as_ref()
            && cached.key == key
        {
            let index = cached.index.clone();
            *cache = Some(CachedSemanticIndex {
                key,
                last_checked: Instant::now(),
                index: index.clone(),
            });
            return Ok(index);
        }
        let index = Arc::new(PersistentSemanticIndex::load(
            relation,
            &self.semantic_index_path,
        )?);
        *cache = Some(CachedSemanticIndex {
            key,
            last_checked: Instant::now(),
            index: index.clone(),
        });
        Ok(index)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SemanticIndexKey {
    len: u64,
    modified: Option<SystemTime>,
}

#[derive(Clone, Debug)]
struct CachedSemanticIndex {
    key: Option<SemanticIndexKey>,
    last_checked: Instant,
    index: Arc<PersistentSemanticIndex>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceDocumentKey {
    len: u64,
    modified: Option<SystemTime>,
}

#[derive(Debug)]
struct CachedSourceDocument {
    key: SourceDocumentKey,
    text: String,
    hash: String,
    line_count: usize,
    syntax: OnceLock<SyntaxDocument>,
}

impl CachedSourceDocument {
    fn syntax(&self, path: &str) -> &SyntaxDocument {
        self.syntax
            .get_or_init(|| SyntaxDocument::parse(path, &self.text))
    }
}

fn source_document_key(
    relation: RelationId,
    path: &Path,
) -> Result<SourceDocumentKey, KernelError> {
    let metadata = fs::metadata(path).map_err(|error| {
        invalid_relation(
            relation,
            format!("failed to stat source file {}: {error}", path.display()),
        )
    })?;
    Ok(SourceDocumentKey {
        len: metadata.len(),
        modified: metadata.modified().ok(),
    })
}

fn semantic_index_key(
    relation: RelationId,
    path: &Path,
) -> Result<Option<SemanticIndexKey>, KernelError> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(Some(SemanticIndexKey {
            len: metadata.len(),
            modified: metadata.modified().ok(),
        })),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(invalid_relation(
            relation,
            format!("failed to stat semantic index {}: {error}", path.display()),
        )),
    }
}

fn repository_index_name(
    reader: &dyn ComputedRelationRead,
    relation: RelationId,
    repository: &Value,
) -> Result<Option<String>, KernelError> {
    let Some(name_relation) = relation_id(reader, "source/RepositoryName", 2) else {
        return Ok(None);
    };
    let rows = reader.scan_relation(name_relation, &[Some(repository.clone()), None])?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    row.values()[1]
        .with_str(str::to_owned)
        .ok_or_else(|| invalid_relation(relation, "repository name must be a string"))
        .map(Some)
}

struct RepositoryEntryRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for RepositoryEntryRelation {
    fn name(&self) -> &'static str {
        "local-source-repository-entry"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/RepositoryEntry") && metadata.arity() == 6
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        REPOSITORY_ENTRY_BOUND
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let repository = bound_value(metadata.id(), bindings, 0, "repository")?;
        let revision = bound_value(metadata.id(), bindings, 1, "revision")?;
        let parent = bound_string(metadata.id(), bindings, 2, "parent path")?;
        let (root, directory) =
            self.provider
                .resolve_path(reader, metadata.id(), &repository, &revision, &parent)?;
        if !directory.is_dir() {
            return Err(invalid_relation(
                metadata.id(),
                "repository entry parent path must be a directory",
            ));
        }

        let mut rows = fs::read_dir(&directory)
            .map_err(|error| {
                invalid_relation(metadata.id(), format!("failed to list directory: {error}"))
            })?
            .map(|entry| {
                let entry = entry.map_err(|error| {
                    invalid_relation(
                        metadata.id(),
                        format!("failed to read directory entry: {error}"),
                    )
                })?;
                let path = entry.path();
                let kind = if path.is_dir() { "directory" } else { "file" };
                if kind == "file" && fs::read_to_string(&path).is_err() {
                    return Ok(None);
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                let relative = path.strip_prefix(&root).map_err(|_| {
                    invalid_relation(metadata.id(), "directory entry escaped repository root")
                })?;
                let relative = path_to_mica_string(metadata.id(), relative)?;
                Ok(Some(Tuple::from([
                    repository.clone(),
                    revision.clone(),
                    Value::string(&parent),
                    Value::string(relative),
                    Value::string(kind),
                    Value::string(name),
                ])))
            })
            .collect::<Result<Vec<_>, KernelError>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        rows.sort_by(|left, right| left.values().cmp(right.values()));
        Ok(filter_bound_rows(rows, bindings))
    }
}

struct FileTextRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for FileTextRelation {
    fn name(&self) -> &'static str {
        "local-source-file-text"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/FileText") && metadata.arity() == 5
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        FILE_TEXT_BOUND
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let repository = bound_value(metadata.id(), bindings, 0, "repository")?;
        let revision = bound_value(metadata.id(), bindings, 1, "revision")?;
        let path = bound_string(metadata.id(), bindings, 2, "path")?;
        let (_root, file) =
            self.provider
                .resolve_path(reader, metadata.id(), &repository, &revision, &path)?;
        let document = self.provider.source_document(metadata.id(), &file)?;
        Ok(filter_bound_rows(
            vec![Tuple::from([
                repository,
                revision,
                Value::string(path),
                Value::string(&document.text),
                Value::string(&document.hash),
            ])],
            bindings,
        ))
    }
}

struct FileLinesRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for FileLinesRelation {
    fn name(&self) -> &'static str {
        "local-source-file-lines"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/FileLines") && metadata.arity() == 7
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        FILE_LINES_BOUND
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let repository = bound_value(metadata.id(), bindings, 0, "repository")?;
        let revision = bound_value(metadata.id(), bindings, 1, "revision")?;
        let path = bound_string(metadata.id(), bindings, 2, "path")?;
        let start_line = bound_positive_int(metadata.id(), bindings, 3, "start line")?;
        let line_count = bound_non_negative_int(metadata.id(), bindings, 4, "line count")?;
        let (_root, file) =
            self.provider
                .resolve_path(reader, metadata.id(), &repository, &revision, &path)?;
        let document = self.provider.source_document(metadata.id(), &file)?;
        let lines = document
            .text
            .lines()
            .skip(start_line.saturating_sub(1))
            .take(line_count)
            .map(Value::string)
            .collect::<Vec<_>>();
        Ok(filter_bound_rows(
            vec![Tuple::from([
                repository,
                revision,
                Value::string(path),
                int_value(metadata.id(), start_line as i64)?,
                int_value(metadata.id(), line_count as i64)?,
                Value::list(lines),
                Value::string(&document.hash),
            ])],
            bindings,
        ))
    }
}

struct FileLineCountRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for FileLineCountRelation {
    fn name(&self) -> &'static str {
        "local-source-file-line-count"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/FileLineCount") && metadata.arity() == 4
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        FILE_LINE_COUNT_BOUND
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let repository = bound_value(metadata.id(), bindings, 0, "repository")?;
        let revision = bound_value(metadata.id(), bindings, 1, "revision")?;
        let path = bound_string(metadata.id(), bindings, 2, "path")?;
        let (_root, file) =
            self.provider
                .resolve_path(reader, metadata.id(), &repository, &revision, &path)?;
        let document = self.provider.source_document(metadata.id(), &file)?;
        Ok(filter_bound_rows(
            vec![Tuple::from([
                repository,
                revision,
                Value::string(path),
                int_value(metadata.id(), document.line_count as i64)?,
            ])],
            bindings,
        ))
    }
}

struct FileContentHashRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for FileContentHashRelation {
    fn name(&self) -> &'static str {
        "local-source-file-content-hash"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/FileContentHash") && metadata.arity() == 4
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        FILE_CONTENT_HASH_BOUND
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let repository = bound_value(metadata.id(), bindings, 0, "repository")?;
        let revision = bound_value(metadata.id(), bindings, 1, "revision")?;
        let path = bound_string(metadata.id(), bindings, 2, "path")?;
        let (_root, file) =
            self.provider
                .resolve_path(reader, metadata.id(), &repository, &revision, &path)?;
        let document = self.provider.source_document(metadata.id(), &file)?;
        Ok(filter_bound_rows(
            vec![Tuple::from([
                repository,
                revision,
                Value::string(path),
                Value::string(&document.hash),
            ])],
            bindings,
        ))
    }
}

struct SyntaxLineRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for SyntaxLineRelation {
    fn name(&self) -> &'static str {
        "local-source-syntax-line"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/SyntaxLine") && metadata.arity() == 8
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        SYNTAX_LINE_BOUND
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let repository = bound_value(metadata.id(), bindings, 0, "repository")?;
        let revision = bound_value(metadata.id(), bindings, 1, "revision")?;
        let path = bound_string(metadata.id(), bindings, 2, "path")?;
        let start_line = bound_positive_int(metadata.id(), bindings, 3, "start line")?;
        let line_count = bound_non_negative_int(metadata.id(), bindings, 4, "line count")?;
        let (_root, file) =
            self.provider
                .resolve_path(reader, metadata.id(), &repository, &revision, &path)?;
        let document = self.provider.source_document(metadata.id(), &file)?;
        let syntax = document.syntax(&path);
        let rows = syntax_lines(metadata.id(), syntax, start_line, line_count)?
            .into_iter()
            .map(|line| {
                Ok(Tuple::from([
                    repository.clone(),
                    revision.clone(),
                    Value::string(&path),
                    int_value(metadata.id(), start_line as i64)?,
                    int_value(metadata.id(), line_count as i64)?,
                    int_value(metadata.id(), line.number as i64)?,
                    Value::list(line.segments),
                    Value::string(&document.hash),
                ]))
            })
            .collect::<Result<Vec<_>, KernelError>>()?;
        Ok(filter_bound_rows(rows, bindings))
    }
}

struct SyntaxOutlineRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for SyntaxOutlineRelation {
    fn name(&self) -> &'static str {
        "local-source-syntax-outline"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/SyntaxOutline") && metadata.arity() == 10
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        SYNTAX_OUTLINE_BOUND
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let repository = bound_value(metadata.id(), bindings, 0, "repository")?;
        let revision = bound_value(metadata.id(), bindings, 1, "revision")?;
        let path = bound_string(metadata.id(), bindings, 2, "path")?;
        let (_root, file) =
            self.provider
                .resolve_path(reader, metadata.id(), &repository, &revision, &path)?;
        let document = self.provider.source_document(metadata.id(), &file)?;
        let syntax = document.syntax(&path);
        let rows = syntax
            .outline
            .iter()
            .map(|item| {
                Ok(Tuple::from([
                    repository.clone(),
                    revision.clone(),
                    Value::string(&path),
                    Value::string(&item.node),
                    Value::string(&item.kind),
                    Value::string(&item.name),
                    int_value(metadata.id(), item.start_line as i64)?,
                    int_value(metadata.id(), item.end_line as i64)?,
                    int_value(metadata.id(), item.start_byte as i64)?,
                    int_value(metadata.id(), item.end_byte as i64)?,
                ]))
            })
            .collect::<Result<Vec<_>, KernelError>>()?;
        Ok(filter_bound_rows(rows, bindings))
    }
}

struct SyntaxNodeAtRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for SyntaxNodeAtRelation {
    fn name(&self) -> &'static str {
        "local-source-syntax-node-at"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/SyntaxNodeAt") && metadata.arity() == 11
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        SYNTAX_NODE_AT_BOUND
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let repository = bound_value(metadata.id(), bindings, 0, "repository")?;
        let revision = bound_value(metadata.id(), bindings, 1, "revision")?;
        let path = bound_string(metadata.id(), bindings, 2, "path")?;
        let byte_offset = bound_non_negative_int(metadata.id(), bindings, 3, "byte offset")?;
        let (_root, file) =
            self.provider
                .resolve_path(reader, metadata.id(), &repository, &revision, &path)?;
        let document = self.provider.source_document(metadata.id(), &file)?;
        if byte_offset > document.text.len() {
            return Err(invalid_relation(
                metadata.id(),
                "byte offset is beyond source file length",
            ));
        }
        let syntax = document.syntax(&path);
        let item = syntax.node_at(byte_offset);
        let rows = if let Some(item) = item {
            vec![Tuple::from([
                repository,
                revision,
                Value::string(path),
                int_value(metadata.id(), byte_offset as i64)?,
                Value::string(item.node),
                Value::string(item.kind),
                Value::string(item.name),
                int_value(metadata.id(), item.start_line as i64)?,
                int_value(metadata.id(), item.end_line as i64)?,
                int_value(metadata.id(), item.start_byte as i64)?,
                int_value(metadata.id(), item.end_byte as i64)?,
            ])]
        } else {
            Vec::new()
        };
        Ok(filter_bound_rows(rows, bindings))
    }
}

struct DefinitionAtRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for DefinitionAtRelation {
    fn name(&self) -> &'static str {
        "rust-analyzer-definition-at"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/DefinitionAt") && metadata.arity() == 13
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        DEFINITION_AT_BOUND
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let repository = bound_value(metadata.id(), bindings, 0, "repository")?;
        let revision = bound_value(metadata.id(), bindings, 1, "revision")?;
        let path = bound_string(metadata.id(), bindings, 2, "path")?;
        let byte_offset = bound_non_negative_int(metadata.id(), bindings, 3, "byte offset")?;
        let (root, file) =
            self.provider
                .resolve_path(reader, metadata.id(), &repository, &revision, &path)?;
        let document = self.provider.source_document(metadata.id(), &file)?;
        if byte_offset > document.text.len() {
            return Err(invalid_relation(
                metadata.id(),
                "byte offset is beyond source file length",
            ));
        }
        let index = self.provider.semantic_index(metadata.id())?;
        let repository_name = repository_index_name(reader, metadata.id(), &repository)?;
        let indexed_rows = index
            .definition_at(repository_name.as_deref(), &path, byte_offset)
            .into_iter()
            .map(|symbol| {
                Ok(Tuple::from([
                    repository.clone(),
                    revision.clone(),
                    Value::string(&path),
                    int_value(metadata.id(), byte_offset as i64)?,
                    Value::string(symbol.symbol),
                    Value::string(symbol.name),
                    Value::string(symbol.kind),
                    Value::string(symbol.path),
                    int_value(metadata.id(), symbol.start_line as i64)?,
                    int_value(metadata.id(), symbol.end_line as i64)?,
                    int_value(metadata.id(), symbol.start_byte as i64)?,
                    int_value(metadata.id(), symbol.end_byte as i64)?,
                    Value::string(format!("{} {}", index.provider, index.version)),
                ]))
            })
            .collect::<Result<Vec<_>, KernelError>>()?;
        if !indexed_rows.is_empty() {
            return Ok(filter_bound_rows(indexed_rows, bindings));
        }
        if SourceLanguage::from_path(&path) != SourceLanguage::Rust {
            return Ok(Vec::new());
        }
        let mut locations = self
            .provider
            .rust_analyzer
            .definition(
                &rust_workspace_root(&root),
                &file,
                &document.text,
                byte_offset_to_lsp_position(metadata.id(), &document.text, byte_offset)?,
            )
            .unwrap_or_default();
        if locations.is_empty()
            && let Some(ch) = document
                .text
                .get(byte_offset..)
                .and_then(|text| text.chars().next())
        {
            let inner_offset = byte_offset + ch.len_utf8();
            if inner_offset <= document.text.len() {
                locations = self
                    .provider
                    .rust_analyzer
                    .definition(
                        &rust_workspace_root(&root),
                        &file,
                        &document.text,
                        byte_offset_to_lsp_position(metadata.id(), &document.text, inner_offset)?,
                    )
                    .unwrap_or_default();
            }
        }
        let mut rows = Vec::new();
        for location in locations {
            let Some(location) = semantic_location(metadata.id(), &root, location)? else {
                continue;
            };
            let symbol = semantic_symbol(&location);
            rows.push(Tuple::from([
                repository.clone(),
                revision.clone(),
                Value::string(&path),
                int_value(metadata.id(), byte_offset as i64)?,
                Value::string(symbol),
                Value::string(location.name),
                Value::string(location.kind),
                Value::string(location.path),
                int_value(metadata.id(), location.start_line as i64)?,
                int_value(metadata.id(), location.end_line as i64)?,
                int_value(metadata.id(), location.start_byte as i64)?,
                int_value(metadata.id(), location.end_byte as i64)?,
                Value::string(location.provider),
            ]));
        }
        Ok(filter_bound_rows(rows, bindings))
    }
}

struct ReferencesOfRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for ReferencesOfRelation {
    fn name(&self) -> &'static str {
        "rust-analyzer-references-of"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/ReferencesOf") && metadata.arity() == 10
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        REFERENCES_OF_BOUND
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let repository = bound_value(metadata.id(), bindings, 0, "repository")?;
        let revision = bound_value(metadata.id(), bindings, 1, "revision")?;
        let symbol = bound_string(metadata.id(), bindings, 2, "symbol")?;
        let Some(request) = SemanticSymbol::parse(&symbol) else {
            return Ok(Vec::new());
        };
        let index = self.provider.semantic_index(metadata.id())?;
        let repository_name = repository_index_name(reader, metadata.id(), &repository)?;
        if request.provider == SemanticSymbolProvider::Index {
            let rows = index
                .references_of(repository_name.as_deref(), &request)
                .into_iter()
                .map(|reference| {
                    Ok(Tuple::from([
                        repository.clone(),
                        revision.clone(),
                        Value::string(&symbol),
                        Value::string(reference.path),
                        int_value(metadata.id(), reference.start_line as i64)?,
                        int_value(metadata.id(), reference.end_line as i64)?,
                        int_value(metadata.id(), reference.start_byte as i64)?,
                        int_value(metadata.id(), reference.end_byte as i64)?,
                        Value::string(format!("{} {}", index.provider, index.version)),
                        Value::string(reference.name),
                    ]))
                })
                .collect::<Result<Vec<_>, KernelError>>()?;
            return Ok(filter_bound_rows(rows, bindings));
        }
        let root = self
            .provider
            .repository_root(reader, metadata.id(), &repository, &revision)?;
        validate_relative_path(metadata.id(), &request.path)?;
        let file = root.join(&request.path).canonicalize().map_err(|error| {
            invalid_relation(
                metadata.id(),
                format!("failed to resolve symbol path: {error}"),
            )
        })?;
        if !file.starts_with(&root) {
            return Err(invalid_relation(
                metadata.id(),
                "symbol path escapes repository root",
            ));
        }
        let document = self.provider.source_document(metadata.id(), &file)?;
        let position =
            byte_offset_to_lsp_position(metadata.id(), &document.text, request.start_byte)?;
        let locations = self
            .provider
            .rust_analyzer
            .references(&rust_workspace_root(&root), &file, &document.text, position)
            .unwrap_or_default();
        let mut rows = Vec::new();
        for location in locations {
            let Some(location) = semantic_location(metadata.id(), &root, location)? else {
                continue;
            };
            rows.push(Tuple::from([
                repository.clone(),
                revision.clone(),
                Value::string(&symbol),
                Value::string(location.path),
                int_value(metadata.id(), location.start_line as i64)?,
                int_value(metadata.id(), location.end_line as i64)?,
                int_value(metadata.id(), location.start_byte as i64)?,
                int_value(metadata.id(), location.end_byte as i64)?,
                Value::string(location.provider),
                Value::string(location.name),
            ]));
        }
        Ok(filter_bound_rows(rows, bindings))
    }
}

struct SymbolSearchRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for SymbolSearchRelation {
    fn name(&self) -> &'static str {
        "persistent-source-symbol-search"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/SymbolSearch") && metadata.arity() == 11
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        SYMBOL_SEARCH_BOUND
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let repository = bound_value(metadata.id(), bindings, 0, "repository")?;
        let revision = bound_value(metadata.id(), bindings, 1, "revision")?;
        let query = bound_string(metadata.id(), bindings, 2, "query")?;
        let limit = bound_non_negative_int(metadata.id(), bindings, 3, "limit")?;
        let index = self.provider.semantic_index(metadata.id())?;
        let repository_name = repository_index_name(reader, metadata.id(), &repository)?;
        let rows = index
            .search(repository_name.as_deref(), &query, limit)
            .into_iter()
            .map(|symbol| {
                Ok(Tuple::from([
                    repository.clone(),
                    revision.clone(),
                    Value::string(&query),
                    int_value(metadata.id(), limit as i64)?,
                    Value::string(symbol.symbol),
                    Value::string(symbol.name),
                    Value::string(symbol.kind),
                    Value::string(symbol.path),
                    int_value(metadata.id(), symbol.start_line as i64)?,
                    int_value(metadata.id(), symbol.end_line as i64)?,
                    Value::string(format!("{} {}", index.provider, index.version)),
                ]))
            })
            .collect::<Result<Vec<_>, KernelError>>()?;
        Ok(filter_bound_rows(rows, bindings))
    }
}

struct IndexedTextUnitRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for IndexedTextUnitRelation {
    fn name(&self) -> &'static str {
        "persistent-source-indexed-text-unit"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/IndexedTextUnit") && metadata.arity() == 9
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        INDEXED_TEXT_UNIT_BOUND
    }

    fn scan(
        &self,
        _reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let index = self.provider.semantic_index(metadata.id())?;
        if !index.is_complete() {
            return Ok(Vec::new());
        }
        let unit_filter = bindings.first().and_then(Option::as_ref);
        let rows = index
            .text_units
            .iter()
            .filter(|unit| {
                unit_filter.is_none_or(|filter| {
                    filter.with_str(|value| value == unit.unit).unwrap_or(false)
                })
            })
            .map(|unit| {
                Ok(Tuple::from([
                    Value::string(&unit.unit),
                    int_value(metadata.id(), unit.ordinal as i64)?,
                    Value::string(&unit.kind),
                    Value::string(&unit.title),
                    Value::string(&unit.path),
                    int_value(metadata.id(), unit.start_line as i64)?,
                    int_value(metadata.id(), unit.end_line as i64)?,
                    Value::string(&unit.model),
                    Value::string(&unit.text),
                ]))
            })
            .collect::<Result<Vec<_>, KernelError>>()?;
        Ok(filter_bound_rows(rows, bindings))
    }
}

struct IndexedFileRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for IndexedFileRelation {
    fn name(&self) -> &'static str {
        "persistent-source-indexed-file"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/IndexedFile") && metadata.arity() == 6
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        INDEXED_FILE_BOUND
    }

    fn scan(
        &self,
        _reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let index = self.provider.semantic_index(metadata.id())?;
        if !index.is_complete() {
            return Ok(Vec::new());
        }

        let mut files = BTreeMap::<(&str, &str), &IndexedTextUnit>::new();
        for unit in &index.text_units {
            files
                .entry((unit.repository.as_str(), unit.path.as_str()))
                .or_insert(unit);
        }

        let rows = files
            .into_values()
            .map(|unit| {
                Ok(Tuple::from([
                    Value::string(&index.id),
                    Value::string(&unit.repository),
                    Value::string(&unit.path),
                    Value::string(&unit.title),
                    Value::string(indexed_file_language(&unit.path)),
                    Value::string(&unit.model),
                ]))
            })
            .collect::<Result<Vec<_>, KernelError>>()?;
        Ok(filter_bound_rows(rows, bindings))
    }
}

struct TextSearchRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for TextSearchRelation {
    fn name(&self) -> &'static str {
        "persistent-source-text-search"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/TextSearch") && metadata.arity() == 11
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        TEXT_SEARCH_BOUND
    }

    fn scan(
        &self,
        _reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let query = bound_string(metadata.id(), bindings, 0, "query")?;
        let limit = bound_non_negative_int(metadata.id(), bindings, 1, "limit")?;
        let scope = bound_string(metadata.id(), bindings, 2, "scope")?;
        let index = self.provider.semantic_index(metadata.id())?;
        if !index.is_complete() || limit == 0 {
            return Ok(Vec::new());
        }

        let rows = text_search(&index, &query, limit, &scope)
            .into_iter()
            .map(|hit| {
                Ok(Tuple::from([
                    Value::string(&query),
                    int_value(metadata.id(), limit as i64)?,
                    Value::string(&scope),
                    Value::string(&hit.unit.unit),
                    int_value(metadata.id(), hit.score)?,
                    Value::string(&hit.unit.path),
                    int_value(metadata.id(), hit.match_line as i64)?,
                    int_value(metadata.id(), hit.match_line as i64)?,
                    Value::string(&hit.unit.kind),
                    Value::string(&hit.unit.title),
                    Value::string(hit.snippet),
                ]))
            })
            .collect::<Result<Vec<_>, KernelError>>()?;
        Ok(filter_bound_rows(rows, bindings))
    }
}

#[derive(Debug)]
struct TextSearchHit<'a> {
    unit: &'a IndexedTextUnit,
    score: i64,
    match_line: usize,
    snippet: String,
}

fn text_search<'a>(
    index: &'a PersistentSemanticIndex,
    query: &str,
    limit: usize,
    scope: &str,
) -> Vec<TextSearchHit<'a>> {
    let terms = search_terms(query);
    if terms.is_empty() {
        return Vec::new();
    }
    let query_lower = query.to_ascii_lowercase();
    let symbol_matches = index
        .symbols
        .iter()
        .filter(|symbol| {
            let name = symbol.name.to_ascii_lowercase();
            name.contains(&query_lower) || terms.iter().any(|term| name.contains(term))
        })
        .collect::<Vec<_>>();

    let mut hits = index
        .text_units
        .iter()
        .filter(|unit| source_text_scope_matches(&unit.path, scope))
        .filter_map(|unit| {
            let mut score = score_text_unit(unit, &query_lower, &terms);
            let mut match_line = text_match_line(unit, &query_lower, &terms);
            for symbol in &symbol_matches {
                if symbol.repository == unit.repository
                    && symbol.path == unit.path
                    && symbol.start_line <= unit.end_line
                    && unit.start_line <= symbol.end_line
                {
                    if symbol.name.eq_ignore_ascii_case(query) {
                        score += 320;
                    } else {
                        score += 190;
                    }
                    if match_line.is_none() {
                        match_line = Some(symbol.start_line);
                    }
                }
            }
            if score == 0 {
                return None;
            }
            Some(TextSearchHit {
                unit,
                score,
                match_line: match_line.unwrap_or(unit.start_line),
                snippet: search_snippet(unit, &query_lower, &terms),
            })
        })
        .collect::<Vec<_>>();

    hits.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.unit.path.cmp(&right.unit.path))
            .then_with(|| left.unit.start_line.cmp(&right.unit.start_line))
            .then_with(|| left.unit.unit.cmp(&right.unit.unit))
    });
    hits.truncate(limit);
    hits
}

fn indexed_file_language(path: &str) -> &'static str {
    match SourceLanguage::from_path(path) {
        SourceLanguage::Rust => "rust",
        SourceLanguage::Mica => "mica",
        SourceLanguage::Markdown => "markdown",
        SourceLanguage::JavaScript => "javascript",
        SourceLanguage::Plain => "file",
    }
}

fn search_terms(query: &str) -> Vec<String> {
    let mut terms = query
        .split(|character: char| {
            !(character.is_ascii_alphanumeric() || character == '_' || character == '-')
        })
        .map(str::trim)
        .filter(|term| !term.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    terms.sort();
    terms.dedup();
    terms
}

fn score_text_unit(unit: &IndexedTextUnit, query_lower: &str, terms: &[String]) -> i64 {
    let path = unit.path.to_ascii_lowercase();
    let title = unit.title.to_ascii_lowercase();
    let kind = unit.kind.to_ascii_lowercase();
    let text = unit.text.to_ascii_lowercase();
    let file_name = Path::new(&unit.path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&unit.path)
        .to_ascii_lowercase();

    let mut score = 0;
    if path.contains(query_lower) {
        score += 260;
    }
    if title.contains(query_lower) {
        score += 210;
    }
    if text.contains(query_lower) {
        score += 120;
    }

    for term in terms {
        if file_name == *term {
            score += 240;
        } else if file_name.contains(term) {
            score += 180;
        }
        if path.contains(term) {
            score += 110;
        }
        if title.contains(term) {
            score += 95;
        }
        if text.contains(term) {
            score += 55;
        }
        if kind.contains(term) {
            score += 20;
        }
    }
    score
}

fn text_match_line(unit: &IndexedTextUnit, query_lower: &str, terms: &[String]) -> Option<usize> {
    let lower = unit.text.to_ascii_lowercase();
    let mut position = if query_lower.is_empty() {
        None
    } else {
        lower.find(query_lower)
    };
    if position.is_none() {
        position = terms.iter().filter_map(|term| lower.find(term)).min();
    }
    let position = position?;
    let mut relative_line = unit.text[..position]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count();
    if unit
        .text
        .find('\n')
        .is_some_and(|header_end| position > header_end)
    {
        relative_line = relative_line.saturating_sub(1);
    }
    Some(unit.start_line + relative_line)
}

fn source_text_scope_matches(path: &str, scope: &str) -> bool {
    match scope {
        "all" => true,
        "docs" => source_text_scope(path) == "docs",
        "code" => source_text_scope(path) == "code",
        "tests" => source_text_scope(path) == "tests",
        "benches" => source_text_scope(path) == "benches",
        "sketches" => source_text_scope(path) == "sketches",
        _ => true,
    }
}

fn source_text_scope(path: &str) -> &'static str {
    if path.starts_with("sketches/") {
        return "sketches";
    }
    if path.contains("/benches/") || path.starts_with("benches/") {
        return "benches";
    }
    if path.contains("/tests/") || path.starts_with("tests/") || path.ends_with("_test.rs") {
        return "tests";
    }
    if path.ends_with(".md") || path.ends_with(".markdown") || path.starts_with("docs/") {
        return "docs";
    }
    "code"
}

fn search_snippet(unit: &IndexedTextUnit, query_lower: &str, terms: &[String]) -> String {
    let body = normalize_search_text(&unit.text);
    if let Some(snippet) = snippet_from_match(&body, query_lower, terms) {
        return snippet;
    }
    let combined = normalize_search_text(&format!("{} {} {}", unit.path, unit.title, unit.text));
    snippet_from_match(&combined, query_lower, terms)
        .unwrap_or_else(|| clip_chars(&combined, 0, 260))
}

fn normalize_search_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn snippet_from_match(text: &str, query_lower: &str, terms: &[String]) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let mut position = if query_lower.is_empty() {
        None
    } else {
        lower.find(query_lower)
    };
    if position.is_none() {
        position = terms.iter().filter_map(|term| lower.find(term)).min();
    }
    let position = position?;
    let prefix_chars = text[..position].chars().count();
    let start = prefix_chars.saturating_sub(70);
    let end = prefix_chars + 190;
    let mut snippet = clip_chars(text, start, end);
    if start > 0 {
        snippet = format!("...{snippet}");
    }
    if text.chars().count() > end {
        snippet.push_str("...");
    }
    Some(snippet)
}

fn clip_chars(text: &str, start: usize, end: usize) -> String {
    text.chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

macro_rules! index_value_relation {
    ($name:ident, $relation:literal, $field:ident) => {
        struct $name {
            provider: Arc<LocalSourceProvider>,
        }

        impl ComputedRelation for $name {
            fn name(&self) -> &'static str {
                concat!("persistent-source-", $relation)
            }

            fn matches(&self, metadata: &RelationMetadata) -> bool {
                metadata.name().name() == Some(concat!("source/", $relation))
                    && metadata.arity() == 2
            }

            fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
                INDEX_VALUE_BOUND
            }

            fn scan(
                &self,
                _reader: &dyn ComputedRelationRead,
                metadata: &RelationMetadata,
                bindings: &[Option<Value>],
            ) -> Result<Vec<Tuple>, KernelError> {
                let index = self.provider.semantic_index(metadata.id())?;
                Ok(filter_bound_rows(
                    vec![Tuple::from([
                        Value::string(&index.id),
                        Value::string(&index.$field),
                    ])],
                    bindings,
                ))
            }
        }
    };
}

struct SourceIndexRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for SourceIndexRelation {
    fn name(&self) -> &'static str {
        "persistent-source-index"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/SourceIndex") && metadata.arity() == 1
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        &[]
    }

    fn scan(
        &self,
        _reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let index = self.provider.semantic_index(metadata.id())?;
        Ok(filter_bound_rows(
            vec![Tuple::from([Value::string(&index.id)])],
            bindings,
        ))
    }
}

struct IndexRepositoryRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for IndexRepositoryRelation {
    fn name(&self) -> &'static str {
        "persistent-source-index-repository"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/IndexRepository") && metadata.arity() == 2
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        INDEX_VALUE_BOUND
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let index = self.provider.semantic_index(metadata.id())?;
        let repository_relation = relation_id(reader, "source/Repository", 1).ok_or_else(|| {
            invalid_relation(metadata.id(), "missing relation source/Repository/1")
        })?;
        let mut rows = Vec::new();
        for row in reader.scan_relation(repository_relation, &[None])? {
            let repository = row.values()[0].clone();
            let name = repository_index_name(reader, metadata.id(), &repository)?;
            if name
                .as_deref()
                .is_none_or(|name| index.covers_repository(name))
            {
                rows.push(Tuple::from([Value::string(&index.id), repository]));
            }
        }
        Ok(filter_bound_rows(rows, bindings))
    }
}

struct IndexRevisionRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for IndexRevisionRelation {
    fn name(&self) -> &'static str {
        "persistent-source-index-revision"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/IndexRevision") && metadata.arity() == 2
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        INDEX_VALUE_BOUND
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let index = self.provider.semantic_index(metadata.id())?;
        let revision_relation = relation_id(reader, "source/Revision", 1)
            .ok_or_else(|| invalid_relation(metadata.id(), "missing relation source/Revision/1"))?;
        let revision_of_relation =
            relation_id(reader, "source/RevisionOf", 2).ok_or_else(|| {
                invalid_relation(metadata.id(), "missing relation source/RevisionOf/2")
            })?;
        let mut rows = Vec::new();
        for row in reader.scan_relation(revision_relation, &[None])? {
            let revision = row.values()[0].clone();
            let revision_of =
                reader.scan_relation(revision_of_relation, &[Some(revision.clone()), None])?;
            let Some(repository) = revision_of
                .into_iter()
                .next()
                .map(|row| row.values()[1].clone())
            else {
                rows.push(Tuple::from([Value::string(&index.id), revision]));
                continue;
            };
            let name = repository_index_name(reader, metadata.id(), &repository)?;
            if name
                .as_deref()
                .is_none_or(|name| index.covers_repository(name))
            {
                rows.push(Tuple::from([Value::string(&index.id), revision]));
            }
        }
        Ok(filter_bound_rows(rows, bindings))
    }
}

index_value_relation!(IndexProviderRelation, "IndexProvider", provider);
index_value_relation!(IndexStatusRelation, "IndexStatus", status);
index_value_relation!(IndexVersionRelation, "IndexVersion", version);
index_value_relation!(IndexBuildErrorRelation, "IndexBuildError", error);

struct RepositoryVcsRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for RepositoryVcsRelation {
    fn name(&self) -> &'static str {
        "local-source-repository-vcs"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/RepositoryVcs") && metadata.arity() == 2
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        VCS_REPOSITORY_BOUND
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let repository = bound_value(metadata.id(), bindings, 0, "repository")?;
        let _vcs = self
            .provider
            .vcs_provider_for(reader, metadata.id(), &repository)?;
        Ok(filter_bound_rows(
            vec![Tuple::from([
                repository,
                Value::symbol(Symbol::intern("source/vcs_jj")),
            ])],
            bindings,
        ))
    }
}

struct GitReceivedRefUpdateRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for GitReceivedRefUpdateRelation {
    fn name(&self) -> &'static str {
        "local-source-git-received-ref-update"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/GitReceivedRefUpdate") && metadata.arity() == 12
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        GIT_RECEIVED_REF_UPDATE_BOUND
    }

    fn scan(
        &self,
        _reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let git_dir = bound_string(metadata.id(), bindings, 0, "git dir")?;
        let git_dir_path = self.provider.allowed_git_dir(metadata.id(), &git_dir)?;
        let recorder = GitReceiveRecorder::new(&git_dir_path);
        let updates = recorder
            .read_updates()
            .map_err(|error| invalid_relation(metadata.id(), error))?;
        let mut rows = Vec::new();
        for update in updates {
            let first_parent_id = update.parent_ids.first().cloned().unwrap_or_default();
            rows.push(Tuple::from([
                Value::string(git_dir_path.display().to_string()),
                Value::string(update.update_id),
                Value::string(update.target_ref),
                Value::string(update.ref_name),
                Value::string(update.commit_id),
                Value::string(first_parent_id),
                Value::string(update.change_id_footer.unwrap_or_default()),
                Value::string(update.subject),
                Value::string(update.author_name),
                Value::string(update.author_email),
                int_value(metadata.id(), update.author_time)?,
                int_value(metadata.id(), update.received_at)?,
            ]));
        }
        Ok(filter_bound_rows(rows, bindings))
    }
}

struct CommitExistsRelation {
    provider: Arc<LocalSourceProvider>,
}

struct RefTargetRelation {
    provider: Arc<LocalSourceProvider>,
}

struct GitRefTargetRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for RefTargetRelation {
    fn name(&self) -> &'static str {
        "local-source-ref-target"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/RefTarget") && metadata.arity() == 3
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        VCS_REF_TARGET_BOUND
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let repository = bound_value(metadata.id(), bindings, 0, "repository")?;
        let ref_name = bound_string(metadata.id(), bindings, 1, "ref name")?;
        let vcs = self
            .provider
            .vcs_provider_for(reader, metadata.id(), &repository)?;
        let Some(commit_id) = vcs
            .resolve_ref(&ref_name)
            .map_err(|e| invalid_relation(metadata.id(), e))?
        else {
            return Ok(Vec::new());
        };
        Ok(filter_bound_rows(
            vec![Tuple::from([
                repository,
                Value::string(ref_name),
                Value::string(commit_id.hex()),
            ])],
            bindings,
        ))
    }
}

impl ComputedRelation for GitRefTargetRelation {
    fn name(&self) -> &'static str {
        "local-source-git-ref-target"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/GitRefTarget") && metadata.arity() == 3
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        GIT_REF_TARGET_BOUND
    }

    fn scan(
        &self,
        _reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let git_dir = bound_string(metadata.id(), bindings, 0, "git dir")?;
        let ref_name = bound_string(metadata.id(), bindings, 1, "ref name")?;
        let git_dir_path = self.provider.allowed_git_dir(metadata.id(), &git_dir)?;
        let vcs = VcsProvider::open(&git_dir_path).map_err(|error| {
            invalid_relation(
                metadata.id(),
                format!("failed to open vcs for {}: {error}", git_dir_path.display()),
            )
        })?;
        let Some(commit_id) = vcs
            .resolve_ref(&ref_name)
            .map_err(|e| invalid_relation(metadata.id(), e))?
        else {
            return Ok(Vec::new());
        };
        Ok(filter_bound_rows(
            vec![Tuple::from([
                Value::string(git_dir_path.display().to_string()),
                Value::string(ref_name),
                Value::string(commit_id.hex()),
            ])],
            bindings,
        ))
    }
}

impl ComputedRelation for CommitExistsRelation {
    fn name(&self) -> &'static str {
        "local-source-commit-exists"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/CommitExists") && metadata.arity() == 2
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        VCS_COMMIT_KEY_BOUND
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let repository = bound_value(metadata.id(), bindings, 0, "repository")?;
        let commit_hex = bound_string(metadata.id(), bindings, 1, "commit")?;
        let vcs = self
            .provider
            .vcs_provider_for(reader, metadata.id(), &repository)?;
        let commit_id = vcs
            .resolve_commit(&commit_hex)
            .map_err(|e| invalid_relation(metadata.id(), e))?;
        if vcs
            .commit_exists(&commit_id)
            .map_err(|e| invalid_relation(metadata.id(), e))?
        {
            Ok(filter_bound_rows(
                vec![Tuple::from([repository, Value::string(commit_hex)])],
                bindings,
            ))
        } else {
            Ok(Vec::new())
        }
    }
}

struct CommitTreeRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for CommitTreeRelation {
    fn name(&self) -> &'static str {
        "local-source-commit-tree"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/CommitTree") && metadata.arity() == 3
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        VCS_COMMIT_KEY_BOUND
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let repository = bound_value(metadata.id(), bindings, 0, "repository")?;
        let commit_hex = bound_string(metadata.id(), bindings, 1, "commit")?;
        let vcs = self
            .provider
            .vcs_provider_for(reader, metadata.id(), &repository)?;
        let commit_id = vcs
            .resolve_commit(&commit_hex)
            .map_err(|e| invalid_relation(metadata.id(), e))?;
        let tree_id = vcs
            .commit_tree(&commit_id)
            .map_err(|e| invalid_relation(metadata.id(), e))?;
        Ok(filter_bound_rows(
            vec![Tuple::from([
                repository,
                Value::string(commit_hex),
                Value::string(tree_id.hex()),
            ])],
            bindings,
        ))
    }
}

struct CommitAuthorRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for CommitAuthorRelation {
    fn name(&self) -> &'static str {
        "local-source-commit-author"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/CommitAuthor") && metadata.arity() == 5
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        VCS_COMMIT_KEY_BOUND
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let repository = bound_value(metadata.id(), bindings, 0, "repository")?;
        let commit_hex = bound_string(metadata.id(), bindings, 1, "commit")?;
        let vcs = self
            .provider
            .vcs_provider_for(reader, metadata.id(), &repository)?;
        let commit_id = vcs
            .resolve_commit(&commit_hex)
            .map_err(|e| invalid_relation(metadata.id(), e))?;
        let (name, email, timestamp) = vcs
            .commit_author(&commit_id)
            .map_err(|e| invalid_relation(metadata.id(), e))?;
        Ok(filter_bound_rows(
            vec![Tuple::from([
                repository,
                Value::string(commit_hex),
                Value::string(name),
                Value::string(email),
                int_value(metadata.id(), timestamp)?,
            ])],
            bindings,
        ))
    }
}

struct CommitMessageRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for CommitMessageRelation {
    fn name(&self) -> &'static str {
        "local-source-commit-message"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/CommitMessage") && metadata.arity() == 3
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        VCS_COMMIT_KEY_BOUND
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let repository = bound_value(metadata.id(), bindings, 0, "repository")?;
        let commit_hex = bound_string(metadata.id(), bindings, 1, "commit")?;
        let vcs = self
            .provider
            .vcs_provider_for(reader, metadata.id(), &repository)?;
        let commit_id = vcs
            .resolve_commit(&commit_hex)
            .map_err(|e| invalid_relation(metadata.id(), e))?;
        let message = vcs
            .commit_message(&commit_id)
            .map_err(|e| invalid_relation(metadata.id(), e))?;
        Ok(filter_bound_rows(
            vec![Tuple::from([
                repository,
                Value::string(commit_hex),
                Value::string(message),
            ])],
            bindings,
        ))
    }
}

struct CommitParentsRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for CommitParentsRelation {
    fn name(&self) -> &'static str {
        "local-source-commit-parents"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/CommitParents") && metadata.arity() == 4
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        VCS_COMMIT_KEY_BOUND
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let repository = bound_value(metadata.id(), bindings, 0, "repository")?;
        let commit_hex = bound_string(metadata.id(), bindings, 1, "commit")?;
        let vcs = self
            .provider
            .vcs_provider_for(reader, metadata.id(), &repository)?;
        let commit_id = vcs
            .resolve_commit(&commit_hex)
            .map_err(|e| invalid_relation(metadata.id(), e))?;
        let parents = vcs
            .commit_parents(&commit_id)
            .map_err(|e| invalid_relation(metadata.id(), e))?;
        let tuples: Vec<Tuple> = parents
            .iter()
            .enumerate()
            .map(|(idx, parent)| {
                Tuple::from([
                    repository.clone(),
                    Value::string(&commit_hex),
                    int_value(metadata.id(), idx as i64).unwrap(),
                    Value::string(parent.hex()),
                ])
            })
            .collect();
        Ok(filter_bound_rows(tuples, bindings))
    }
}

struct ChangedFilesRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for ChangedFilesRelation {
    fn name(&self) -> &'static str {
        "local-source-changed-files"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/ChangedFiles") && metadata.arity() == 5
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        VCS_TWO_COMMIT_BOUND
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let repository = bound_value(metadata.id(), bindings, 0, "repository")?;
        let from_hex = bound_string(metadata.id(), bindings, 1, "from_commit")?;
        let to_hex = bound_string(metadata.id(), bindings, 2, "to_commit")?;
        let vcs = self
            .provider
            .vcs_provider_for(reader, metadata.id(), &repository)?;
        let from_id = vcs
            .resolve_commit(&from_hex)
            .map_err(|e| invalid_relation(metadata.id(), e))?;
        let to_id = vcs
            .resolve_commit(&to_hex)
            .map_err(|e| invalid_relation(metadata.id(), e))?;
        let changed = vcs
            .changed_files(&from_id, &to_id)
            .map_err(|e| invalid_relation(metadata.id(), e))?;
        let tuples: Vec<Tuple> = changed
            .into_iter()
            .map(|(path, kind)| {
                Tuple::from([
                    repository.clone(),
                    Value::string(&from_hex),
                    Value::string(&to_hex),
                    Value::string(path),
                    Value::symbol(Symbol::intern(kind.symbol_name())),
                ])
            })
            .collect();
        Ok(filter_bound_rows(tuples, bindings))
    }
}

struct FileDiffRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for FileDiffRelation {
    fn name(&self) -> &'static str {
        "local-source-file-diff"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/FileDiff") && metadata.arity() == 7
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        VCS_TWO_COMMIT_PATH_BOUND
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let repository = bound_value(metadata.id(), bindings, 0, "repository")?;
        let from_hex = bound_string(metadata.id(), bindings, 1, "from_commit")?;
        let to_hex = bound_string(metadata.id(), bindings, 2, "to_commit")?;
        let path = bound_string(metadata.id(), bindings, 3, "path")?;
        let vcs = self
            .provider
            .vcs_provider_for(reader, metadata.id(), &repository)?;
        let from_id = vcs
            .resolve_commit(&from_hex)
            .map_err(|e| invalid_relation(metadata.id(), e))?;
        let to_id = vcs
            .resolve_commit(&to_hex)
            .map_err(|e| invalid_relation(metadata.id(), e))?;
        let diff = vcs
            .file_diff(&from_id, &to_id, &path)
            .map_err(|e| invalid_relation(metadata.id(), e))?;
        Ok(filter_bound_rows(
            diff.into_iter()
                .map(|(kind, text)| {
                    Tuple::from([
                        repository.clone(),
                        Value::string(&from_hex),
                        Value::string(&to_hex),
                        Value::string(&path),
                        Value::symbol(Symbol::intern(kind.symbol_name())),
                        Value::string(&path),
                        Value::string(text),
                    ])
                })
                .collect(),
            bindings,
        ))
    }
}

#[derive(Clone, Debug)]
struct StructuredDiffLine {
    hunk: usize,
    line_index: usize,
    side: &'static str,
    old_line: Option<usize>,
    new_line: Option<usize>,
    kind: &'static str,
    text: String,
}

fn parse_hunk_header(line: &str) -> Option<(usize, usize)> {
    parse_hunk_header_full(line).map(|header| (header.old_start, header.new_start))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HunkHeader {
    old_start: usize,
    old_count: usize,
    new_start: usize,
    new_count: usize,
}

fn parse_hunk_range(part: &str) -> Option<(usize, usize)> {
    let (start, count) = part.split_once(',').unwrap_or((part, "1"));
    Some((start.parse().ok()?, count.parse().ok()?))
}

fn parse_hunk_header_full(line: &str) -> Option<HunkHeader> {
    let rest = line.strip_prefix("@@ -")?;
    let (old_part, rest) = rest.split_once(" +")?;
    let (new_part, _) = rest.split_once(" @@")?;
    let (old_start, old_count) = parse_hunk_range(old_part)?;
    let (new_start, new_count) = parse_hunk_range(new_part)?;
    Some(HunkHeader {
        old_start,
        old_count,
        new_start,
        new_count,
    })
}

fn structured_diff_lines(diff_text: &str) -> Vec<StructuredDiffLine> {
    let mut rows = Vec::new();
    let mut hunk = 0usize;
    let mut line_index = 0usize;
    let mut old_line = 0usize;
    let mut new_line = 0usize;

    for line in diff_text.lines() {
        if line.starts_with("--- ") || line.starts_with("+++ ") {
            continue;
        }
        if let Some((old_start, new_start)) = parse_hunk_header(line) {
            hunk += 1;
            line_index = 0;
            old_line = old_start;
            new_line = new_start;
            continue;
        }
        if hunk == 0 || line.starts_with("\\ No newline") {
            continue;
        }
        if let Some(text) = line.strip_prefix('-') {
            line_index += 1;
            rows.push(StructuredDiffLine {
                hunk,
                line_index,
                side: "source/review_old_side",
                old_line: Some(old_line),
                new_line: None,
                kind: "source/diff_removed",
                text: text.to_owned(),
            });
            old_line += 1;
            continue;
        }
        if let Some(text) = line.strip_prefix('+') {
            line_index += 1;
            rows.push(StructuredDiffLine {
                hunk,
                line_index,
                side: "source/review_new_side",
                old_line: None,
                new_line: Some(new_line),
                kind: "source/diff_added",
                text: text.to_owned(),
            });
            new_line += 1;
            continue;
        }
        if let Some(text) = line.strip_prefix(' ') {
            line_index += 1;
            rows.push(StructuredDiffLine {
                hunk,
                line_index,
                side: "source/review_both_sides",
                old_line: Some(old_line),
                new_line: Some(new_line),
                kind: "source/diff_context",
                text: text.to_owned(),
            });
            old_line += 1;
            new_line += 1;
        }
    }

    rows
}

fn project_line_range(
    diff_text: &str,
    start: usize,
    end: usize,
) -> Option<(usize, usize, &'static str)> {
    if start == 0 || end < start {
        return None;
    }

    let mut delta: isize = 0;
    let mut shifted = false;
    for line in diff_text.lines() {
        let Some(header) = parse_hunk_header_full(line) else {
            continue;
        };
        let old_hunk_end = if header.old_count == 0 {
            header.old_start
        } else {
            header.old_start + header.old_count - 1
        };

        if end < header.old_start {
            break;
        }
        if start > old_hunk_end {
            let hunk_delta = header.new_count as isize - header.old_count as isize;
            if hunk_delta != 0 {
                shifted = true;
            }
            delta += hunk_delta;
            continue;
        }

        let new_start = header.new_start;
        let new_end = if header.new_count == 0 {
            header.new_start
        } else {
            header.new_start + header.new_count - 1
        };
        if header.old_count == header.new_count && start >= header.old_start && end <= old_hunk_end
        {
            let offset = start - header.old_start;
            return Some((
                new_start + offset,
                new_start + offset + (end - start),
                "source/review_projection_fuzzy",
            ));
        }
        return Some((
            new_start,
            new_end.max(new_start),
            "source/review_projection_fuzzy",
        ));
    }

    let projected_start = (start as isize + delta).max(1) as usize;
    let projected_end = (end as isize + delta).max(projected_start as isize) as usize;
    let quality = if shifted {
        "source/review_projection_shifted"
    } else {
        "source/review_projection_exact"
    };
    Some((projected_start, projected_end, quality))
}

struct FileDiffLineRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for FileDiffLineRelation {
    fn name(&self) -> &'static str {
        "local-source-file-diff-line"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/FileDiffLine") && metadata.arity() == 11
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        VCS_TWO_COMMIT_PATH_BOUND
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let repository = bound_value(metadata.id(), bindings, 0, "repository")?;
        let from_hex = bound_string(metadata.id(), bindings, 1, "from_commit")?;
        let to_hex = bound_string(metadata.id(), bindings, 2, "to_commit")?;
        let path = bound_string(metadata.id(), bindings, 3, "path")?;
        let vcs = self
            .provider
            .vcs_provider_for(reader, metadata.id(), &repository)?;
        let from_id = vcs
            .resolve_commit(&from_hex)
            .map_err(|e| invalid_relation(metadata.id(), e))?;
        let to_id = vcs
            .resolve_commit(&to_hex)
            .map_err(|e| invalid_relation(metadata.id(), e))?;
        let Some((_kind, diff_text)) = vcs
            .file_diff(&from_id, &to_id, &path)
            .map_err(|e| invalid_relation(metadata.id(), e))?
        else {
            return Ok(Vec::new());
        };

        let mut rows = Vec::new();
        for line in structured_diff_lines(&diff_text) {
            rows.push(Tuple::from([
                repository.clone(),
                Value::string(&from_hex),
                Value::string(&to_hex),
                Value::string(&path),
                int_value(metadata.id(), line.hunk as i64)?,
                int_value(metadata.id(), line.line_index as i64)?,
                Value::symbol(Symbol::intern(line.side)),
                line.old_line
                    .map(|line| int_value(metadata.id(), line as i64))
                    .transpose()?
                    .map_or_else(Value::option_none, Value::option_some),
                line.new_line
                    .map(|line| int_value(metadata.id(), line as i64))
                    .transpose()?
                    .map_or_else(Value::option_none, Value::option_some),
                Value::symbol(Symbol::intern(line.kind)),
                Value::string(line.text),
            ]));
        }

        Ok(filter_bound_rows(rows, bindings))
    }
}

struct FileLineProjectionRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for FileLineProjectionRelation {
    fn name(&self) -> &'static str {
        "local-source-file-line-projection"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/FileLineProjection") && metadata.arity() == 9
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        VCS_TWO_COMMIT_PATH_RANGE_BOUND
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let repository = bound_value(metadata.id(), bindings, 0, "repository")?;
        let from_hex = bound_string(metadata.id(), bindings, 1, "from_commit")?;
        let to_hex = bound_string(metadata.id(), bindings, 2, "to_commit")?;
        let path = bound_string(metadata.id(), bindings, 3, "path")?;
        let start_line = bound_positive_int(metadata.id(), bindings, 4, "start_line")?;
        let end_line = bound_positive_int(metadata.id(), bindings, 5, "end_line")?;
        if end_line < start_line {
            return Err(invalid_relation(
                metadata.id(),
                "end_line must be greater than or equal to start_line",
            ));
        }

        let vcs = self
            .provider
            .vcs_provider_for(reader, metadata.id(), &repository)?;
        let from_id = vcs
            .resolve_commit(&from_hex)
            .map_err(|e| invalid_relation(metadata.id(), e))?;
        let to_id = vcs
            .resolve_commit(&to_hex)
            .map_err(|e| invalid_relation(metadata.id(), e))?;
        let diff = vcs
            .file_diff(&from_id, &to_id, &path)
            .map_err(|e| invalid_relation(metadata.id(), e))?;
        let (projected_start, projected_end, quality) = if let Some((_kind, diff_text)) = diff {
            project_line_range(&diff_text, start_line, end_line).ok_or_else(|| {
                invalid_relation(metadata.id(), "could not project line range through diff")
            })?
        } else {
            (start_line, end_line, "source/review_projection_exact")
        };

        Ok(filter_bound_rows(
            vec![Tuple::from([
                repository,
                Value::string(&from_hex),
                Value::string(&to_hex),
                Value::string(&path),
                int_value(metadata.id(), start_line as i64)?,
                int_value(metadata.id(), end_line as i64)?,
                int_value(metadata.id(), projected_start as i64)?,
                int_value(metadata.id(), projected_end as i64)?,
                Value::symbol(Symbol::intern(quality)),
            ])],
            bindings,
        ))
    }
}

struct CommitLogRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for CommitLogRelation {
    fn name(&self) -> &'static str {
        "local-source-commit-log"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/CommitLog") && metadata.arity() == 9
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        VCS_REPOSITORY_BOUND
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let repository = bound_value(metadata.id(), bindings, 0, "repository")?;
        let limit = match bindings.get(1) {
            Some(Some(val)) => val.as_int().unwrap_or(20) as usize,
            _ => 20,
        };
        let vcs = self
            .provider
            .vcs_provider_for(reader, metadata.id(), &repository)?;
        let commits = vcs
            .commit_log(limit)
            .map_err(|e| invalid_relation(metadata.id(), e))?;
        let tuples: Vec<Tuple> = commits
            .into_iter()
            .map(|(commit, parents, name, email, ts, msg)| {
                let parent_list: Vec<Value> =
                    parents.iter().map(|p| Value::string(p.hex())).collect();
                Tuple::from([
                    repository.clone(),
                    int_value(metadata.id(), limit as i64).unwrap(),
                    Value::string(commit.hex()),
                    Value::list(parent_list),
                    Value::string(name),
                    Value::string(email),
                    int_value(metadata.id(), ts).unwrap(),
                    Value::string(first_line(&msg)),
                    Value::string(msg),
                ])
            })
            .collect();
        Ok(filter_bound_rows(tuples, bindings))
    }
}

struct CommitSearchRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for CommitSearchRelation {
    fn name(&self) -> &'static str {
        "local-source-commit-search"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/CommitSearch") && metadata.arity() == 9
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        VCS_SEARCH_BOUND
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let repository = bound_value(metadata.id(), bindings, 0, "repository")?;
        let query = bound_string(metadata.id(), bindings, 1, "query")?;
        let limit = match bindings.get(2) {
            Some(Some(val)) => val.as_int().unwrap_or(20) as usize,
            _ => 20,
        };
        let vcs = self
            .provider
            .vcs_provider_for(reader, metadata.id(), &repository)?;
        let commits = vcs
            .commit_search(&query, limit)
            .map_err(|e| invalid_relation(metadata.id(), e))?;
        let tuples: Vec<Tuple> = commits
            .into_iter()
            .map(|(commit, parents, name, email, ts, msg)| {
                let parent_list: Vec<Value> =
                    parents.iter().map(|p| Value::string(p.hex())).collect();
                Tuple::from([
                    repository.clone(),
                    Value::string(&query),
                    int_value(metadata.id(), limit as i64).unwrap(),
                    Value::string(commit.hex()),
                    Value::list(parent_list),
                    Value::string(name),
                    Value::string(email),
                    int_value(metadata.id(), ts).unwrap(),
                    Value::string(first_line(&msg)),
                ])
            })
            .collect();
        Ok(filter_bound_rows(tuples, bindings))
    }
}

struct FileHistoryRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for FileHistoryRelation {
    fn name(&self) -> &'static str {
        "local-source-file-history"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/FileHistory") && metadata.arity() == 10
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        VCS_COMMIT_PATH_BOUND
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let repository = bound_value(metadata.id(), bindings, 0, "repository")?;
        let path = bound_string(metadata.id(), bindings, 1, "path")?;
        let limit = match bindings.get(2) {
            Some(Some(val)) => val.as_int().unwrap_or(20) as usize,
            _ => 20,
        };
        let vcs = self
            .provider
            .vcs_provider_for(reader, metadata.id(), &repository)?;
        let commits = vcs
            .file_history(&path, limit)
            .map_err(|e| invalid_relation(metadata.id(), e))?;
        let tuples: Vec<Tuple> = commits
            .into_iter()
            .map(|(commit, parents, name, email, ts, msg)| {
                let parent_list: Vec<Value> =
                    parents.iter().map(|p| Value::string(p.hex())).collect();
                Tuple::from([
                    repository.clone(),
                    Value::string(&path),
                    int_value(metadata.id(), limit as i64).unwrap(),
                    Value::string(commit.hex()),
                    Value::list(parent_list),
                    Value::string(name),
                    Value::string(email),
                    int_value(metadata.id(), ts).unwrap(),
                    Value::string(first_line(&msg)),
                    Value::string(msg),
                ])
            })
            .collect();
        Ok(filter_bound_rows(tuples, bindings))
    }
}

struct FileBlameRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for FileBlameRelation {
    fn name(&self) -> &'static str {
        "local-source-file-blame"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/FileBlame") && metadata.arity() == 9
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        VCS_BLAME_BOUND
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let repository = bound_value(metadata.id(), bindings, 0, "repository")?;
        let commit_hex = bound_string(metadata.id(), bindings, 1, "commit")?;
        let path = bound_string(metadata.id(), bindings, 2, "path")?;
        let vcs = self
            .provider
            .vcs_provider_for(reader, metadata.id(), &repository)?;
        let commit_id = vcs
            .resolve_commit(&commit_hex)
            .map_err(|e| invalid_relation(metadata.id(), e))?;
        let lines = vcs
            .blame(&commit_id, &path)
            .map_err(|e| invalid_relation(metadata.id(), e))?;
        let tuples: Vec<Tuple> = lines
            .into_iter()
            .map(|(line, origin, name, email, ts, msg)| {
                Tuple::from([
                    repository.clone(),
                    Value::string(&commit_hex),
                    Value::string(&path),
                    int_value(metadata.id(), line as i64).unwrap(),
                    Value::string(origin.hex()),
                    Value::string(name),
                    Value::string(email),
                    int_value(metadata.id(), ts).unwrap(),
                    Value::string(first_line(&msg)),
                ])
            })
            .collect();
        Ok(filter_bound_rows(tuples, bindings))
    }
}

struct FileBlameHunkRelation {
    provider: Arc<LocalSourceProvider>,
}

impl ComputedRelation for FileBlameHunkRelation {
    fn name(&self) -> &'static str {
        "local-source-file-blame-hunk"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/FileBlameHunk") && metadata.arity() == 10
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        VCS_BLAME_BOUND
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let repository = bound_value(metadata.id(), bindings, 0, "repository")?;
        let commit_hex = bound_string(metadata.id(), bindings, 1, "commit")?;
        let path = bound_string(metadata.id(), bindings, 2, "path")?;
        let vcs = self
            .provider
            .vcs_provider_for(reader, metadata.id(), &repository)?;
        let commit_id = vcs
            .resolve_commit(&commit_hex)
            .map_err(|e| invalid_relation(metadata.id(), e))?;
        let lines = vcs
            .blame(&commit_id, &path)
            .map_err(|e| invalid_relation(metadata.id(), e))?;
        let tuples =
            merge_adjacent_blame_lines(lines, &repository, &commit_hex, &path, metadata.id())?;
        Ok(filter_bound_rows(tuples, bindings))
    }
}

fn first_line(msg: &str) -> String {
    msg.lines().next().unwrap_or("").to_string()
}

fn merge_adjacent_blame_lines(
    lines: Vec<BlameRow>,
    repository: &Value,
    commit_hex: &str,
    path: &str,
    relation: RelationId,
) -> Result<Vec<Tuple>, KernelError> {
    if lines.is_empty() {
        return Ok(Vec::new());
    }
    let mut tuples = Vec::new();
    let mut hunk_start = lines[0].0;
    let mut hunk_end = lines[0].0;
    let mut current = BlameHunkEntry::from_line(&lines[0]);
    for (line, origin, name, email, ts, msg) in lines.iter().skip(1) {
        let next = BlameHunkEntry {
            origin: origin.clone(),
            name: name.clone(),
            email: email.clone(),
            ts: *ts,
            msg: msg.clone(),
        };
        if next == current && *line == hunk_end + 1 {
            hunk_end = *line;
        } else {
            tuples.push(blame_hunk_tuple(
                repository, commit_hex, path, hunk_start, hunk_end, &current, relation,
            )?);
            hunk_start = *line;
            hunk_end = *line;
            current = next;
        }
    }
    tuples.push(blame_hunk_tuple(
        repository, commit_hex, path, hunk_start, hunk_end, &current, relation,
    )?);
    Ok(tuples)
}

struct BlameHunkEntry {
    origin: jj_lib::backend::CommitId,
    name: String,
    email: String,
    ts: i64,
    msg: String,
}

impl BlameHunkEntry {
    fn from_line(line: &(u64, jj_lib::backend::CommitId, String, String, i64, String)) -> Self {
        Self {
            origin: line.1.clone(),
            name: line.2.clone(),
            email: line.3.clone(),
            ts: line.4,
            msg: line.5.clone(),
        }
    }
}

impl PartialEq for BlameHunkEntry {
    fn eq(&self, other: &Self) -> bool {
        self.origin == other.origin
            && self.name == other.name
            && self.email == other.email
            && self.ts == other.ts
            && self.msg == other.msg
    }
}

impl Eq for BlameHunkEntry {}

fn blame_hunk_tuple(
    repository: &Value,
    commit_hex: &str,
    path: &str,
    start_line: u64,
    end_line: u64,
    entry: &BlameHunkEntry,
    relation: RelationId,
) -> Result<Tuple, KernelError> {
    Ok(Tuple::from([
        repository.clone(),
        Value::string(commit_hex),
        Value::string(path),
        int_value(relation, start_line as i64)?,
        int_value(relation, end_line as i64)?,
        Value::string(entry.origin.hex()),
        Value::string(&entry.name),
        Value::string(&entry.email),
        int_value(relation, entry.ts)?,
        Value::string(first_line(&entry.msg)),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_diff_lines_preserve_side_and_line_numbers() {
        let diff = "\
--- a/src/lib.rs\told
+++ b/src/lib.rs\tnew
@@ -2,2 +2,2 @@
-old line
+new line
";

        let rows = structured_diff_lines(diff);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].hunk, 1);
        assert_eq!(rows[0].line_index, 1);
        assert_eq!(rows[0].side, "source/review_old_side");
        assert_eq!(rows[0].old_line, Some(2));
        assert_eq!(rows[0].new_line, None);
        assert_eq!(rows[0].kind, "source/diff_removed");
        assert_eq!(rows[0].text, "old line");

        assert_eq!(rows[1].hunk, 1);
        assert_eq!(rows[1].line_index, 2);
        assert_eq!(rows[1].side, "source/review_new_side");
        assert_eq!(rows[1].old_line, None);
        assert_eq!(rows[1].new_line, Some(2));
        assert_eq!(rows[1].kind, "source/diff_added");
        assert_eq!(rows[1].text, "new line");
    }

    #[test]
    fn project_line_range_accounts_for_prior_hunk_delta() {
        let diff = "\
--- a/src/lib.rs\told
+++ b/src/lib.rs\tnew
@@ -2,1 +2,3 @@
-old line
+new line
+extra one
+extra two
";

        assert_eq!(
            project_line_range(diff, 10, 12),
            Some((12, 14, "source/review_projection_shifted"))
        );
    }
}
