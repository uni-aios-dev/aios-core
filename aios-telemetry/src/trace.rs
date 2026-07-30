use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Span {
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub trace_id: String,
    pub operation: String,
    pub start_time_ms: u128,
    pub end_time_ms: Option<u128>,
    pub tags: HashMap<String, String>,
    pub status: SpanStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SpanStatus {
    Ok,
    Error(String),
    Timeout,
}

pub struct TraceContext {
    trace_id: String,
    current_span_id: Option<String>,
    spans: Vec<Span>,
}

impl TraceContext {
    pub fn new() -> Self {
        let trace_id = Uuid::new_v4().to_string();
        Self {
            trace_id,
            current_span_id: None,
            spans: Vec::new(),
        }
    }

    pub fn new_with_id(trace_id: &str) -> Self {
        Self {
            trace_id: trace_id.to_string(),
            current_span_id: None,
            spans: Vec::new(),
        }
    }

    pub fn trace_id(&self) -> &str {
        &self.trace_id
    }

    pub fn begin_span(&mut self, operation: &str) -> String {
        let span_id = Uuid::new_v4().to_string();
        let start = now_ms();

        let span = Span {
            span_id: span_id.clone(),
            parent_span_id: self.current_span_id.clone(),
            trace_id: self.trace_id.clone(),
            operation: operation.to_string(),
            start_time_ms: start,
            end_time_ms: None,
            tags: HashMap::new(),
            status: SpanStatus::Ok,
        };

        self.current_span_id = Some(span_id.clone());
        self.spans.push(span);
        span_id
    }

    pub fn end_span(&mut self, status: SpanStatus) {
        let now = now_ms();
        if let Some(span) = self
            .spans
            .iter_mut()
            .rev()
            .find(|s| s.end_time_ms.is_none())
        {
            span.end_time_ms = Some(now);
            span.status = status;
            self.current_span_id = span.parent_span_id.clone();
        }
    }

    pub fn add_tag(&mut self, key: &str, value: &str) {
        if let Some(span) = self
            .spans
            .iter_mut()
            .rev()
            .find(|s| s.end_time_ms.is_none())
        {
            span.tags.insert(key.to_string(), value.to_string());
        }
    }

    pub fn spans(&self) -> &[Span] {
        &self.spans
    }

    pub fn into_spans(self) -> Vec<Span> {
        self.spans
    }

    pub fn duration_ms(&self) -> u128 {
        let start = self.spans.first().map(|s| s.start_time_ms).unwrap_or(0);
        let end = self
            .spans
            .last()
            .and_then(|s| s.end_time_ms)
            .unwrap_or(now_ms());
        end - start
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(&self.spans).unwrap_or_default()
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trace_creation() {
        let trace = TraceContext::new();
        assert!(!trace.trace_id().is_empty());
        assert_eq!(trace.spans().len(), 0);
    }

    #[test]
    fn test_begin_and_end_span() {
        let mut trace = TraceContext::new();
        let span_id = trace.begin_span("test-operation");
        assert!(!span_id.is_empty());
        assert_eq!(trace.spans().len(), 1);
        trace.end_span(SpanStatus::Ok);
        assert!(trace.spans()[0].end_time_ms.is_some());
    }

    #[test]
    fn test_nested_spans() {
        let mut trace = TraceContext::new();
        trace.begin_span("root");
        trace.add_tag("key", "val");
        trace.begin_span("child");
        trace.end_span(SpanStatus::Ok);
        trace.end_span(SpanStatus::Ok);
        assert_eq!(trace.spans().len(), 2);
        assert!(trace.spans()[1].end_time_ms.is_some());
    }

    #[test]
    fn test_error_status() {
        let mut trace = TraceContext::new();
        trace.begin_span("failing");
        trace.end_span(SpanStatus::Error("something broke".into()));
        match &trace.spans()[0].status {
            SpanStatus::Error(msg) => assert_eq!(msg, "something broke"),
            _ => panic!("Expected error"),
        }
    }

    #[test]
    fn test_to_json() {
        let mut trace = TraceContext::new();
        trace.begin_span("test");
        trace.end_span(SpanStatus::Ok);
        let json = trace.to_json();
        assert!(json.contains("test"));
        assert!(json.contains("span_id"));
    }
}
