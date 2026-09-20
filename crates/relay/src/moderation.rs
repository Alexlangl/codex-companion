use axum::{
    body::Body,
    http::{header, StatusCode, Uri},
    response::Response,
};
use serde_json::{json, Value};

const NOTICE: &str = "本轮内容被上游内容审核拦截，请切换模型后重试。";

pub(crate) fn response_object(model: Option<&str>) -> Value {
    let id = format!("resp_moderation_{}", chrono::Utc::now().timestamp_micros());
    json!({"id":id,"object":"response","created_at":chrono::Utc::now().timestamp(),
        "status":"completed","model":model,"background":false,"error":null,"incomplete_details":null,
        "output":[{"id":format!("msg_{id}"),"type":"message","role":"assistant","status":"completed",
            "content":[{"type":"output_text","text":NOTICE}]}],
        "usage":{"input_tokens":0,"output_tokens":0,"total_tokens":0,
            "input_tokens_details":{"cached_tokens":0},"output_tokens_details":{"reasoning_tokens":0}}})
}

pub(crate) fn frames(model: Option<&str>) -> Vec<Value> {
    let completed = response_object(model);
    let mut created = completed.clone();
    created["status"] = json!("in_progress");
    created["output"] = json!([]);
    let item = completed["output"][0].clone();
    vec![
        json!({"type":"response.created","sequence_number":0,"response":created}),
        json!({"type":"response.in_progress","sequence_number":1,"response":created}),
        json!({"type":"response.output_item.added","sequence_number":2,"output_index":0,"item":item}),
        json!({"type":"response.output_text.delta","sequence_number":3,"item_id":item["id"],"output_index":0,"content_index":0,"delta":NOTICE}),
        json!({"type":"response.output_item.done","sequence_number":4,"output_index":0,"item":item}),
        json!({"type":"response.completed","sequence_number":5,"response":completed}),
    ]
}

pub(crate) fn response(uri: &Uri, body: &[u8], model: Option<&str>) -> Response {
    let streaming = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.get("stream").and_then(Value::as_bool))
        .unwrap_or(false);
    let chat = uri.path().ends_with("/chat/completions");
    let value = if chat {
        json!({"id":format!("chatcmpl_moderation_{}",chrono::Utc::now().timestamp_micros()),"object":"chat.completion",
            "created":chrono::Utc::now().timestamp(),"model":model,
            "choices":[{"index":0,"message":{"role":"assistant","content":NOTICE},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":0,"completion_tokens":0,"total_tokens":0}})
    } else {
        response_object(model)
    };
    let payload = if streaming && chat {
        let mut chunk = value.clone();
        chunk["object"] = json!("chat.completion.chunk");
        chunk["choices"] =
            json!([{"index":0,"delta":{"role":"assistant","content":NOTICE},"finish_reason":null}]);
        let first = format!("data: {chunk}\n\n");
        chunk["choices"] = json!([{"index":0,"delta":{},"finish_reason":"stop"}]);
        format!("{first}data: {chunk}\n\ndata: [DONE]\n\n")
    } else if streaming {
        frames(model)
            .into_iter()
            .map(|v| format!("event: {}\ndata: {v}\n\n", v["type"].as_str().unwrap()))
            .collect()
    } else {
        value.to_string()
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            if streaming {
                "text/event-stream"
            } else {
                "application/json"
            },
        )
        .header("x-codex-companion-moderation", "blocked")
        .body(Body::from(payload))
        .expect("moderation response")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn notice_is_terminal_with_zero_usage_and_ordered_frames() {
        let events = frames(Some("fixture-model"));
        for (index, event) in events.iter().enumerate() {
            assert_eq!(event["sequence_number"], index);
        }
        assert_eq!(events.last().unwrap()["response"]["status"], "completed");
        assert_eq!(
            events.last().unwrap()["response"]["usage"]["total_tokens"],
            0
        );
        assert!(
            events.last().unwrap()["response"]["output"][0]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("审核拦截")
        );
    }
}
