use crate::util::content_hash;
use mica_var::Value;
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Resource limits applied to source-provider operations.
#[derive(Clone, Debug)]
pub struct SourceBounds {
    /// Maximum UTF-8 bytes returned for one document.
    pub max_document_bytes: usize,
    /// Maximum directory entries returned for one listing.
    pub max_directory_entries: usize,
    /// Maximum rows returned by search-like capabilities.
    pub max_search_rows: usize,
    /// Maximum commits walked by history-like capabilities.
    pub max_history_rows: usize,
    /// Maximum native providers invoked for one operation.
    pub max_provider_fanout: usize,
}

impl Default for SourceBounds {
    fn default() -> Self {
        Self {
            max_document_bytes: 2 * 1024 * 1024,
            max_directory_entries: 4096,
            max_search_rows: 256,
            max_history_rows: 512,
            max_provider_fanout: 16,
        }
    }
}

/// Explicit configuration for Mica's source browsing relations.
///
/// A host supplies allowed roots and native providers per runtime. Configured
/// roots also install the bounded local-worktree provider. Empty roots disable
/// filesystem and VCS access while still allowing host-owned providers.
#[derive(Clone, Default)]
pub struct SourceConfig {
    pub roots: Vec<PathBuf>,
    pub semantic_index_path: Option<PathBuf>,
    pub rust_analyzer_binary: Option<String>,
    pub bounds: SourceBounds,
    providers: BTreeMap<SourceProviderKey, Arc<dyn SourceProvider>>,
}

impl fmt::Debug for SourceConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SourceConfig")
            .field("roots", &self.roots)
            .field("semantic_index_path", &self.semantic_index_path)
            .field("rust_analyzer_binary", &self.rust_analyzer_binary)
            .field("bounds", &self.bounds)
            .field("providers", &self.providers.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl SourceConfig {
    pub fn new<P>(roots: impl IntoIterator<Item = P>) -> Self
    where
        P: Into<PathBuf>,
    {
        Self {
            roots: roots.into_iter().map(Into::into).collect(),
            ..Self::default()
        }
    }

    pub fn with_provider(mut self, provider: impl SourceProvider + 'static) -> Self {
        self.insert_provider(Arc::new(provider));
        self
    }

    pub fn with_shared_provider(mut self, provider: Arc<dyn SourceProvider>) -> Self {
        self.insert_provider(provider);
        self
    }

    pub fn with_semantic_index(mut self, path: PathBuf) -> Self {
        self.semantic_index_path = Some(path);
        self
    }

    pub fn with_rust_analyzer(mut self, binary: String) -> Self {
        self.rust_analyzer_binary = Some(binary);
        self
    }

    pub fn with_bounds(mut self, bounds: SourceBounds) -> Self {
        self.bounds = bounds;
        self
    }

    fn insert_provider(&mut self, provider: Arc<dyn SourceProvider>) {
        self.providers.insert(provider.key(), provider);
    }

    pub(crate) fn take_providers(
        &mut self,
    ) -> BTreeMap<SourceProviderKey, Arc<dyn SourceProvider>> {
        std::mem::take(&mut self.providers)
    }
}

/// A stable key that relates a native provider to Mica policy facts.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceProviderKey(String);

impl SourceProviderKey {
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SourceProviderKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Capabilities declared by a native source provider.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceCapabilities(u32);

impl SourceCapabilities {
    pub const READ: Self = Self(1 << 0);
    pub const LIST: Self = Self(1 << 1);
    pub const SYNTAX: Self = Self(1 << 2);
    pub const SYMBOLS: Self = Self(1 << 3);
    pub const DEFINITION: Self = Self(1 << 4);
    pub const REFERENCES: Self = Self(1 << 5);
    pub const HISTORY: Self = Self(1 << 6);
    pub const DIFF: Self = Self(1 << 7);
    pub const BLAME: Self = Self(1 << 8);

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn names(self) -> impl Iterator<Item = &'static str> {
        [
            (Self::READ, "read"),
            (Self::LIST, "list"),
            (Self::SYNTAX, "syntax"),
            (Self::SYMBOLS, "symbols"),
            (Self::DEFINITION, "definition"),
            (Self::REFERENCES, "references"),
            (Self::HISTORY, "history"),
            (Self::DIFF, "diff"),
            (Self::BLAME, "blame"),
        ]
        .into_iter()
        .filter_map(move |(capability, name)| self.contains(capability).then_some(name))
    }
}

/// A provider refused a request for an authority or scope reason.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceDenial {
    pub reason: String,
}

impl SourceDenial {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

/// A provider could not complete a request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceFailure {
    pub message: String,
}

impl SourceFailure {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// The result of invoking one native source provider.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderResult<T> {
    Found(T),
    Absent,
    Denied(SourceDenial),
    Failed(SourceFailure),
}

impl<T> ProviderResult<T> {
    fn unsupported(capability: &str) -> Self {
        Self::Failed(SourceFailure::new(format!(
            "provider does not support {capability}"
        )))
    }
}

#[derive(Clone, Debug)]
pub struct ReadRequest {
    pub repository: Value,
    pub revision: Value,
    pub revision_kind: String,
    pub root: PathBuf,
    pub relative_path: String,
    pub max_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceDocument {
    pub text: String,
    pub content_hash: String,
    pub source_version: String,
}

impl SourceDocument {
    pub fn new(
        text: impl Into<String>,
        content_hash: impl Into<String>,
        source_version: impl Into<String>,
    ) -> Self {
        Self {
            text: text.into(),
            content_hash: content_hash.into(),
            source_version: source_version.into(),
        }
    }

    pub fn from_text(text: impl Into<String>, source_version: impl Into<String>) -> Self {
        let text = text.into();
        let hash = content_hash(text.as_bytes());
        Self::new(text, hash, source_version)
    }
}

#[derive(Clone, Debug)]
pub struct ListRequest {
    pub repository: Value,
    pub revision: Value,
    pub revision_kind: String,
    pub root: PathBuf,
    pub relative_path: String,
    pub limit: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceEntry {
    pub relative_path: String,
    pub kind: String,
    pub name: String,
}

impl SourceEntry {
    pub fn new(
        relative_path: impl Into<String>,
        kind: impl Into<String>,
        name: impl Into<String>,
    ) -> Self {
        Self {
            relative_path: relative_path.into(),
            kind: kind.into(),
            name: name.into(),
        }
    }
}

/// A flattened text-search hit used by the persistent index adapter.
#[derive(Clone, Debug)]
pub(crate) struct TextSearchHit {
    pub(crate) unit: String,
    pub(crate) score: i64,
    pub(crate) path: String,
    pub(crate) match_line: usize,
    pub(crate) kind: String,
    pub(crate) title: String,
    pub(crate) snippet: String,
}

/// A named native source provider.
///
/// Providers must enforce their own repository and authority scope. The
/// dispatcher additionally validates the repository root and relational
/// selection policy before invoking a provider.
pub trait SourceProvider: Send + Sync {
    fn key(&self) -> SourceProviderKey;

    fn name(&self) -> &str {
        "source provider"
    }

    fn capabilities(&self) -> SourceCapabilities;

    fn is_available(&self) -> bool {
        true
    }

    fn supports_revision_kind(&self, _kind: &str) -> bool {
        true
    }

    fn read(&self, _request: &ReadRequest) -> ProviderResult<SourceDocument> {
        ProviderResult::unsupported("read")
    }

    fn list(&self, _request: &ListRequest) -> ProviderResult<Vec<SourceEntry>> {
        ProviderResult::unsupported("list")
    }
}

#[derive(Default)]
pub(crate) struct SourceProviderRegistry {
    providers: BTreeMap<SourceProviderKey, Arc<dyn SourceProvider>>,
}

impl SourceProviderRegistry {
    pub(crate) fn insert(&mut self, provider: Arc<dyn SourceProvider>) {
        self.providers.insert(provider.key(), provider);
    }

    pub(crate) fn get(&self, key: &str) -> Option<&Arc<dyn SourceProvider>> {
        self.providers.get(&SourceProviderKey::new(key))
    }

    pub(crate) fn iter(
        &self,
    ) -> impl Iterator<Item = (&SourceProviderKey, &Arc<dyn SourceProvider>)> {
        self.providers.iter()
    }
}

/// Bounded access to local worktree files.
pub struct LocalSourceProvider {
    bounds: SourceBounds,
}

impl LocalSourceProvider {
    pub fn new(bounds: SourceBounds) -> Self {
        Self { bounds }
    }

    fn resolve(&self, root: &Path, relative_path: &str) -> Result<PathBuf, ProviderError> {
        if let Err(error) = validate_relative_path_for_provider(relative_path) {
            return Err(ProviderError::Denied(SourceDenial::new(error)));
        }
        let candidate = root.join(relative_path);
        let absolute = match candidate.canonicalize() {
            Ok(absolute) => absolute,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(ProviderError::Absent);
            }
            Err(error) => {
                return Err(ProviderError::Failed(SourceFailure::new(format!(
                    "failed to resolve path {}: {error}",
                    candidate.display()
                ))));
            }
        };
        if !absolute.starts_with(root) {
            return Err(ProviderError::Denied(SourceDenial::new(
                "source path escapes repository root",
            )));
        }
        Ok(absolute)
    }
}

enum ProviderError {
    Absent,
    Denied(SourceDenial),
    Failed(SourceFailure),
}

impl ProviderError {
    fn into_result<T>(self) -> ProviderResult<T> {
        match self {
            Self::Absent => ProviderResult::Absent,
            Self::Denied(denial) => ProviderResult::Denied(denial),
            Self::Failed(failure) => ProviderResult::Failed(failure),
        }
    }
}

impl SourceProvider for LocalSourceProvider {
    fn key(&self) -> SourceProviderKey {
        SourceProviderKey::new("local-worktree")
    }

    fn name(&self) -> &str {
        "local worktree"
    }

    fn capabilities(&self) -> SourceCapabilities {
        SourceCapabilities::READ.union(SourceCapabilities::LIST)
    }

    fn supports_revision_kind(&self, kind: &str) -> bool {
        kind == "worktree"
    }

    fn read(&self, request: &ReadRequest) -> ProviderResult<SourceDocument> {
        if request.revision_kind != "worktree" {
            return ProviderResult::Absent;
        }
        let absolute = match self.resolve(&request.root, &request.relative_path) {
            Ok(path) => path,
            Err(error) => return error.into_result(),
        };
        if !absolute.is_file() {
            return ProviderResult::Failed(SourceFailure::new("source path must be a file"));
        }
        let metadata = match fs::metadata(&absolute) {
            Ok(metadata) => metadata,
            Err(error) => {
                return ProviderResult::Failed(SourceFailure::new(format!(
                    "failed to stat source file {}: {error}",
                    absolute.display()
                )));
            }
        };
        let max_bytes = self.bounds.max_document_bytes.min(request.max_bytes);
        if metadata.len() > max_bytes as u64 {
            return ProviderResult::Failed(SourceFailure::new(format!(
                "source file {} exceeds the {max_bytes} byte document bound",
                absolute.display()
            )));
        }
        let bytes = match fs::read(&absolute) {
            Ok(bytes) => bytes,
            Err(error) => {
                return ProviderResult::Failed(SourceFailure::new(format!(
                    "failed to read file: {error}"
                )));
            }
        };
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(error) => {
                return ProviderResult::Failed(SourceFailure::new(format!(
                    "source file is not utf-8: {error}"
                )));
            }
        };
        let hash = content_hash(text.as_bytes());
        ProviderResult::Found(SourceDocument::new(text, hash.clone(), hash))
    }

    fn list(&self, request: &ListRequest) -> ProviderResult<Vec<SourceEntry>> {
        if request.revision_kind != "worktree" {
            return ProviderResult::Absent;
        }
        let absolute = match self.resolve(&request.root, &request.relative_path) {
            Ok(path) => path,
            Err(error) => return error.into_result(),
        };
        if !absolute.is_dir() {
            return ProviderResult::Failed(SourceFailure::new(
                "repository entry parent path must be a directory",
            ));
        }
        let entries = match fs::read_dir(&absolute) {
            Ok(entries) => entries,
            Err(error) => {
                return ProviderResult::Failed(SourceFailure::new(format!(
                    "failed to list directory: {error}"
                )));
            }
        };
        let mut rows = Vec::new();
        let mut examined = 0usize;
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    return ProviderResult::Failed(SourceFailure::new(format!(
                        "failed to read directory entry: {error}"
                    )));
                }
            };
            examined += 1;
            if examined > self.bounds.max_directory_entries {
                return ProviderResult::Failed(SourceFailure::new(format!(
                    "directory {} exceeds the {} entry bound",
                    absolute.display(),
                    self.bounds.max_directory_entries
                )));
            }
            let path = entry.path();
            let kind = if path.is_dir() { "directory" } else { "file" };
            if kind == "file" && fs::read_to_string(&path).is_err() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = match path.strip_prefix(&request.root) {
                Ok(relative) => relative,
                Err(_) => {
                    return ProviderResult::Denied(SourceDenial::new(
                        "directory entry escaped repository root",
                    ));
                }
            };
            let relative = match relative.to_str() {
                Some(relative) => relative.replace('\\', "/"),
                None => {
                    return ProviderResult::Failed(SourceFailure::new(
                        "source path is not valid utf-8",
                    ));
                }
            };
            rows.push(SourceEntry::new(relative, kind, name));
        }
        rows.sort_by(|left, right| {
            left.relative_path
                .cmp(&right.relative_path)
                .then_with(|| left.kind.cmp(&right.kind))
                .then_with(|| left.name.cmp(&right.name))
        });
        rows.truncate(request.limit);
        ProviderResult::Found(rows)
    }
}

fn validate_relative_path_for_provider(path: &str) -> Result<(), String> {
    let path = Path::new(path);
    if path.is_absolute() {
        return Err("source path must be relative to repository root".to_owned());
    }
    for component in path.components() {
        match component {
            std::path::Component::Normal(_) | std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                return Err("source path must not contain parent components".to_owned());
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return Err("source path must be relative to repository root".to_owned());
            }
        }
    }
    Ok(())
}
