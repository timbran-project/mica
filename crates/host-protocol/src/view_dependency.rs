// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com> This program is free
// software: you can redistribute it and/or modify it under the terms of the GNU
// Affero General Public License as published by the Free Software Foundation,
// version 3.
//
// This program is distributed in the hope that it will be useful, but WITHOUT
// ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS
// FOR A PARTICULAR PURPOSE. See the GNU Affero General Public License for more
// details.
//
// You should have received a copy of the GNU Affero General Public License along
// with this program. If not, see <https://www.gnu.org/licenses/>.

use mica_var::{Identity, Symbol, Value};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncViewDependencySubject {
    Facts,
    Relation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncViewRelation {
    Identity(Identity),
    Name(Symbol),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyncViewDependency {
    pub subject: SyncViewDependencySubject,
    pub relation: SyncViewRelation,
    pub bindings: Vec<Option<Value>>,
}

pub fn decode_sync_view_dependencies(value: &Value) -> Result<Vec<SyncViewDependency>, String> {
    value
        .with_list(|dependencies| {
            dependencies
                .iter()
                .enumerate()
                .map(|(index, dependency)| decode_dependency(index, dependency))
                .collect()
        })
        .ok_or_else(|| "sync_view_dependencies must return a list".to_owned())?
}

fn decode_dependency(index: usize, value: &Value) -> Result<SyncViewDependency, String> {
    value
        .with_map(|entries| {
            let subject = map_field(entries, "subject")
                .and_then(Value::as_symbol)
                .and_then(Symbol::name)
                .ok_or_else(|| dependency_error(index, "subject must be :facts or :relation"))?;
            let subject = match subject {
                "facts" => SyncViewDependencySubject::Facts,
                "relation" => SyncViewDependencySubject::Relation,
                _ => {
                    return Err(dependency_error(
                        index,
                        "subject must be :facts or :relation",
                    ));
                }
            };
            let relation = map_field(entries, "relation")
                .ok_or_else(|| dependency_error(index, "relation is required"))?;
            let relation = relation
                .as_identity()
                .map(SyncViewRelation::Identity)
                .or_else(|| relation.as_symbol().map(SyncViewRelation::Name))
                .ok_or_else(|| dependency_error(index, "relation must be an identity or symbol"))?;
            let bindings = map_field(entries, "bindings")
                .and_then(|bindings| {
                    bindings.with_list(|bindings| {
                        bindings
                            .iter()
                            .map(|binding| {
                                binding.option_payload().ok_or_else(|| {
                                    dependency_error(index, "bindings must contain option values")
                                })
                            })
                            .collect::<Result<Vec<_>, _>>()
                    })
                })
                .ok_or_else(|| dependency_error(index, "bindings must be a list"))??;
            Ok(SyncViewDependency {
                subject,
                relation,
                bindings,
            })
        })
        .ok_or_else(|| dependency_error(index, "entry must be a map"))?
}

fn map_field<'a>(entries: &'a [(Value, Value)], name: &str) -> Option<&'a Value> {
    let key = Symbol::intern(name);
    entries
        .iter()
        .find_map(|(candidate, value)| (candidate.as_symbol() == Some(key)).then_some(value))
}

fn dependency_error(index: usize, message: &str) -> String {
    format!("sync view dependency {index}: {message}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_relation_dependency_with_bound_columns() {
        let actor = Identity::new(7).unwrap();
        let value = Value::list([Value::map([
            (
                Value::symbol(Symbol::intern("subject")),
                Value::symbol(Symbol::intern("relation")),
            ),
            (
                Value::symbol(Symbol::intern("relation")),
                Value::symbol(Symbol::intern("VisibleObject")),
            ),
            (
                Value::symbol(Symbol::intern("bindings")),
                Value::list([
                    Value::option_some(Value::identity(actor)),
                    Value::option_none(),
                ]),
            ),
        ])]);

        assert_eq!(
            decode_sync_view_dependencies(&value).unwrap(),
            vec![SyncViewDependency {
                subject: SyncViewDependencySubject::Relation,
                relation: SyncViewRelation::Name(Symbol::intern("VisibleObject")),
                bindings: vec![Some(Value::identity(actor)), None],
            }]
        );
    }

    #[test]
    fn rejects_catalogue_dependencies() {
        let value = Value::list([Value::map([
            (
                Value::symbol(Symbol::intern("subject")),
                Value::symbol(Symbol::intern("catalogue")),
            ),
            (
                Value::symbol(Symbol::intern("relation")),
                Value::option_none(),
            ),
            (Value::symbol(Symbol::intern("bindings")), Value::list([])),
        ])]);

        assert_eq!(
            decode_sync_view_dependencies(&value).unwrap_err(),
            "sync view dependency 0: subject must be :facts or :relation"
        );
    }
}
