//! balanced piece tree. leaves hold pieces of shared text, every node keeps
//! byte and newline totals, so offset and line lookups walk one path.
//!
//! nodes sit behind `Arc` and edits copy only the path they touch, so a
//! cloned tree is a cheap snapshot that later edits never change.

use std::ops::Range;
use std::sync::Arc;

/// most pieces in one leaf.
const MAX_PIECES: usize = 32;
/// most children in one inner node.
const MAX_CHILDREN: usize = 16;
/// a node with fewer pieces or children than this gets merged into a neighbour.
const MIN_FILL: usize = 4;
/// longest piece cut from loaded or pasted text.
const PIECE_MAX: usize = 1024;
/// two pieces side by side get copied into one while both fit in this.
const PIECE_JOIN: usize = 128;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Summary {
    pub bytes: usize,
    pub newlines: usize,
}

impl Summary {
    fn add(&mut self, other: Summary) {
        self.bytes += other.bytes;
        self.newlines += other.newlines;
    }
}

/// a range of some shared source text. never empty, always on char boundaries.
#[derive(Clone, Debug)]
struct Piece {
    source: Arc<str>,
    start: usize,
    len: usize,
    newlines: usize,
}

impl Piece {
    fn new(source: Arc<str>, start: usize, len: usize) -> Self {
        let newlines = count_newlines(&source[start..start + len]);
        Self {
            source,
            start,
            len,
            newlines,
        }
    }

    fn text(&self) -> &str {
        &self.source[self.start..self.start + self.len]
    }

    fn summary(&self) -> Summary {
        Summary {
            bytes: self.len,
            newlines: self.newlines,
        }
    }

    /// first `n` bytes.
    fn head(&self, n: usize) -> Piece {
        Piece::new(self.source.clone(), self.start, n)
    }

    /// bytes from `from` to the end.
    fn tail(&self, from: usize) -> Piece {
        Piece::new(self.source.clone(), self.start + from, self.len - from)
    }

    fn joined(&self, other: &Piece) -> Piece {
        let mut text = String::with_capacity(self.len + other.len);
        text.push_str(self.text());
        text.push_str(other.text());
        let len = text.len();
        Piece::new(Arc::from(text), 0, len)
    }
}

fn count_newlines(text: &str) -> usize {
    text.bytes().filter(|&b| b == b'\n').count()
}

/// cuts text into pieces of at most `PIECE_MAX` bytes, all sharing one source.
fn pieces_from(text: &str) -> Vec<Piece> {
    let source: Arc<str> = Arc::from(text);
    let mut pieces = Vec::with_capacity(text.len() / PIECE_MAX + 1);
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + PIECE_MAX).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        pieces.push(Piece::new(source.clone(), start, end - start));
        start = end;
    }
    pieces
}

/// splits items into the fewest groups of at most `max`, sizes kept even.
fn split_even<T>(mut items: Vec<T>, max: usize) -> Vec<Vec<T>> {
    let groups = items.len().div_ceil(max).max(1);
    let base = items.len() / groups;
    let extra = items.len() % groups;
    let mut out = Vec::with_capacity(groups);
    for i in (0..groups).rev() {
        let size = base + usize::from(i < extra);
        out.push(items.split_off(items.len() - size));
    }
    out.reverse();
    out
}

#[derive(Clone, Debug)]
struct Node {
    summary: Summary,
    kind: Kind,
}

#[derive(Clone, Debug)]
enum Kind {
    Leaf(Vec<Piece>),
    Inner(Vec<Arc<Node>>),
}

impl Node {
    fn leaf(pieces: Vec<Piece>) -> Node {
        let mut node = Node {
            summary: Summary::default(),
            kind: Kind::Leaf(pieces),
        };
        node.resum();
        node
    }

    fn inner(children: Vec<Arc<Node>>) -> Node {
        let mut node = Node {
            summary: Summary::default(),
            kind: Kind::Inner(children),
        };
        node.resum();
        node
    }

    fn width(&self) -> usize {
        match &self.kind {
            Kind::Leaf(pieces) => pieces.len(),
            Kind::Inner(children) => children.len(),
        }
    }

    fn resum(&mut self) {
        let mut summary = Summary::default();
        match &self.kind {
            Kind::Leaf(pieces) => pieces.iter().for_each(|p| summary.add(p.summary())),
            Kind::Inner(children) => children.iter().for_each(|c| summary.add(c.summary)),
        }
        self.summary = summary;
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Tree {
    root: Arc<Node>,
}

impl Default for Tree {
    fn default() -> Self {
        Self {
            root: Arc::new(Node::leaf(Vec::new())),
        }
    }
}

impl Tree {
    pub fn from_text(text: &str) -> Self {
        let pieces = pieces_from(text);
        if pieces.is_empty() {
            return Self::default();
        }
        let mut level: Vec<Node> = split_even(pieces, MAX_PIECES)
            .into_iter()
            .map(Node::leaf)
            .collect();
        while level.len() > 1 {
            let children = level.into_iter().map(Arc::new).collect();
            level = split_even(children, MAX_CHILDREN)
                .into_iter()
                .map(Node::inner)
                .collect();
        }
        Self {
            root: Arc::new(level.pop().expect("one node left")),
        }
    }

    pub fn summary(&self) -> Summary {
        self.root.summary
    }

    /// inserts at `offset`. the caller checked bounds and char boundary.
    pub fn insert(&mut self, offset: usize, text: &str) {
        if text.is_empty() {
            return;
        }
        let root = Arc::make_mut(&mut self.root);
        let extra = insert_into(root, offset, pieces_from(text));
        if !extra.is_empty() {
            self.grow(extra);
        }
    }

    /// deletes `range`. the caller checked bounds and char boundaries.
    pub fn delete(&mut self, range: Range<usize>) {
        if range.is_empty() {
            return;
        }
        let root = Arc::make_mut(&mut self.root);
        delete_from(root, range.start, range.end);
        while let Kind::Inner(children) = &self.root.kind {
            match children.len() {
                0 => self.root = Arc::new(Node::leaf(Vec::new())),
                1 => self.root = children[0].clone(),
                _ => break,
            }
        }
    }

    /// root split: stack new levels until one node holds everything.
    fn grow(&mut self, extra: Vec<Node>) {
        let old = std::mem::take(self).root;
        let mut level: Vec<Arc<Node>> = std::iter::once(old)
            .chain(extra.into_iter().map(Arc::new))
            .collect();
        loop {
            let mut nodes: Vec<Node> = split_even(level, MAX_CHILDREN)
                .into_iter()
                .map(Node::inner)
                .collect();
            if nodes.len() == 1 {
                self.root = Arc::new(nodes.pop().expect("one node"));
                return;
            }
            level = nodes.into_iter().map(Arc::new).collect();
        }
    }

    /// piece holding `offset`, its start, and lines before it.
    /// `offset == len` lands on the last piece. none for an empty tree.
    fn find(&self, offset: usize) -> Option<(&Piece, usize, usize)> {
        let mut node = &*self.root;
        let mut start = 0;
        let mut lines = 0;
        loop {
            match &node.kind {
                Kind::Inner(children) => {
                    let last = children.len() - 1;
                    for (i, child) in children.iter().enumerate() {
                        if offset < start + child.summary.bytes || i == last {
                            node = child;
                            break;
                        }
                        start += child.summary.bytes;
                        lines += child.summary.newlines;
                    }
                }
                Kind::Leaf(pieces) => {
                    let last = pieces.len().checked_sub(1)?;
                    for (i, piece) in pieces.iter().enumerate() {
                        if offset < start + piece.len || i == last {
                            return Some((piece, start, lines));
                        }
                        start += piece.len;
                        lines += piece.newlines;
                    }
                    unreachable!("leaf has pieces");
                }
            }
        }
    }

    pub fn is_char_boundary(&self, offset: usize) -> bool {
        if offset == 0 || offset == self.summary().bytes {
            return true;
        }
        match self.find(offset) {
            Some((piece, start, _)) => piece.text().is_char_boundary(offset - start),
            None => false,
        }
    }

    /// line holding `offset`, counted from 0. `offset <= len`.
    pub fn line_of(&self, offset: usize) -> usize {
        match self.find(offset) {
            Some((piece, start, lines)) => lines + count_newlines(&piece.text()[..offset - start]),
            None => 0,
        }
    }

    /// byte where `line` starts. `line <= newlines`.
    pub fn line_start(&self, line: usize) -> usize {
        if line == 0 {
            return 0;
        }
        let mut node = &*self.root;
        let mut start = 0;
        let mut k = line;
        loop {
            match &node.kind {
                Kind::Inner(children) => {
                    let child = children.iter().find(|c| {
                        if k <= c.summary.newlines {
                            return true;
                        }
                        k -= c.summary.newlines;
                        start += c.summary.bytes;
                        false
                    });
                    node = child.expect("line is in range");
                }
                Kind::Leaf(pieces) => {
                    for piece in pieces {
                        if k <= piece.newlines {
                            let at = piece
                                .text()
                                .bytes()
                                .enumerate()
                                .filter(|&(_, b)| b == b'\n')
                                .nth(k - 1)
                                .map(|(i, _)| i)
                                .expect("piece has the newline");
                            return start + at + 1;
                        }
                        k -= piece.newlines;
                        start += piece.len;
                    }
                    unreachable!("line is in range");
                }
            }
        }
    }

    /// text chunks covering `range`. the caller checked the range.
    pub fn chunks(&self, range: Range<usize>) -> Chunks<'_> {
        let mut chunks = Chunks {
            stack: Vec::new(),
            leaf: &[],
            index: 0,
            pos: 0,
            start: range.start,
            end: range.start,
        };
        if range.is_empty() {
            return chunks;
        }
        let mut node = &*self.root;
        let mut start = 0;
        loop {
            match &node.kind {
                Kind::Inner(children) => {
                    let last = children.len() - 1;
                    for (i, child) in children.iter().enumerate() {
                        if range.start < start + child.summary.bytes || i == last {
                            chunks.stack.push((children.as_slice(), i));
                            node = child;
                            break;
                        }
                        start += child.summary.bytes;
                    }
                }
                Kind::Leaf(pieces) => {
                    let mut index = 0;
                    while index + 1 < pieces.len() && range.start >= start + pieces[index].len {
                        start += pieces[index].len;
                        index += 1;
                    }
                    chunks.leaf = pieces;
                    chunks.index = index;
                    chunks.pos = start;
                    chunks.end = range.end;
                    return chunks;
                }
            }
        }
    }
}

fn child_at(children: &[Arc<Node>], offset: usize) -> (usize, usize) {
    let mut start = 0;
    for (i, child) in children.iter().enumerate() {
        if offset <= start + child.summary.bytes {
            return (i, start);
        }
        start += child.summary.bytes;
    }
    let last = children.len() - 1;
    (last, start - children[last].summary.bytes)
}

/// inserts pieces into a subtree. returns new siblings when the node overflowed.
fn insert_into(node: &mut Node, offset: usize, pieces: Vec<Piece>) -> Vec<Node> {
    let extra = match &mut node.kind {
        Kind::Leaf(list) => {
            insert_pieces(list, offset, pieces);
            if list.len() > MAX_PIECES {
                let mut groups = split_even(std::mem::take(list), MAX_PIECES);
                *list = groups.remove(0);
                groups.into_iter().map(Node::leaf).collect()
            } else {
                Vec::new()
            }
        }
        Kind::Inner(children) => {
            let (index, start) = child_at(children, offset);
            let child = Arc::make_mut(&mut children[index]);
            let split = insert_into(child, offset - start, pieces);
            let at = index + 1;
            children.splice(at..at, split.into_iter().map(Arc::new));
            if children.len() > MAX_CHILDREN {
                let mut groups = split_even(std::mem::take(children), MAX_CHILDREN);
                *children = groups.remove(0);
                groups.into_iter().map(Node::inner).collect()
            } else {
                Vec::new()
            }
        }
    };
    node.resum();
    extra
}

fn insert_pieces(list: &mut Vec<Piece>, offset: usize, pieces: Vec<Piece>) {
    let mut index = list.len();
    let mut start = 0;
    for (i, piece) in list.iter().enumerate() {
        let end = start + piece.len;
        if offset <= end {
            if offset == start {
                index = i;
            } else if offset == end {
                index = i + 1;
            } else {
                let local = offset - start;
                let (head, tail) = (piece.head(local), piece.tail(local));
                list[i] = head;
                list.insert(i + 1, tail);
                index = i + 1;
            }
            break;
        }
        start = end;
    }

    // typing makes many tiny pieces; fold a small one into a small neighbour.
    if let [piece] = pieces.as_slice() {
        if index > 0 && list[index - 1].len + piece.len <= PIECE_JOIN {
            list[index - 1] = list[index - 1].joined(piece);
            return;
        }
        if index < list.len() && list[index].len + piece.len <= PIECE_JOIN {
            list[index] = piece.joined(&list[index]);
            return;
        }
    }
    list.splice(index..index, pieces);
}

/// deletes `start..end` (relative to the node) from a subtree.
fn delete_from(node: &mut Node, start: usize, end: usize) {
    match &mut node.kind {
        Kind::Leaf(list) => {
            let mut kept = Vec::with_capacity(list.len() + 1);
            let mut seam = None;
            let mut pos = 0;
            for piece in list.drain(..) {
                let (ps, pe) = (pos, pos + piece.len);
                pos = pe;
                if pe <= start || ps >= end {
                    kept.push(piece);
                    continue;
                }
                if ps < start {
                    kept.push(piece.head(start - ps));
                }
                seam.get_or_insert(kept.len());
                if pe > end {
                    kept.push(piece.tail(end - ps));
                }
            }
            if let Some(s) = seam
                && s > 0
                && s < kept.len()
                && kept[s - 1].len + kept[s].len <= PIECE_JOIN
            {
                let joined = kept[s - 1].joined(&kept[s]);
                kept[s - 1] = joined;
                kept.remove(s);
            }
            *list = kept;
        }
        Kind::Inner(children) => {
            let mut kept = Vec::with_capacity(children.len());
            let mut pos = 0;
            for mut child in children.drain(..) {
                let (cs, ce) = (pos, pos + child.summary.bytes);
                pos = ce;
                if ce <= start || cs >= end {
                    kept.push(child);
                } else if start <= cs && ce <= end {
                    // fully covered, drop it
                } else {
                    delete_from(
                        Arc::make_mut(&mut child),
                        start.max(cs) - cs,
                        end.min(ce) - cs,
                    );
                    kept.push(child);
                }
            }
            rebalance(&mut kept);
            *children = kept;
        }
    }
    node.resum();
}

/// merges underfull children into a neighbour.
fn rebalance(children: &mut Vec<Arc<Node>>) {
    let mut i = 0;
    while i < children.len() {
        if children.len() > 1 && children[i].width() < MIN_FILL {
            let a = if i + 1 < children.len() { i } else { i - 1 };
            let right = children.remove(a + 1);
            let left = children.remove(a);
            let merged = merge(left, right);
            children.splice(a..a, merged);
            i = a;
            if children[a].width() >= MIN_FILL {
                i += 1;
            }
            continue;
        }
        i += 1;
    }
}

/// joins two siblings at the same depth. one node back, or two when too wide.
fn merge(left: Arc<Node>, right: Arc<Node>) -> Vec<Arc<Node>> {
    let left = Arc::unwrap_or_clone(left);
    let right = Arc::unwrap_or_clone(right);
    match (left.kind, right.kind) {
        (Kind::Leaf(mut a), Kind::Leaf(b)) => {
            let seam = a.len();
            a.extend(b);
            if seam > 0 && seam < a.len() && a[seam - 1].len + a[seam].len <= PIECE_JOIN {
                let joined = a[seam - 1].joined(&a[seam]);
                a[seam - 1] = joined;
                a.remove(seam);
            }
            split_even(a, MAX_PIECES)
                .into_iter()
                .map(|g| Arc::new(Node::leaf(g)))
                .collect()
        }
        (Kind::Inner(mut a), Kind::Inner(b)) => {
            a.extend(b);
            split_even(a, MAX_CHILDREN)
                .into_iter()
                .map(|g| Arc::new(Node::inner(g)))
                .collect()
        }
        _ => unreachable!("siblings sit at the same depth"),
    }
}

/// text pieces over a byte range, in order.
pub struct Chunks<'a> {
    stack: Vec<(&'a [Arc<Node>], usize)>,
    leaf: &'a [Piece],
    index: usize,
    pos: usize,
    start: usize,
    end: usize,
}

impl<'a> Iterator for Chunks<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        loop {
            if self.pos >= self.end {
                return None;
            }
            if let Some(piece) = self.leaf.get(self.index) {
                let ps = self.pos;
                self.pos += piece.len;
                self.index += 1;
                let from = self.start.saturating_sub(ps);
                let to = (self.end - ps).min(piece.len);
                return Some(&piece.text()[from..to]);
            }
            // leaf done, climb to the next one
            loop {
                let (children, i) = self.stack.last_mut()?;
                *i += 1;
                if let Some(next) = children.get(*i) {
                    let mut node: &'a Node = next;
                    loop {
                        match &node.kind {
                            Kind::Inner(c) => {
                                self.stack.push((c.as_slice(), 0));
                                node = &c[0];
                            }
                            Kind::Leaf(p) => {
                                self.leaf = p;
                                self.index = 0;
                                break;
                            }
                        }
                    }
                    break;
                }
                self.stack.pop();
            }
        }
    }
}

#[cfg(test)]
impl Tree {
    /// checks every invariant, returns the depth.
    pub fn check(&self) -> usize {
        fn walk(node: &Node, is_root: bool) -> usize {
            let mut sum = Summary::default();
            let depth = match &node.kind {
                Kind::Leaf(pieces) => {
                    assert!(is_root || !pieces.is_empty(), "empty leaf");
                    assert!(pieces.len() <= MAX_PIECES, "leaf too wide");
                    for p in pieces {
                        assert!(p.len > 0, "empty piece");
                        assert_eq!(p.newlines, count_newlines(p.text()));
                        sum.add(p.summary());
                    }
                    0
                }
                Kind::Inner(children) => {
                    assert!(children.len() >= 2 || !is_root, "root with one child");
                    assert!(!children.is_empty(), "empty inner");
                    assert!(children.len() <= MAX_CHILDREN, "inner too wide");
                    let depths: Vec<usize> = children
                        .iter()
                        .map(|c| {
                            sum.add(c.summary);
                            walk(c, false)
                        })
                        .collect();
                    assert!(depths.windows(2).all(|w| w[0] == w[1]), "uneven depth");
                    depths[0] + 1
                }
            };
            assert_eq!(sum, node.summary, "stale summary");
            depth
        }
        walk(&self.root, true)
    }

    pub fn text(&self) -> String {
        self.chunks(0..self.summary().bytes).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_even_keeps_groups_close() {
        let groups = split_even((0..35).collect(), 16);
        let sizes: Vec<usize> = groups.iter().map(Vec::len).collect();
        assert_eq!(sizes, [12, 12, 11]);
        assert_eq!(split_even(Vec::<u8>::new(), 4).len(), 1);
    }

    #[test]
    fn big_text_builds_balanced() {
        let text = "line of text\n".repeat(50_000);
        let tree = Tree::from_text(&text);
        assert!(tree.check() >= 2);
        assert_eq!(tree.text(), text);
        assert_eq!(tree.summary().newlines, 50_000);
        assert_eq!(tree.line_start(1234), 1234 * 13);
        assert_eq!(tree.line_of(1234 * 13 + 5), 1234);
    }

    #[test]
    fn pieces_never_split_chars() {
        let text = "가".repeat(2000);
        for piece in pieces_from(&text) {
            assert!(piece.len <= PIECE_MAX);
            assert_eq!(piece.len % 3, 0);
        }
    }

    #[test]
    fn typing_folds_into_one_piece() {
        let mut tree = Tree::default();
        for (i, c) in "hello".chars().enumerate() {
            tree.insert(i, &c.to_string());
        }
        let Kind::Leaf(pieces) = &tree.root.kind else {
            panic!("small tree is one leaf");
        };
        assert_eq!(pieces.len(), 1);
        assert_eq!(tree.text(), "hello");
    }

    #[test]
    fn delete_everything_leaves_empty_leaf() {
        let text = "abc\n".repeat(20_000);
        let mut tree = Tree::from_text(&text);
        tree.delete(0..text.len());
        tree.check();
        assert_eq!(tree.summary(), Summary::default());
        assert_eq!(tree.chunks(0..0).count(), 0);
    }

    #[test]
    fn clone_is_untouched_by_edits() {
        let text = "0123456789\n".repeat(5000);
        let mut tree = Tree::from_text(&text);
        let snapshot = tree.clone();
        tree.insert(500, "new");
        tree.delete(1000..30_000);
        assert_eq!(snapshot.text(), text);
        snapshot.check();
        tree.check();
    }

    /// small xorshift so tests need no deps and stay repeatable.
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % n.max(1) as u64) as usize
        }
    }

    fn boundary(text: &str, mut at: usize) -> usize {
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        at
    }

    #[test]
    fn random_edits_match_string() {
        const BITS: [&str; 7] = ["a", "bc", "\n", "가나", "é", "😀\n", "\r\n"];
        for seed in 1..=6u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            let mut model = "start\n".repeat(200 * seed as usize);
            let mut tree = Tree::from_text(&model);
            for step in 0..3000 {
                let at = boundary(&model, rng.below(model.len() + 1));
                if rng.below(3) == 0 && !model.is_empty() {
                    let end = boundary(&model, (at + rng.below(400)).min(model.len()));
                    tree.delete(at..end);
                    model.replace_range(at..end, "");
                } else {
                    let mut text = String::new();
                    let big = rng.below(50) == 0;
                    for _ in 0..if big { 900 } else { 1 + rng.below(4) } {
                        text.push_str(BITS[rng.below(BITS.len())]);
                    }
                    tree.insert(at, &text);
                    model.insert_str(at, &text);
                }
                if step % 100 == 0 {
                    tree.check();
                    assert_eq!(tree.text(), model, "seed {seed} step {step}");
                }
                let probe = boundary(&model, rng.below(model.len() + 1));
                assert_eq!(tree.line_of(probe), model[..probe].matches('\n').count());
            }
            tree.check();
            assert_eq!(tree.text(), model);
            let lines = model.matches('\n').count();
            assert_eq!(tree.summary().newlines, lines);
            for line in [0, lines / 3, lines] {
                let expect = if line == 0 {
                    0
                } else {
                    model.match_indices('\n').nth(line - 1).unwrap().0 + 1
                };
                assert_eq!(tree.line_start(line), expect);
            }
        }
    }

    #[test]
    fn chunks_clip_to_range() {
        let tree = Tree::from_text(&"abcdefghij".repeat(500));
        let got: String = tree.chunks(1003..2507).collect();
        assert_eq!(got.len(), 1504);
        assert!(got.starts_with("defg"));
    }
}
