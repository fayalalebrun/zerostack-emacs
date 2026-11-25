use std::borrow::Cow;

use compact_str::CompactString;
use rig::completion::ToolDefinition;
use rig::message::ToolResultContent;
use rig::tool::{DynamicTool, ToolExecutionError, ToolOutput};
use rmcp::model::{CallToolRequestParams, CallToolResult, JsonObject, RawContent};
use rmcp::service::{Peer, RoleClient};

use crate::agent::tools::check_perm;
use crate::permission::ask::AskSender;
use crate::permission::checker::PermCheck;

pub struct McpTool {
    pub server_name: CompactString,
    pub definition: rmcp::model::Tool,
    pub peer: Peer<RoleClient>,
    pub permission: Option<PermCheck>,
    pub ask_tx: Option<AskSender>,
    pub timeout: Option<std::time::Duration>,
}

fn normalize_schema(schema: &mut serde_json::Value) {
    let Some(node) = schema.as_object_mut() else {
        return;
    };
    if !["type", "$ref", "anyOf", "oneOf", "allOf"]
        .iter()
        .any(|key| node.contains_key(*key))
    {
        node.insert(
            "type".into(),
            serde_json::json!(["object", "array", "string", "number", "boolean", "null"]),
        );
    }
    for key in [
        "properties",
        "patternProperties",
        "$defs",
        "definitions",
        "dependentSchemas",
    ] {
        if let Some(children) = node.get_mut(key).and_then(serde_json::Value::as_object_mut) {
            for child in children.values_mut() {
                normalize_schema(child);
            }
        }
    }
    for key in ["anyOf", "oneOf", "allOf", "prefixItems"] {
        if let Some(children) = node.get_mut(key).and_then(serde_json::Value::as_array_mut) {
            for child in children {
                normalize_schema(child);
            }
        }
    }
    for key in [
        "items",
        "additionalProperties",
        "not",
        "if",
        "then",
        "else",
        "contains",
        "propertyNames",
    ] {
        if let Some(child) = node.get_mut(key) {
            normalize_schema(child);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_schema;
    use serde_json::json;

    #[test]
    fn text_only_results_keep_existing_format_and_errors() {
        use rmcp::model::{CallToolResult, Content};

        let result = CallToolResult::success(vec![Content::text("first"), Content::text("second")]);
        let output = super::convert_result(result, Some("coaching".into())).unwrap();
        assert_eq!(output.as_text(), Some("coaching\n\nfirstsecond"));
        let result = CallToolResult::error(vec![Content::text("message"), Content::text("detail")]);
        let error = super::convert_result(result, None).unwrap_err();
        assert!(error.to_string().contains("message\ndetail"));
    }

    #[cfg(feature = "multimodal")]
    #[test]
    fn invalid_images_keep_other_tool_result_content() {
        use base64::{Engine, prelude::BASE64_STANDARD};
        use rig::message::ToolResultContent;
        use rmcp::model::{CallToolResult, Content};

        let png = BASE64_STANDARD.encode(crate::extras::image_validate::test_png());
        let result = CallToolResult::success(vec![
            Content::image("not base64", "image/png"),
            Content::image(png, "image/jpeg"),
            Content::text("still available"),
        ]);
        let output = super::convert_result(result, None).unwrap();
        assert_eq!(
            output.as_content(),
            &[
                ToolResultContent::text("[MCP image omitted: invalid base64 data]"),
                ToolResultContent::text(
                    "[MCP image omitted: image data does not match its declared MIME type]"
                ),
                ToolResultContent::text("still available"),
            ]
        );
        assert!(
            super::image_block("AAAA".into(), "image/png", &mut 3)
                .unwrap_err()
                .contains("size limit")
        );
    }

    #[test]
    fn linear_responses_serialization_preserves_optional_fields_and_non_strict_mode() {
        let mut parameters = json!({
            "type":"object",
            "properties":{
                "id":{"type":"string"},
                "statusUpdateType":{"type":"string", "enum":["onTrack","atRisk","offTrack"]},
                "customView":{"type":"object", "properties":{"id":{"type":"string"}}}
            },
            "required":["id"]
        });
        let original = parameters.clone();
        normalize_schema(&mut parameters);
        let definition = rig::completion::ToolDefinition {
            name: rig::message::ToolName::new("linear_update_project").unwrap(),
            description: "Update a Linear project".into(),
            parameters,
        };
        let responses =
            rig::providers::openai::responses_api::ResponsesToolDefinition::from(definition);
        let wire = serde_json::to_value(responses).unwrap();
        assert_eq!(wire["type"], "function");
        assert_eq!(wire["strict"], false);
        assert_eq!(wire["parameters"], original);
        assert_eq!(wire["parameters"]["required"], json!(["id"]));
        assert!(wire["parameters"].get("additionalProperties").is_none());
        assert!(
            wire["parameters"]["properties"]["customView"]
                .get("required")
                .is_none()
        );
    }

    #[test]
    fn notion_additional_properties_gets_explicit_unrestricted_type() {
        let mut schema = json!({"type":"object", "additionalProperties":{}, "properties":{
            "filters":{"type":"object", "additionalProperties":{"description":"Any value"}}
        }});
        normalize_schema(&mut schema);
        let types = json!(["object", "array", "string", "number", "boolean", "null"]);
        assert_eq!(schema["additionalProperties"]["type"], types);
        assert_eq!(
            schema["properties"]["filters"]["additionalProperties"]["type"],
            types
        );
        assert_eq!(
            schema["properties"]["filters"]["additionalProperties"]["description"],
            "Any value"
        );
        let normalized = schema.clone();
        normalize_schema(&mut schema);
        assert_eq!(schema, normalized);
    }

    #[test]
    fn valid_schemas_and_non_schema_values_are_preserved() {
        let mut schema = json!({"type":"object", "additionalProperties":false,
            "properties":{"value":{"anyOf":[{"type":"string"},{"$ref":"#/$defs/value"}]}},
            "$defs":{"value":{"type":"integer","minimum":0}},
            "default":{"additionalProperties":{}}, "examples":[{}]
        });
        let original = schema.clone();
        normalize_schema(&mut schema);
        assert_eq!(schema, original);
        let mut boolean_schema = json!(true);
        normalize_schema(&mut boolean_schema);
        assert_eq!(boolean_schema, true);
    }

    #[test]
    fn traverses_array_items_definitions_and_composition_branches() {
        let mut schema = json!({"anyOf":[{"type":"array","items":{}},{"$ref":"#/$defs/value"}],
            "$defs":{"value":{"type":"object","additionalProperties":{}}}});
        normalize_schema(&mut schema);
        assert!(schema.get("type").is_none());
        assert!(schema["anyOf"][0]["items"]["type"].is_array());
        assert!(schema["$defs"]["value"]["additionalProperties"]["type"].is_array());
        assert_eq!(schema["anyOf"][1], json!({"$ref":"#/$defs/value"}));
    }
}

impl McpTool {
    pub fn tool_definition(&self) -> Result<ToolDefinition, rig::message::EmptyToolName> {
        let mut parameters =
            serde_json::to_value(&self.definition.input_schema).unwrap_or_default();
        normalize_schema(&mut parameters);
        Ok(ToolDefinition {
            name: rig::message::ToolName::new(self.definition.name.to_string())?,
            description: self
                .definition
                .description
                .clone()
                .unwrap_or(Cow::from(""))
                .to_string(),
            parameters,
        })
    }

    #[cfg(test)]
    pub fn into_dynamic(self) -> Result<DynamicTool, rig::message::EmptyToolName> {
        let name = self.definition.name.to_string();
        self.into_dynamic_named(&name)
    }

    pub(super) fn into_dynamic_named(
        self,
        name: &str,
    ) -> Result<DynamicTool, rig::message::EmptyToolName> {
        let mut definition = self.tool_definition()?;
        definition.name = rig::message::ToolName::new(name)?;
        let tool = std::sync::Arc::new(self);
        Ok(DynamicTool::new(
            definition.name,
            definition.description,
            definition.parameters,
            move |args| {
                let tool = tool.clone();
                Box::pin(async move { tool.call(args).await })
            },
        ))
    }

    async fn call(&self, args: serde_json::Value) -> Result<ToolOutput, ToolExecutionError> {
        let server_name = self.server_name.clone();
        let tool_name = self.definition.name.to_string();
        let peer = self.peer.clone();
        let permission = self.permission.clone();
        let ask_tx = self.ask_tx.clone();
        let timeout = self.timeout;

        async move {
            let perm_key = format!("mcp_tool:{server_name}:{tool_name}");
            let coaching = super::trace_operation(
                "permission",
                &server_name,
                Some(&tool_name),
                check_perm(&permission, &ask_tx, "mcp_tool", &perm_key),
            )
            .await
            .map_err(|e| ToolExecutionError::other(e.to_string()))?;

            let arguments: JsonObject = serde_json::from_value(args)
                .map_err(|e| ToolExecutionError::other(format!("Invalid MCP arguments: {e}")))?;
            let params = CallToolRequestParams::new(tool_name.clone()).with_arguments(arguments);

            let result = super::timed_operation(
                "call_tool",
                &server_name,
                Some(&tool_name),
                timeout,
                peer.call_tool(params),
            )
            .await
            .map_err(|e| ToolExecutionError::other(e.to_string()))?
            .map_err(|e| ToolExecutionError::other(format!("MCP tool error: {e}")))?;

            convert_result(result, coaching)
        }
        .await
    }
}

fn convert_result(
    result: CallToolResult,
    coaching: Option<String>,
) -> Result<ToolOutput, ToolExecutionError> {
    if result.is_error.unwrap_or(false) {
        let message = result
            .content
            .into_iter()
            .filter_map(|item| match item.raw {
                RawContent::Text(text) => Some(text.text),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Err(ToolExecutionError::other(if message.is_empty() {
            "MCP tool returned an error".to_string()
        } else {
            message
        }));
    }

    let mut content = Vec::new();
    let mut text = coaching.map_or_else(String::new, |message| format!("{message}\n\n"));
    #[cfg(feature = "multimodal")]
    let mut image_budget = (crate::extras::multimodal::MAX_MEDIA_BYTES as usize).div_ceil(3) * 4;
    for item in result.content {
        match item.raw {
            RawContent::Text(block) => text.push_str(&block.text),
            RawContent::Resource(resource) => match resource.resource {
                rmcp::model::ResourceContents::TextResourceContents { text: value, .. } => {
                    text.push_str(&value);
                }
                rmcp::model::ResourceContents::BlobResourceContents { blob, .. } => {
                    text.push_str(&blob);
                }
            },
            RawContent::Image(image) => {
                if !text.is_empty() {
                    content.push(ToolResultContent::text(std::mem::take(&mut text)));
                }
                #[cfg(feature = "multimodal")]
                let block = image_block(image.data, &image.mime_type, &mut image_budget)
                    .unwrap_or_else(|error| {
                        ToolResultContent::text(format!("[MCP image omitted: {error}]"))
                    });
                #[cfg(not(feature = "multimodal"))]
                let block = {
                    drop(image);
                    ToolResultContent::text("[MCP image omitted: multimodal support is disabled]")
                };
                content.push(block);
            }
            _ => {}
        }
    }
    if !text.is_empty() || content.is_empty() {
        content.push(ToolResultContent::text(text));
    }
    ToolOutput::content(content)
}

#[cfg(feature = "multimodal")]
fn image_block(
    data: String,
    mime: &str,
    encoded_budget: &mut usize,
) -> Result<ToolResultContent, String> {
    use base64::{Engine, prelude::BASE64_STANDARD};
    use rig::message::UserContent;

    *encoded_budget = encoded_budget
        .checked_sub(data.len())
        .ok_or("image data exceeds the MCP result size limit")?;
    let bytes = BASE64_STANDARD
        .decode(&data)
        .map_err(|_| "invalid base64 data")?;
    if bytes.len() as u64 > crate::extras::multimodal::MAX_MEDIA_BYTES {
        return Err("image data exceeds the size limit".into());
    }
    let info =
        crate::extras::image_validate::validate(&bytes)?.ok_or("unsupported image format")?;
    if info.mime != mime {
        return Err("image data does not match its declared MIME type".into());
    }
    let UserContent::Image(image) = UserContent::image_base64(
        data,
        Some(crate::agent::runner::image_media_type(info.mime)),
        None,
    ) else {
        unreachable!()
    };
    Ok(ToolResultContent::Image(image))
}
