//! Tool results as content blocks: an image a tool returns reaches the model's conversation
//! and the log, and a result of text is recorded as it always was. Native plugins only: no
//! WebAssembly, network or LLM.

use std::sync::Arc;

use corduroy_harness::core::{
    Content, Context, Pattern, PluginError, TOOLS, ToolError, ToolOutput, ToolSpec, Tools,
    plugin_fn, tool_fn,
};
use corduroy_harness::oi_kit_provider_types::conversation::message::{Message, MessageContent};
use corduroy_harness::scripted::{ScriptedProvider, call, say};
use corduroy_harness::{Harness, HarnessError, Session, Status};
use rmcp::model::{CallToolRequestParams, ContentBlock};
use serde_json::{Value, json};

/// The eight bytes every PNG file starts with. Not text, and not valid UTF-8.
const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

/// `PNG` in base64, worked out by hand so the test does not lean on the code it checks.
const PNG_BASE64: &str = "iVBORw0KGgo=";

/// A native plugin contributing `snap`, which returns a caption and an image; `label`, which
/// returns text; `lines`, which returns two blocks of text; `nothing`, which returns no
/// blocks at all; and `broken`, which fails.
fn camera(ctx: &mut Context<'_>) -> Result<(), PluginError> {
    let spec = |name: &str| ToolSpec::new(name, "", r#"{"type":"object"}"#).read_only(true);
    let tools = ctx.service::<Tools>(TOOLS)?;
    let snap = tool_fn(spec("snap"), |_| {
        Ok(vec![
            Content::text("the view"),
            Content::image("image/png", PNG),
        ])
    });
    tools.register(ctx, "snap", Arc::new(snap))?;
    let label = tool_fn(spec("label"), |_| Ok("a view"));
    tools.register(ctx, "label", Arc::new(label))?;
    let lines = tool_fn(spec("lines"), |_| {
        Ok(vec![Content::text("one"), Content::text("two")])
    });
    tools.register(ctx, "lines", Arc::new(lines))?;
    let nothing = tool_fn(spec("nothing"), |_| Ok(ToolOutput::default()));
    tools.register(ctx, "nothing", Arc::new(nothing))?;
    let broken = tool_fn(spec("broken"), |_| {
        Err::<ToolOutput, _>(ToolError::Failed("no film".into()))
    });
    tools.register(ctx, "broken", Arc::new(broken))?;
    Ok(())
}

fn harness(replies: impl IntoIterator<Item = Message>) -> Result<Harness, HarnessError> {
    let harness = Harness::new(Arc::new(ScriptedProvider::new(replies)))?;
    harness.plugin(plugin_fn("camera", &[TOOLS], camera));
    Ok(harness)
}

/// The blocks of each tool result in the conversation, in order.
fn results(session: &Session) -> Vec<Vec<ContentBlock>> {
    let contents = session.conversation().messages().iter();
    contents
        .flat_map(|message| &message.content)
        .filter_map(|content| match content {
            MessageContent::ToolResponse(response) => response.tool_result.as_ref().ok(),
            _ => None,
        })
        .map(|result| result.content.clone())
        .collect()
}

/// The payload of each `tool/result` event, parsed.
fn recorded(harness: &Harness) -> Vec<Value> {
    let events = harness.events().matching(&Pattern::new("tool/result"));
    events
        .iter()
        .filter_map(|event| serde_json::from_str(&event.payload).ok())
        .collect()
}

#[tokio::test]
async fn an_image_a_tool_returns_reaches_the_model_and_the_log() {
    let harness = harness([call("c1", "snap", &json!({})), say("a fine view")]).unwrap();

    let outcome = harness.run("take a picture").await.unwrap();

    assert_eq!(
        outcome.status,
        Status::Finished {
            answer: "a fine view".into()
        }
    );
    // The model's conversation holds both blocks, in order, the image's bytes in base64.
    assert_eq!(
        results(&outcome.session),
        [[
            ContentBlock::text("the view"),
            ContentBlock::image(PNG_BASE64, "image/png"),
        ]]
    );
    // The log has the result's text as `output`, as for any tool, and its blocks beside it.
    assert_eq!(
        recorded(&harness),
        [json!({
            "id": "c1",
            "tool": "snap",
            "ok": true,
            "output": "the view\n[image: image/png, 8 bytes]",
            "content": [
                {"type": "text", "text": "the view"},
                {"type": "image", "mime_type": "image/png", "data": PNG_BASE64},
            ],
        })]
    );
}

#[tokio::test]
async fn only_a_result_that_is_more_than_one_text_is_recorded_block_by_block() {
    // Four calls in one message from the model.
    let calls = [
        ("c1", "label"),
        ("c2", "lines"),
        ("c3", "nothing"),
        ("c4", "broken"),
    ];
    let asked = calls
        .into_iter()
        .fold(Message::assistant(), |message, (id, tool)| {
            message.with_tool_request(id, Ok(CallToolRequestParams::new(tool)))
        });
    let harness = harness([asked, say("done")]).unwrap();

    let outcome = harness.run("label it").await.unwrap();

    let recorded = recorded(&harness);
    // Text is the whole of its record, exactly as before there were blocks.
    assert_eq!(
        recorded[0],
        json!({"id": "c1", "tool": "label", "ok": true, "output": "a view"})
    );
    // Blocks that `output` alone would not give back are there as `content`.
    assert_eq!(
        recorded[1],
        json!({
            "id": "c2", "tool": "lines", "ok": true, "output": "one\ntwo",
            "content": [{"type": "text", "text": "one"}, {"type": "text", "text": "two"}],
        })
    );
    assert_eq!(
        recorded[2],
        json!({"id": "c3", "tool": "nothing", "ok": true, "output": "", "content": []})
    );
    // A failure is its text.
    assert_eq!(
        recorded[3],
        json!({"id": "c4", "tool": "broken", "ok": false, "output": "tool failed: no film"})
    );
    assert_eq!(
        results(&outcome.session),
        [
            vec![ContentBlock::text("a view")],
            vec![ContentBlock::text("one"), ContentBlock::text("two")],
            vec![],
            vec![ContentBlock::text("tool failed: no film")],
        ]
    );
}
