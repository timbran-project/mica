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

use mica_relation_kernel::{
    ComputedPreparationCache, ComputedRelation, ComputedRelationRead, ComputedRow, KernelError,
    RelationId, RelationMetadata, RelationRead, Tuple, system_computed_relations,
};
use mica_var::{Symbol, Value};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::Arc;

const REQUIRED_BOUND_POSITIONS: &[u16] = &[0, 1, 2];

pub(crate) fn default_computed_relations() -> Vec<Arc<dyn ComputedRelation>> {
    let mut relations = system_computed_relations();
    relations.push(Arc::new(ExactEmbeddingSearchRelation));
    relations.extend(crate::buffer_computed::relations());
    relations
}

struct ExactEmbeddingSearchRelation;

impl ComputedRelation for ExactEmbeddingSearchRelation {
    fn name(&self) -> &'static str {
        "exact-embedding-search"
    }

    fn matches(&self, metadata: &RelationMetadata) -> bool {
        metadata.name().name() == Some("NearestEmbedding") && metadata.arity() == 6
    }

    fn required_bound_positions(&self, _metadata: &RelationMetadata) -> &[u16] {
        REQUIRED_BOUND_POSITIONS
    }

    fn scan(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, KernelError> {
        let local = ComputedPreparationCache::default();
        search(
            reader,
            metadata,
            bindings,
            reader.preparation_cache().unwrap_or(&local),
        )
    }

    fn scan_batch(
        &self,
        reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Vec<Option<Value>>],
    ) -> Result<Vec<ComputedRow>, KernelError> {
        let local = ComputedPreparationCache::default();
        let cache = reader.preparation_cache().unwrap_or(&local);
        let mut rows = Vec::new();
        for (input_row, bindings) in bindings.iter().enumerate() {
            rows.extend(
                search(reader, metadata, bindings, cache)?
                    .into_iter()
                    .map(|tuple| ComputedRow { input_row, tuple }),
            );
        }
        Ok(rows)
    }

    fn estimate(
        &self,
        _reader: &dyn ComputedRelationRead,
        metadata: &RelationMetadata,
        bindings: &[Option<Value>],
    ) -> Result<usize, KernelError> {
        parse_limit(
            metadata.id(),
            bindings[2]
                .as_ref()
                .expect("registry checks required inputs"),
        )
    }
}

struct PreparedEmbedding {
    subject: Value,
    vector: Vec<f64>,
    squared_norm: f64,
}

fn prepare_embeddings(
    reader: &dyn ComputedRelationRead,
    relation: RelationId,
    index: &Value,
) -> Result<Vec<PreparedEmbedding>, KernelError> {
    let vector_index_contains = relation_id(reader, "VectorIndexContains", 2)
        .ok_or_else(|| invalid_relation(relation, "missing relation VectorIndexContains/2"))?;
    let embedding_of = relation_id(reader, "EmbeddingOf", 2)
        .ok_or_else(|| invalid_relation(relation, "missing relation EmbeddingOf/2"))?;
    let embedding_vector = relation_id(reader, "EmbeddingVector", 2)
        .ok_or_else(|| invalid_relation(relation, "missing relation EmbeddingVector/2"))?;
    let members = reader.scan_relation(vector_index_contains, &[Some(index.clone()), None])?;
    let mut prepared = Vec::with_capacity(members.len());
    for membership in members {
        let Some(embedding) = membership.values().get(1).cloned() else {
            continue;
        };
        let vector = expect_single_value(
            reader,
            embedding_vector,
            &[Some(embedding.clone()), None],
            relation,
            "expected EmbeddingVector(embedding, payload)",
        )?;
        let subject = expect_single_value(
            reader,
            embedding_of,
            &[Some(embedding), None],
            relation,
            "expected EmbeddingOf(embedding, subject)",
        )?;
        let vector = parse_vector(relation, &vector)?;
        let squared_norm = vector.iter().map(|value| value * value).sum();
        prepared.push(PreparedEmbedding {
            subject,
            vector,
            squared_norm,
        });
    }
    Ok(prepared)
}

fn search(
    reader: &dyn ComputedRelationRead,
    metadata: &RelationMetadata,
    bindings: &[Option<Value>],
    cache: &ComputedPreparationCache,
) -> Result<Vec<Tuple>, KernelError> {
    let index = bindings[0]
        .as_ref()
        .expect("registry checks required inputs");
    let query_value = bindings[1]
        .as_ref()
        .expect("registry checks required inputs");
    let limit_value = bindings[2]
        .as_ref()
        .expect("registry checks required inputs");
    let query = parse_vector(metadata.id(), query_value)?;
    let query_norm = query.iter().map(|value| value * value).sum();
    let limit = parse_limit(metadata.id(), limit_value)?;
    let prepared = cache.get_or_prepare(metadata.id(), index, || {
        prepare_embeddings(reader, metadata.id(), index)
    })?;
    let mut best_by_subject = BTreeMap::<Value, f64>::new();
    for candidate in prepared.iter() {
        let score = cosine_similarity(
            metadata.id(),
            &query,
            query_norm,
            &candidate.vector,
            candidate.squared_norm,
        )?;
        best_by_subject
            .entry(candidate.subject.clone())
            .and_modify(|current| {
                if score > *current {
                    *current = score;
                }
            })
            .or_insert(score);
    }
    let snapshot_version = Value::int(reader.version() as i64)
        .map_err(|_| invalid_relation(metadata.id(), "snapshot version exceeds integer range"))?;
    let mut ranked = best_by_subject.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });
    // Output bindings filter the selected top-k subjects, never the search population.
    ranked
        .into_iter()
        .take(limit)
        .map(|(subject, score)| {
            Ok(Tuple::from([
                index.clone(),
                query_value.clone(),
                limit_value.clone(),
                subject,
                host_score_to_value(metadata.id(), score)?,
                snapshot_version.clone(),
            ]))
        })
        .filter_map(|row| match row {
            Ok(row) if row.matches_bindings(bindings) => Some(Ok(row)),
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .collect()
}

fn invalid_relation(relation: RelationId, message: impl Into<String>) -> KernelError {
    KernelError::InvalidComputedRelation {
        relation,
        message: message.into(),
    }
}

fn relation_id(reader: &dyn ComputedRelationRead, name: &str, arity: u16) -> Option<RelationId> {
    reader.relation_id(Symbol::intern(name), arity)
}

fn parse_limit(relation: RelationId, value: &Value) -> Result<usize, KernelError> {
    let Some(limit) = value.as_int() else {
        return Err(invalid_relation(relation, "limit must be an integer"));
    };
    if limit < 0 {
        return Err(invalid_relation(relation, "limit must be non-negative"));
    }
    Ok(limit as usize)
}

fn parse_vector(relation: RelationId, value: &Value) -> Result<Vec<f64>, KernelError> {
    value
        .with_list(|values| {
            values
                .iter()
                .map(|value| {
                    value
                        .as_float()
                        .map(|value| value as f64)
                        .or_else(|| value.as_int().map(|value| value as f64))
                        .ok_or_else(|| {
                            invalid_relation(
                                relation,
                                "embedding payloads must be lists of ints or floats",
                            )
                        })
                })
                .collect::<Result<Vec<_>, KernelError>>()
        })
        .ok_or_else(|| invalid_relation(relation, "embedding payload must be a list"))?
}

/// Narrows a host f64 score to a finite binary32 Mica value.
///
/// Host-side embedding math is done in f64. The final score crosses this
/// boundary into Mica's binary32 float representation.
fn host_score_to_value(relation: RelationId, score: f64) -> Result<Value, KernelError> {
    let value = score as f32;
    Value::float(value)
        .map_err(|_| invalid_relation(relation, "host score is not a finite binary32 value"))
}

fn expect_single_value(
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

fn cosine_similarity(
    relation: RelationId,
    left: &[f64],
    left_norm: f64,
    right: &[f64],
    right_norm: f64,
) -> Result<f64, KernelError> {
    if left.is_empty() || right.is_empty() {
        return Err(invalid_relation(
            relation,
            "embedding vectors must be non-empty",
        ));
    }
    if left.len() != right.len() {
        return Err(invalid_relation(
            relation,
            "query and candidate embeddings must have the same dimension",
        ));
    }

    let mut dot = 0.0;
    for (left_value, right_value) in left.iter().zip(right.iter()) {
        dot += left_value * right_value;
    }
    if left_norm == 0.0 || right_norm == 0.0 {
        return Err(invalid_relation(
            relation,
            "embedding vectors must not have zero magnitude",
        ));
    }
    Ok(dot / (left_norm.sqrt() * right_norm.sqrt()))
}

#[cfg(test)]
mod tests {
    use crate::{SourceRunner, TaskInput, TaskOutcome, TaskRequest};
    use mica_relation_kernel::{
        ComputedPreparationCache, ComputedRelation, ComputedRelationRead, KernelError, RelationId,
        RelationMetadata, RelationRead, RuleDefinition, Snapshot, Version,
    };
    use mica_var::{Symbol, Tuple, Value};
    use mica_vm::AuthorityContext;
    use std::cell::Cell;
    use std::sync::Arc;

    struct ConstantEmbeddingProvider;

    impl crate::embedding::EmbeddingProvider for ConstantEmbeddingProvider {
        fn embed_text(&self, model: &str, text: &str) -> Result<Vec<f64>, String> {
            Ok(vec![model.len() as f64, text.len() as f64])
        }
    }

    #[test]
    fn nearest_embedding_filters_bound_outputs_after_selecting_top_candidates() {
        let mut runner = SourceRunner::new_empty();
        runner
            .run_filein(
                "make_relation(:NearestEmbedding, 6)
             make_relation(:VectorIndexContains, 2)
             make_functional_relation(:EmbeddingOf, 2, [0])
             make_functional_relation(:EmbeddingVector, 2, [0])
             assert VectorIndexContains(:manuals, :first)
             assert VectorIndexContains(:manuals, :second)
             assert EmbeddingOf(:first, :calibration)
             assert EmbeddingOf(:second, :cleaning)
             assert EmbeddingVector(:first, [1.0, 0.0])
             assert EmbeddingVector(:second, [0.0, 1.0])",
            )
            .unwrap();
        for source in [
            "return !NearestEmbedding(:manuals, [1.0, 0.0], 1, :cleaning, _, _)",
            "return !NearestEmbedding(:manuals, [1.0, 0.0], 2, :missing, _, _)",
            "return !NearestEmbedding(:manuals, [1.0, 0.0], 2, _, 0.5, _)",
            "return !NearestEmbedding(:manuals, [1.0, 0.0], 2, _, _, -1)",
            "return NearestEmbedding(:manuals, [1.0, 0.0], 2, :cleaning, 0.0, _)",
            "return NearestEmbedding(:manuals, [1.0, 0.0], 2, :cleaning, ?score, _) == [:score] { [0.0] }",
        ] {
            let report = runner.run_source(source).unwrap();
            assert!(
                matches!(report.outcome, TaskOutcome::Complete { value, .. } if value == Value::bool(true)),
                "{source}"
            );
        }
    }

    #[test]
    fn runner_queries_exact_nearest_embedding_relation() {
        let mut runner = SourceRunner::new_empty();
        runner
            .run_filein(include_str!("../../../apps/shared/retrieval.mica"))
            .unwrap();
        runner
            .run_source(
                "make_identity(:main_index)\n\
                 make_identity(:doc_one)\n\
                 make_identity(:doc_two)\n\
                 make_identity(:emb_one)\n\
                 make_identity(:emb_two)\n\
                 assert VectorIndex(#main_index)\n\
                 assert VectorIndexContains(#main_index, #emb_one)\n\
                 assert VectorIndexContains(#main_index, #emb_two)\n\
                 assert Embedding(#emb_one)\n\
                 assert Embedding(#emb_two)\n\
                 assert EmbeddingOf(#emb_one, #doc_one)\n\
                 assert EmbeddingOf(#emb_two, #doc_two)\n\
                 assert EmbeddingVector(#emb_one, [1.0, 0.0])\n\
                 assert EmbeddingVector(#emb_two, [0.0, 1.0])",
            )
            .unwrap();

        let report = runner
            .run_source("return NearestEmbedding(#main_index, [1.0, 0.0], 2, ?subject, ?score, _)")
            .unwrap();

        let TaskOutcome::Complete { value, .. } = report.outcome else {
            panic!("expected complete outcome, got {:?}", report.outcome);
        };
        assert_eq!(
            value,
            Value::relation(
                [Symbol::intern("subject"), Symbol::intern("score")],
                [
                    Tuple::from([
                        Value::identity(runner.named_identity(Symbol::intern("doc_one")).unwrap()),
                        Value::float(1.0).unwrap(),
                    ]),
                    Tuple::from([
                        Value::identity(runner.named_identity(Symbol::intern("doc_two")).unwrap()),
                        Value::float(0.0).unwrap(),
                    ]),
                ],
            )
            .unwrap()
        );
    }

    #[test]
    fn runner_rejects_writes_to_nearest_embedding_relation() {
        let mut runner = SourceRunner::new_empty();
        runner
            .run_filein(include_str!("../../../apps/shared/retrieval.mica"))
            .unwrap();
        runner.run_source("make_identity(:main_index)").unwrap();

        let error = runner
            .run_source("assert NearestEmbedding(#main_index, [1.0], 1, #main_index, 1.0, 0)")
            .unwrap_err();
        assert!(format!("{error:?}").contains("ReadOnlyRelation"));
    }

    #[test]
    fn runner_indexes_text_units_with_host_embedding_builtin() {
        let mut runner = SourceRunner::new_empty();
        runner
            .run_filein(include_str!("../../../apps/shared/retrieval.mica"))
            .unwrap();
        runner
            .run_source(
                "make_identity(:main_index)\n\
                 make_identity(:unit_one)\n\
                 assert VectorIndex(#main_index)\n\
                 assert VectorIndexMetric(#main_index, \"cosine\")\n\
                 assert TextUnit(#unit_one)\n\
                 assert TextUnitText(#unit_one, \"red brass lamp\")\n\
                 return index_text_unit(none, #main_index, #unit_one, \"host-test\")",
            )
            .unwrap();

        let query = runner
            .run_source(
                "let exactly {:embedding -> embedding} = VectorIndexContains(#main_index, ?embedding)\n\
                 let exactly {:subject -> subject} = EmbeddingOf(embedding, ?subject)\n\
                 let exactly {:model -> model} = EmbeddingModel(embedding, ?model)\n\
                 return [embedding, subject, model]",
            )
            .unwrap();

        let TaskOutcome::Complete { value, .. } = query.outcome else {
            panic!("expected complete outcome");
        };
        value
            .with_list(|values| {
                assert_eq!(values.len(), 3);
                assert_eq!(
                    values[1],
                    Value::identity(runner.named_identity(Symbol::intern("unit_one")).unwrap())
                );
                assert_eq!(values[2], Value::string("host-test"));
            })
            .expect("expected list result");
    }

    #[test]
    fn runner_uses_configured_embedding_provider() {
        let mut runner = SourceRunner::with_kernel_and_embedding_provider(
            crate::bootstrap_kernel(),
            Arc::new(ConstantEmbeddingProvider),
        );
        runner
            .run_filein(include_str!("../../../apps/shared/retrieval.mica"))
            .unwrap();
        runner
            .run_source(
                "make_identity(:main_index)\n\
                 make_identity(:unit_one)\n\
                 assert VectorIndex(#main_index)\n\
                 assert VectorIndexMetric(#main_index, \"cosine\")\n\
                 assert TextUnit(#unit_one)\n\
                 assert TextUnitText(#unit_one, \"red brass lamp\")\n\
                 return index_text_unit(none, #main_index, #unit_one, \"custom-model\")",
            )
            .unwrap();

        let query = runner
            .run_source(
                "let exactly {:embedding -> embedding} = VectorIndexContains(#main_index, ?embedding)\n\
                 let exactly {:vector -> vector} = EmbeddingVector(embedding, ?vector)\n\
                 return vector",
            )
            .unwrap();

        let TaskOutcome::Complete { value, .. } = query.outcome else {
            panic!("expected complete outcome");
        };
        value
            .with_list(|values| {
                assert_eq!(
                    values[0],
                    Value::float("custom-model".len() as f32).unwrap()
                );
                assert_eq!(
                    values[1],
                    Value::float("red brass lamp".len() as f32).unwrap()
                );
            })
            .expect("expected list result");
    }

    #[test]
    fn text_unit_status_tracks_missing_ready_and_stale_embeddings() {
        let mut runner = SourceRunner::new_empty();
        runner
            .run_filein(include_str!("../../../apps/shared/retrieval.mica"))
            .unwrap();

        let initial = runner
            .run_source(
                "make_identity(:main_index)\n\
                 make_identity(:unit_one)\n\
                 assert VectorIndex(#main_index)\n\
                 assert VectorIndexMetric(#main_index, \"cosine\")\n\
                 assert TextUnit(#unit_one)\n\
                 return retrieval/text_unit_status(#main_index, #unit_one, \"host-test\")",
            )
            .unwrap();
        let TaskOutcome::Complete { value, .. } = initial.outcome else {
            panic!("expected complete outcome");
        };
        assert_eq!(value, Value::string("missing"));

        runner
            .run_source(
                "assert TextUnitText(#unit_one, \"red brass lamp\")\n\
                 index_text_unit(none, #main_index, #unit_one, \"host-test\")",
            )
            .unwrap();

        let ready = runner
            .run_source(
                "let status = retrieval/text_unit_status(#main_index, #unit_one, \"host-test\")\n\
                 let refresh = EmbeddingRefreshNeeded(#main_index, #unit_one, \"host-test\")\n\
                 return [status, refresh]",
            )
            .unwrap();
        let TaskOutcome::Complete { value, .. } = ready.outcome else {
            panic!("expected complete outcome");
        };
        value
            .with_list(|values| {
                assert_eq!(values[0], Value::string("ready"));
                assert_eq!(values[1], Value::bool(false));
            })
            .expect("expected list result");

        runner
            .run_source(
                "retract TextUnitText(#unit_one, _)\n\
                 assert TextUnitText(#unit_one, \"blue steel lantern\")",
            )
            .unwrap();

        let stale = runner
            .run_source(
                "let status = retrieval/text_unit_status(#main_index, #unit_one, \"host-test\")\n\
                 let refresh = EmbeddingRefreshNeeded(#main_index, #unit_one, \"host-test\")\n\
                 let exactly {:embedding -> embedding} = IndexEntryEmbedding(#main_index, #unit_one, \"host-test\", ?embedding)\n\
                 let exactly {:status -> embedding_status} = EmbeddingStatus(embedding, ?status)\n\
                 let indexed = VectorIndexContains(#main_index, embedding)\n\
                 return [status, refresh, embedding_status, indexed]",
            )
            .unwrap();
        let TaskOutcome::Complete { value, .. } = stale.outcome else {
            panic!("expected complete outcome");
        };
        value
            .with_list(|values| {
                assert_eq!(values[0], Value::string("stale"));
                assert_eq!(values[1], Value::bool(true));
                assert_eq!(values[2], Value::string("stale"));
                assert_eq!(values[3], Value::bool(false));
            })
            .expect("expected list result");
    }

    #[test]
    fn answer_question_records_retrieval_artifacts_and_citations() {
        let mut runner = SourceRunner::new_empty();
        runner
            .run_filein(include_str!("../../../apps/shared/retrieval.mica"))
            .unwrap();
        runner
            .run_source(
                "make_identity(:main_index)\n\
                 make_identity(:unit_one)\n\
                 make_identity(:unit_two)\n\
                 assert VectorIndex(#main_index)\n\
                 assert VectorIndexMetric(#main_index, \"cosine\")\n\
                 assert TextUnit(#unit_one)\n\
                 assert TextUnit(#unit_two)\n\
                 assert TextUnitText(#unit_one, \"lamp oil brass light\")\n\
                 assert TextUnitText(#unit_two, \"apple orchard green fruit\")\n\
                 assert CanRetrieveSubject(#main_index, #unit_one)\n\
                 index_text_unit(none, #main_index, #unit_one, \"host-test\")\n\
                 index_text_unit(none, #main_index, #unit_two, \"host-test\")\n\
                 return answer_question(#main_index, #main_index, \"brass lamp\", 2, \"host-test\")",
            )
            .unwrap();

        let report = runner
            .run_source(
                "let exactly {:answer -> answer} = Answer(?answer)\n\
                 let exactly {:question -> question} = Question(?question)\n\
                 let exactly {:plan -> plan} = RetrievalPlan(?plan)\n\
                 let exactly {:context -> context} = RetrievedContext(?context)\n\
                 let exactly {:embedding -> embedding} = IndexEntryEmbedding(#main_index, #unit_one, \"host-test\", ?embedding)\n\
                 let exactly {:text -> question_text} = QuestionText(question, ?text)\n\
                 let exactly {:kind -> plan_kind} = PlanKind(plan, ?kind)\n\
                 let exactly {:model -> plan_model} = PlanModel(plan, ?model)\n\
                 let exactly {:reason -> context_reason} = ContextReason(context, ?reason)\n\
                 let exactly {:snapshot_version -> context_version} = ContextSnapshotVersion(context, ?snapshot_version)\n\
                 let exactly {:context_text -> answer_context} = AnswerContextText(answer, ?context_text)\n\
                 let exactly {:status -> answer_status} = AnswerStatus(answer, ?status)\n\
                 let exactly {:status -> embedding_status} = EmbeddingStatus(embedding, ?status)\n\
                 return [question_text, plan_kind, plan_model, context_reason, context_version != none, string_contains(answer_context, \"lamp oil\"), answer_status, embedding_status]",
            )
            .unwrap();

        let TaskOutcome::Complete { value, .. } = report.outcome else {
            panic!("expected complete outcome, got {:?}", report.outcome);
        };
        value
            .with_list(|values| {
                assert_eq!(values[0], Value::string("brass lamp"));
                assert_eq!(values[1], Value::string("nearest_embedding"));
                assert_eq!(values[2], Value::string("host-test"));
                assert_eq!(values[3], Value::string("nearest_embedding"));
                assert_eq!(values[4], Value::bool(true));
                assert_eq!(values[5], Value::bool(true));
                assert_eq!(values[6], Value::string("fresh"));
                assert_eq!(values[7], Value::string("ready"));
            })
            .expect("expected list result");
    }

    #[test]
    fn retrieve_context_records_only_authorized_subjects() {
        let mut runner = SourceRunner::new_empty();
        runner
            .run_filein(include_str!("../../../apps/shared/retrieval.mica"))
            .unwrap();
        runner
            .run_source(
                "make_identity(:main_index)\n\
                 make_identity(:actor)\n\
                 make_identity(:unit_allowed)\n\
                 make_identity(:unit_hidden)\n\
                 assert VectorIndex(#main_index)\n\
                 assert VectorIndexMetric(#main_index, \"cosine\")\n\
                 assert TextUnit(#unit_allowed)\n\
                 assert TextUnit(#unit_hidden)\n\
                 assert TextUnitText(#unit_allowed, \"lamp oil brass light\")\n\
                 assert TextUnitText(#unit_hidden, \"lamp oil secret room\")\n\
                 assert CanRetrieveSubject(#actor, #unit_allowed)\n\
                 index_text_unit(none, #main_index, #unit_allowed, \"host-test\")\n\
                 index_text_unit(none, #main_index, #unit_hidden, \"host-test\")\n\
                 return retrieve_context(#actor, #main_index, \"brass lamp\", 5, \"host-test\")",
            )
            .unwrap();

        let report = runner
            .run_source(
                "let has_allowed = false\n\
                 let has_hidden = false\n\
                 for found in RetrievedContext(?context)\n\
                   let exactly {:subject -> subject} = ContextSubject(found[:context], ?subject)\n\
                   if subject == #unit_allowed\n\
                     has_allowed = true\n\
                   elseif subject == #unit_hidden\n\
                     has_hidden = true\n\
                   end\n\
                 end\n\
                 return [has_allowed, has_hidden]",
            )
            .unwrap();

        let TaskOutcome::Complete { value, .. } = report.outcome else {
            panic!("expected complete outcome, got {:?}", report.outcome);
        };
        value
            .with_list(|values| {
                assert_eq!(values[0], Value::bool(true));
                assert_eq!(values[1], Value::bool(false));
            })
            .expect("expected list result");
    }

    #[test]
    fn answer_refresh_status_marks_answers_stale_after_text_changes() {
        let mut runner = SourceRunner::new_empty();
        runner
            .run_filein(include_str!("../../../apps/shared/retrieval.mica"))
            .unwrap();
        runner
            .run_source(
                "make_identity(:main_index)\n\
                 make_identity(:unit_one)\n\
                 assert VectorIndex(#main_index)\n\
                 assert VectorIndexMetric(#main_index, \"cosine\")\n\
                 assert TextUnit(#unit_one)\n\
                 assert TextUnitText(#unit_one, \"red brass lamp\")\n\
                 assert CanRetrieveSubject(#main_index, #unit_one)\n\
                 index_text_unit(none, #main_index, #unit_one, \"host-test\")\n\
                 answer_question(#main_index, #main_index, \"brass lamp\", 1, \"host-test\")\n\
                 retract TextUnitText(#unit_one, _)\n\
                 assert TextUnitText(#unit_one, \"blue steel lantern\")\n\
                 let exactly {:answer -> answer} = Answer(?answer)\n\
                 return answer_refresh_status(answer)",
            )
            .unwrap();

        let report = runner
            .run_source(
                "let exactly {:answer -> answer} = Answer(?answer)\n\
                 let exactly {:status -> status} = AnswerStatus(answer, ?status)\n\
                 let needs_review = AnswerNeedsReview(answer)\n\
                 let refresh = AnswerRefreshNeeded(answer)\n\
                 return [status, needs_review, refresh]",
            )
            .unwrap();

        let TaskOutcome::Complete { value, .. } = report.outcome else {
            panic!("expected complete outcome, got {:?}", report.outcome);
        };
        value
            .with_list(|values| {
                assert_eq!(values[0], Value::string("stale"));
                assert_eq!(values[1], Value::bool(true));
                assert_eq!(values[2], Value::bool(true));
            })
            .expect("expected list result");
    }

    #[test]
    fn mud_retrieval_query_writes_session_plan_and_ready_index_status() {
        let mut runner = SourceRunner::new_empty();
        for filein in [
            include_str!("../../../apps/shared/sync-host.mica"),
            include_str!("../../../apps/shared/string.mica"),
            include_str!("../../../apps/shared/events.mica"),
            include_str!("../../../apps/mud/core.mica"),
            include_str!("../../../apps/mud/auth.mica"),
            include_str!("../../../apps/mud/event-substitutions.mica"),
            include_str!("../../../apps/mud/command-parser.mica"),
            include_str!("../../../apps/shared/retrieval.mica"),
            include_str!("../../../apps/shared/sync-dom.mica"),
            include_str!("../../../apps/mud/ui-session.mica"),
            include_str!("../../../apps/mud/ui-retrieval.mica"),
            include_str!("../../../apps/mud/ui-mica-inspect.mica"),
            include_str!("../../../apps/mud/ui-compose.mica"),
            include_str!("../../../apps/mud/ui-narrative.mica"),
            include_str!("../../../apps/mud/ui-actions.mica"),
            include_str!("../../../apps/mud/http.mica"),
        ] {
            runner.run_filein(filein).unwrap();
        }

        runner
            .run_source(
                "assert session/Actor(endpoint(), #alice)\n\
                 assert session/Inspect(endpoint(), #coin)\n\
                 return ui/retrieval_query_selected(#alice, endpoint(), #coin)",
            )
            .unwrap();

        let report = runner
            .run_source(
                "let exactly {:plan -> plan} = session/RetrievalPlan(endpoint(), ?plan)\n\
                 let exactly {:question -> question} = PlanForQuestion(plan, ?question)\n\
                 let has_context = false\n\
                 for found in ContextForPlan(?context, plan)\n\
                   has_context = true\n\
                 end\n\
                 let exactly {:status -> status} = IndexEntryStatus(#retrieval/mud_world, #coin, \"mud-world\", ?status)\n\
                 let exactly {:text -> prompt} = QuestionText(question, ?text)\n\
                 return [status, prompt, has_context]",
            )
            .unwrap();

        let TaskOutcome::Complete { value, .. } = report.outcome else {
            panic!("expected complete outcome, got {:?}", report.outcome);
        };
        value
            .with_list(|values| {
                assert_eq!(values[0], Value::string("ready"));
                assert_eq!(
                    values[1],
                    Value::string("coin: A tarnished brass coin catches the light.")
                );
                assert_eq!(values[2], Value::bool(true));
            })
            .expect("expected list result");
    }

    #[test]
    fn mud_retrieval_sync_event_updates_the_session_panel() {
        let mut runner = SourceRunner::new_empty();
        for filein in [
            include_str!("../../../apps/shared/sync-host.mica"),
            include_str!("../../../apps/shared/string.mica"),
            include_str!("../../../apps/shared/events.mica"),
            include_str!("../../../apps/mud/core.mica"),
            include_str!("../../../apps/mud/auth.mica"),
            include_str!("../../../apps/mud/event-substitutions.mica"),
            include_str!("../../../apps/mud/command-parser.mica"),
            include_str!("../../../apps/shared/retrieval.mica"),
            include_str!("../../../apps/shared/sync-dom.mica"),
            include_str!("../../../apps/mud/ui-session.mica"),
            include_str!("../../../apps/mud/ui-retrieval.mica"),
            include_str!("../../../apps/mud/ui-mica-inspect.mica"),
            include_str!("../../../apps/mud/ui-compose.mica"),
            include_str!("../../../apps/mud/ui-narrative.mica"),
            include_str!("../../../apps/mud/ui-actions.mica"),
            include_str!("../../../apps/mud/http.mica"),
        ] {
            runner.run_filein(filein).unwrap();
        }

        runner
            .run_source(
                "assert session/Actor(endpoint(), #alice)\n\
                 assert session/Inspect(endpoint(), #bob)",
            )
            .unwrap();

        let report = runner
            .run_source_as(
                Symbol::intern("alice"),
                "return sync_event(endpoint(), none, 21, \"submit\", \"\", \"mud_retrieve_related\", {})",
            )
            .unwrap();
        let TaskOutcome::Complete { value, .. } = report.outcome else {
            panic!("expected complete outcome, got {:?}", report.outcome);
        };
        assert_eq!(value, Value::bool(true));

        let panel = runner
            .run_source(
                "let exactly {:plan -> plan} = session/RetrievalPlan(endpoint(), ?plan)\n\
                 let has_context = false\n\
                 for found in ContextForPlan(?context, plan)\n\
                   has_context = true\n\
                 end\n\
                 return has_context",
            )
            .unwrap();
        let TaskOutcome::Complete { value, .. } = panel.outcome else {
            panic!("expected complete outcome, got {:?}", panel.outcome);
        };
        assert_eq!(value, Value::bool(true));
    }
    fn search_fixture() -> SourceRunner {
        let mut runner = SourceRunner::new_empty();
        runner
            .run_filein(
                "make_relation(:NearestEmbedding, 6)
            make_relation(:VectorIndexContains, 2)
            make_functional_relation(:EmbeddingOf, 2, [0])
            make_functional_relation(:EmbeddingVector, 2, [0])
            assert VectorIndexContains(:docs, :a)
            assert VectorIndexContains(:docs, :b)
            assert VectorIndexContains(:docs, :c)
            assert EmbeddingOf(:a, 1)
            assert EmbeddingOf(:b, 2)
            assert EmbeddingOf(:c, 1)
            assert EmbeddingVector(:a, [1.0, 0.0])
            assert EmbeddingVector(:b, [1.0, 0.0])
            assert EmbeddingVector(:c, [0.0, 1.0])",
            )
            .unwrap();
        runner
    }

    struct CountedSnapshot<'a> {
        snapshot: &'a Snapshot,
        reads: Cell<usize>,
        cache: ComputedPreparationCache,
    }

    impl RelationRead for CountedSnapshot<'_> {
        fn scan_relation(
            &self,
            relation: RelationId,
            bindings: &[Option<Value>],
        ) -> Result<Vec<Tuple>, KernelError> {
            self.reads.set(self.reads.get() + 1);
            self.snapshot.scan_relation(relation, bindings)
        }
    }

    impl ComputedRelationRead for CountedSnapshot<'_> {
        fn version(&self) -> Version {
            self.snapshot.version()
        }
        fn relation_metadata_vec(&self) -> Vec<RelationMetadata> {
            self.snapshot.relation_metadata_vec()
        }
        fn rules_vec(&self) -> Vec<RuleDefinition> {
            self.snapshot.rules_vec()
        }
        fn index_storage_kind(&self, relation: RelationId, ordinal: usize) -> Option<Symbol> {
            self.snapshot.index_storage_kind(relation, ordinal)
        }
        fn extensional_facts(&self) -> Result<Vec<(RelationId, Tuple)>, KernelError> {
            self.snapshot.extensional_facts()
        }
        fn preparation_cache(&self) -> Option<&ComputedPreparationCache> {
            Some(&self.cache)
        }
    }

    #[test]
    fn batched_exact_search_prepares_once_and_preserves_ranking_and_output_filters() {
        let runner = search_fixture();
        let snapshot = runner.task_manager.kernel().snapshot();
        let reader = CountedSnapshot {
            snapshot: &snapshot,
            reads: Cell::new(0),
            cache: ComputedPreparationCache::default(),
        };
        let relation = super::relation_id(&reader, "NearestEmbedding", 6).unwrap();
        let metadata = snapshot
            .relation_metadata()
            .find(|metadata| metadata.id() == relation)
            .unwrap();
        let query = Value::list([Value::float(1.0).unwrap(), Value::float(0.0).unwrap()]);
        let keys = [
            vec![
                Some(Value::symbol(Symbol::intern("docs"))),
                Some(query.clone()),
                Some(Value::int(1).unwrap()),
                None,
                None,
                None,
            ],
            vec![
                Some(Value::symbol(Symbol::intern("docs"))),
                Some(query),
                Some(Value::int(1).unwrap()),
                Some(Value::int(2).unwrap()),
                None,
                None,
            ],
            vec![
                Some(Value::symbol(Symbol::intern("docs"))),
                Some(Value::list([
                    Value::float(0.0).unwrap(),
                    Value::float(1.0).unwrap(),
                ])),
                Some(Value::int(2).unwrap()),
                None,
                None,
                None,
            ],
        ];
        let rows = super::ExactEmbeddingSearchRelation
            .scan_batch(&reader, metadata, &keys)
            .unwrap();
        assert_eq!(
            reader.reads.get(),
            7,
            "one membership scan plus vector and subject reads for three embeddings"
        );
        let expected = [(0, 1, 1.0), (2, 1, 1.0), (2, 2, 0.0)];
        assert_eq!(rows.len(), expected.len());
        for (row, (input, subject, score)) in rows.iter().zip(expected) {
            assert_eq!(row.input_row, input);
            assert_eq!(row.tuple.values()[3], Value::int(subject).unwrap());
            assert_eq!(row.tuple.values()[4], Value::float(score).unwrap());
        }
        for (input, key) in keys.iter().enumerate() {
            assert_eq!(
                super::ExactEmbeddingSearchRelation
                    .scan(&reader, metadata, key)
                    .unwrap(),
                rows.iter()
                    .filter(|row| row.input_row == input)
                    .map(|row| row.tuple.clone())
                    .collect::<Vec<_>>()
            );
        }
        assert_eq!(
            reader.reads.get(),
            7,
            "later scalar calls reuse prepared candidates"
        );
    }

    #[test]
    fn prepared_search_observes_membership_vector_and_subject_writes_and_rollback() {
        let mut runner = search_fixture();
        let error = runner
            .run_source(
                "require NearestEmbedding(:docs, [1.0, 0.0], 1, 1, _, _)
            retract VectorIndexContains(:docs, :a)
            require NearestEmbedding(:docs, [1.0, 0.0], 1, 2, _, _)
            retract EmbeddingVector(:b, [1.0, 0.0])
            assert EmbeddingVector(:b, [0.0, 1.0])
            require NearestEmbedding(:docs, [1.0, 0.0], 1, 1, _, _)
            retract EmbeddingOf(:c, 1)
            assert EmbeddingOf(:c, 3)
            require NearestEmbedding(:docs, [1.0, 0.0], 1, 2, _, _)
            raise E_ROLLBACK, \"discard this transaction\"",
            )
            .unwrap();
        assert!(
            matches!(error.outcome, TaskOutcome::Aborted { error, .. } if error.error_code_symbol() == Some(Symbol::intern("E_ROLLBACK")))
        );
        let report = runner
            .run_source("return NearestEmbedding(:docs, [1.0, 0.0], 1, 1, _, _)")
            .unwrap();
        assert!(
            matches!(report.outcome, TaskOutcome::Complete { value, .. } if value == Value::bool(true))
        );
    }

    #[test]
    fn search_cache_does_not_bypass_relation_authority() {
        let mut runner = search_fixture();
        let source = "return NearestEmbedding(:docs, [1.0, 0.0], 1, ?subject, _, _)";
        runner.run_source(source).unwrap();
        let result = runner.submit_source(TaskRequest {
            authority: AuthorityContext::empty(),
            ..SourceRunner::root_source_request(source)
        });
        assert!(format!("{result:?}").contains("PermissionDenied"));
    }
    #[test]
    fn embedding_rules_bind_queries_and_observe_local_changes() {
        let mut runner = search_fixture();
        runner.run_filein("make_relation(:SearchQuery, 2)
            make_relation(:SearchHit, 3)
            SearchHit(query, subject, score) :- SearchQuery(query, ?vector), NearestEmbedding(:docs, ?vector, 1, subject, score, ?version)
            assert SearchQuery(1, [1.0, 0.0])
            assert SearchQuery(2, [0.0, 1.0])").unwrap();
        let result = runner
            .run_source(
                "require SearchHit(1, 1, 1.0)
            require SearchHit(2, 1, 1.0)
            retract VectorIndexContains(:docs, :a)
            require SearchHit(1, 2, 1.0)
            return SearchHit(2, 1, 1.0)",
            )
            .unwrap();
        assert!(
            matches!(result.outcome, TaskOutcome::Complete { value, .. } if value == Value::bool(true)),
            "expected local writes to update computed rule results"
        );
    }
    #[test]
    fn resumed_search_uses_fresh_preparation() {
        let mut runner = search_fixture();
        let report = runner
            .run_source(
                "require NearestEmbedding(:docs, [1.0, 0.0], 1, 1, _, _)
suspend()
return NearestEmbedding(:docs, [1.0, 0.0], 1, 2, _, _)",
            )
            .unwrap();
        assert!(matches!(report.outcome, TaskOutcome::Suspended { .. }));
        runner
            .run_source("retract VectorIndexContains(:docs, :a)")
            .unwrap();
        let outcome = runner
            .resume_task(TaskRequest {
                input: TaskInput::Continuation {
                    task_id: report.task_id,
                    value: Value::unit(),
                },
                ..SourceRunner::root_source_request("")
            })
            .unwrap();
        assert!(
            matches!(outcome, TaskOutcome::Complete { value, .. } if value == Value::bool(true))
        );
    }

    #[test]
    fn prepared_search_reports_invalid_vectors_and_recovers_after_writes() {
        let mut runner = search_fixture();
        let result = runner
            .run_source(
                "require NearestEmbedding(:docs, [1.0, 0.0], 1, 1, _, _)
            retract EmbeddingVector(:b, [1.0, 0.0])
            assert EmbeddingVector(:b, [0.0, 0.0])
            let invalid = try
              NearestEmbedding(:docs, [1.0, 0.0], 1, ?subject, _, _)
              false
            catch E_DB
              true
            end
            require invalid
            retract EmbeddingVector(:b, [0.0, 0.0])
            assert EmbeddingVector(:b, [1.0, 0.0])
            require NearestEmbedding(:docs, [1.0, 0.0], 1, 1, _, _)
            return try
              NearestEmbedding(:docs, [1.0], 1, ?subject, _, _)
              false
            catch E_DB
              true
            end",
            )
            .unwrap();
        assert!(
            matches!(result.outcome, TaskOutcome::Complete { value, .. } if value == Value::bool(true))
        );
    }
}
