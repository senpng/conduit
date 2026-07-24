//! Client-facing Responses API SSE stream encoder.

use std::collections::BTreeMap;

use conduit_ir::canonical::{
    BlockDelta, BlockKind, CanonicalChunk, FinishReason, Usage,
};
use serde_json::{json, Value};

use super::helpers::{
    output_text_part, sse, text_item_json, tool_item_json, usage_json,
};

/// Stateful encoder for client-facing Responses API SSE streams.
///
/// The Responses protocol is an item lifecycle, not merely a sequence of text
/// deltas. In particular, SDKs reconstruct the final response from the
/// `response.completed.response.output` array. Keep that array present even
/// for an empty response, and emit the message/tool item lifecycle around
/// deltas so strict Responses clients can consume the stream.
#[derive(Debug)]
pub struct ResponsesStreamEncoder {
    response_id: String,
    model: String,
    store: bool,
    started: bool,
    next_output_index: u32,
    text_item: Option<usize>,
    tools: BTreeMap<u32, usize>,
    output: Vec<StreamOutputItem>,
    completed: bool,
}

#[derive(Debug)]
enum StreamOutputItem {
    Text {
        output_index: u32,
        id: String,
        text: String,
        started: bool,
        closed: bool,
    },
    Tool {
        output_index: u32,
        id: String,
        name: String,
        arguments: String,
        started: bool,
        closed: bool,
    },
}

impl ResponsesStreamEncoder {
    pub fn new(response_id: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new_with_store(response_id, model, true)
    }

    pub fn new_with_store(
        response_id: impl Into<String>,
        model: impl Into<String>,
        store: bool,
    ) -> Self {
        Self {
            response_id: response_id.into(),
            model: model.into(),
            store,
            started: false,
            next_output_index: 0,
            text_item: None,
            tools: BTreeMap::new(),
            output: Vec::new(),
            completed: false,
        }
    }

    /// Emit the initial Responses lifecycle events. Calling this repeatedly is
    /// safe; only the first call produces frames.
    pub fn start(&mut self) -> Vec<String> {
        if self.started {
            return vec![];
        }
        self.started = true;
        let response = self.response_json("in_progress", vec![], None);
        vec![
            sse(json!({"type": "response.created", "response": response.clone()})),
            sse(json!({"type": "response.in_progress", "response": response})),
        ]
    }

    /// Encode one canonical chunk into zero or more Responses SSE frames.
    pub fn push(&mut self, chunk: &CanonicalChunk) -> Vec<String> {
        if self.completed {
            return vec![];
        }

        let mut out = self.start();

        if let Some(BlockDelta::TextDelta { text }) = &chunk.delta {
            if !text.is_empty() {
                let item = self.ensure_text_item(&mut out);
                let (output_index, item_id) = match &mut self.output[item] {
                    StreamOutputItem::Text {
                        output_index,
                        id,
                        text: accumulated,
                        ..
                    } => {
                        accumulated.push_str(text);
                        (*output_index, id.clone())
                    }
                    StreamOutputItem::Tool { .. } => unreachable!("text item has text shape"),
                };
                out.push(sse(json!({
                    "type": "response.output_text.delta",
                    "output_index": output_index,
                    "content_index": 0,
                    "item_id": item_id,
                    "delta": text,
                })));
            }
        }

        let is_tool = matches!(chunk.block_kind, Some(BlockKind::ToolUse))
            || chunk.tool_use_id.is_some()
            || chunk.tool_name.is_some()
            || matches!(chunk.delta, Some(BlockDelta::InputJsonDelta { .. }));
        if is_tool {
            let item = self.ensure_tool_item(chunk.block_index);
            if let StreamOutputItem::Tool {
                id,
                name,
                arguments,
                ..
            } = &mut self.output[item]
            {
                if let Some(tool_id) = &chunk.tool_use_id {
                    if !tool_id.is_empty() {
                        *id = tool_id.clone();
                    }
                }
                if let Some(tool_name) = &chunk.tool_name {
                    if !tool_name.is_empty() {
                        *name = tool_name.clone();
                    }
                }
                if let Some(BlockDelta::InputJsonDelta { partial_json }) = &chunk.delta {
                    arguments.push_str(partial_json);
                }
            }
            self.start_tool_item(item, &mut out);

            if let Some(BlockDelta::InputJsonDelta { partial_json }) = &chunk.delta {
                if !partial_json.is_empty() {
                    let (output_index, item_id) = match &self.output[item] {
                        StreamOutputItem::Tool {
                            output_index, id, ..
                        } => (*output_index, id.clone()),
                        StreamOutputItem::Text { .. } => unreachable!("tool item has tool shape"),
                    };
                    out.push(sse(json!({
                        "type": "response.function_call_arguments.delta",
                        "output_index": output_index,
                        "item_id": item_id,
                        "delta": partial_json,
                    })));
                }
            }
        }

        if let Some(reason) = &chunk.finish_reason {
            let usage = chunk.usage.as_ref();
            self.complete(reason, usage, &mut out);
        }

        out
    }

    /// Complete an otherwise unterminated stream. This is mainly useful for
    /// consumers that observe an EOF without a canonical terminal chunk.
    pub fn finish(&mut self) -> Vec<String> {
        if self.completed {
            return vec![];
        }
        let mut out = self.start();
        self.complete(&FinishReason::Stop, None, &mut out);
        out
    }

    /// Completed Responses output items, suitable for durable continuation
    /// replay after the stream's terminal event.
    pub fn output_items(&self) -> Vec<Value> {
        self.output.iter().map(StreamOutputItem::as_json).collect()
    }

    fn ensure_text_item(&mut self, out: &mut Vec<String>) -> usize {
        if let Some(item) = self.text_item {
            return item;
        }
        let output_index = self.allocate_output_index();
        let id = format!("msg_{}", self.response_id);
        let item = self.output.len();
        self.output.push(StreamOutputItem::Text {
            output_index,
            id: id.clone(),
            text: String::new(),
            started: true,
            closed: false,
        });
        self.text_item = Some(item);
        out.push(sse(json!({
            "type": "response.output_item.added",
            "output_index": output_index,
            "item": {
                "id": id,
                "type": "message",
                "status": "in_progress",
                "role": "assistant",
                "content": [],
            },
        })));
        out.push(sse(json!({
            "type": "response.content_part.added",
            "output_index": output_index,
            "item_id": format!("msg_{}", self.response_id),
            "content_index": 0,
            "part": {"type": "output_text", "text": "", "annotations": []},
        })));
        item
    }

    fn ensure_tool_item(&mut self, block_index: u32) -> usize {
        if let Some(&item) = self.tools.get(&block_index) {
            return item;
        }
        let output_index = self.allocate_output_index();
        let item = self.output.len();
        self.output.push(StreamOutputItem::Tool {
            output_index,
            id: String::new(),
            name: String::new(),
            arguments: String::new(),
            started: false,
            closed: false,
        });
        self.tools.insert(block_index, item);
        item
    }

    fn start_tool_item(&mut self, item: usize, out: &mut Vec<String>) {
        let payload = match &mut self.output[item] {
            StreamOutputItem::Tool {
                output_index,
                id,
                name,
                arguments,
                started,
                ..
            } if !*started && !id.is_empty() && !name.is_empty() => {
                *started = true;
                Some((*output_index, id.clone(), name.clone(), arguments.clone()))
            }
            _ => None,
        };
        if let Some((output_index, id, name, arguments)) = payload {
            out.push(sse(json!({
                "type": "response.output_item.added",
                "output_index": output_index,
                "item": {
                    "id": id,
                    "type": "function_call",
                    "status": "in_progress",
                    "call_id": id,
                    "name": name,
                    "arguments": arguments,
                },
            })));
        }
    }

    fn complete(&mut self, _reason: &FinishReason, usage: Option<&Usage>, out: &mut Vec<String>) {
        if self.completed {
            return;
        }
        for item in &mut self.output {
            match item {
                StreamOutputItem::Text {
                    output_index,
                    id,
                    text,
                    started,
                    closed,
                } if *started && !*closed => {
                    let part = output_text_part(text);
                    out.push(sse(json!({
                        "type": "response.output_text.done",
                        "output_index": *output_index,
                        "content_index": 0,
                        "item_id": id,
                        "text": text,
                    })));
                    out.push(sse(json!({
                        "type": "response.content_part.done",
                        "output_index": *output_index,
                        "item_id": id,
                        "content_index": 0,
                        "part": part,
                    })));
                    out.push(sse(json!({
                        "type": "response.output_item.done",
                        "output_index": *output_index,
                        "item": text_item_json(id, text, "completed"),
                    })));
                    *closed = true;
                }
                StreamOutputItem::Tool {
                    output_index,
                    id,
                    name,
                    arguments,
                    started,
                    closed,
                } if *started && !*closed => {
                    out.push(sse(json!({
                        "type": "response.function_call_arguments.done",
                        "output_index": *output_index,
                        "item_id": id,
                        "arguments": arguments,
                    })));
                    out.push(sse(json!({
                        "type": "response.output_item.done",
                        "output_index": *output_index,
                        "item": tool_item_json(id, name, arguments, "completed"),
                    })));
                    *closed = true;
                }
                _ => {}
            }
        }
        let output = self.output.iter().map(StreamOutputItem::as_json).collect();
        let response = self.response_json("completed", output, usage);
        out.push(sse(
            json!({"type": "response.completed", "response": response}),
        ));
        self.completed = true;
    }

    fn allocate_output_index(&mut self) -> u32 {
        let index = self.next_output_index;
        self.next_output_index = self.next_output_index.saturating_add(1);
        index
    }

    fn response_json(&self, status: &str, output: Vec<Value>, usage: Option<&Usage>) -> Value {
        json!({
            "id": self.response_id,
            "object": "response",
            "created_at": chrono::Utc::now().timestamp(),
            "model": self.model,
            "status": status,
            "store": self.store,
            "output": output,
            "usage": usage.map(usage_json),
        })
    }
}

impl StreamOutputItem {
    fn as_json(&self) -> Value {
        match self {
            Self::Text { id, text, .. } => text_item_json(id, text, "completed"),
            Self::Tool {
                id,
                name,
                arguments,
                ..
            } => tool_item_json(id, name, arguments, "completed"),
        }
    }
}
