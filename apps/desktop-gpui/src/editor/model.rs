use std::{collections::VecDeque, ops::Range, sync::Arc};

use serde_json::{Map, Value, json};
use unicode_segmentation::UnicodeSegmentation;

use super::{
    document::{
        Document, EditResult, Node, NodeRef, concat_inline, inline_text, split_inline, utf8,
    },
    sequence::Sequence,
};

const HISTORY_BYTES: usize = 8 * 1024 * 1024;
const HISTORY_ENTRIES: usize = 2048;
const MAX_INSERT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
}

impl Selection {
    pub fn caret(position: usize) -> Self {
        Self {
            anchor: position,
            head: position,
        }
    }
    pub fn range(self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }
    pub fn is_empty(self) -> bool {
        self.anchor == self.head
    }
}

#[derive(Clone)]
struct Checkpoint {
    root: NodeRef,
    selection: Selection,
}

#[derive(Clone)]
struct HistoryEntry {
    before: Checkpoint,
    after: Checkpoint,
    bytes: usize,
}

struct Composition {
    before: Checkpoint,
    range: Range<usize>,
    bytes: usize,
}

#[derive(Clone, Debug)]
pub struct Mapping {
    pub old: Range<usize>,
    pub inserted: usize,
}

impl Mapping {
    pub fn map(&self, position: usize, after: bool) -> usize {
        if position < self.old.start {
            position
        } else if position > self.old.end {
            position - self.old.len() + self.inserted
        } else {
            self.old.start + if after { self.inserted } else { 0 }
        }
    }

    pub fn map_anchor(&self, range: Range<usize>) -> Option<Range<usize>> {
        let start = self.map(range.start, true);
        let end = self.map(range.end, false);
        (start < end).then_some(start..end)
    }
}

pub struct EditorModel {
    pub document: Document,
    pub selection: Selection,
    pub revision: u64,
    pub last_mapping: Option<Mapping>,
    pub read_only: bool,
    stored_marks: Option<Vec<Value>>,
    undo: VecDeque<HistoryEntry>,
    redo: Vec<HistoryEntry>,
    history_bytes: usize,
    composition: Option<Composition>,
    batch_cost: Option<usize>,
}

impl EditorModel {
    pub fn new(document: Document) -> Self {
        let selection = Selection::caret(document.first_caret());
        Self {
            document,
            selection,
            revision: 0,
            last_mapping: None,
            read_only: false,
            stored_marks: None,
            undo: VecDeque::new(),
            redo: Vec::new(),
            history_bytes: 0,
            composition: None,
            batch_cost: None,
        }
    }

    fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            root: self.document.root.clone(),
            selection: self.selection,
        }
    }

    fn restore(&mut self, checkpoint: &Checkpoint) {
        self.document.root = checkpoint.root.clone();
        self.selection = checkpoint.selection;
        self.stored_marks = None;
        self.last_mapping = None;
        self.revision += 1;
    }

    fn ensure_editable(&self) -> EditResult<()> {
        if self.read_only {
            Err("Document is read-only".into())
        } else {
            Ok(())
        }
    }

    fn record(&mut self, before: Checkpoint, bytes: usize) {
        self.revision += 1;
        if let Some(cost) = &mut self.batch_cost {
            *cost += bytes;
            return;
        }
        if let Some(composition) = &mut self.composition {
            composition.bytes += bytes;
            return;
        }
        self.push_history(before, bytes);
    }

    pub fn transaction(
        &mut self,
        action: impl FnOnce(&mut Self) -> EditResult<()>,
    ) -> EditResult<()> {
        self.ensure_editable()?;
        if self.composing() || self.batch_cost.is_some() {
            return Err("Finish composition before a structural transaction".into());
        }
        let before = self.checkpoint();
        let revision = self.revision;
        let mapping = self.last_mapping.take();
        let marks = self.stored_marks.clone();
        self.batch_cost = Some(0);
        let result = action(self);
        let cost = self.batch_cost.take().expect("active transaction");
        if result.is_err() {
            self.document.root = before.root;
            self.selection = before.selection;
            self.revision = revision;
            self.last_mapping = mapping;
            self.stored_marks = marks;
        } else if revision != self.revision {
            self.push_history(before, cost);
        }
        result
    }

    fn push_history(&mut self, before: Checkpoint, bytes: usize) {
        self.redo.clear();
        let entry = HistoryEntry {
            before,
            after: self.checkpoint(),
            bytes,
        };
        self.history_bytes += bytes;
        self.undo.push_back(entry);
        while (self.history_bytes > HISTORY_BYTES && self.undo.len() > 1)
            || self.undo.len() > HISTORY_ENTRIES
        {
            self.history_bytes -= self.undo.pop_front().expect("nonempty history").bytes;
        }
    }

    pub fn select(&mut self, selection: Selection) {
        self.selection = selection;
        self.stored_marks = None;
    }

    pub fn marked_range(&self) -> Option<Range<usize>> {
        self.composition
            .as_ref()
            .map(|composition| composition.range.clone())
    }

    pub fn composing(&self) -> bool {
        self.composition.is_some()
    }

    pub fn replace(&mut self, range: Range<usize>, text: &str) -> EditResult<()> {
        self.ensure_editable()?;
        if text.len() > MAX_INSERT_BYTES {
            return Err("Paste exceeds the 1 MiB transaction limit".into());
        }
        if range.start > range.end {
            return Err("Invalid replacement range".into());
        }
        if range.start == 0 && range.end == self.document.units() {
            let fields = Arc::new(Map::from_iter([("type".into(), json!("text"))]));
            let paragraph = Node::fresh(
                "paragraph",
                if text.is_empty() {
                    Sequence::default()
                } else {
                    Sequence::one(Node::text_node(text, fields))
                },
            );
            let before = self.checkpoint();
            self.document.root = self.document.root.with_children(Sequence::one(paragraph));
            self.selection = Selection::caret(1 + text.encode_utf16().count());
            self.last_mapping = Some(Mapping {
                old: range.clone(),
                inserted: self.document.units(),
            });
            self.record(before, text.len() + range.len() * 4 + 4096);
            return Ok(());
        }
        let from = self.document.resolve(range.start)?;
        let to = self.document.resolve(range.end)?;
        let (left, _) = split_inline(&from.node.children, from.offset)?;
        let (_, right) = split_inline(&to.node.children, to.offset)?;
        let fields = self.insertion_fields(&from.node, from.offset);
        let inserted = if text.is_empty() {
            Sequence::default()
        } else {
            Sequence::one(Node::text_node(text, fields))
        };
        let children = concat_inline(&concat_inline(&left, &inserted), &right);
        let before = self.checkpoint();
        if from.path == to.path {
            self.document
                .replace_node(&from.path, from.node.with_children(children));
        } else {
            let parent_path = &from.path[..from.path.len() - 1];
            if parent_path != &to.path[..to.path.len() - 1] {
                return Err("Cross-container replacement is unavailable; selection and original content retained".into());
            }
            let start = *from.path.last().expect("textblock path");
            let end = *to.path.last().expect("textblock path");
            let parent = self.document.node(parent_path).expect("resolved parent");
            for i in start..=end {
                if !parent
                    .children
                    .get(i)
                    .is_some_and(|node| node.is_textblock())
                {
                    return Err(
                        "Replacement across an embedded block is unavailable; original retained"
                            .into(),
                    );
                }
            }
            self.document.replace_node(
                parent_path,
                parent.with_children(parent.children.splice(
                    start..end + 1,
                    &Sequence::one(from.node.with_children(children)),
                )),
            );
        }
        let inserted = text.encode_utf16().count();
        self.selection = Selection::caret(range.start + inserted);
        let removed = range.len();
        self.last_mapping = Some(Mapping {
            old: range,
            inserted,
        });
        self.record(
            before,
            text.len() + removed * 4 + 2048 * (from.path.len() + to.path.len()),
        );
        Ok(())
    }

    pub fn type_text(&mut self, range: Range<usize>, text: &str) -> EditResult<()> {
        if range.is_empty() && text.chars().count() == 1 && self.composition.is_none() {
            let resolved = self.document.resolve(range.start)?;
            let fields = self.insertion_fields(&resolved.node, resolved.offset);
            let marks = fields.get("marks").and_then(Value::as_array);
            if resolved.node.kind() != "codeBlock"
                && !marks.is_some_and(|marks| marks.iter().any(|mark| mark["type"] == "code"))
            {
                let mut offset = resolved.offset.saturating_sub(4);
                let tail = loop {
                    match split_inline(&resolved.node.children, offset) {
                        Ok((_, right)) => {
                            let left = split_inline(&right, resolved.offset - offset)?.0;
                            break inline_text(&left);
                        }
                        Err(_) if offset < resolved.offset => offset += 1,
                        Err(error) => return Err(error),
                    }
                };
                if text == "." && tail.ends_with("..") {
                    return self.replace(range.start - 2..range.end, "…");
                }
                if text == "-"
                    && tail.ends_with('-')
                    && tail.chars().rev().nth(1).is_some_and(|c| c != '-')
                {
                    return self.replace(range.start - 1..range.end, "—");
                }
                if matches!(text, "\"" | "'") {
                    let open = tail
                        .chars()
                        .last()
                        .is_none_or(|c| c.is_whitespace() || "{[(<'\"‘“".contains(c));
                    let quote = match (text, open) {
                        ("\"", true) => "“",
                        ("\"", false) => "”",
                        (_, true) => "‘",
                        _ => "’",
                    };
                    return self.replace(range, quote);
                }
            }
        }
        self.replace(range, text)
    }

    pub fn insert_slice(&mut self, fragment: Document, open: bool) -> EditResult<()> {
        self.ensure_editable()?;
        if self.composing() {
            return Err("Finish composition before pasting rich content".into());
        }
        let range = self.selection.range();
        if range.start == 0 && range.end == self.document.units() {
            let before = self.checkpoint();
            let cost = fragment.original.len() + range.len() * 4 + 4096;
            self.document.root = self
                .document
                .root
                .with_children(fragment.root.children.clone());
            self.selection = Selection::caret(self.document.first_caret());
            self.record(before, cost);
            return Ok(());
        }
        let from = self.document.resolve(range.start)?;
        let to = self.document.resolve(range.end)?;
        if from.path != to.path {
            return Err("Rich paste across blocks needs an explicit block selection".into());
        }
        let first = fragment
            .root
            .children
            .get(0)
            .ok_or("Empty clipboard slice")?;
        let last = fragment
            .root
            .children
            .get(fragment.root.children.len() - 1)
            .expect("first");
        if from.node.kind() == "codeBlock" {
            return Err("Use plain-text paste inside code blocks".into());
        }
        let (left, _) = split_inline(&from.node.children, from.offset)?;
        let (_, right) = split_inline(&from.node.children, to.offset)?;
        let before = self.checkpoint();
        if fragment.root.children.len() == 1 && open && first.is_textblock() {
            self.document.replace_node(
                &from.path,
                from.node.with_children(concat_inline(
                    &concat_inline(&left, &first.children),
                    &right,
                )),
            );
            self.selection = Selection::caret(range.start + first.children.units());
        } else {
            let path = &from.path[..from.path.len() - 1];
            let index = *from.path.last().expect("textblock");
            let parent = self.document.node(path).expect("parent");
            if parent.kind() != "doc" {
                return Err("Block paste in nested containers is unavailable".into());
            }
            let mut inserted = fragment.root.children.clone();
            if open && first.is_textblock() && last.is_textblock() {
                inserted = inserted.splice(
                    0..1,
                    &Sequence::one(
                        from.node
                            .with_children(concat_inline(&left, &first.children)),
                    ),
                );
                inserted = inserted.splice(
                    inserted.len() - 1..inserted.len(),
                    &Sequence::one(last.with_children(concat_inline(&last.children, &right))),
                );
                let caret = from.start - 1
                    + inserted.prefix(inserted.len() - 1)
                    + 1
                    + last.children.units();
                self.selection = Selection::caret(caret);
            } else {
                inserted = Sequence::one(from.node.with_children(left))
                    .concat(&inserted)
                    .concat(&Sequence::one(Node::fresh("paragraph", right)));
                self.selection =
                    Selection::caret(from.start - 1 + inserted.prefix(inserted.len() - 1) + 1);
            }
            self.document.replace_node(
                path,
                parent.with_children(parent.children.splice(index..index + 1, &inserted)),
            );
        }
        self.record(before, fragment.original.len() + range.len() * 4 + 8192);
        Ok(())
    }

    fn insertion_fields(&self, block: &NodeRef, offset: usize) -> Arc<Map<String, Value>> {
        let previous = block.children.locate(offset.saturating_sub(1));
        let next = block.children.locate(offset);
        let source = previous
            .as_ref()
            .map(|(_, _, node)| *node)
            .or_else(|| next.as_ref().map(|(_, _, node)| *node));
        let marks = self.stored_marks.clone().unwrap_or_else(|| {
            let mut marks = source.map(|node| node.marks().to_vec()).unwrap_or_default();
            let at_link_end = offset == block.children.units()
                || match (previous, next) {
                    (Some((a, _, _)), Some((b, _, _))) => a != b,
                    _ => true,
                };
            if at_link_end || offset == 0 {
                marks.retain(|mark| mark.get("type").and_then(Value::as_str) != Some("link"));
            }
            marks
        });
        if let Some(node) = source.filter(|node| node.text.is_some() && node.marks() == marks) {
            return node.fields.clone();
        }
        let mut fields = source
            .filter(|node| node.text.is_some())
            .map(|node| (*node.fields).clone())
            .unwrap_or_else(|| Map::from_iter([("type".into(), json!("text"))]));
        if marks.is_empty() {
            fields.remove("marks");
        } else {
            fields.insert("marks".into(), marks.into());
        }
        Arc::new(fields)
    }

    pub fn compose(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Range<usize>,
    ) -> EditResult<()> {
        let replacement = range
            .or_else(|| self.marked_range())
            .unwrap_or_else(|| self.selection.range());
        if selected.start > selected.end || selected.end > text.encode_utf16().count() {
            return Err("Composition selection is outside its text".into());
        }
        utf8(text, selected.start)?;
        utf8(text, selected.end)?;
        let new_composition = self.composition.is_none();
        if new_composition {
            self.composition = Some(Composition {
                before: self.checkpoint(),
                range: replacement.clone(),
                bytes: 0,
            });
        }
        if let Err(error) = self.replace(replacement.clone(), text) {
            if new_composition {
                self.composition = None;
            }
            return Err(error);
        }
        let end = replacement.start + text.encode_utf16().count();
        self.composition
            .as_mut()
            .expect("composition started")
            .range = replacement.start..end;
        self.selection = Selection {
            anchor: replacement.start + selected.start,
            head: replacement.start + selected.end,
        };
        Ok(())
    }

    pub fn commit_composition(&mut self) {
        if let Some(composition) = self.composition.take() {
            self.push_history(composition.before, composition.bytes.max(2048));
        }
    }

    pub fn cancel_composition(&mut self) {
        if let Some(composition) = self.composition.take() {
            self.restore(&composition.before);
        }
    }

    pub fn undo(&mut self) -> EditResult<()> {
        self.ensure_editable()?;
        if self.composing() {
            self.cancel_composition();
            return Ok(());
        }
        if let Some(entry) = self.undo.pop_back() {
            self.history_bytes -= entry.bytes;
            self.restore(&entry.before);
            self.redo.push(entry);
        }
        Ok(())
    }

    pub fn redo(&mut self) -> EditResult<()> {
        self.ensure_editable()?;
        if self.composing() {
            return Err("Finish composition before redo".into());
        }
        if let Some(entry) = self.redo.pop() {
            self.restore(&entry.after);
            self.history_bytes += entry.bytes;
            self.undo.push_back(entry);
        }
        Ok(())
    }

    pub fn toggle_mark(&mut self, kind: &str) -> EditResult<()> {
        self.ensure_editable()?;
        let range = self.selection.range();
        let from = self.document.resolve(range.start)?;
        let to = self.document.resolve(range.end)?;
        if from.node.kind() == "codeBlock" {
            return Err("Code blocks do not accept formatting marks".into());
        }
        if kind == "code" {
            let mut unknown = false;
            from.node.children.visit(&mut |node| {
                unknown |= node.marks().iter().any(|mark| {
                    !matches!(
                        mark["type"].as_str(),
                        Some(
                            "bold"
                                | "italic"
                                | "underline"
                                | "strike"
                                | "highlight"
                                | "link"
                                | "code"
                        )
                    )
                });
            });
            if unknown {
                return Err(
                    "Code formatting would discard an unknown mark; original retained".into(),
                );
            }
        }
        if self.selection.is_empty() {
            let fields = self.insertion_fields(&from.node, from.offset);
            let mut marks = fields
                .get("marks")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            toggle_marks(&mut marks, kind, None);
            self.stored_marks = Some(marks);
            return Ok(());
        }
        if from.path != to.path {
            return Err("Formatting across text blocks is not available yet".into());
        }
        let (through, right) = split_inline(&from.node.children, to.offset)?;
        let (left, selected) = split_inline(&through, from.offset)?;
        let mut all_marked = true;
        selected.visit(&mut |node| {
            if node.text.is_some() {
                all_marked &= node.marks().iter().any(|mark| mark["type"] == kind);
            }
        });
        let mut changed = Vec::new();
        selected.visit(&mut |node| {
            if node.text.is_none() {
                changed.push(node.clone());
                return;
            }
            let mut fields = (*node.fields).clone();
            let mut marks = node.marks().to_vec();
            toggle_marks(&mut marks, kind, Some(!all_marked));
            if marks.is_empty() {
                fields.remove("marks");
            } else {
                fields.insert("marks".into(), marks.into());
            }
            changed.push(node.with_fields(fields));
        });
        let before = self.checkpoint();
        self.document.replace_node(
            &from.path,
            from.node.with_children(concat_inline(
                &concat_inline(&left, &Sequence::from_items(changed)),
                &right,
            )),
        );
        self.record(before, 4096 + selected.len() * 1024);
        Ok(())
    }

    pub fn set_link(&mut self, href: &str) -> EditResult<()> {
        self.ensure_editable()?;
        if !super::clipboard::openable_link(href) {
            return Err("Links must use HTTP or HTTPS".into());
        }
        let range = self.selection.range();
        let from = self.document.resolve(range.start)?;
        let to = self.document.resolve(range.end)?;
        if from.path != to.path || range.is_empty() || from.node.kind() == "codeBlock" {
            return Err("Select link text inside one non-code block".into());
        }
        let (through, right) = split_inline(&from.node.children, to.offset)?;
        let (left, selected) = split_inline(&through, from.offset)?;
        let mut linked = Vec::new();
        selected.visit(&mut |node| {
            let mut fields = (*node.fields).clone();
            let mut marks = node.marks().to_vec();
            marks.retain(|mark| mark["type"] != "link");
            marks.push(json!({"type":"link","attrs":{"href":href,"target":null}}));
            fields.insert("marks".into(), marks.into());
            linked.push(node.with_fields(fields));
        });
        let before = self.checkpoint();
        self.document.replace_node(
            &from.path,
            from.node.with_children(concat_inline(
                &concat_inline(&left, &Sequence::from_items(linked)),
                &right,
            )),
        );
        self.record(before, selected.len() * 1024 + 4096);
        Ok(())
    }

    pub fn insert_inline_atom(&mut self, kind: &str, attrs: Map<String, Value>) -> EditResult<()> {
        self.ensure_editable()?;
        if !matches!(kind, "mention-@" | "appLink") {
            return Err("Unsupported inline atom".into());
        }
        let range = self.selection.range();
        let from = self.document.resolve(range.start)?;
        let to = self.document.resolve(range.end)?;
        if from.path != to.path || from.node.kind() == "codeBlock" {
            return Err("Choose one non-code block for this item".into());
        }
        let (left, _) = split_inline(&from.node.children, from.offset)?;
        let (_, right) = split_inline(&from.node.children, to.offset)?;
        let node = Node::fresh(kind, Sequence::default());
        let mut fields = (*node.fields).clone();
        fields.insert("attrs".into(), attrs.into());
        let before = self.checkpoint();
        self.document.replace_node(
            &from.path,
            from.node.with_children(
                left.concat(&Sequence::one(node.with_fields(fields)))
                    .concat(&right),
            ),
        );
        self.selection = Selection::caret(range.start + 1);
        self.last_mapping = Some(Mapping {
            old: range,
            inserted: 1,
        });
        self.record(before, 8192);
        Ok(())
    }

    pub fn insert_block_atom(&mut self, kind: &str, attrs: Map<String, Value>) -> EditResult<()> {
        self.ensure_editable()?;
        if !matches!(kind, "horizontalRule" | "image" | "fileAttachment") {
            return Err("Unsupported block atom".into());
        }
        if attrs.contains_key("path") || attrs.contains_key("src") {
            return Err(
                "Use a catalogued attachment ID; local paths must not enter stored documents"
                    .into(),
            );
        }
        if kind != "horizontalRule"
            && attrs
                .get("attachmentId")
                .and_then(Value::as_str)
                .is_none_or(|id| id.is_empty())
        {
            return Err("Attachment must have a catalogued attachment ID".into());
        }
        let resolved = self.document.resolve(self.selection.head)?;
        if !self.selection.is_empty() {
            return Err("Collapse the selection before inserting a block".into());
        }
        let node = Node::fresh(kind, Sequence::default());
        let mut fields = (*node.fields).clone();
        fields.insert("attrs".into(), attrs.into());
        let (left, right) = split_inline(&resolved.node.children, resolved.offset)?;
        let path = &resolved.path[..resolved.path.len() - 1];
        let index = *resolved.path.last().expect("textblock");
        let parent = self.document.node(path).expect("parent");
        if !matches!(parent.kind(), "doc" | "blockquote") {
            return Err("Insert block attachments outside lists and tables".into());
        }
        let before = self.checkpoint();
        let inserted = Sequence::from_items([
            resolved.node.with_children(left),
            node.with_fields(fields),
            Node::fresh("paragraph", right),
        ]);
        self.document.replace_node(
            path,
            parent.with_children(parent.children.splice(index..index + 1, &inserted)),
        );
        self.selection = Selection::caret(resolved.start - 1 + inserted.prefix(2) + 1);
        self.record(before, 8192);
        Ok(())
    }

    pub fn set_block(&mut self, kind: &str, level: Option<u8>) -> EditResult<()> {
        self.ensure_editable()?;
        let resolved = self.document.resolve(self.selection.head)?;
        let mut fields = (*resolved.node.fields).clone();
        fields.insert("type".into(), json!(kind));
        if let Some(level) = level {
            let mut attrs = resolved.node.attrs().cloned().unwrap_or_default();
            attrs.insert("level".into(), json!(level.clamp(1, 6)));
            fields.insert("attrs".into(), attrs.into());
        }
        if kind == "codeBlock" {
            let mut has_marks_or_atoms = false;
            resolved.node.children.visit(&mut |child| {
                has_marks_or_atoms |= child.text.is_none() || !child.marks().is_empty()
            });
            if has_marks_or_atoms {
                return Err("Remove marks and embedded content before converting to code".into());
            }
        }
        let before = self.checkpoint();
        self.document
            .replace_node(&resolved.path, resolved.node.with_fields(fields));
        self.record(before, 4096);
        Ok(())
    }

    pub fn split_block(&mut self) -> EditResult<()> {
        self.ensure_editable()?;
        if !self.selection.is_empty() {
            return Err("Split with a selection is unavailable; delete the selection first".into());
        }
        let resolved = self.document.resolve(self.selection.head)?;
        if resolved.node.kind() == "codeBlock" {
            if resolved.offset == resolved.node.children.units() && resolved.offset > 0 {
                let tail = split_inline(&resolved.node.children, resolved.offset - 1)?.1;
                if inline_text(&tail) == "\n" {
                    let path = &resolved.path[..resolved.path.len() - 1];
                    let index = *resolved.path.last().expect("textblock");
                    let parent = self.document.node(path).expect("parent");
                    let before = self.checkpoint();
                    let trimmed = split_inline(&resolved.node.children, resolved.offset - 1)?.0;
                    let replacement = if trimmed.len() == 0 {
                        Sequence::one(Node::fresh("paragraph", Sequence::default()))
                    } else {
                        Sequence::from_items([
                            resolved.node.with_children(trimmed),
                            Node::fresh("paragraph", Sequence::default()),
                        ])
                    };
                    let position =
                        resolved.start - 1 + replacement.prefix(replacement.len() - 1) + 1;
                    self.document.replace_node(
                        path,
                        parent
                            .with_children(parent.children.splice(index..index + 1, &replacement)),
                    );
                    self.selection = Selection::caret(position);
                    self.record(before, 8192);
                    return Ok(());
                }
            }
            return self.replace(self.selection.range(), "\n");
        }
        let (left, right) = split_inline(&resolved.node.children, resolved.offset)?;
        let path = &resolved.path[..resolved.path.len() - 1];
        let index = *resolved.path.last().expect("textblock");
        let parent = self.document.node(path).expect("parent");
        if parent.kind() == "taskItem" {
            return Err(
                "Task splitting requires an allocated task-item identity from the task service"
                    .into(),
            );
        }
        if parent.kind() == "listItem" {
            if index != 0 || path.is_empty() {
                return Err("Split requires the first paragraph of a list item".into());
            }
            let list_path = &path[..path.len() - 1];
            let item_index = *path.last().expect("list item");
            let list = self.document.node(list_path).expect("list");
            if resolved.node.children.len() == 0 {
                if list_path.len() != 1
                    || item_index + 1 != list.children.len()
                    || parent.children.len() != 1
                {
                    return Err(
                        "Exiting a nonterminal or nested empty list item is unavailable".into(),
                    );
                }
                let before = self.checkpoint();
                let list_index = list_path[0];
                let kept = list.children.split(item_index).0;
                let replacement = if kept.len() == 0 {
                    Sequence::one(resolved.node.clone())
                } else {
                    Sequence::from_items([list.with_children(kept), resolved.node.clone()])
                };
                let position = self.document.root.children.prefix(list_index)
                    + replacement.prefix(replacement.len() - 1)
                    + 1;
                self.document.root = self.document.root.with_children(
                    self.document
                        .root
                        .children
                        .splice(list_index..list_index + 1, &replacement),
                );
                self.selection = Selection::caret(position);
                self.record(before, 8192);
                return Ok(());
            }
            let before = self.checkpoint();
            let left_item = parent.with_children(Sequence::one(resolved.node.with_children(left)));
            let right_item = Node::fresh(
                "listItem",
                Sequence::one(Node::fresh("paragraph", right)).concat(&parent.children.split(1).1),
            );
            self.document.replace_node(
                list_path,
                list.with_children(list.children.splice(
                    item_index..item_index + 1,
                    &Sequence::from_items([left_item, right_item]),
                )),
            );
            self.selection = Selection::caret(self.selection.head + 4);
            self.record(before, 8192);
            return Ok(());
        }
        let before = self.checkpoint();
        let right = Node::fresh("paragraph", right);
        self.document.replace_node(
            path,
            parent.with_children(parent.children.splice(
                index..index + 1,
                &Sequence::from_items([resolved.node.with_children(left), right]),
            )),
        );
        self.selection = Selection::caret(self.selection.head + 2);
        self.last_mapping = Some(Mapping {
            old: before.selection.range(),
            inserted: 2,
        });
        self.record(before, 8192);
        Ok(())
    }

    pub fn wrap_block(&mut self, kind: &str) -> EditResult<()> {
        self.ensure_editable()?;
        let resolved = self.document.resolve(self.selection.head)?;
        let (replacement, depth) = match kind {
            "blockquote" => (Node::fresh(kind, Sequence::one(resolved.node.clone())), 1),
            "bulletList" | "orderedList" => {
                if resolved.node.kind() != "paragraph" {
                    return Err("Lists require a paragraph".into());
                }
                (
                    Node::fresh(
                        kind,
                        Sequence::one(Node::fresh(
                            "listItem",
                            Sequence::one(resolved.node.clone()),
                        )),
                    ),
                    2,
                )
            }
            _ => return Err("Unsupported wrapping command".into()),
        };
        let before = self.checkpoint();
        self.document.replace_node(&resolved.path, replacement);
        self.selection.anchor += depth;
        self.selection.head += depth;
        self.record(before, 8192);
        Ok(())
    }

    pub fn indent_list(&mut self, outdent: bool) -> EditResult<()> {
        self.ensure_editable()?;
        let resolved = self.document.resolve(self.selection.head)?;
        if resolved.path.len() < 3 {
            return Err("Selection is not in a list".into());
        }
        let item_path = &resolved.path[..resolved.path.len() - 1];
        let list_path = &item_path[..item_path.len() - 1];
        let item = self.document.node(item_path).expect("item");
        let list = self.document.node(list_path).expect("list");
        if item.kind() != "listItem" || !matches!(list.kind(), "bulletList" | "orderedList") {
            return Err("Only standard list items support native indentation currently".into());
        }
        let item_index = *item_path.last().expect("item");
        let before = self.checkpoint();
        let paragraph_offset = item
            .children
            .prefix(*resolved.path.last().expect("paragraph"))
            + 2
            + resolved.offset;
        let caret;
        if !outdent {
            if item_index == 0 {
                return Err("The first list item has no preceding item to nest under".into());
            }
            let previous = list.children.get(item_index - 1).expect("previous");
            let nested_index = previous.children.len().saturating_sub(1);
            let existing = previous
                .children
                .get(nested_index)
                .filter(|node| node.kind() == list.kind());
            let previous_start = self.document.position_at_path(list_path).expect("list")
                + 1
                + list.children.prefix(item_index - 1);
            caret = previous_start
                + 1
                + if let Some(nested) = existing {
                    previous.children.prefix(nested_index) + 1 + nested.children.units()
                } else {
                    previous.children.units() + 1
                }
                + paragraph_offset;
            let nested = if let Some(nested) = existing {
                nested.with_children(nested.children.concat(&Sequence::one(item.clone())))
            } else {
                Node::fresh(list.kind(), Sequence::one(item.clone()))
            };
            let previous = previous.with_children(if existing.is_some() {
                previous
                    .children
                    .splice(nested_index..nested_index + 1, &Sequence::one(nested))
            } else {
                previous.children.concat(&Sequence::one(nested))
            });
            self.document.replace_node(
                list_path,
                list.with_children(
                    list.children
                        .splice(item_index - 1..item_index + 1, &Sequence::one(previous)),
                ),
            );
        } else {
            if list_path.len() < 3 || item_index + 1 != list.children.len() {
                return Err("Outdent currently requires the last item of a nested list".into());
            }
            let parent_item_path = &list_path[..list_path.len() - 1];
            let outer_path = &parent_item_path[..parent_item_path.len() - 1];
            let parent_item = self.document.node(parent_item_path).expect("parent item");
            let outer = self.document.node(outer_path).expect("outer list");
            if parent_item.kind() != "listItem" {
                return Err("Cannot outdent across this container".into());
            }
            let nested_index = *list_path.last().expect("nested list");
            let parent_index = *parent_item_path.last().expect("parent item");
            let kept = list.children.split(item_index).0;
            let nested = if kept.len() == 0 {
                Sequence::default()
            } else {
                Sequence::one(list.with_children(kept))
            };
            let parent_item = parent_item.with_children(
                parent_item
                    .children
                    .splice(nested_index..nested_index + 1, &nested),
            );
            caret = self
                .document
                .position_at_path(outer_path)
                .expect("outer list")
                + 1
                + outer.children.prefix(parent_index)
                + super::sequence::Measured::units(&parent_item)
                + paragraph_offset;
            self.document.replace_node(
                outer_path,
                outer.with_children(outer.children.splice(
                    parent_index..parent_index + 1,
                    &Sequence::from_items([parent_item, item]),
                )),
            );
        }
        self.selection = Selection::caret(caret);
        self.record(before, 16384);
        Ok(())
    }

    pub fn update_node_attrs(
        &mut self,
        path: &[usize],
        attrs: Map<String, Value>,
    ) -> EditResult<()> {
        self.ensure_editable()?;
        let node = self.document.node(path).ok_or("Node no longer exists")?;
        if !matches!(node.kind(), "image" | "taskItem") {
            return Err("Unsupported metadata command".into());
        }
        if attrs
            .keys()
            .any(|key| !matches!(key.as_str(), "editorWidth" | "status" | "checked"))
        {
            return Err(
                "Identity and attachment paths cannot be changed through metadata commands".into(),
            );
        }
        let mut merged = node.attrs().cloned().unwrap_or_default();
        for (key, value) in attrs {
            if key == "editorWidth" {
                let width = value.as_f64().ok_or("Image width must be numeric")?;
                merged.insert(key, json!(width.clamp(15., 100.)));
            } else {
                merged.insert(key, value);
            }
        }
        let mut fields = (*node.fields).clone();
        fields.insert("attrs".into(), merged.into());
        let before = self.checkpoint();
        self.document.replace_node(path, node.with_fields(fields));
        self.record(before, 4096);
        Ok(())
    }

    pub fn move_grapheme(&mut self, forward: bool, extend: bool) -> EditResult<()> {
        let range = self.selection.range();
        if !extend && !range.is_empty() {
            self.selection = Selection::caret(if forward { range.end } else { range.start });
            return Ok(());
        }
        let resolved = self.document.resolve(self.selection.head)?;
        // Only the bounded neighborhood is materialized, even in a multi-megabyte paragraph.
        let start = resolved.offset.saturating_sub(128);
        let end = (resolved.offset + 128).min(resolved.node.children.units());
        let mut safe_start = start;
        let mut safe_end = end;
        let (left, _) = loop {
            if let Ok(parts) = split_inline(&resolved.node.children, safe_end) {
                break parts;
            }
            safe_end -= 1;
        };
        let (_, slice) = loop {
            if let Ok(parts) = split_inline(&left, safe_start) {
                break parts;
            }
            safe_start += 1;
        };
        let text = inline_text(&slice);
        let cursor = resolved.offset - safe_start;
        let mut position = if forward {
            resolved.node.children.units()
        } else {
            0
        };
        let mut units = 0;
        for grapheme in text.graphemes(true) {
            if forward && units > cursor {
                position = safe_start + units;
                break;
            }
            if !forward && units >= cursor {
                break;
            }
            if !forward {
                position = safe_start + units;
            }
            units += grapheme.encode_utf16().count();
        }
        if forward && position == resolved.node.children.units() && units > cursor {
            position = safe_start + units;
        }
        if (!forward && position == safe_start && safe_start > 0)
            || (forward && position >= safe_end && safe_end < resolved.node.children.units())
        {
            return Err("Grapheme exceeds the native navigation window; selection retained".into());
        }
        if (forward && resolved.offset == resolved.node.children.units())
            || (!forward && resolved.offset == 0)
        {
            let candidate = if forward {
                self.selection.head + 2
            } else {
                self.selection.head.saturating_sub(2)
            };
            if self.document.resolve(candidate).is_ok() {
                position = candidate;
            } else {
                return Ok(());
            }
        } else {
            position += resolved.start;
        }
        self.selection.head = position;
        if !extend {
            self.selection.anchor = position;
        }
        self.stored_marks = None;
        Ok(())
    }

    pub fn delete(&mut self, forward: bool) -> EditResult<()> {
        if self.selection.is_empty() {
            self.move_grapheme(forward, true)?;
        }
        self.replace(self.selection.range(), "")
    }
}

fn toggle_marks(marks: &mut Vec<Value>, kind: &str, enabled: Option<bool>) {
    let active = marks.iter().any(|mark| mark["type"] == kind);
    let enabled = enabled.unwrap_or(!active);
    if enabled && kind == "code" {
        marks.clear();
    }
    if enabled && kind != "code" {
        marks.retain(|mark| mark["type"] != "code");
    }
    marks.retain(|mark| mark["type"] != kind);
    if enabled {
        marks.push(json!({"type": kind}));
    }
}
