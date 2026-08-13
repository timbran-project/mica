use mica_relation_kernel::{ComputedRelationRead, KernelError, RelationId, RelationRead, Tuple};
use mica_var::{Symbol, Value};
use std::fs;
use std::path::{Component, Path, PathBuf};

pub(crate) fn relation_id(
    reader: &dyn ComputedRelationRead,
    name: &str,
    arity: u16,
) -> Option<RelationId> {
    reader.relation_id(Symbol::intern(name), arity)
}

pub(crate) fn expect_single_value(
    reader: &dyn RelationRead,
    relation: RelationId,
    bindings: &[Option<Value>],
    computed_relation: RelationId,
    message: &str,
) -> Result<Value, KernelError> {
    let rows = reader.scan_relation(relation, bindings)?;
    rows.first()
        .and_then(|row| row.values().get(1))
        .cloned()
        .ok_or_else(|| invalid_relation(computed_relation, message))
}

pub(crate) fn bound_value(
    relation: RelationId,
    bindings: &[Option<Value>],
    position: usize,
    label: &str,
) -> Result<Value, KernelError> {
    bindings
        .get(position)
        .and_then(Clone::clone)
        .ok_or_else(|| invalid_relation(relation, format!("expected bound {label}")))
}

pub(crate) fn bound_string(
    relation: RelationId,
    bindings: &[Option<Value>],
    position: usize,
    label: &str,
) -> Result<String, KernelError> {
    bound_value(relation, bindings, position, label)?
        .with_str(str::to_owned)
        .ok_or_else(|| invalid_relation(relation, format!("{label} must be a string")))
}

pub(crate) fn bound_positive_int(
    relation: RelationId,
    bindings: &[Option<Value>],
    position: usize,
    label: &str,
) -> Result<usize, KernelError> {
    let value = bound_value(relation, bindings, position, label)?
        .as_int()
        .ok_or_else(|| invalid_relation(relation, format!("{label} must be an integer")))?;
    if value < 1 {
        return Err(invalid_relation(
            relation,
            format!("{label} must be greater than zero"),
        ));
    }
    Ok(value as usize)
}

pub(crate) fn bound_non_negative_int(
    relation: RelationId,
    bindings: &[Option<Value>],
    position: usize,
    label: &str,
) -> Result<usize, KernelError> {
    let value = bound_value(relation, bindings, position, label)?
        .as_int()
        .ok_or_else(|| invalid_relation(relation, format!("{label} must be an integer")))?;
    if value < 0 {
        return Err(invalid_relation(
            relation,
            format!("{label} must be non-negative"),
        ));
    }
    Ok(value as usize)
}

pub(crate) fn validate_relative_path(relation: RelationId, path: &str) -> Result<(), KernelError> {
    let path = Path::new(path);
    if path.is_absolute() {
        return Err(invalid_relation(
            relation,
            "source path must be relative to repository root",
        ));
    }
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => {
                return Err(invalid_relation(
                    relation,
                    "source path must not contain parent components",
                ));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(invalid_relation(
                    relation,
                    "source path must be relative to repository root",
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn read_file_bytes(relation: RelationId, file: &Path) -> Result<Vec<u8>, KernelError> {
    if !file.is_file() {
        return Err(invalid_relation(relation, "source path must be a file"));
    }
    fs::read(file)
        .map_err(|error| invalid_relation(relation, format!("failed to read file: {error}")))
}

pub(crate) fn read_utf8_file(relation: RelationId, file: &Path) -> Result<String, KernelError> {
    let bytes = read_file_bytes(relation, file)?;
    String::from_utf8(bytes)
        .map_err(|error| invalid_relation(relation, format!("source file is not utf-8: {error}")))
}

pub(crate) fn rust_workspace_root(repository_root: &Path) -> PathBuf {
    for ancestor in repository_root.ancestors() {
        let manifest = ancestor.join("Cargo.toml");
        if fs::read_to_string(&manifest)
            .is_ok_and(|source| source.lines().any(|line| line.trim() == "[workspace]"))
        {
            return ancestor.to_owned();
        }
    }
    repository_root.to_owned()
}

pub(crate) fn path_to_mica_string(
    relation: RelationId,
    path: &Path,
) -> Result<String, KernelError> {
    path.to_str()
        .map(|path| path.replace('\\', "/"))
        .ok_or_else(|| invalid_relation(relation, "source path is not valid utf-8"))
}

pub(crate) fn content_hash(bytes: &[u8]) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("fnv1a64:{hash:016x}")
}

pub(crate) fn int_value(relation: RelationId, value: i64) -> Result<Value, KernelError> {
    Value::int(value).map_err(|error| invalid_relation(relation, format!("{error:?}")))
}

pub(crate) fn filter_bound_rows(rows: Vec<Tuple>, bindings: &[Option<Value>]) -> Vec<Tuple> {
    rows.into_iter()
        .filter(|row| {
            row.values()
                .iter()
                .zip(bindings.iter())
                .all(|(value, binding)| binding.as_ref().is_none_or(|binding| binding == value))
        })
        .collect()
}

pub(crate) fn invalid_relation(relation: RelationId, message: impl Into<String>) -> KernelError {
    KernelError::InvalidComputedRelation {
        relation,
        message: message.into(),
    }
}
