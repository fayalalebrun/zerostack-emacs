use std::borrow::Cow;

use compact_str::CompactString;
use rig::completion::ToolDefinition;
use rig::tool::{DynamicTool, ToolExecutionError, ToolOutput};
use rmcp::model::{CallToolRequestParams, JsonObject, RawContent};
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
                Box::pin(async move { tool.call(args).await.map(ToolOutput::text) })
            },
        ))
    }

    async fn call(&self, args: serde_json::Value) -> Result<String, ToolExecutionError> {
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

            if result.is_error.unwrap_or(false) {
                let error_msg = result
                    .content
                    .iter()
                    .filter_map(|c| match &c.raw {
                        RawContent::Text(t) => Some(t.text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let msg = if error_msg.is_empty() {
                    "MCP tool returned an error".to_string()
                } else {
                    error_msg
                };
                return Err(ToolExecutionError::other(msg));
            }

            let mut content = String::new();
            for item in result.content {
                match item.raw {
                    RawContent::Text(t) => content.push_str(&t.text),
                    RawContent::Image(img) => {
                        content.push_str(&format!("data:{};base64,{}", img.mime_type, img.data));
                    }
                    RawContent::Resource(r) => match r.resource {
                        rmcp::model::ResourceContents::TextResourceContents { text, .. } => {
                            content.push_str(&text);
                        }
                        rmcp::model::ResourceContents::BlobResourceContents { blob, .. } => {
                            content.push_str(&blob);
                        }
                    },
                    _ => {}
                }
            }
            if let Some(msg) = coaching {
                content = format!("{}\n\n{}", msg, content);
            }
            Ok(content)
        }
        .await
    }
}
