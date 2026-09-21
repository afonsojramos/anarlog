use std::{sync::Arc, time::Instant};

use serde_json::{Value, json};

use super::{
    clipboard,
    document::{Document, Text, utf8},
    menu::{MentionCandidate, MentionResults, MentionTarget},
    model::{EditorModel, Mapping, Selection},
    surface::ProjectedBlock,
};

fn model(text: &str) -> EditorModel {
    EditorModel::new(Document::parse(serde_json::json!({
        "type": "doc", "content": [{"type": "paragraph", "content": [{"type": "text", "text": text}]}]
    }).to_string().into()).unwrap())
}

#[test]
fn composition_is_one_undo_and_cancel_restores_selection() {
    let mut editor = model("hello");
    editor.select(Selection::caret(6));
    editor.compose(None, "に", 1..1).unwrap();
    editor.compose(None, "日本", 0..2).unwrap();
    assert_eq!(editor.selection, Selection { anchor: 6, head: 8 });
    editor.commit_composition();
    editor.undo().unwrap();
    assert_eq!(
        editor.document.serialize().unwrap().as_ref(),
        model("hello").document.serialize().unwrap().as_ref()
    );
    editor.redo().unwrap();
    assert!(editor.document.serialize().unwrap().contains("hello日本"));
    editor.compose(None, "語", 1..1).unwrap();
    editor.cancel_composition();
    assert!(editor.document.serialize().unwrap().contains("hello日本"));
}

#[test]
fn schema_roundtrip_preserves_unknown_data_after_neighbor_edit() {
    let paragraph = json!({"type":"paragraph","attrs":{"future":9},"content":[
        {"type":"text","text":"edit","vendor":{"x":[1,2]},"marks":[
            {"type":"bold"},{"type":"italic"},{"type":"underline"},{"type":"strike"},
            {"type":"highlight"},{"type":"future-mark","attrs":{"opaque":true}}
        ]},
        {"type":"hardBreak"},{"type":"mention-@","attrs":{"id":"person","type":"human","label":"名字"}},
        {"type":"appLink","attrs":{"provider":"github","resourceId":"123","label":"issue"}}
    ]});
    let node_types = [
        "horizontalRule",
        "image",
        "fileAttachment",
        "session",
        "clip",
    ];
    let mut blocks = vec![paragraph.clone()];
    for kind in node_types {
        blocks
            .push(json!({"type":kind,"attrs":{"attachmentId":"portable","future": {"keep":true}}}));
    }
    for level in 1..=6 {
        blocks.push(json!({"type":"heading","attrs":{"level":level},"content":[{"type":"text","text":"Title"}]}));
    }
    blocks.push(json!({"type":"codeBlock","content":[{"type":"text","text":"code\n日本"}]}));
    blocks.push(json!({"type":"blockquote","content":[paragraph.clone()]}));
    blocks.push(json!({"type":"orderedList","attrs":{"start":7},"content":[{"type":"listItem","content":[paragraph.clone(),{"type":"bulletList","content":[{"type":"listItem","content":[paragraph.clone()]}]}]}]}));
    blocks.push(json!({"type":"taskList","content":[{"type":"taskItem","attrs":{"taskId":"task","taskItemId":"item","status":"pending","checked":false},"content":[paragraph.clone()]}]}));
    blocks.push(json!({"type":"table","content":[{"type":"tableRow","content":[{"type":"tableHeader","attrs":{"colspan":2,"rowspan":1,"colwidth":[100,140]},"content":[paragraph.clone()]},{"type":"tableCell","attrs":{"colspan":1,"rowspan":2,"colwidth":null},"content":[paragraph]}]}]}));
    blocks.push(json!({"type":"opaque-future","attrs":{"keep":"exact"},"content":[{"type":"text","text":"hidden extension"}]}));
    let original = json!({"type":"doc","futureRoot":[false,42],"content":blocks});
    let bytes: Arc<str> = format!(" \n{}\n", original).into();
    let document = Document::parse(bytes.clone()).unwrap();
    assert!(Arc::ptr_eq(&bytes, &document.serialize().unwrap()));
    let mut editor = EditorModel::new(document);
    editor.replace(2..2, "X").unwrap();
    let changed: Value = serde_json::from_str(&editor.document.serialize().unwrap()).unwrap();
    assert_eq!(changed["futureRoot"], original["futureRoot"]);
    assert_eq!(
        changed["content"][0]["attrs"],
        original["content"][0]["attrs"]
    );
    assert_eq!(
        changed["content"][0]["content"][0]["vendor"],
        original["content"][0]["content"][0]["vendor"]
    );
    assert_eq!(
        changed["content"][0]["content"][0]["marks"],
        original["content"][0]["content"][0]["marks"]
    );
    for index in 1..original["content"].as_array().unwrap().len() {
        assert_eq!(changed["content"][index], original["content"][index]);
    }
}

#[test]
fn malformed_document_never_becomes_an_editable_blank() {
    for body in [
        "oops",
        "null",
        "{}",
        r#"{"type":"doc","content":[]}"#,
        r#"{"type":"doc","content":[{"type":"paragraph","content":[{"type":"heading"}]}]}"#,
        r#"{"type":"doc","content":[{"type":"paragraph","marks":false}]}"#,
    ] {
        assert!(Document::parse(body.into()).is_err(), "{body}");
    }
}

#[test]
fn utf16_boundaries_and_large_text_pieces() {
    assert_eq!(utf8("a😀文", 3).unwrap(), 5);
    assert!(utf8("a😀文", 2).is_err());
    let text = Text::new(&"😀é漢".repeat(20_000));
    let original = text.as_string();
    let (a, b) = text.split(40_002).unwrap();
    assert_eq!(a.concat(&b).as_string(), original);
    assert!(text.split(40_001).is_err());
}

#[test]
fn grapheme_navigation_handles_combining_emoji_rtl_and_cjk() {
    for cluster in ["e\u{301}", "👨‍👩‍👧‍👦", "🇯🇵", "😀", "ש\u{5b8}", "漢"]
    {
        let mut editor = model(&format!("a{cluster}b"));
        editor.select(Selection::caret(2));
        editor.move_grapheme(true, false).unwrap();
        assert_eq!(
            editor.selection.head,
            2 + cluster.encode_utf16().count(),
            "{cluster}"
        );
        editor.delete(false).unwrap();
        assert!(editor.document.serialize().unwrap().contains("\"ab\""));
        editor.undo().unwrap();
        assert!(editor.document.serialize().unwrap().contains(cluster));
    }
}

#[test]
fn reversed_multiblock_selection_and_undo_preserve_fields() {
    let doc = json!({"type":"doc","content":[{"type":"paragraph","attrs":{"x":1},"content":[{"type":"text","text":"abc"}]},{"type":"paragraph","attrs":{"x":2},"content":[{"type":"text","text":"def"}]}]});
    let mut editor = EditorModel::new(Document::parse(doc.to_string().into()).unwrap());
    editor.select(Selection { anchor: 8, head: 2 });
    assert_eq!(
        clipboard::copy(&editor.document, editor.selection)
            .unwrap()
            .text,
        "bc\n\nde"
    );
    editor.replace(editor.selection.range(), "X").unwrap();
    assert_eq!(
        editor.document.root.value()["content"][0]["content"][0]["text"],
        "aXf"
    );
    editor.undo().unwrap();
    assert_eq!(editor.document.root.value(), doc);
    assert_eq!(editor.selection, Selection { anchor: 8, head: 2 });
}

#[test]
fn link_is_noninclusive_but_unknown_marks_survive_typing() {
    let doc = json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"web","marks":[{"type":"link","attrs":{"href":"https://example.com","target":"_blank"}},{"type":"future"}]}]}]});
    let mut editor = EditorModel::new(Document::parse(doc.to_string().into()).unwrap());
    editor.replace(4..4, "!").unwrap();
    let value = editor.document.root.value();
    assert_eq!(
        value["content"][0]["content"][1]["marks"],
        json!([{"type":"future"}])
    );
    assert!(clipboard::openable_link("HTTPS://example.com"));
    for link in [
        "javascript:alert(1)",
        "file:///etc/passwd",
        "https://",
        "http://a\nb",
    ] {
        assert!(!clipboard::openable_link(link));
    }
}

#[test]
fn rich_clipboard_preserves_marks_and_portable_attachment_metadata() {
    let original = json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"bold","marks":[{"type":"bold"}]}]},{"type":"image","attrs":{"attachmentId":"img-1","alt":"[a](b)","editorWidth":80}}]});
    let document = Document::parse(original.to_string().into()).unwrap();
    let payload = clipboard::copy(
        &document,
        Selection {
            anchor: 0,
            head: document.units(),
        },
    )
    .unwrap();
    assert!(payload.text.contains("![\\[a\\]\\(b\\)]()"));
    let (fragment, open) = clipboard::parse_slice(&payload.metadata).unwrap();
    assert!(!open);
    let mut editor = model("replace");
    editor.select(Selection {
        anchor: 0,
        head: editor.document.units(),
    });
    editor.insert_slice(fragment, open).unwrap();
    assert_eq!(editor.document.root.value(), original);
    assert!(!editor.document.serialize().unwrap().contains("file://"));
}

#[test]
fn rich_inline_paste_inherits_slice_marks_without_losing_surroundings() {
    let mut source = model("bold");
    source.select(Selection { anchor: 1, head: 5 });
    source.toggle_mark("bold").unwrap();
    let payload = clipboard::copy(&source.document, source.selection).unwrap();
    let mut destination = model("ab");
    destination.select(Selection::caret(2));
    let (fragment, open) = clipboard::parse_slice(&payload.metadata).unwrap();
    destination.insert_slice(fragment, open).unwrap();
    let value = destination.document.root.value();
    assert_eq!(value["content"][0]["content"][1]["text"], "bold");
    assert_eq!(
        value["content"][0]["content"][1]["marks"][0]["type"],
        "bold"
    );
}

#[test]
fn list_split_indent_and_outdent_keep_identity_and_ordered_start() {
    let doc = json!({"type":"doc","content":[{"type":"orderedList","attrs":{"start":7,"future":true},"content":[{"type":"listItem","content":[{"type":"paragraph","content":[{"type":"text","text":"one"}]}]},{"type":"listItem","content":[{"type":"paragraph","content":[{"type":"text","text":"two"}]}]}]}]});
    let mut editor = EditorModel::new(Document::parse(doc.to_string().into()).unwrap());
    editor.select(Selection::caret(10));
    let id = editor.document.resolve(10).unwrap().node.id;
    editor.indent_list(false).unwrap();
    assert_eq!(
        editor
            .document
            .resolve(editor.selection.head)
            .unwrap()
            .node
            .id,
        id
    );
    editor.indent_list(true).unwrap();
    assert_eq!(editor.document.root.value(), doc);
    assert_eq!(editor.selection.head, 10);
    editor.split_block().unwrap();
    assert_eq!(
        editor.document.root.value()["content"][0]["attrs"]["start"],
        7
    );
    assert_eq!(
        editor.document.root.children.get(0).unwrap().children.len(),
        3
    );
}

#[test]
fn code_block_exit_and_readonly_protection() {
    let mut editor = model("code\n");
    editor.set_block("codeBlock", None).unwrap();
    editor.select(Selection::caret(6));
    editor.split_block().unwrap();
    assert_eq!(editor.document.root.children.len(), 2);
    assert_eq!(
        editor
            .document
            .resolve(editor.selection.head)
            .unwrap()
            .node
            .kind(),
        "paragraph"
    );
    editor.read_only = true;
    assert!(editor.replace(editor.selection.range(), "no").is_err());
    assert!(editor.undo().is_err());
    editor.select(Selection { anchor: 4, head: 1 });
    assert_eq!(editor.selection.range(), 1..4);
}

#[test]
fn mention_generation_rejects_stale_errors_and_dismissed_results() {
    let mut mentions = MentionResults::default();
    let stale = mentions.begin();
    let current = mentions.begin();
    assert!(!mentions.resolve(stale, Err("old".into())));
    let results = (0..10)
        .map(|n| MentionCandidate {
            label: n.to_string(),
            target: MentionTarget::Human(n.to_string()),
        })
        .collect();
    assert!(mentions.resolve(current, Ok(results)));
    assert_eq!(mentions.candidates.len(), 5);
    mentions.dismiss();
    assert!(!mentions.resolve(current, Err("late".into())));
    assert!(mentions.error.is_none());
}

#[test]
fn attachment_resize_preserves_identity_and_clamps_width() {
    let mut editor = EditorModel::new(Document::parse(json!({"type":"doc","content":[{"type":"image","attrs":{"attachmentId":"stable","editorWidth":80,"future":true}}]}).to_string().into()).unwrap());
    editor
        .update_node_attrs(
            &[0],
            serde_json::Map::from_iter([("editorWidth".into(), json!(130))]),
        )
        .unwrap();
    let attrs = editor.document.root.value()["content"][0]["attrs"].clone();
    assert_eq!(
        attrs,
        json!({"attachmentId":"stable","editorWidth":100.0,"future":true})
    );
    assert!(
        editor
            .update_node_attrs(
                &[0],
                serde_json::Map::from_iter([("path".into(), json!("/home/user/private"))])
            )
            .is_err()
    );
}

#[test]
fn projection_utf16_mapping_and_unknown_node_visibility() {
    let editor = model("a😀漢");
    let projected = ProjectedBlock::build(editor.document.root.children.get(0).unwrap().clone());
    let row = &projected.rows[0];
    assert_eq!(row.position(5), 4);
    assert_eq!(row.byte(4), Some(5));
    let document = Document::parse(r#"{"type":"doc","content":[{"type":"future-node","content":[{"type":"text","text":"retained"}]}]}"#.into()).unwrap();
    let projected = ProjectedBlock::build(document.root.children.get(0).unwrap().clone());
    assert!(projected.rows[0].text.contains("Unsupported future-node"));
}

#[test]
fn comment_range_mapping_drops_deleted_anchors() {
    let map = Mapping {
        old: 4..7,
        inserted: 0,
    };
    assert_eq!(map.map_anchor(4..7), None);
    assert_eq!(map.map_anchor(8..12), Some(5..9));
    assert_eq!(map.map_anchor(1..3), Some(1..3));
}

#[test]
fn native_text_range_uses_pm_structural_utf16_units() {
    let document = Document::parse(json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"😀"}]},{"type":"paragraph","content":[{"type":"text","text":"漢"}]}]}).to_string().into()).unwrap();
    assert_eq!(
        clipboard::text_for_range(&document, 0..document.units()).unwrap(),
        "\n😀\n\n漢\n"
    );
    assert!(clipboard::text_for_range(&document, 1..2).is_err());
}

#[test]
fn punctuation_respects_code_context_and_keeps_one_history_step() {
    let mut editor = model("a-");
    editor.select(Selection::caret(3));
    editor.type_text(3..3, "-").unwrap();
    assert_eq!(
        editor.document.root.value()["content"][0]["content"][0]["text"],
        "a—"
    );
    editor.undo().unwrap();
    assert_eq!(
        editor.document.root.value()["content"][0]["content"][0]["text"],
        "a-"
    );
    editor.set_block("codeBlock", None).unwrap();
    editor.type_text(3..3, "-").unwrap();
    assert_eq!(
        editor.document.root.value()["content"][0]["content"][0]["text"],
        "a--"
    );
    let mut editor = model("..");
    editor.type_text(3..3, ".").unwrap();
    assert_eq!(
        editor.document.root.value()["content"][0]["content"][0]["text"],
        "…"
    );
}

#[test]
fn atomic_mention_and_attachment_insertions_are_portable_and_undoable() {
    let mut editor = model("@Jo");
    editor.select(Selection { anchor: 1, head: 4 });
    editor
        .insert_inline_atom(
            "mention-@",
            serde_json::Map::from_iter([
                ("id".into(), json!("person")),
                ("type".into(), json!("human")),
                ("label".into(), json!("John")),
            ]),
        )
        .unwrap();
    assert_eq!(
        editor.document.root.value()["content"][0]["content"][0]["attrs"]["id"],
        "person"
    );
    editor
        .insert_block_atom(
            "image",
            serde_json::Map::from_iter([("attachmentId".into(), json!("attachment"))]),
        )
        .unwrap();
    assert_eq!(
        editor.document.root.value()["content"][1]["attrs"]["attachmentId"],
        "attachment"
    );
    assert_eq!(editor.document.root.children.len(), 3);
    editor.undo().unwrap();
    editor.undo().unwrap();
    assert_eq!(
        editor.document.root.value()["content"][0]["content"][0]["text"],
        "@Jo"
    );
    editor.select(Selection::caret(4));
    assert!(
        editor
            .insert_block_atom(
                "image",
                serde_json::Map::from_iter([("src".into(), json!("file:///private"))])
            )
            .is_err()
    );
}

#[test]
fn failed_structural_transaction_restores_text_selection_and_history() {
    let mut editor = model("/quote");
    editor.select(Selection::caret(7));
    let before = editor.document.serialize().unwrap();
    let revision = editor.revision;
    let result = editor.transaction(|model| {
        model.replace(1..7, "")?;
        Err("Service cannot allocate task identity".into())
    });
    assert!(result.is_err());
    assert_eq!(editor.revision, revision);
    assert_eq!(editor.document.serialize().unwrap(), before);
    assert_eq!(editor.selection, Selection::caret(7));
    editor
        .transaction(|model| {
            model.replace(1..7, "")?;
            model.wrap_block("blockquote")
        })
        .unwrap();
    editor.undo().unwrap();
    assert_eq!(editor.document.serialize().unwrap(), before);
}

#[test]
fn saved_attachments_match_portable_url_normalization() {
    let value = json!({"type":"doc","content":[
        {"type":"image","attrs":{"attachmentId":"id","src":"asset://localhost/private/file.png","path":"/private/file.png","editorWidth":80,"future":true}},
        {"type":"future-node","attrs":{"path":"opaque path preserved"}}
    ]});
    let document = Document::parse(value.to_string().into()).unwrap();
    let saved: Value = serde_json::from_str(&document.serialize_for_save().unwrap()).unwrap();
    assert_eq!(
        saved["content"][0]["attrs"],
        json!({"attachmentId":"id","editorWidth":80,"future":true})
    );
    assert_eq!(saved["content"][1], value["content"][1]);
    assert_eq!(document.root.value(), value);
    let document = Document::parse(json!({"type":"doc","content":[{"type":"image","attrs":{"src":"file:///private/file.png"}}]}).to_string().into()).unwrap();
    assert!(document.serialize_for_save().is_err());
}

#[test]
fn unicode_edits_match_a_flat_text_oracle() {
    let mut editor = model("seed😀");
    let mut expected = String::from("seed😀");
    let mut seed = 27u64;
    for step in 0..500 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let mut boundaries: Vec<_> = expected.char_indices().map(|(index, _)| index).collect();
        boundaries.push(expected.len());
        let a = (seed as usize) % boundaries.len();
        let b = ((seed >> 32) as usize) % boundaries.len();
        let start = boundaries[a.min(b)];
        let end = boundaries[a.max(b)];
        let insert = ["é", "👩‍💻", "漢字", "שלום", "", "x"][step % 6];
        let from = 1 + expected[..start].encode_utf16().count();
        let to = 1 + expected[..end].encode_utf16().count();
        editor.replace(from..to, insert).unwrap();
        expected.replace_range(start..end, insert);
        assert_eq!(
            super::document::inline_text(&editor.document.root.children.get(0).unwrap().children),
            expected
        );
    }
}

#[test]
#[ignore = "Run explicitly with --release --ignored --nocapture; editor model microbenchmark only"]
fn measured_editor_workload() {
    for count in [100, 1000, 10_000] {
        let fixture = json!({"type":"doc","content": (0..count).map(|i| json!({"type":"paragraph","content":[{"type":"text","text":format!("Block {i}: 中文 😀 é")}]})).collect::<Vec<_>>()});
        let bytes = fixture.to_string();
        for trial in 0..6 {
            let mut editor = EditorModel::new(Document::parse(bytes.clone().into()).unwrap());
            let before = editor
                .document
                .root
                .children
                .get(count - 1)
                .unwrap()
                .clone();
            let start = Instant::now();
            for _ in 0..2000 {
                editor.replace(1..1, "x").unwrap();
            }
            let edit_ns = start.elapsed().as_nanos();
            assert!(Arc::ptr_eq(
                &before,
                editor.document.root.children.get(count - 1).unwrap()
            ));
            let start = Instant::now();
            for _ in 0..2000 {
                editor.undo().unwrap();
            }
            let undo_ns = start.elapsed().as_nanos();
            assert_eq!(editor.document.root.value(), fixture);
            println!(
                "model_microbench blocks={count} trial={trial} warmup={} source_bytes={} edits=2000 edit_total_ns={edit_ns} undo_total_ns={undo_ns} unaffected_node_shared=true",
                trial == 0,
                bytes.len()
            );
        }
    }
}
