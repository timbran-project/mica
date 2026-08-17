use crate::dispatcher::{ComposedTextSearchQuery, SourceDispatcher};
use crate::index::IndexedTextUnit;
use crate::navigation::{
    SemanticSymbol, SemanticSymbolProvider, byte_offset_to_lsp_position, semantic_location,
    semantic_symbol,
};
use crate::provider::SourceConfig;
use crate::receive::GitReceiveRecorder;
use crate::syntax::{SourceLanguage, syntax_lines};
use crate::util::*;
use crate::vcs::{BlameRow, VcsProvider};
use jj_lib::object_id::ObjectId;
use mica_relation_kernel::{
    ComputedRelation, ComputedRelationRead, KernelError, RelationId, RelationMetadata, Tuple,
};
use mica_var::{Symbol, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

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
const TEXT_SEARCH_BOUND: &[u16] = &[0, 1, 2, 3, 4];
const INDEXED_TEXT_SEARCH_BOUND: &[u16] = &[0, 1, 2];
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

/// Builds the source browsing computed relations from explicit configuration.
///
/// This is the only construction path; the source provider never reads
/// process-global environment variables.
pub fn computed_relations(config: SourceConfig) -> Vec<Arc<dyn ComputedRelation>> {
    let dispatcher = Arc::new(SourceDispatcher::new(config));
    vec![
        Arc::new(ProviderRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(ProviderNameRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(ProviderCapabilityRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(ProviderAvailableRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(RepositoryEntryRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(FileTextRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(FileLinesRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(FileLineCountRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(FileContentHashRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(SyntaxLineRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(SyntaxOutlineRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(SyntaxNodeAtRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(DefinitionAtRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(ReferencesOfRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(SymbolSearchRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(IndexedTextUnitRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(IndexedFileRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(TextSearchRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(IndexedTextSearchRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(SourceIndexRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(IndexRepositoryRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(IndexRevisionRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(IndexProviderRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(IndexStatusRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(IndexVersionRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(IndexBuildErrorRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(RepositoryVcsRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(RefTargetRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(GitRefTargetRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(GitReceivedRefUpdateRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(CommitExistsRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(CommitTreeRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(CommitAuthorRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(CommitMessageRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(CommitParentsRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(ChangedFilesRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(FileDiffRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(FileDiffLineRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(FileLineProjectionRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(CommitLogRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(CommitSearchRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(FileHistoryRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(FileBlameRelation {
            dispatcher: dispatcher.clone(),
        }),
        Arc::new(FileBlameHunkRelation {
            dispatcher: dispatcher.clone(),
        }),
    ]
}

struct ProviderRelation {
    dispatcher: Arc<SourceDispatcher>,
}

impl ComputedRelation for ProviderRelation {
    fn name(&self) -> &'static str {
        "source-provider"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/Provider") && metadata.arity() == 1
    }

    fn scan(
        &self,
        _reader: &dyn ComputedRelationRead,
        _metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let rows = self
            .dispatcher
            .providers()
            .map(|(key, _)| Tuple::from([Value::string(key.as_str())]))
            .collect();
        Ok(filter_bound_rows(rows, bindings))
    }
}

struct ProviderNameRelation {
    dispatcher: Arc<SourceDispatcher>,
}

impl ComputedRelation for ProviderNameRelation {
    fn name(&self) -> &'static str {
        "source-provider-name"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/ProviderName") && metadata.arity() == 2
    }

    fn scan(
        &self,
        _reader: &dyn ComputedRelationRead,
        _metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let rows = self
            .dispatcher
            .providers()
            .map(|(key, provider)| {
                Tuple::from([Value::string(key.as_str()), Value::string(provider.name())])
            })
            .collect();
        Ok(filter_bound_rows(rows, bindings))
    }
}

struct ProviderCapabilityRelation {
    dispatcher: Arc<SourceDispatcher>,
}

impl ComputedRelation for ProviderCapabilityRelation {
    fn name(&self) -> &'static str {
        "source-provider-capability"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/ProviderCapability") && metadata.arity() == 2
    }

    fn scan(
        &self,
        _reader: &dyn ComputedRelationRead,
        _metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let rows = self
            .dispatcher
            .providers()
            .flat_map(|(key, provider)| {
                provider.capabilities().names().map(|capability| {
                    Tuple::from([Value::string(key.as_str()), Value::string(capability)])
                })
            })
            .collect();
        Ok(filter_bound_rows(rows, bindings))
    }
}

struct ProviderAvailableRelation {
    dispatcher: Arc<SourceDispatcher>,
}

impl ComputedRelation for ProviderAvailableRelation {
    fn name(&self) -> &'static str {
        "source-provider-available"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/ProviderAvailable") && metadata.arity() == 1
    }

    fn scan(
        &self,
        _reader: &dyn ComputedRelationRead,
        _metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let rows = self
            .dispatcher
            .providers()
            .filter(|(_, provider)| provider.is_available())
            .map(|(key, _)| Tuple::from([Value::string(key.as_str())]))
            .collect();
        Ok(filter_bound_rows(rows, bindings))
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
    dispatcher: Arc<SourceDispatcher>,
}

impl ComputedRelation for RepositoryEntryRelation {
    fn name(&self) -> &'static str {
        "source-dispatch-repository-entry"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/RepositoryEntry") && metadata.arity() == 7
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
        let (root, relative) =
            self.dispatcher
                .resolve_path(reader, metadata.id(), &repository, &revision, &parent)?;
        let entries = self.dispatcher.list_entries(
            reader,
            metadata.id(),
            &repository,
            &revision,
            &root,
            &relative,
        )?;
        let mut rows = entries
            .into_iter()
            .map(|entry| {
                Tuple::from([
                    repository.clone(),
                    revision.clone(),
                    Value::string(&parent),
                    Value::string(entry.entry.relative_path),
                    Value::string(entry.entry.kind),
                    Value::string(entry.entry.name),
                    Value::string(entry.provider.as_str()),
                ])
            })
            .collect::<Vec<_>>();
        rows.sort_by(|left, right| left.values().cmp(right.values()));
        Ok(filter_bound_rows(rows, bindings))
    }
}

struct FileTextRelation {
    dispatcher: Arc<SourceDispatcher>,
}

impl ComputedRelation for FileTextRelation {
    fn name(&self) -> &'static str {
        "source-dispatch-file-text"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/FileText") && metadata.arity() == 7
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
        let (root, relative) =
            self.dispatcher
                .resolve_path(reader, metadata.id(), &repository, &revision, &path)?;
        let Some(document) = self.dispatcher.source_document(
            reader,
            metadata.id(),
            &repository,
            &revision,
            &root,
            &relative,
        )?
        else {
            return Ok(Vec::new());
        };
        Ok(filter_bound_rows(
            vec![Tuple::from([
                repository,
                revision,
                Value::string(path),
                Value::string(document.provider.as_str()),
                Value::string(&document.text),
                Value::string(&document.hash),
                Value::string(&document.source_version),
            ])],
            bindings,
        ))
    }
}

struct FileLinesRelation {
    dispatcher: Arc<SourceDispatcher>,
}

impl ComputedRelation for FileLinesRelation {
    fn name(&self) -> &'static str {
        "source-dispatch-file-lines"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/FileLines") && metadata.arity() == 9
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
        let (root, relative) =
            self.dispatcher
                .resolve_path(reader, metadata.id(), &repository, &revision, &path)?;
        let Some(document) = self.dispatcher.source_document(
            reader,
            metadata.id(),
            &repository,
            &revision,
            &root,
            &relative,
        )?
        else {
            return Ok(Vec::new());
        };
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
                Value::string(document.provider.as_str()),
                Value::string(&document.source_version),
            ])],
            bindings,
        ))
    }
}

struct FileLineCountRelation {
    dispatcher: Arc<SourceDispatcher>,
}

impl ComputedRelation for FileLineCountRelation {
    fn name(&self) -> &'static str {
        "source-dispatch-file-line-count"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/FileLineCount") && metadata.arity() == 6
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
        let (root, relative) =
            self.dispatcher
                .resolve_path(reader, metadata.id(), &repository, &revision, &path)?;
        let Some(document) = self.dispatcher.source_document(
            reader,
            metadata.id(),
            &repository,
            &revision,
            &root,
            &relative,
        )?
        else {
            return Ok(Vec::new());
        };
        Ok(filter_bound_rows(
            vec![Tuple::from([
                repository,
                revision,
                Value::string(path),
                int_value(metadata.id(), document.line_count as i64)?,
                Value::string(document.provider.as_str()),
                Value::string(&document.source_version),
            ])],
            bindings,
        ))
    }
}

struct FileContentHashRelation {
    dispatcher: Arc<SourceDispatcher>,
}

impl ComputedRelation for FileContentHashRelation {
    fn name(&self) -> &'static str {
        "source-dispatch-file-content-hash"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/FileContentHash") && metadata.arity() == 6
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
        let (root, relative) =
            self.dispatcher
                .resolve_path(reader, metadata.id(), &repository, &revision, &path)?;
        let Some(document) = self.dispatcher.source_document(
            reader,
            metadata.id(),
            &repository,
            &revision,
            &root,
            &relative,
        )?
        else {
            return Ok(Vec::new());
        };
        Ok(filter_bound_rows(
            vec![Tuple::from([
                repository,
                revision,
                Value::string(path),
                Value::string(&document.hash),
                Value::string(document.provider.as_str()),
                Value::string(&document.source_version),
            ])],
            bindings,
        ))
    }
}

struct SyntaxLineRelation {
    dispatcher: Arc<SourceDispatcher>,
}

impl ComputedRelation for SyntaxLineRelation {
    fn name(&self) -> &'static str {
        "source-dispatch-syntax-line"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/SyntaxLine") && metadata.arity() == 10
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
        let (root, relative) =
            self.dispatcher
                .resolve_path(reader, metadata.id(), &repository, &revision, &path)?;
        let Some(document) = self.dispatcher.source_document(
            reader,
            metadata.id(),
            &repository,
            &revision,
            &root,
            &relative,
        )?
        else {
            return Ok(Vec::new());
        };
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
                    Value::string(document.provider.as_str()),
                    Value::string(&document.source_version),
                ]))
            })
            .collect::<Result<Vec<_>, KernelError>>()?;
        Ok(filter_bound_rows(rows, bindings))
    }
}

struct SyntaxOutlineRelation {
    dispatcher: Arc<SourceDispatcher>,
}

impl ComputedRelation for SyntaxOutlineRelation {
    fn name(&self) -> &'static str {
        "source-dispatch-syntax-outline"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/SyntaxOutline") && metadata.arity() == 12
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
        let (root, relative) =
            self.dispatcher
                .resolve_path(reader, metadata.id(), &repository, &revision, &path)?;
        let Some(document) = self.dispatcher.source_document(
            reader,
            metadata.id(),
            &repository,
            &revision,
            &root,
            &relative,
        )?
        else {
            return Ok(Vec::new());
        };
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
                    Value::string(document.provider.as_str()),
                    Value::string(&document.source_version),
                ]))
            })
            .collect::<Result<Vec<_>, KernelError>>()?;
        Ok(filter_bound_rows(rows, bindings))
    }
}

struct SyntaxNodeAtRelation {
    dispatcher: Arc<SourceDispatcher>,
}

impl ComputedRelation for SyntaxNodeAtRelation {
    fn name(&self) -> &'static str {
        "source-dispatch-syntax-node-at"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/SyntaxNodeAt") && metadata.arity() == 13
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
        let (root, relative) =
            self.dispatcher
                .resolve_path(reader, metadata.id(), &repository, &revision, &path)?;
        let Some(document) = self.dispatcher.source_document(
            reader,
            metadata.id(),
            &repository,
            &revision,
            &root,
            &relative,
        )?
        else {
            return Ok(Vec::new());
        };
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
                Value::string(document.provider.as_str()),
                Value::string(&document.source_version),
            ])]
        } else {
            Vec::new()
        };
        Ok(filter_bound_rows(rows, bindings))
    }
}

struct DefinitionAtRelation {
    dispatcher: Arc<SourceDispatcher>,
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
        let (root, relative) =
            self.dispatcher
                .resolve_path(reader, metadata.id(), &repository, &revision, &path)?;
        let Some(document) = self.dispatcher.source_document(
            reader,
            metadata.id(),
            &repository,
            &revision,
            &root,
            &relative,
        )?
        else {
            return Ok(Vec::new());
        };
        if byte_offset > document.text.len() {
            return Err(invalid_relation(
                metadata.id(),
                "byte offset is beyond source file length",
            ));
        }
        let index = self.dispatcher.semantic_index(metadata.id())?;
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
        let Some(rust_analyzer) = self.dispatcher.rust_analyzer() else {
            return Ok(Vec::new());
        };
        let file = root.join(&relative);
        let mut locations = rust_analyzer
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
                locations = rust_analyzer
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
    dispatcher: Arc<SourceDispatcher>,
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
        let index = self.dispatcher.semantic_index(metadata.id())?;
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
        let Some(rust_analyzer) = self.dispatcher.rust_analyzer() else {
            return Ok(Vec::new());
        };
        let root =
            self.dispatcher
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
        let Some(document) = self.dispatcher.source_document(
            reader,
            metadata.id(),
            &repository,
            &revision,
            &root,
            &request.path,
        )?
        else {
            return Ok(Vec::new());
        };
        let position =
            byte_offset_to_lsp_position(metadata.id(), &document.text, request.start_byte)?;
        let locations = rust_analyzer
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
    dispatcher: Arc<SourceDispatcher>,
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
        let index = self.dispatcher.semantic_index(metadata.id())?;
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
    dispatcher: Arc<SourceDispatcher>,
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
        let index = self.dispatcher.semantic_index(metadata.id())?;
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
    dispatcher: Arc<SourceDispatcher>,
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
        let index = self.dispatcher.semantic_index(metadata.id())?;
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
    dispatcher: Arc<SourceDispatcher>,
}

impl ComputedRelation for TextSearchRelation {
    fn name(&self) -> &'static str {
        "source-dispatch-text-search"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/TextSearch") && metadata.arity() == 15
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        TEXT_SEARCH_BOUND
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
        let scope = bound_string(metadata.id(), bindings, 4, "scope")?;
        let hits = self.dispatcher.text_search(
            reader,
            metadata.id(),
            &repository,
            &revision,
            ComposedTextSearchQuery {
                query: &query,
                limit,
                scope: &scope,
            },
        )?;
        let rows = hits
            .into_iter()
            .map(|hit| {
                Ok(Tuple::from([
                    repository.clone(),
                    revision.clone(),
                    Value::string(&query),
                    int_value(metadata.id(), limit as i64)?,
                    Value::string(&scope),
                    Value::string(hit.subject),
                    int_value(metadata.id(), hit.score as i64)?,
                    Value::string(hit.path),
                    int_value(metadata.id(), hit.line as i64)?,
                    int_value(metadata.id(), hit.line as i64)?,
                    Value::string(hit.kind),
                    Value::string(hit.title),
                    Value::string(hit.snippet),
                    Value::string(hit.provider.as_str()),
                    Value::string(hit.source_version),
                ]))
            })
            .collect::<Result<Vec<_>, KernelError>>()?;
        Ok(filter_bound_rows(rows, bindings))
    }
}

struct IndexedTextSearchRelation {
    dispatcher: Arc<SourceDispatcher>,
}

impl ComputedRelation for IndexedTextSearchRelation {
    fn name(&self) -> &'static str {
        "persistent-source-indexed-text-search"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("source/IndexedTextSearch") && metadata.arity() == 11
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        INDEXED_TEXT_SEARCH_BOUND
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
        let hits =
            self.dispatcher
                .semantic_index_text_search(metadata.id(), &query, limit, &scope)?;
        let rows = hits
            .into_iter()
            .map(|hit| {
                Ok(Tuple::from([
                    Value::string(&query),
                    int_value(metadata.id(), limit as i64)?,
                    Value::string(&scope),
                    Value::string(hit.unit),
                    int_value(metadata.id(), hit.score)?,
                    Value::string(hit.path),
                    int_value(metadata.id(), hit.match_line as i64)?,
                    int_value(metadata.id(), hit.match_line as i64)?,
                    Value::string(hit.kind),
                    Value::string(hit.title),
                    Value::string(hit.snippet),
                ]))
            })
            .collect::<Result<Vec<_>, KernelError>>()?;
        Ok(filter_bound_rows(rows, bindings))
    }
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

macro_rules! index_value_relation {
    ($name:ident, $relation:literal, $field:ident) => {
        struct $name {
            dispatcher: Arc<SourceDispatcher>,
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
                let index = self.dispatcher.semantic_index(metadata.id())?;
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
    dispatcher: Arc<SourceDispatcher>,
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
        let index = self.dispatcher.semantic_index(metadata.id())?;
        Ok(filter_bound_rows(
            vec![Tuple::from([Value::string(&index.id)])],
            bindings,
        ))
    }
}

struct IndexRepositoryRelation {
    dispatcher: Arc<SourceDispatcher>,
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
        let index = self.dispatcher.semantic_index(metadata.id())?;
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
    dispatcher: Arc<SourceDispatcher>,
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
        let index = self.dispatcher.semantic_index(metadata.id())?;
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
    dispatcher: Arc<SourceDispatcher>,
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
            .dispatcher
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
    dispatcher: Arc<SourceDispatcher>,
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
        let git_dir_path = self.dispatcher.allowed_git_dir(metadata.id(), &git_dir)?;
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
    dispatcher: Arc<SourceDispatcher>,
}

struct RefTargetRelation {
    dispatcher: Arc<SourceDispatcher>,
}

struct GitRefTargetRelation {
    dispatcher: Arc<SourceDispatcher>,
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
            .dispatcher
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
        let git_dir_path = self.dispatcher.allowed_git_dir(metadata.id(), &git_dir)?;
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
            .dispatcher
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
    dispatcher: Arc<SourceDispatcher>,
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
            .dispatcher
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
    dispatcher: Arc<SourceDispatcher>,
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
            .dispatcher
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
    dispatcher: Arc<SourceDispatcher>,
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
            .dispatcher
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
    dispatcher: Arc<SourceDispatcher>,
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
            .dispatcher
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
    dispatcher: Arc<SourceDispatcher>,
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
            .dispatcher
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
    dispatcher: Arc<SourceDispatcher>,
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
            .dispatcher
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
    dispatcher: Arc<SourceDispatcher>,
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
            .dispatcher
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
    dispatcher: Arc<SourceDispatcher>,
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
            .dispatcher
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
    dispatcher: Arc<SourceDispatcher>,
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
            .dispatcher
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
    dispatcher: Arc<SourceDispatcher>,
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
            .dispatcher
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
    dispatcher: Arc<SourceDispatcher>,
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
            .dispatcher
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
    dispatcher: Arc<SourceDispatcher>,
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
            .dispatcher
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
    dispatcher: Arc<SourceDispatcher>,
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
            .dispatcher
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
