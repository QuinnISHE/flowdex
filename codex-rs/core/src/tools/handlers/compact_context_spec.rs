use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use std::collections::BTreeMap;

pub(crate) const COMPACT_CONTEXT_TOOL_NAME: &str = "compact_context";

pub fn create_compact_context_tool() -> ToolSpec {
    ToolSpec::Function(ResponsesApiTool {
        name: COMPACT_CONTEXT_TOOL_NAME.to_string(),
        description: "Compact this thread's context at a natural task boundary when older detail is no longer needed.".to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(BTreeMap::new(), /*required*/ None, Some(false.into())),
        output_schema: None,
    })
}
