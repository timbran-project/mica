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
    ComputedRelation, ComputedRelationRead, KernelError, RelationId, RelationMetadata,
    RelationRead, Tuple, system_computed_relations,
};
use mica_var::{Symbol, Value};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::Arc;

const REQUIRED_BOUND_POSITIONS: &[u16] = &[0, 1, 2];

pub(crate) fn default_computed_relations() -> Vec<Arc<dyn ComputedRelation>> {
    let mut relations = system_computed_relations();
    relations.push(Arc::new(ExactEmbeddingSearchRelation));
    #[cfg(feature = "source-provider")]
    relations.extend(mica_source_provider::default_computed_relations());
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
        let index = bindings[0]
            .clone()
            .ok_or_else(|| invalid_relation(metadata.id(), "expected bound index in position 0"))?;
        let query = parse_vector(
            metadata.id(),
            &bindings[1].clone().ok_or_else(|| {
                invalid_relation(
                    metadata.id(),
                    "expected bound query embedding in position 1",
                )
            })?,
        )?;
        let limit = parse_limit(
            metadata.id(),
            &bindings[2].clone().ok_or_else(|| {
                invalid_relation(metadata.id(), "expected bound limit in position 2")
            })?,
        )?;

        let vector_index_contains =
            relation_id(reader, "VectorIndexContains", 2).ok_or_else(|| {
                invalid_relation(metadata.id(), "missing relation VectorIndexContains/2")
            })?;
        let embedding_of = relation_id(reader, "EmbeddingOf", 2)
            .ok_or_else(|| invalid_relation(metadata.id(), "missing relation EmbeddingOf/2"))?;
        let embedding_vector = relation_id(reader, "EmbeddingVector", 2)
            .ok_or_else(|| invalid_relation(metadata.id(), "missing relation EmbeddingVector/2"))?;

        let mut best_by_subject = BTreeMap::<Value, f64>::new();
        for membership in
            reader.scan_relation(vector_index_contains, &[Some(index.clone()), None])?
        {
            let Some(embedding) = membership.values().get(1).cloned() else {
                continue;
            };
            let vector_row = expect_single_value(
                reader,
                embedding_vector,
                &[Some(embedding.clone()), None],
                metadata.id(),
                "expected EmbeddingVector(embedding, payload)",
            )?;
            let subject = expect_single_value(
                reader,
                embedding_of,
                &[Some(embedding.clone()), None],
                metadata.id(),
                "expected EmbeddingOf(embedding, subject)",
            )?;
            let candidate = parse_vector(metadata.id(), &vector_row)?;
            let score = cosine_similarity(metadata.id(), &query, &candidate)?;
            best_by_subject
                .entry(subject)
                .and_modify(|current| {
                    if score > *current {
                        *current = score;
                    }
                })
                .or_insert(score);
        }

        let snapshot_version = Value::int(reader.version() as i64)
            .map_err(|_| invalid_relation(metadata.id(), "snapshot version exceeds i64"))?;
        let mut rows = best_by_subject
            .into_iter()
            .map(|(subject, score)| {
                Ok((
                    subject.clone(),
                    score,
                    Tuple::from([
                        index.clone(),
                        bindings[1]
                            .clone()
                            .expect("validated query embedding should exist"),
                        bindings[2].clone().expect("validated limit should exist"),
                        subject,
                        host_score_to_value(metadata.id(), score)?,
                        snapshot_version.clone(),
                    ]),
                ))
            })
            .collect::<Result<Vec<_>, KernelError>>()?;
        rows.sort_by(|left, right| {
            right
                .1
                .partial_cmp(&left.1)
                .unwrap_or(Ordering::Equal)
                .then_with(|| left.0.cmp(&right.0))
        });
        Ok(rows
            .into_iter()
            .take(limit)
            .map(|(_, _, tuple)| tuple)
            .collect())
    }
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
    right: &[f64],
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
    let mut left_norm = 0.0;
    let mut right_norm = 0.0;
    for (left_value, right_value) in left.iter().zip(right.iter()) {
        dot += left_value * right_value;
        left_norm += left_value * left_value;
        right_norm += right_value * right_value;
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
    use crate::{SourceRunner, TaskOutcome};
    use mica_var::{Symbol, Value};
    use std::sync::Arc;

    struct ConstantEmbeddingProvider;

    impl crate::embedding::EmbeddingProvider for ConstantEmbeddingProvider {
        fn embed_text(&self, model: &str, text: &str) -> Result<Vec<f64>, String> {
            Ok(vec![model.len() as f64, text.len() as f64])
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
            .run_source(
                "let rows = NearestEmbedding(#main_index, [1.0, 0.0], 2, ?subject, ?score, ?snapshot_version)\n\
                 return [rows[0][:subject], rows[1][:subject]]",
            )
            .unwrap();

        let TaskOutcome::Complete { value, .. } = report.outcome else {
            panic!("expected complete outcome, got {:?}", report.outcome);
        };
        value
            .with_list(|values| {
                assert_eq!(values.len(), 2);
                assert_eq!(
                    values[0],
                    Value::identity(runner.named_identity(Symbol::intern("doc_one")).unwrap())
                );
                assert_eq!(
                    values[1],
                    Value::identity(runner.named_identity(Symbol::intern("doc_two")).unwrap())
                );
            })
            .expect("expected list result");
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
}
