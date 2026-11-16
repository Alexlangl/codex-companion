//! Explicit client-side discovery bridged through an ordinary function. This
//! never grants hosted tool execution or invokes tools inside the relay.
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn declared(value: &Value) -> bool {
    value["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|tool| tool["type"] == "tool_search" && tool["execution"] == "client")
        || value["input"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|v| v["type"] == "additional_tools")
            .any(declared)
}

fn wire_name(value: &Value) -> String {
    fn names(value: &Value, found: &mut BTreeSet<String>) {
        if let Some(name) = value["name"].as_str() {
            found.insert(name.into());
        }
        if value["type"] == "tool_search_output" {
            if let Ok(Some(tools)) = result_tools(value) {
                for tool in tools.as_array().into_iter().flatten() {
                    names(tool, found);
                }
            }
        }
        for field in ["tools", "input"] {
            for child in value[field].as_array().into_iter().flatten() {
                names(child, found);
            }
        }
    }
    let mut used = BTreeSet::new();
    names(value, &mut used);
    (0..)
        .map(|index| format!("companion_client_search_{index}"))
        .find(|name| !used.contains(name))
        .unwrap()
}

pub(crate) fn result_tools(item: &Value) -> Result<Option<Value>, String> {
    if let Some(tools) = item.get("tools") {
        return Ok(Some(tools.clone()));
    }
    let Some(output) = item.get("output") else {
        return Ok(None);
    };
    let value = if let Some(text) = output.as_str() {
        serde_json::from_str(text).map_err(|_| "工具发现结果不是有效 JSON")?
    } else {
        output.clone()
    };
    if value.is_array() {
        Ok(Some(value))
    } else {
        Ok(value.get("tools").cloned())
    }
}

pub(crate) fn lower(value: &mut Value) -> Result<(), String> {
    let wire = wire_name(value);
    fn declarations(value: &mut Value, wire: &str) {
        if let Some(tools) = value.get_mut("tools").and_then(Value::as_array_mut) {
            for tool in tools {
                if tool["type"] == "tool_search" && tool["execution"] == "client" {
                    let parameters = tool.get("parameters").cloned().unwrap_or_else(|| json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"],"additionalProperties":false}));
                    *tool = json!({"type":"function","name":wire,"description":tool.get("description").and_then(Value::as_str).unwrap_or("Search client tools available for this task"),"parameters":parameters});
                }
            }
        }
    }
    fn choice(value: &mut Value, wire: &str) {
        if value["type"] == "tool_search" {
            *value = json!({"type":"function","name":wire});
        }
        if let Some(tools) = value.get_mut("tools").and_then(Value::as_array_mut) {
            for tool in tools {
                choice(tool, wire);
            }
        }
    }
    declarations(value, &wire);
    if let Some(value) = value.get_mut("tool_choice") {
        choice(value, &wire);
    }
    let mut additional = Vec::new();
    if let Some(items) = value.get_mut("input").and_then(Value::as_array_mut) {
        for item in items {
            match item["type"].as_str() {
                Some("additional_tools") => declarations(item, &wire),
                Some("tool_search_call") => {
                    let arguments = item["arguments"]
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| item["arguments"].to_string());
                    *item = json!({"type":"function_call","name":wire,"call_id":item["call_id"],"arguments":arguments});
                }
                Some("tool_search_output") => {
                    if let Some(tools) = result_tools(item)? {
                        additional.extend(
                            tools
                                .as_array()
                                .ok_or("工具发现结果 tools 必须为数组")?
                                .clone(),
                        );
                    }
                    let output = item
                        .get("output")
                        .cloned()
                        .unwrap_or_else(|| json!({"tools":item["tools"]}));
                    *item = json!({"type":"function_call_output","call_id":item["call_id"],"output":output.as_str().map(str::to_owned).unwrap_or_else(||output.to_string())});
                }
                _ => {}
            }
        }
        if !additional.is_empty() {
            value["input"]
                .as_array_mut()
                .unwrap()
                .push(json!({"type":"additional_tools","role":"developer","tools":additional}));
        }
    }
    Ok(())
}

#[derive(Debug, Default)]
pub(crate) struct SearchRestore {
    wire: String,
    calls: BTreeMap<String, String>,
    original_tools: Option<Value>,
    original_choice: Option<Value>,
}
impl SearchRestore {
    pub(crate) fn from_request(mut value: &Value) -> Option<Self> {
        if value["response"].is_object() {
            value = &value["response"];
        }
        declared(value).then(|| Self {
            wire: wire_name(value),
            calls: BTreeMap::new(),
            original_tools: value.get("tools").cloned(),
            original_choice: value.get("tool_choice").cloned(),
        })
    }
    pub(crate) fn restore(&mut self, event: &mut Value) -> bool {
        let before = event.clone();
        if self.convert(event).is_err() {
            *event = json!({"type":"error","error":{"type":"invalid_response_error","code":"invalid_client_tool_search_arguments","message":"上游客户端工具发现参数无效"}});
        }
        *event != before
    }
    fn item(&mut self, value: &mut Value, partial: bool) -> Result<(), ()> {
        if value["type"] != "function_call"
            || value["name"] != self.wire
            || value["namespace"].as_str().is_some_and(|v| !v.is_empty())
        {
            return Ok(());
        }
        if value["call_id"].as_str().is_none_or(str::is_empty) {
            return Err(());
        }
        let id = value["id"].as_str().unwrap_or("").to_owned();
        if self.calls.len() >= 4096 && !self.calls.contains_key(&id) {
            return Err(());
        }
        let raw = value["arguments"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| self.calls.get(&id).map(String::as_str))
            .unwrap_or("");
        let args = if partial && raw.is_empty() {
            json!({})
        } else {
            serde_json::from_str::<Value>(raw).map_err(|_| ())?
        };
        if !args.is_object() {
            return Err(());
        }
        self.calls.entry(id.clone()).or_default();
        if let Some(suffix) = id.strip_prefix("fc_") {
            value["id"] = json!(format!("tsc_{suffix}"));
        }
        value["type"] = json!("tool_search_call");
        value["execution"] = json!("client");
        value["arguments"] = args;
        value.as_object_mut().unwrap().remove("name");
        value.as_object_mut().unwrap().remove("namespace");
        Ok(())
    }
    fn convert(&mut self, event: &mut Value) -> Result<(), ()> {
        match event["type"].as_str() {
            Some("function_call") => self.item(event, false)?,
            Some("response.output_item.added" | "response.output_item.done") => {
                let partial = event["type"] == "response.output_item.added";
                self.item(&mut event["item"], partial)?;
            }
            Some(
                "response.function_call_arguments.delta" | "response.function_call_arguments.done",
            ) => {
                if let Some(raw) = event["item_id"]
                    .as_str()
                    .and_then(|id| self.calls.get_mut(id))
                {
                    if let Some(delta) = event["delta"].as_str() {
                        if raw.len().saturating_add(delta.len()) > 16 * 1024 * 1024 {
                            return Err(());
                        }
                        raw.push_str(delta);
                    }
                    // Discovery arguments are a JSON object, not public function
                    // deltas. The corresponding done item carries the full object.
                    *event = json!({"type":"keepalive"});
                }
            }
            _ => {}
        }
        if let Some(response) = event.get_mut("response") {
            self.convert(response)?;
        }
        if let Some(items) = event.get_mut("output").and_then(Value::as_array_mut) {
            for item in items {
                self.item(item, false)?;
            }
        }
        if event["object"] == "response" || event.get("output").is_some() {
            if let Some(object) = event.as_object_mut() {
                for (key, original) in [
                    ("tools", &self.original_tools),
                    ("tool_choice", &self.original_choice),
                ] {
                    if object.contains_key(key) {
                        if let Some(value) = original {
                            object.insert(key.into(), value.clone());
                        } else {
                            object.remove(key);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> Value {
        json!({"tools":[{"type":"tool_search","execution":"client","parameters":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}},{"type":"function","name":"companion_client_search_0","parameters":{"type":"object"}}],"tool_choice":{"type":"tool_search"},"input":[]})
    }
    #[test]
    fn discovery_round_trip_preserves_identity_and_promotes_validated_results() {
        let original = request();
        let mut context = SearchRestore::from_request(&original).unwrap();
        let mut body = original.clone();
        crate::plan_request::normalize(&mut body, false).unwrap();
        assert_eq!(body["tool_choice"]["name"], "companion_client_search_1");
        let wire = body["tool_choice"]["name"].clone();
        let mut added = json!({"type":"response.output_item.added","item":{"type":"function_call","id":"fc_search","call_id":"c","name":wire,"arguments":""}});
        assert!(context.restore(&mut added));
        assert_eq!(added["item"]["type"], "tool_search_call");
        assert_eq!(added["item"]["id"], "tsc_search");
        let mut delta = json!({"type":"response.function_call_arguments.delta","item_id":"fc_search","delta":"{\"query\":\"read a file\"}"});
        assert!(context.restore(&mut delta));
        assert_eq!(delta["type"], "keepalive");
        let mut done = json!({"type":"response.output_item.done","item":{"type":"function_call","id":"fc_search","call_id":"c","name":wire,"arguments":""}});
        context.restore(&mut done);
        assert_eq!(done["item"]["arguments"], json!({"query":"read a file"}));
        let mut next = original;
        next["input"] = json!([done["item"],{"type":"tool_search_output","execution":"client","call_id":"c","tools":[{"type":"function","name":"read_file","parameters":{"type":"object"}}]}]);
        crate::plan_request::normalize(&mut next, false).unwrap();
        assert_eq!(next["input"][0]["type"], "function_call");
        assert_eq!(next["input"][1]["type"], "function_call_output");
        assert_eq!(next["input"][2]["tools"][0]["name"], "read_file");
    }
    #[test]
    fn discovered_hosted_capabilities_and_malformed_arguments_are_rejected() {
        let mut body = request();
        body["input"] = json!([{"type":"tool_search_output","execution":"client","call_id":"c","output":"{\"tools\":[{\"type\":\"computer_use\"}]}"}]);
        assert!(crate::plan_request::validate(&body, false).is_err());
        body["input"] = json!([]);
        body["tools"][0]["execution"] = json!("server");
        assert!(crate::plan_request::validate(&body, false).is_err());
        let mut context = SearchRestore::from_request(&request()).unwrap();
        let mut event = json!({"type":"response.output_item.done","item":{"type":"function_call","name":"companion_client_search_1","call_id":"c","arguments":"not json"}});
        context.restore(&mut event);
        assert_eq!(event["type"], "error");
    }
    #[tokio::test]
    async fn fragmented_sse_restores_search_without_affecting_ordinary_tools() {
        use futures_util::{stream, StreamExt};
        let original = request();
        let mut identity =
            crate::tool_identity::ToolIdentity::from_request(original.to_string().as_bytes());
        identity.enable_plan_search(original.to_string().as_bytes());
        let item = json!({"type":"function_call","id":"fc_s","call_id":"c","name":"companion_client_search_1","arguments":"{\"query\":\"files\"}"});
        let event = json!({"type":"response.completed","response":{"object":"response","tools":[{"type":"function","name":"companion_client_search_1"}],"output":[item,{"type":"function_call","name":"companion_client_search_0","arguments":"{}","call_id":"regular"}]}});
        let wire = format!("event: response.completed\ndata: {event}\n\n");
        let chunks = wire
            .as_bytes()
            .chunks(7)
            .map(|v| Ok(bytes::Bytes::copy_from_slice(v)))
            .collect::<Vec<_>>();
        let mut output = identity.restore_stream(Box::pin(stream::iter(chunks)));
        let bytes = output.next().await.unwrap().unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        let value: Value =
            serde_json::from_str(text.lines().find_map(|l| l.strip_prefix("data: ")).unwrap())
                .unwrap();
        assert_eq!(value["response"]["output"][0]["type"], "tool_search_call");
        assert_eq!(
            value["response"]["output"][1]["name"],
            "companion_client_search_0"
        );
        assert_eq!(value["response"]["tools"], original["tools"]);
        assert!(output.next().await.is_none());
    }
}
