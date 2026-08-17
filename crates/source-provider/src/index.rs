use crate::navigation::SemanticSymbol;
use crate::provider::TextSearchHit;
use crate::syntax::{SourceLanguage, SyntaxDocument};
use crate::util::invalid_relation;
use mica_relation_kernel::{KernelError, RelationId};
use serde_json::{Value as JsonValue, json};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SOURCE_INDEX_ID: &str = "source-index:mica-worktree";
const SOURCE_INDEX_SCHEMA: &str = "mica-source-index-v1";
const SOURCE_INDEX_PROVIDER: &str = "mica-source-index/static-analysis";
const SOURCE_INDEX_VERSION: &str = "4";
const SOURCE_TEXT_UNIT_MODEL: &str = "source-workspace";
const SOURCE_TEXT_CHUNK_LINES: usize = 40;
const SOURCE_TEXT_CHUNK_BYTES: usize = 1_200;
const SEMANTIC_INDEX_REFRESH_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Debug)]
pub struct SourceIndexRoot {
    pub name: String,
    pub root: PathBuf,
}

#[derive(Clone, Debug)]
pub(crate) struct PersistentSemanticIndex {
    pub(crate) id: String,
    pub(crate) provider: String,
    pub(crate) version: String,
    pub(crate) status: String,
    pub(crate) error: String,
    pub(crate) repositories: Vec<IndexedRepository>,
    pub(crate) symbols: Vec<IndexedSymbol>,
    pub(crate) references: Vec<IndexedReference>,
    pub(crate) text_units: Vec<IndexedTextUnit>,
}

#[derive(Clone, Debug)]
pub(crate) struct IndexedRepository {
    pub(crate) name: String,
}

#[derive(Clone, Debug)]
pub(crate) struct IndexedSymbol {
    pub(crate) repository: String,
    pub(crate) symbol: String,
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) path: String,
    pub(crate) start_line: usize,
    pub(crate) end_line: usize,
    pub(crate) start_byte: usize,
    pub(crate) end_byte: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct IndexedReference {
    pub(crate) repository: String,
    pub(crate) symbol: String,
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) start_line: usize,
    pub(crate) end_line: usize,
    pub(crate) start_byte: usize,
    pub(crate) end_byte: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct IndexedTextUnit {
    pub(crate) repository: String,
    pub(crate) unit: String,
    pub(crate) ordinal: usize,
    pub(crate) kind: String,
    pub(crate) title: String,
    pub(crate) path: String,
    pub(crate) start_line: usize,
    pub(crate) end_line: usize,
    pub(crate) model: String,
    pub(crate) text: String,
}

impl PersistentSemanticIndex {
    fn missing(path: &Path) -> Self {
        Self {
            id: SOURCE_INDEX_ID.to_owned(),
            provider: SOURCE_INDEX_PROVIDER.to_owned(),
            version: SOURCE_INDEX_VERSION.to_owned(),
            status: "missing".to_owned(),
            error: format!("semantic index not found at {}", path.display()),
            repositories: Vec::new(),
            symbols: Vec::new(),
            references: Vec::new(),
            text_units: Vec::new(),
        }
    }

    fn unconfigured() -> Self {
        Self {
            id: SOURCE_INDEX_ID.to_owned(),
            provider: SOURCE_INDEX_PROVIDER.to_owned(),
            version: SOURCE_INDEX_VERSION.to_owned(),
            status: "missing".to_owned(),
            error: "no semantic index configured".to_owned(),
            repositories: Vec::new(),
            symbols: Vec::new(),
            references: Vec::new(),
            text_units: Vec::new(),
        }
    }

    pub(crate) fn load(relation: RelationId, path: &Path) -> Result<Self, KernelError> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::missing(path));
            }
            Err(error) => {
                return Err(invalid_relation(
                    relation,
                    format!("failed to read semantic index {}: {error}", path.display()),
                ));
            }
        };
        let json = serde_json::from_slice::<JsonValue>(&bytes).map_err(|error| {
            invalid_relation(
                relation,
                format!("failed to parse semantic index {}: {error}", path.display()),
            )
        })?;
        if json.get("schema").and_then(JsonValue::as_str) != Some(SOURCE_INDEX_SCHEMA) {
            return Err(invalid_relation(
                relation,
                format!("semantic index {} has unsupported schema", path.display()),
            ));
        }
        let id = json
            .get("id")
            .and_then(JsonValue::as_str)
            .unwrap_or(SOURCE_INDEX_ID)
            .to_owned();
        let provider = json
            .get("provider")
            .and_then(JsonValue::as_str)
            .unwrap_or(SOURCE_INDEX_PROVIDER)
            .to_owned();
        let version = json
            .get("version")
            .and_then(JsonValue::as_str)
            .unwrap_or(SOURCE_INDEX_VERSION)
            .to_owned();
        let status = json
            .get("status")
            .and_then(JsonValue::as_str)
            .unwrap_or("failed")
            .to_owned();
        let error = json
            .get("error")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_owned();
        let default_repository = json
            .get("repository")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_owned();
        let repositories = json
            .get("repositories")
            .and_then(JsonValue::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(indexed_repository_from_json)
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()
            .map_err(|error| invalid_relation(relation, error))?
            .unwrap_or_else(|| {
                if default_repository.is_empty() {
                    Vec::new()
                } else {
                    vec![IndexedRepository {
                        name: default_repository.clone(),
                    }]
                }
            });
        let symbols = json
            .get("symbols")
            .and_then(JsonValue::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|item| indexed_symbol_from_json(item, &default_repository))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()
            .map_err(|error| invalid_relation(relation, error))?
            .unwrap_or_default();
        let references = json
            .get("references")
            .and_then(JsonValue::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|item| indexed_reference_from_json(item, &default_repository))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()
            .map_err(|error| invalid_relation(relation, error))?
            .unwrap_or_default();
        let text_units = json
            .get("text_units")
            .and_then(JsonValue::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|item| indexed_text_unit_from_json(item, &default_repository))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()
            .map_err(|error| invalid_relation(relation, error))?
            .unwrap_or_default();
        Ok(Self {
            id,
            provider,
            version,
            status,
            error,
            repositories,
            symbols,
            references,
            text_units,
        })
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.status == "complete"
    }

    pub(crate) fn covers_repository(&self, repository: &str) -> bool {
        self.repositories.is_empty()
            || self
                .repositories
                .iter()
                .any(|candidate| candidate.name == repository)
    }

    pub(crate) fn definition_at(
        &self,
        repository: Option<&str>,
        path: &str,
        byte_offset: usize,
    ) -> Vec<IndexedSymbol> {
        if !self.is_complete() {
            return Vec::new();
        }
        let Some(reference) = self
            .references
            .iter()
            .find(|reference| {
                repository_matches(repository, &reference.repository)
                    && reference.path == path
                    && reference.start_byte <= byte_offset
                    && byte_offset <= reference.end_byte
            })
            .or_else(|| {
                self.references.iter().find(|reference| {
                    repository_matches(repository, &reference.repository)
                        && reference.path == path
                        && reference.start_byte <= byte_offset.saturating_add(1)
                        && byte_offset <= reference.end_byte
                })
            })
        else {
            return Vec::new();
        };
        let mut symbols = self
            .symbols
            .iter()
            .filter(|symbol| {
                repository_matches(repository, &symbol.repository) && symbol.name == reference.name
            })
            .cloned()
            .collect::<Vec<_>>();
        symbols.sort_by_key(|symbol| {
            (
                symbol.path != reference.path,
                symbol.start_byte.abs_diff(reference.start_byte),
                symbol.path.clone(),
                symbol.start_byte,
            )
        });
        symbols
    }

    pub(crate) fn references_of(
        &self,
        repository: Option<&str>,
        symbol: &SemanticSymbol,
    ) -> Vec<IndexedReference> {
        if !self.is_complete() {
            return Vec::new();
        }
        self.references
            .iter()
            .filter(|reference| {
                repository_matches(repository, &reference.repository)
                    && (reference.symbol == symbol.id || reference.name == symbol.name)
            })
            .cloned()
            .collect()
    }

    pub(crate) fn search(
        &self,
        repository: Option<&str>,
        query: &str,
        limit: usize,
    ) -> Vec<IndexedSymbol> {
        if !self.is_complete() {
            return Vec::new();
        }
        let needle = query.to_ascii_lowercase();
        let mut symbols = self
            .symbols
            .iter()
            .filter(|symbol| {
                repository_matches(repository, &symbol.repository)
                    && symbol.name.to_ascii_lowercase().contains(&needle)
            })
            .cloned()
            .collect::<Vec<_>>();
        symbols.sort_by_key(|symbol| {
            (
                !symbol.name.eq_ignore_ascii_case(query),
                !symbol.name.to_ascii_lowercase().starts_with(&needle),
                symbol.name.clone(),
                symbol.path.clone(),
                symbol.start_byte,
            )
        });
        symbols.truncate(limit);
        symbols
    }
}

fn repository_matches(filter: Option<&str>, repository: &str) -> bool {
    filter.is_none_or(|filter| repository.is_empty() || repository == filter)
}

fn indexed_repository_from_json(value: &JsonValue) -> Result<IndexedRepository, String> {
    let _root = json_string(value, "root")?;
    Ok(IndexedRepository {
        name: json_string(value, "name")?,
    })
}

fn optional_json_string(value: &JsonValue, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
}

fn indexed_symbol_from_json(
    value: &JsonValue,
    default_repository: &str,
) -> Result<IndexedSymbol, String> {
    Ok(IndexedSymbol {
        repository: optional_json_string(value, "repository")
            .unwrap_or_else(|| default_repository.to_owned()),
        symbol: json_string(value, "symbol")?,
        name: json_string(value, "name")?,
        kind: json_string(value, "kind")?,
        path: json_string(value, "path")?,
        start_line: json_usize(value, "start_line")?,
        end_line: json_usize(value, "end_line")?,
        start_byte: json_usize(value, "start_byte")?,
        end_byte: json_usize(value, "end_byte")?,
    })
}

fn indexed_reference_from_json(
    value: &JsonValue,
    default_repository: &str,
) -> Result<IndexedReference, String> {
    Ok(IndexedReference {
        repository: optional_json_string(value, "repository")
            .unwrap_or_else(|| default_repository.to_owned()),
        symbol: json_string(value, "symbol")?,
        name: json_string(value, "name")?,
        path: json_string(value, "path")?,
        start_line: json_usize(value, "start_line")?,
        end_line: json_usize(value, "end_line")?,
        start_byte: json_usize(value, "start_byte")?,
        end_byte: json_usize(value, "end_byte")?,
    })
}

fn indexed_text_unit_from_json(
    value: &JsonValue,
    default_repository: &str,
) -> Result<IndexedTextUnit, String> {
    Ok(IndexedTextUnit {
        repository: optional_json_string(value, "repository")
            .unwrap_or_else(|| default_repository.to_owned()),
        unit: json_string(value, "unit")?,
        ordinal: json_usize(value, "ordinal")?,
        kind: json_string(value, "kind")?,
        title: json_string(value, "title")?,
        path: json_string(value, "path")?,
        start_line: json_usize(value, "start_line")?,
        end_line: json_usize(value, "end_line")?,
        model: json_string(value, "model")?,
        text: json_string(value, "text")?,
    })
}

fn json_string(value: &JsonValue, field: &str) -> Result<String, String> {
    value
        .get(field)
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("semantic index field {field} must be a string"))
}

fn json_usize(value: &JsonValue, field: &str) -> Result<usize, String> {
    value
        .get(field)
        .and_then(JsonValue::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| format!("semantic index field {field} must be a non-negative integer"))
}

pub fn build_source_index_file(root: &Path, output: &Path) -> Result<(), String> {
    let root = SourceIndexRoot {
        name: "default".to_owned(),
        root: root.to_owned(),
    };
    build_source_index_file_for_roots(&[root], output)
}

pub fn build_source_index_file_for_roots(
    roots: &[SourceIndexRoot],
    output: &Path,
) -> Result<(), String> {
    let index = build_source_index_json(roots)?;
    if let Some(parent) = output.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    let bytes = serde_json::to_vec_pretty(&index)
        .map_err(|error| format!("failed to encode source index: {error}"))?;
    fs::write(output, bytes)
        .map_err(|error| format!("failed to write source index {}: {error}", output.display()))
}

pub fn write_failed_source_index_file(
    root: &Path,
    output: &Path,
    error: &str,
) -> Result<(), String> {
    if let Some(parent) = output.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    let root = root
        .canonicalize()
        .unwrap_or_else(|_| root.to_path_buf())
        .display()
        .to_string();
    let index = json!({
        "schema": SOURCE_INDEX_SCHEMA,
        "id": SOURCE_INDEX_ID,
        "provider": SOURCE_INDEX_PROVIDER,
        "version": SOURCE_INDEX_VERSION,
        "status": "failed",
        "root": root,
        "error": error,
        "repositories": [],
        "symbols": [],
        "references": [],
        "text_units": [],
    });
    let bytes = serde_json::to_vec_pretty(&index)
        .map_err(|error| format!("failed to encode failed source index: {error}"))?;
    fs::write(output, bytes).map_err(|error| {
        format!(
            "failed to write failed source index {}: {error}",
            output.display()
        )
    })
}

fn build_source_index_json(roots: &[SourceIndexRoot]) -> Result<JsonValue, String> {
    if roots.is_empty() {
        return Err("source index requires at least one root".to_owned());
    }
    let mut repositories = Vec::new();
    let mut symbols = Vec::new();
    let mut references = Vec::new();
    let mut text_units = Vec::new();
    let mut text_unit_ordinal = 0usize;

    for root_spec in roots {
        let root = root_spec.root.canonicalize().map_err(|error| {
            format!(
                "invalid source index root {}: {error}",
                root_spec.root.display()
            )
        })?;
        repositories.push(json!({
            "name": root_spec.name,
            "root": root.display().to_string(),
        }));
        let mut files = indexed_source_files(&root)?;
        files.sort();
        for file in files {
            let relative = file
                .strip_prefix(&root)
                .map_err(|_| format!("indexed file escaped root: {}", file.display()))?;
            let path = relative_path_string(relative)?;
            let text = fs::read_to_string(&file)
                .map_err(|error| format!("failed to read {}: {error}", file.display()))?;
            text_units.extend(indexed_text_units_for_file(
                &root_spec.name,
                &path,
                &text,
                &mut text_unit_ordinal,
            ));
            let syntax = SyntaxDocument::parse(&path, &text);
            for item in &syntax.outline {
                if !is_index_identifier(&item.name) {
                    continue;
                }
                let symbol = indexed_symbol_id(
                    &root_spec.name,
                    &path,
                    item.start_byte,
                    item.end_byte,
                    &item.name,
                );
                symbols.push(json!({
                    "repository": root_spec.name,
                    "symbol": symbol,
                    "name": item.name,
                    "kind": item.kind,
                    "path": path,
                    "start_line": item.start_line,
                    "end_line": item.end_line,
                    "start_byte": item.start_byte,
                    "end_byte": item.end_byte,
                }));
            }
            for span in &syntax.highlights {
                if !matches!(span.kind, "function" | "type" | "property" | "identifier") {
                    continue;
                }
                let Some(name) = text.get(span.start..span.end) else {
                    continue;
                };
                if !is_index_identifier(name) {
                    continue;
                }
                let start_line = byte_line(&syntax.line_starts, span.start);
                let end_line = byte_line(&syntax.line_starts, span.end);
                references.push(json!({
                    "repository": root_spec.name,
                    "symbol": "",
                    "name": name,
                    "path": path,
                    "start_line": start_line,
                    "end_line": end_line,
                    "start_byte": span.start,
                    "end_byte": span.end,
                }));
            }
        }
    }
    symbols.sort_by_key(|value| {
        (
            value
                .get("repository")
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .to_owned(),
            value
                .get("name")
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .to_owned(),
            value
                .get("path")
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .to_owned(),
            value
                .get("start_byte")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0),
        )
    });
    references.sort_by_key(|value| {
        (
            value
                .get("repository")
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .to_owned(),
            value
                .get("name")
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .to_owned(),
            value
                .get("path")
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .to_owned(),
            value
                .get("start_byte")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0),
        )
    });
    symbols.dedup();
    references.dedup();
    let symbol_by_repository_name = symbols
        .iter()
        .filter_map(|symbol| {
            Some((
                (
                    symbol.get("repository")?.as_str()?.to_owned(),
                    symbol.get("name")?.as_str()?.to_owned(),
                ),
                symbol.get("symbol")?.as_str()?.to_owned(),
            ))
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    for reference in &mut references {
        let Some(repository) = reference.get("repository").and_then(JsonValue::as_str) else {
            continue;
        };
        if let Some(name) = reference.get("name").and_then(JsonValue::as_str)
            && let Some(symbol) =
                symbol_by_repository_name.get(&(repository.to_owned(), name.to_owned()))
            && let Some(object) = reference.as_object_mut()
        {
            object.insert("symbol".to_owned(), JsonValue::String(symbol.clone()));
        }
    }
    let root = if repositories.len() == 1 {
        repositories[0]
            .get("root")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_owned()
    } else {
        "multiple".to_owned()
    };
    Ok(json!({
        "schema": SOURCE_INDEX_SCHEMA,
        "id": SOURCE_INDEX_ID,
        "provider": SOURCE_INDEX_PROVIDER,
        "version": SOURCE_INDEX_VERSION,
        "status": "complete",
        "root": root,
        "repositories": repositories,
        "symbols": symbols,
        "references": references,
        "text_units": text_units,
        "error": "",
    }))
}

fn indexed_text_units_for_file(
    repository: &str,
    path: &str,
    text: &str,
    next_ordinal: &mut usize,
) -> Vec<JsonValue> {
    let mut units = Vec::new();
    let mut chunk = Vec::new();
    let mut chunk_start_line = 1usize;
    let mut chunk_bytes = 0usize;

    for (line_index, line) in text.lines().enumerate() {
        let line_number = line_index + 1;
        let next_bytes = if chunk.is_empty() {
            line.len()
        } else {
            chunk_bytes + 1 + line.len()
        };
        if !chunk.is_empty()
            && (chunk.len() >= SOURCE_TEXT_CHUNK_LINES || next_bytes > SOURCE_TEXT_CHUNK_BYTES)
        {
            push_indexed_text_unit(
                &mut units,
                repository,
                path,
                chunk_start_line,
                line_number - 1,
                &chunk.join("\n"),
                next_ordinal,
            );
            chunk.clear();
            chunk_bytes = 0;
            chunk_start_line = line_number;
        }

        if line.len() > SOURCE_TEXT_CHUNK_BYTES {
            for part in split_text_by_bytes(line, SOURCE_TEXT_CHUNK_BYTES) {
                push_indexed_text_unit(
                    &mut units,
                    repository,
                    path,
                    line_number,
                    line_number,
                    part,
                    next_ordinal,
                );
            }
            chunk_start_line = line_number + 1;
            continue;
        }

        if chunk.is_empty() {
            chunk_start_line = line_number;
            chunk_bytes = line.len();
        } else {
            chunk_bytes += 1 + line.len();
        }
        chunk.push(line);
    }

    if !chunk.is_empty() {
        let end_line = chunk_start_line + chunk.len() - 1;
        push_indexed_text_unit(
            &mut units,
            repository,
            path,
            chunk_start_line,
            end_line,
            &chunk.join("\n"),
            next_ordinal,
        );
    }

    units
}

fn push_indexed_text_unit(
    units: &mut Vec<JsonValue>,
    repository: &str,
    path: &str,
    start_line: usize,
    end_line: usize,
    text: &str,
    next_ordinal: &mut usize,
) {
    if text.trim().is_empty() {
        return;
    }

    *next_ordinal += 1;
    let title = if start_line == end_line {
        format!("{path}:{start_line}")
    } else {
        format!("{path}:{start_line}-{end_line}")
    };
    units.push(json!({
        "repository": repository,
        "unit": indexed_text_unit_id(repository, path, start_line, end_line, *next_ordinal),
        "ordinal": *next_ordinal,
        "kind": source_language_kind(SourceLanguage::from_path(path)),
        "title": title,
        "path": path,
        "start_line": start_line,
        "end_line": end_line,
        "model": SOURCE_TEXT_UNIT_MODEL,
        "text": format!("{repository}:{path}:{start_line}-{end_line}\n{text}"),
    }));
}

fn split_text_by_bytes(text: &str, max_bytes: usize) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut last_boundary = 0usize;
    for (index, _) in text.char_indices() {
        if index - start > max_bytes {
            parts.push(&text[start..last_boundary]);
            start = last_boundary;
        }
        last_boundary = index;
    }
    if start < text.len() {
        parts.push(&text[start..]);
    }
    parts
}

fn indexed_source_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    collect_indexed_source_files(root, &mut files)?;
    Ok(files)
}

fn collect_indexed_source_files(directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("failed to list {}: {error}", directory.display()))?;
    for entry in entries {
        let entry = entry
            .map_err(|error| format!("failed to read entry in {}: {error}", directory.display()))?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if matches!(
                name.as_ref(),
                ".git" | ".cache" | "target" | "node_modules" | ".playwright-mcp" | "book"
            ) {
                continue;
            }
            collect_indexed_source_files(&path, files)?;
        } else if is_indexed_source_path(&path) {
            files.push(path);
        }
    }
    Ok(())
}

fn is_indexed_source_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("rs" | "mica" | "md" | "markdown" | "js" | "mjs" | "cjs" | "toml")
    )
}

fn relative_path_string(path: &Path) -> Result<String, String> {
    let mut parts = Vec::new();
    for component in path.components() {
        let Component::Normal(part) = component else {
            return Err(format!(
                "path contains unsupported component: {}",
                path.display()
            ));
        };
        parts.push(part.to_string_lossy().into_owned());
    }
    Ok(parts.join("/"))
}

fn indexed_symbol_id(
    repository: &str,
    path: &str,
    start_byte: usize,
    end_byte: usize,
    name: &str,
) -> String {
    format!("idx:{repository}:{path}:{start_byte}:{end_byte}:{name}")
}

fn indexed_text_unit_id(
    repository: &str,
    path: &str,
    start_line: usize,
    end_line: usize,
    ordinal: usize,
) -> String {
    format!("idx:text:{repository}:{path}:{start_line}:{end_line}:{ordinal}")
}

fn source_language_kind(language: SourceLanguage) -> &'static str {
    match language {
        SourceLanguage::Rust => "rust",
        SourceLanguage::Mica => "mica",
        SourceLanguage::Markdown => "markdown",
        SourceLanguage::JavaScript => "javascript",
        SourceLanguage::Plain => "text",
    }
}

fn byte_line(line_starts: &[usize], byte_offset: usize) -> usize {
    line_starts.partition_point(|start| *start <= byte_offset)
}

fn is_index_identifier(name: &str) -> bool {
    name.split('/').all(is_index_identifier_segment)
}

fn is_index_identifier_segment(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

/// The persistent semantic index provider.
///
/// Loads and caches the configured index file and answers symbol, reference,
/// and text-search queries. It is dispatcher-owned in this pass; relational
/// selection policy is a later stage.
pub(crate) struct SemanticIndexProvider {
    index_path: Option<PathBuf>,
    cache: Mutex<Option<CachedSemanticIndex>>,
}

impl SemanticIndexProvider {
    pub(crate) fn new(index_path: Option<PathBuf>) -> Self {
        Self {
            index_path,
            cache: Mutex::new(None),
        }
    }

    pub(crate) fn load(
        &self,
        relation: RelationId,
    ) -> Result<Arc<PersistentSemanticIndex>, KernelError> {
        let mut cache = self.cache.lock().unwrap();
        if let Some(cached) = cache.as_ref()
            && cached.last_checked.elapsed() < SEMANTIC_INDEX_REFRESH_INTERVAL
        {
            return Ok(cached.index.clone());
        }

        let key = match &self.index_path {
            Some(path) => semantic_index_key(relation, path)?,
            None => None,
        };
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
        let index = match &self.index_path {
            Some(path) => Arc::new(PersistentSemanticIndex::load(relation, path)?),
            None => Arc::new(PersistentSemanticIndex::unconfigured()),
        };
        *cache = Some(CachedSemanticIndex {
            key,
            last_checked: Instant::now(),
            index: index.clone(),
        });
        Ok(index)
    }

    pub(crate) fn text_search(
        &self,
        relation: RelationId,
        query: &str,
        limit: usize,
        scope: &str,
    ) -> Result<Vec<TextSearchHit>, KernelError> {
        let index = self.load(relation)?;
        if !index.is_complete() || limit == 0 {
            return Ok(Vec::new());
        }
        Ok(text_search(&index, query, limit, scope))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SemanticIndexKey {
    len: u64,
    modified: Option<std::time::SystemTime>,
}

#[derive(Clone, Debug)]
struct CachedSemanticIndex {
    key: Option<SemanticIndexKey>,
    last_checked: Instant,
    index: Arc<PersistentSemanticIndex>,
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

fn text_search(
    index: &PersistentSemanticIndex,
    query: &str,
    limit: usize,
    scope: &str,
) -> Vec<TextSearchHit> {
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
                unit: unit.unit.clone(),
                score,
                path: unit.path.clone(),
                match_line: match_line.unwrap_or(unit.start_line),
                kind: unit.kind.clone(),
                title: unit.title.clone(),
                snippet: search_snippet(unit, &query_lower, &terms),
            })
        })
        .collect::<Vec<_>>();

    hits.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.match_line.cmp(&right.match_line))
            .then_with(|| left.unit.cmp(&right.unit))
    });
    hits.truncate(limit);
    hits
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
