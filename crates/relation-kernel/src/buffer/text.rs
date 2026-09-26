// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::ops::Range;
use std::sync::Arc;

const CHUNK_BYTES: usize = 4096;
const SCALAR_SAMPLE: usize = 32;

type Root = Option<Arc<Node>>;

/// Immutable UTF-8 text with scalar coordinates and structurally shared edits.
#[derive(Clone, Debug, Default)]
pub struct Text {
    root: Root,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextError {
    InvalidRange,
    InvalidLine,
}

#[derive(Debug)]
struct Chunk {
    text: Box<str>,
    samples: Box<[u16]>,
    newlines: Box<[u16]>,
    scalars: usize,
}

#[derive(Clone, Debug)]
pub(super) struct Piece {
    chunk: Arc<Chunk>,
    range: Range<usize>,
}

#[derive(Debug)]
struct Node {
    scalars: usize,
    bytes: usize,
    newlines: usize,
    height: u8,
    kind: NodeKind,
}

#[derive(Debug)]
enum NodeKind {
    Leaf(Piece),
    Branch(Arc<Node>, Arc<Node>),
}

impl Chunk {
    fn new(text: &str) -> Self {
        let mut samples = Vec::new();
        let mut newlines = Vec::new();
        let mut scalars = 0;
        for (index, (offset, scalar)) in text.char_indices().enumerate() {
            if index % SCALAR_SAMPLE == 0 {
                samples.push(offset as u16);
            }
            if scalar == '\n' {
                newlines.push(index as u16);
            }
            scalars += 1;
        }
        Self {
            text: text.into(),
            samples: samples.into(),
            newlines: newlines.into(),
            scalars,
        }
    }

    fn byte_offset(&self, scalar: usize) -> usize {
        if scalar == self.scalars {
            return self.text.len();
        }
        let start = self.samples[scalar / SCALAR_SAMPLE] as usize;
        start
            + self.text[start..]
                .char_indices()
                .nth(scalar % SCALAR_SAMPLE)
                .unwrap()
                .0
    }
}

impl Piece {
    pub(super) fn len(&self) -> usize {
        self.range.len()
    }

    pub(super) fn text(&self) -> &str {
        &self.chunk.text
            [self.chunk.byte_offset(self.range.start)..self.chunk.byte_offset(self.range.end)]
    }

    pub(super) fn origin(&self) -> (usize, Range<usize>) {
        (Arc::as_ptr(&self.chunk) as usize, self.range.clone())
    }

    fn split(&self, at: usize) -> (Self, Self) {
        let middle = self.range.start + at;
        (
            Self {
                chunk: self.chunk.clone(),
                range: self.range.start..middle,
            },
            Self {
                chunk: self.chunk.clone(),
                range: middle..self.range.end,
            },
        )
    }

    fn newline_range(&self) -> Range<usize> {
        self.chunk
            .newlines
            .partition_point(|&offset| (offset as usize) < self.range.start)
            ..self
                .chunk
                .newlines
                .partition_point(|&offset| (offset as usize) < self.range.end)
    }
}

impl Node {
    fn leaf(piece: Piece) -> Arc<Self> {
        Arc::new(Self {
            scalars: piece.len(),
            bytes: piece.text().len(),
            newlines: piece.newline_range().len(),
            height: 1,
            kind: NodeKind::Leaf(piece),
        })
    }

    fn branch(left: Arc<Self>, right: Arc<Self>) -> Arc<Self> {
        Arc::new(Self {
            scalars: left.scalars + right.scalars,
            bytes: left.bytes + right.bytes,
            newlines: left.newlines + right.newlines,
            height: left.height.max(right.height) + 1,
            kind: NodeKind::Branch(left, right),
        })
    }

    fn children(&self) -> (&Arc<Self>, &Arc<Self>) {
        let NodeKind::Branch(left, right) = &self.kind else {
            unreachable!("height requires a branch");
        };
        (left, right)
    }
}

impl Text {
    pub fn from_text(text: &str) -> Self {
        let mut root = None;
        let mut start = 0;
        while start < text.len() {
            let mut end = (start + CHUNK_BYTES).min(text.len());
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            let chunk = Arc::new(Chunk::new(&text[start..end]));
            let range = 0..chunk.scalars;
            root = join(root, Some(Node::leaf(Piece { chunk, range })));
            start = end;
        }
        Self { root }
    }

    pub fn len(&self) -> usize {
        self.root.as_ref().map_or(0, |root| root.scalars)
    }
    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }
    pub fn byte_len(&self) -> usize {
        self.root.as_ref().map_or(0, |root| root.bytes)
    }
    pub fn line_count(&self) -> usize {
        self.root.as_ref().map_or(1, |root| root.newlines + 1)
    }

    pub fn slice(&self, range: Range<usize>) -> Result<String, TextError> {
        self.validate_range(&range)?;
        let mut out = String::new();
        if let Some(root) = &self.root {
            append_range(root, range, &mut out);
        }
        Ok(out)
    }

    pub fn to_text(&self) -> String {
        let mut out = String::with_capacity(self.byte_len());
        if let Some(root) = &self.root {
            append_range(root, 0..root.scalars, &mut out);
        }
        out
    }

    pub fn replace(&self, range: Range<usize>, text: &str) -> Result<Self, TextError> {
        self.validate_range(&range)?;
        if range.is_empty() && text.is_empty() {
            return Ok(self.clone());
        }
        let (left, tail) = split(self.root.clone(), range.start);
        let (_, right) = split(tail, range.len());
        Ok(Self {
            root: join(join(left, Self::from_text(text).root), right),
        })
    }

    pub fn line_start(&self, line: usize) -> Result<usize, TextError> {
        if line >= self.line_count() {
            return Err(TextError::InvalidLine);
        }
        if line == 0 {
            return Ok(0);
        }
        Ok(newline_position(self.root.as_ref().unwrap(), line - 1) + 1)
    }

    pub fn line_column(&self, position: usize) -> Result<(usize, usize), TextError> {
        if position > self.len() {
            return Err(TextError::InvalidRange);
        }
        let line = self
            .root
            .as_ref()
            .map_or(0, |root| newlines_before(root, position));
        Ok((line, position - self.line_start(line)?))
    }

    /// Finds a nonempty pattern within a scalar-bounded window. Zero limit means the remaining text.
    pub fn find(&self, pattern: &str, from: usize, limit: usize) -> Option<usize> {
        if pattern.is_empty() || from >= self.len() {
            return None;
        }
        let pattern = pattern.chars().collect::<Vec<_>>();
        let mut prefixes = vec![0; pattern.len()];
        let mut matched = 0;
        for index in 1..pattern.len() {
            while matched > 0 && pattern[index] != pattern[matched] {
                matched = prefixes[matched - 1];
            }
            if pattern[index] == pattern[matched] {
                matched += 1;
            }
            prefixes[index] = matched;
        }
        matched = 0;
        let end = if limit == 0 {
            self.len()
        } else {
            from.saturating_add(limit).min(self.len())
        };
        let mut position = from;
        let mut found = None;
        visit_range(self.root.as_ref().unwrap(), from..end, &mut |text| {
            for scalar in text.chars() {
                while matched > 0 && scalar != pattern[matched] {
                    matched = prefixes[matched - 1];
                }
                if scalar == pattern[matched] {
                    matched += 1;
                }
                position += 1;
                if matched == pattern.len() {
                    found = Some(position - matched);
                    return false;
                }
            }
            true
        });
        found
    }

    /// The line's scalar range, excluding its terminating newline.
    pub fn line_span(&self, line: usize) -> Result<Range<usize>, TextError> {
        let start = self.line_start(line)?;
        let stop = if line + 1 < self.line_count() {
            self.line_start(line + 1)? - 1
        } else {
            self.len()
        };
        Ok(start..stop)
    }

    fn validate_range(&self, range: &Range<usize>) -> Result<(), TextError> {
        if range.start > range.end || range.end > self.len() {
            return Err(TextError::InvalidRange);
        }
        Ok(())
    }

    pub(super) fn shares_root(&self, other: &Self) -> bool {
        match (&self.root, &other.root) {
            (None, None) => true,
            (Some(left), Some(right)) => Arc::ptr_eq(left, right),
            _ => false,
        }
    }

    pub(super) fn pieces(&self, limit: usize) -> Option<Vec<Piece>> {
        let mut pieces = Vec::new();
        if let Some(root) = &self.root {
            collect_pieces(root, &mut pieces, limit)?;
        }
        Some(pieces)
    }
}

fn balance(left: Arc<Node>, right: Arc<Node>) -> Arc<Node> {
    if left.height > right.height + 1 {
        let (a, b) = left.children();
        if a.height >= b.height {
            return Node::branch(a.clone(), Node::branch(b.clone(), right));
        }
        let (b1, b2) = b.children();
        return Node::branch(
            Node::branch(a.clone(), b1.clone()),
            Node::branch(b2.clone(), right),
        );
    }
    if right.height > left.height + 1 {
        let (b, c) = right.children();
        if c.height >= b.height {
            return Node::branch(Node::branch(left, b.clone()), c.clone());
        }
        let (b1, b2) = b.children();
        return Node::branch(
            Node::branch(left, b1.clone()),
            Node::branch(b2.clone(), c.clone()),
        );
    }
    Node::branch(left, right)
}

fn join(left: Root, right: Root) -> Root {
    let (left, right) = match (left, right) {
        (None, right) => return right,
        (left, None) => return left,
        (Some(left), Some(right)) => (left, right),
    };
    if left.height > right.height + 1 {
        let (a, b) = left.children();
        return Some(balance(
            a.clone(),
            join(Some(b.clone()), Some(right)).unwrap(),
        ));
    }
    if right.height > left.height + 1 {
        let (b, c) = right.children();
        return Some(balance(
            join(Some(left), Some(b.clone())).unwrap(),
            c.clone(),
        ));
    }
    Some(Node::branch(left, right))
}

fn split(root: Root, at: usize) -> (Root, Root) {
    let Some(root) = root else {
        return (None, None);
    };
    if at == 0 {
        return (None, Some(root));
    }
    if at == root.scalars {
        return (Some(root), None);
    }
    match &root.kind {
        NodeKind::Leaf(piece) => {
            let (left, right) = piece.split(at);
            (Some(Node::leaf(left)), Some(Node::leaf(right)))
        }
        NodeKind::Branch(left, right) if at < left.scalars => {
            let (a, b) = split(Some(left.clone()), at);
            (a, join(b, Some(right.clone())))
        }
        NodeKind::Branch(left, right) => {
            let (a, b) = split(Some(right.clone()), at - left.scalars);
            (join(Some(left.clone()), a), b)
        }
    }
}

fn append_range(node: &Node, range: Range<usize>, out: &mut String) {
    visit_range(node, range, &mut |text| {
        out.push_str(text);
        true
    });
}

fn visit_range(node: &Node, range: Range<usize>, visit: &mut impl FnMut(&str) -> bool) -> bool {
    if range.is_empty() {
        return true;
    }
    match &node.kind {
        NodeKind::Leaf(piece) => {
            let start = piece.chunk.byte_offset(piece.range.start + range.start);
            let end = piece.chunk.byte_offset(piece.range.start + range.end);
            visit(&piece.chunk.text[start..end])
        }
        NodeKind::Branch(left, right) => {
            if range.start < left.scalars
                && !visit_range(left, range.start..range.end.min(left.scalars), visit)
            {
                return false;
            }
            if range.end > left.scalars {
                return visit_range(
                    right,
                    range.start.saturating_sub(left.scalars)..range.end - left.scalars,
                    visit,
                );
            }
            true
        }
    }
}

fn newline_position(node: &Node, index: usize) -> usize {
    match &node.kind {
        NodeKind::Leaf(piece) => {
            piece.chunk.newlines[piece.newline_range().start + index] as usize - piece.range.start
        }
        NodeKind::Branch(left, _) if index < left.newlines => newline_position(left, index),
        NodeKind::Branch(left, right) => {
            left.scalars + newline_position(right, index - left.newlines)
        }
    }
}

fn newlines_before(node: &Node, position: usize) -> usize {
    match &node.kind {
        NodeKind::Leaf(piece) => {
            let end = piece
                .chunk
                .newlines
                .partition_point(|&offset| (offset as usize) < piece.range.start + position);
            end - piece.newline_range().start
        }
        NodeKind::Branch(left, _) if position <= left.scalars => newlines_before(left, position),
        NodeKind::Branch(left, right) => {
            left.newlines + newlines_before(right, position - left.scalars)
        }
    }
}

fn collect_pieces(node: &Node, pieces: &mut Vec<Piece>, limit: usize) -> Option<()> {
    match &node.kind {
        NodeKind::Leaf(piece) => {
            if pieces.len() == limit {
                return None;
            }
            pieces.push(piece.clone());
        }
        NodeKind::Branch(left, right) => {
            collect_pieces(left, pieces, limit)?;
            collect_pieces(right, pieces, limit)?;
        }
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(node: &Node) {
        match &node.kind {
            NodeKind::Leaf(piece) => {
                assert_eq!(node.height, 1);
                assert_eq!(node.scalars, piece.text().chars().count());
                assert_eq!(node.bytes, piece.text().len());
                assert_eq!(
                    node.newlines,
                    piece
                        .text()
                        .chars()
                        .filter(|&scalar| scalar == '\n')
                        .count()
                );
            }
            NodeKind::Branch(left, right) => {
                check(left);
                check(right);
                assert!(left.height.abs_diff(right.height) <= 1);
                assert_eq!(node.height, left.height.max(right.height) + 1);
                assert_eq!(node.scalars, left.scalars + right.scalars);
                assert_eq!(node.bytes, left.bytes + right.bytes);
                assert_eq!(node.newlines, left.newlines + right.newlines);
            }
        }
    }

    fn random(seed: &mut u64) -> usize {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *seed as usize
    }

    #[test]
    fn persistent_splices_match_unicode_reference_and_keep_tree_balanced() {
        let original = "aé🦀\n".repeat(3000);
        let base = Text::from_text(&original);
        let mut view = base.clone();
        let mut reference = original.chars().collect::<Vec<_>>();
        let mut seed = 47;
        for step in 0..1200 {
            let at = random(&mut seed) % (reference.len() + 1);
            let remove = (random(&mut seed) % 31).min(reference.len() - at);
            let inserted = ["", "x", "🦀é", "\nnew\n"][random(&mut seed) % 4];
            view = view.replace(at..at + remove, inserted).unwrap();
            reference.splice(at..at + remove, inserted.chars());
            assert_eq!(
                view.to_text(),
                reference.iter().collect::<String>(),
                "step {step}"
            );
            assert_eq!(view.len(), reference.len());
            assert_eq!(
                view.line_count(),
                reference.iter().filter(|&&scalar| scalar == '\n').count() + 1
            );
            if let Some(root) = &view.root {
                check(root);
            }
            let position = random(&mut seed) % (reference.len() + 1);
            let line = reference[..position]
                .iter()
                .filter(|&&scalar| scalar == '\n')
                .count();
            let start = reference[..position]
                .iter()
                .rposition(|&scalar| scalar == '\n')
                .map_or(0, |index| index + 1);
            assert_eq!(view.line_column(position), Ok((line, position - start)));
            assert_eq!(view.line_start(line), Ok(start));
            let end = (position + 19).min(reference.len());
            assert_eq!(
                view.slice(position..end),
                Ok(reference[position..end].iter().collect())
            );
        }
        assert_eq!(base.to_text(), original);
    }

    #[test]
    fn boundary_splices_share_unchanged_chunks() {
        let text = format!("{}🦀{}", "x".repeat(CHUNK_BYTES - 1), "é\n".repeat(5000));
        let base = Text::from_text(&text);
        let edited = base.replace(CHUNK_BYTES - 1..CHUNK_BYTES, "z").unwrap();
        let original_pieces = base.pieces(100).unwrap();
        let edited_pieces = edited.pieces(100).unwrap();
        let shared = edited_pieces
            .iter()
            .filter(|piece| {
                original_pieces
                    .iter()
                    .any(|base| Arc::ptr_eq(&base.chunk, &piece.chunk))
            })
            .count();
        assert!(shared >= original_pieces.len() - 1);
        assert_eq!(base.to_text(), text);
        assert_eq!(
            edited.slice(CHUNK_BYTES - 2..CHUNK_BYTES + 2),
            Ok("xzé\n".to_owned())
        );
        check(edited.root.as_ref().unwrap());
        let empty = edited.replace(0..edited.len(), "").unwrap();
        assert!(empty.is_empty());
        assert_eq!(empty.line_count(), 1);
        assert_eq!(empty.line_column(0), Ok((0, 0)));
        assert_eq!(empty.line_start(1), Err(TextError::InvalidLine));
        assert_eq!(empty.slice(0..1), Err(TextError::InvalidRange));
        assert!(matches!(
            empty.replace(1..1, "a"),
            Err(TextError::InvalidRange)
        ));
    }

    #[test]
    fn long_append_and_prefix_delete_sequences_remain_balanced() {
        let mut text = Text::default();
        for _ in 0..4096 {
            text = text.replace(text.len()..text.len(), "é").unwrap();
        }
        check(text.root.as_ref().unwrap());
        let checkpoint = text.clone();
        for _ in 0..4096 {
            text = text.replace(0..1, "").unwrap();
        }
        assert!(text.is_empty());
        assert_eq!(checkpoint.to_text(), "é".repeat(4096));
    }
    #[test]
    fn bounded_search_crosses_chunks_and_handles_prefix_overlaps() {
        let base = Text::from_text(&format!(
            "{}🦀é
last
",
            "x".repeat(CHUNK_BYTES - 1)
        ));
        assert_eq!(base.find("x🦀é", CHUNK_BYTES - 2, 3), Some(CHUNK_BYTES - 2));
        assert_eq!(base.find("x🦀é", CHUNK_BYTES - 2, 2), None);
        assert_eq!(base.find("🦀é", 0, 0), Some(CHUNK_BYTES - 1));
        assert_eq!(base.find("", 0, 0), None);
        assert_eq!(base.find("last", base.len(), 0), None);
        assert_eq!(base.line_span(1), Ok(CHUNK_BYTES + 2..CHUNK_BYTES + 6));
        assert_eq!(base.line_span(2), Ok(base.len()..base.len()));
        let fragmented = Text::from_text("aaaaac").replace(3..3, "abaaa").unwrap();
        assert_eq!(fragmented.find("aaaac", 0, 0), Some(6));
    }
}
