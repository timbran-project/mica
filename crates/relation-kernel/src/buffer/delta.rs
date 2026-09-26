// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use super::Text;
use std::collections::HashMap;
use std::ops::Range;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Replacement {
    pub range: Range<usize>,
    pub text: String,
}

/// Sorted, disjoint replacements in the coordinate system of one base revision.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Delta {
    replacements: Vec<Replacement>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeltaError {
    InvalidRange,
    OutOfOrder,
    ForeignProvenance,
    BudgetExceeded,
    Conflict,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InsertionAffinity {
    Before,
    After,
}

/// Remaining reconciliation work. Share one budget across a transaction's buffers.
#[derive(Clone, Debug)]
pub struct DeltaBudget {
    pub pieces: usize,
    pub text_bytes: usize,
    pub replacements: usize,
}

impl Default for DeltaBudget {
    fn default() -> Self {
        Self {
            pieces: 4096,
            text_bytes: 1 << 20,
            replacements: 1024,
        }
    }
}

impl DeltaBudget {
    fn pieces(&mut self, count: usize) -> Result<(), DeltaError> {
        self.pieces = self
            .pieces
            .checked_sub(count)
            .ok_or(DeltaError::BudgetExceeded)?;
        Ok(())
    }

    fn text(&mut self, count: usize) -> Result<(), DeltaError> {
        self.text_bytes = self
            .text_bytes
            .checked_sub(count)
            .ok_or(DeltaError::BudgetExceeded)?;
        Ok(())
    }
}

struct BaseSpan {
    range: Range<usize>,
    offset: usize,
}

impl Delta {
    pub(crate) fn retained_bytes(&self) -> usize {
        self.replacements
            .capacity()
            .saturating_mul(std::mem::size_of::<Replacement>())
            .saturating_add(
                self.replacements
                    .iter()
                    .map(|replacement| replacement.text.capacity())
                    .sum::<usize>(),
            )
    }
    pub fn new(replacements: Vec<Replacement>) -> Result<Self, DeltaError> {
        let mut normalized: Vec<Replacement> = Vec::new();
        for replacement in replacements {
            if replacement.range.start > replacement.range.end {
                return Err(DeltaError::InvalidRange);
            }
            if replacement.range.is_empty() && replacement.text.is_empty() {
                continue;
            }
            if let Some(previous) = normalized.last_mut() {
                if previous.range.end > replacement.range.start {
                    return Err(DeltaError::OutOfOrder);
                }
                if previous.range.end == replacement.range.start {
                    previous.range.end = replacement.range.end;
                    previous.text.push_str(&replacement.text);
                    continue;
                }
            }
            normalized.push(replacement);
        }
        Ok(Self {
            replacements: normalized,
        })
    }

    pub fn replacements(&self) -> &[Replacement] {
        &self.replacements
    }
    pub fn is_empty(&self) -> bool {
        self.replacements.is_empty()
    }

    /// Derives edits from retained chunk ranges. Equal text does not imply shared provenance.
    pub fn between(base: &Text, view: &Text, budget: &mut DeltaBudget) -> Result<Self, DeltaError> {
        if base.shares_root(view) {
            return Ok(Self::default());
        }
        let base_pieces = base
            .pieces(budget.pieces)
            .ok_or(DeltaError::BudgetExceeded)?;
        budget.pieces(base_pieces.len())?;
        let view_pieces = view
            .pieces(budget.pieces)
            .ok_or(DeltaError::BudgetExceeded)?;
        budget.pieces(view_pieces.len())?;
        let mut origins = HashMap::<usize, Vec<BaseSpan>>::new();
        let mut offset = 0;
        for piece in &base_pieces {
            let (origin, range) = piece.origin();
            origins
                .entry(origin)
                .or_default()
                .push(BaseSpan { range, offset });
            offset += piece.len();
        }
        let mut replacements = Vec::new();
        let mut cursor = 0;
        let mut inserted = String::new();
        for piece in &view_pieces {
            let (origin, range) = piece.origin();
            let Some(spans) = origins.get(&origin) else {
                budget.text(piece.text().len())?;
                inserted.push_str(piece.text());
                continue;
            };
            let mut start = range.start;
            let first = spans.partition_point(|span| span.range.end <= start);
            for span in &spans[first..] {
                if start == range.end {
                    break;
                }
                budget.pieces(1)?;
                if start < span.range.start {
                    return Err(DeltaError::ForeignProvenance);
                }
                let end = range.end.min(span.range.end);
                let retained = span.offset + start - span.range.start;
                if retained < cursor {
                    return Err(DeltaError::ForeignProvenance);
                }
                flush(&mut replacements, &mut inserted, cursor..retained, budget)?;
                cursor = retained + end - start;
                start = end;
            }
            if start != range.end {
                return Err(DeltaError::ForeignProvenance);
            }
        }
        flush(&mut replacements, &mut inserted, cursor..base.len(), budget)?;
        Ok(Self { replacements })
    }

    pub fn apply(&self, base: &Text) -> Result<Text, DeltaError> {
        let mut view = base.clone();
        for replacement in self.replacements.iter().rev() {
            view = view
                .replace(replacement.range.clone(), &replacement.text)
                .map_err(|_| DeltaError::InvalidRange)?;
        }
        Ok(view)
    }

    pub fn conflicts(&self, other: &Self) -> bool {
        let mut first = 0;
        for left in &self.replacements {
            while first < other.replacements.len()
                && other.replacements[first].range.end < left.range.start
            {
                first += 1;
            }
            for right in &other.replacements[first..] {
                if right.range.start > left.range.end {
                    break;
                }
                if replacements_conflict(left, right) {
                    return true;
                }
            }
        }
        false
    }

    pub fn transform_after(&self, concurrent: &Self) -> Result<Self, DeltaError> {
        if self.conflicts(concurrent) {
            return Err(DeltaError::Conflict);
        }
        let replacements = self
            .replacements
            .iter()
            .map(|replacement| {
                Ok(Replacement {
                    range: shift(concurrent, replacement.range.start, true)?
                        ..shift(concurrent, replacement.range.end, false)?,
                    text: replacement.text.clone(),
                })
            })
            .collect::<Result<Vec<_>, DeltaError>>()?;
        Self::new(replacements)
    }

    pub fn rebase_marker(
        &self,
        position: usize,
        affinity: InsertionAffinity,
    ) -> Result<usize, DeltaError> {
        let mut removed = 0usize;
        let mut inserted = 0usize;
        for replacement in &self.replacements {
            if replacement.range.is_empty() {
                if replacement.range.start < position
                    || (replacement.range.start == position && affinity == InsertionAffinity::After)
                {
                    inserted = inserted
                        .checked_add(replacement.text.chars().count())
                        .ok_or(DeltaError::InvalidRange)?;
                }
                continue;
            }
            if position <= replacement.range.start {
                break;
            }
            if position < replacement.range.end {
                return (replacement.range.start - removed)
                    .checked_add(inserted)
                    .ok_or(DeltaError::InvalidRange);
            }
            removed += replacement.range.len();
            inserted = inserted
                .checked_add(replacement.text.chars().count())
                .ok_or(DeltaError::InvalidRange)?;
        }
        (position - removed)
            .checked_add(inserted)
            .ok_or(DeltaError::InvalidRange)
    }
}

fn flush(
    out: &mut Vec<Replacement>,
    inserted: &mut String,
    range: Range<usize>,
    budget: &mut DeltaBudget,
) -> Result<(), DeltaError> {
    if range.is_empty() && inserted.is_empty() {
        return Ok(());
    }
    budget.replacements = budget
        .replacements
        .checked_sub(1)
        .ok_or(DeltaError::BudgetExceeded)?;
    out.push(Replacement {
        range,
        text: std::mem::take(inserted),
    });
    Ok(())
}

fn replacements_conflict(left: &Replacement, right: &Replacement) -> bool {
    match (left.range.is_empty(), right.range.is_empty()) {
        (true, true) => left.range.start == right.range.start,
        (true, false) => right.range.start < left.range.start && left.range.start < right.range.end,
        (false, true) => left.range.start < right.range.start && right.range.start < left.range.end,
        (false, false) => left.range.start < right.range.end && right.range.start < left.range.end,
    }
}

fn shift(delta: &Delta, position: usize, start: bool) -> Result<usize, DeltaError> {
    let mut removed = 0;
    let mut inserted = 0usize;
    for replacement in &delta.replacements {
        if replacement.range.end < position
            || (replacement.range.end == position && (start || !replacement.range.is_empty()))
        {
            removed += replacement.range.len();
            inserted = inserted
                .checked_add(replacement.text.chars().count())
                .ok_or(DeltaError::InvalidRange)?;
        }
    }
    position
        .checked_sub(removed)
        .and_then(|position| position.checked_add(inserted))
        .ok_or(DeltaError::InvalidRange)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn replacement(start: usize, end: usize, text: &str) -> Replacement {
        Replacement {
            range: start..end,
            text: text.to_owned(),
        }
    }

    fn delta(base: &Text, view: &Text) -> Delta {
        Delta::between(base, view, &mut DeltaBudget::default()).unwrap()
    }

    #[test]
    fn provenance_distinguishes_identical_text_at_different_positions() {
        let base = Text::from_text("aaa");
        let left = base.replace(0..1, "").unwrap();
        let right = base.replace(2..3, "").unwrap();
        assert_eq!(left.to_text(), right.to_text());
        assert_eq!(delta(&base, &left).replacements(), &[replacement(0, 1, "")]);
        assert_eq!(
            delta(&base, &right).replacements(),
            &[replacement(2, 3, "")]
        );
        assert!(!delta(&base, &left).conflicts(&delta(&base, &right)));
        let both = delta(&base, &right)
            .transform_after(&delta(&base, &left))
            .unwrap()
            .apply(&left)
            .unwrap();
        assert_eq!(both.to_text(), "a");
    }

    #[test]
    fn sequential_edits_normalize_against_the_original_base() {
        let base = Text::from_text("héllo→\nworld");
        let view = base
            .replace(2..2, "abc")
            .unwrap()
            .replace(3..5, "🦀")
            .unwrap()
            .replace(9..11, "!")
            .unwrap();
        let changes = delta(&base, &view);
        assert_eq!(changes.apply(&base).unwrap().to_text(), view.to_text());
        assert_eq!(
            changes.replacements(),
            &[replacement(2, 2, "a🦀"), replacement(7, 9, "!")]
        );
        let unchanged = base
            .replace(2..2, "abc")
            .unwrap()
            .replace(2..5, "")
            .unwrap();
        assert!(delta(&base, &unchanged).is_empty());
        let same_text = base.replace(0..1, "h").unwrap();
        assert_eq!(
            delta(&base, &same_text).replacements(),
            &[replacement(0, 1, "h")]
        );
    }

    #[test]
    fn boundary_merges_conflicts_and_sticky_markers_match_the_contract() {
        let base = Text::from_text("abcdef");
        let remove = Delta::new(vec![replacement(2, 4, "X")]).unwrap();
        for (at, conflict) in [(1, false), (2, false), (3, true), (4, false), (5, false)] {
            let insert = Delta::new(vec![replacement(at, at, "é")]).unwrap();
            assert_eq!(insert.conflicts(&remove), conflict);
            assert_eq!(remove.conflicts(&insert), conflict);
            if !conflict {
                let left = insert
                    .transform_after(&remove)
                    .unwrap()
                    .apply(&remove.apply(&base).unwrap())
                    .unwrap();
                let right = remove
                    .transform_after(&insert)
                    .unwrap()
                    .apply(&insert.apply(&base).unwrap())
                    .unwrap();
                assert_eq!(left.to_text(), right.to_text());
            }
        }
        let insert = Delta::new(vec![replacement(2, 2, "🦀é")]).unwrap();
        assert!(insert.conflicts(&insert));
        assert_eq!(insert.rebase_marker(2, InsertionAffinity::Before), Ok(2));
        assert_eq!(insert.rebase_marker(2, InsertionAffinity::After), Ok(4));
        assert_eq!(remove.rebase_marker(3, InsertionAffinity::After), Ok(2));
        assert_eq!(remove.rebase_marker(4, InsertionAffinity::Before), Ok(3));
        let edges = Delta::new(vec![replacement(2, 2, "L"), replacement(4, 4, "R")]).unwrap();
        let deletion = Delta::new(vec![replacement(2, 4, "")]).unwrap();
        let merged = edges.transform_after(&deletion).unwrap();
        assert_eq!(merged.replacements(), &[replacement(2, 2, "LR")]);
    }

    #[test]
    fn normalization_obeys_shared_work_and_output_budgets() {
        let base = Text::from_text("base");
        let view = base.replace(2..2, "new text").unwrap();
        for mut budget in [
            DeltaBudget {
                pieces: 0,
                ..DeltaBudget::default()
            },
            DeltaBudget {
                text_bytes: 3,
                ..DeltaBudget::default()
            },
            DeltaBudget {
                replacements: 0,
                ..DeltaBudget::default()
            },
        ] {
            assert_eq!(
                Delta::between(&base, &view, &mut budget),
                Err(DeltaError::BudgetExceeded)
            );
        }
        let mut budget = DeltaBudget {
            replacements: 1,
            ..DeltaBudget::default()
        };
        assert!(Delta::between(&base, &view, &mut budget).is_ok());
        assert_eq!(
            Delta::between(&base, &view, &mut budget),
            Err(DeltaError::BudgetExceeded)
        );
    }
    fn random(seed: &mut u64) -> usize {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *seed as usize
    }

    #[test]
    fn normalization_matches_a_scalar_provenance_reference_across_fragmented_bases() {
        let mut base = Text::from_text(&"abé\n🦀".repeat(20));
        let mut seed = 371;
        for _ in 0..50 {
            let mut cells = base
                .to_text()
                .chars()
                .enumerate()
                .map(|(index, scalar)| (Some(index), scalar))
                .collect::<Vec<_>>();
            let mut view = base.clone();
            for _ in 0..20 {
                let at = random(&mut seed) % (cells.len() + 1);
                let remove = (random(&mut seed) % 5).min(cells.len() - at);
                let text = ["", "a", "é🦀", "line\n"][random(&mut seed) % 4];
                view = view.replace(at..at + remove, text).unwrap();
                cells.splice(at..at + remove, text.chars().map(|scalar| (None, scalar)));
                let mut expected = Vec::new();
                let mut cursor = 0;
                let mut pending = String::new();
                for &(origin, scalar) in &cells {
                    let Some(origin) = origin else {
                        pending.push(scalar);
                        continue;
                    };
                    if origin != cursor || !pending.is_empty() {
                        expected.push(Replacement {
                            range: cursor..origin,
                            text: std::mem::take(&mut pending),
                        });
                    }
                    cursor = origin + 1;
                }
                if cursor != base.len() || !pending.is_empty() {
                    expected.push(Replacement {
                        range: cursor..base.len(),
                        text: pending,
                    });
                }
                let actual = delta(&base, &view);
                assert_eq!(actual.replacements(), expected);
                assert_eq!(actual.apply(&base).unwrap().to_text(), view.to_text());
            }
            base = view;
        }
    }

    #[test]
    fn disjoint_edit_transforms_commute_for_generated_unicode_edits() {
        let base = Text::from_text(&"aé🦀b".repeat(16));
        let mut seed = 1729;
        for _ in 0..4000 {
            let mut make_delta = || {
                let at = random(&mut seed) % (base.len() + 1);
                let remove = (random(&mut seed) % 7).min(base.len() - at);
                let text = ["", "X", "🦀é"][random(&mut seed) % 3];
                Delta::new(vec![replacement(at, at + remove, text)]).unwrap()
            };
            let left = make_delta();
            let right = make_delta();
            assert_eq!(left.conflicts(&right), right.conflicts(&left));
            if left.conflicts(&right) {
                continue;
            }
            let a = left
                .transform_after(&right)
                .unwrap()
                .apply(&right.apply(&base).unwrap())
                .unwrap();
            let b = right
                .transform_after(&left)
                .unwrap()
                .apply(&left.apply(&base).unwrap())
                .unwrap();
            assert_eq!(a.to_text(), b.to_text());
        }
    }
}
