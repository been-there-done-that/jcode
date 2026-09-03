//! OpenAI Responses API streaming for Zen Responses-only models.
//!
//! Mirrors `openrouter_sse_stream` but speaks the Responses dialect:
//! `POST {base}/responses` with `{model, input, stream: true}` and SSE events
//! such as `response.output_text.delta`,
//! `response.function_call_arguments.delta` and `response.completed`, which are
//! translated into the shared [`StreamEvent`] vocabulary (text deltas, tool
//! start/input/end, usage, message end).

use super::*;
use bytes::Bytes;
use futures::Stream;
use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};

const RESPONSES_RETRY_BASE_DELAY_MS: u64 = 1000;

/// Pop the next complete SSE event off the front of `buffer`.
///
/// Accepts both `\n\n` and `\r\n\r\n` delimiters, like the chat parser.
fn take_sse_event(buffer: &mut String) -> Option<String> {
    let crlf = buffer.find("\r\n\r\n");
    let lf = buffer.find("\n\n");
    let (pos, sep_len) = match (crlf, lf) {
        (Some(c), Some(l)) if c <= l => (c, 4),
        (Some(c), None) => (c, 4),
        (_, Some(l)) => (l, 2),
        (None, None) => return None,
    };
    let event = buffer[..pos].to_string();
    buffer.drain(..pos + sep_len);
    Some(event)
}

/// Extract the JSON payload of one SSE event (the `data:` lines joined).
fn sse_data(event: &str) -> Option<String> {
    let mut data = String::new();
    for line in event.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(payload) = line.strip_prefix("data:") {
            let payload = payload.strip_prefix(' ').unwrap_or(payload);
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(payload);
        }
    }
    if data.is_empty() || data == "[DONE]" {
        None
    } else {
        Some(data)
    }
}

/// Incremental parser turning Responses SSE events into [`StreamEvent`]s.
struct ResponsesStream {
    inner: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    buffer: String,
    utf8: jcode_base::util::Utf8StreamDecoder,
    pending: VecDeque<StreamEvent>,
    /// A `function_call` item is open (start emitted, end not yet).
    function_call_open: bool,
    reasoning_id: Option<String>,
    thinking_open: bool,
    message_end_emitted: bool,
}

impl ResponsesStream {
    fn new(
        stream: impl Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
        _model: String,
    ) -> Self {
        Self {
            inner: Box::pin(stream),
            buffer: String::new(),
            utf8: jcode_base::util::Utf8StreamDecoder::new(),
            pending: VecDeque::new(),
            function_call_open: false,
            reasoning_id: None,
            thinking_open: false,
            message_end_emitted: false,
        }
    }

    fn close_thinking(&mut self) {
        if self.thinking_open {
            self.thinking_open = false;
            self.pending.push_back(StreamEvent::ThinkingEnd);
        }
    }

    fn close_function_call(&mut self) {
        if self.function_call_open {
            self.function_call_open = false;
            self.pending.push_back(StreamEvent::ToolUseEnd);
        }
    }

    fn handle_event(&mut self, event: &str) -> Result<()> {
        let Some(data) = sse_data(event) else {
            return Ok(());
        };
        let value: Value = serde_json::from_str(&data)
            .with_context(|| format!("parse Responses SSE event: {}", truncate(&data)))?;
        let kind = value.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match kind {
            "response.output_text.delta" => {
                if let Some(delta) = value.get("delta").and_then(|d| d.as_str()) {
                    self.pending
                        .push_back(StreamEvent::TextDelta(delta.to_string()));
                }
            }
            "response.output_item.added" => {
                if let Some(item) = value.get("item") {
                    match item.get("type").and_then(|t| t.as_str()) {
                        Some("function_call") => {
                            let id = item
                                .get("call_id")
                                .or_else(|| item.get("id"))
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string();
                            let name = item
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string();
                            self.function_call_open = true;
                            self.pending
                                .push_back(StreamEvent::ToolUseStart { id, name });
                        }
                        Some("reasoning") => {
                            self.reasoning_id =
                                item.get("id").and_then(|v| v.as_str()).map(str::to_string);
                        }
                        _ => {}
                    }
                }
            }
            "response.function_call_arguments.delta" => {
                if let Some(delta) = value.get("delta").and_then(|d| d.as_str()) {
                    self.pending
                        .push_back(StreamEvent::ToolInputDelta(delta.to_string()));
                }
            }
            "response.reasoning_summary_text.delta" => {
                if !self.thinking_open {
                    self.thinking_open = true;
                    self.pending.push_back(StreamEvent::ThinkingStart);
                }
                if let Some(delta) = value.get("delta").and_then(|d| d.as_str()) {
                    self.pending
                        .push_back(StreamEvent::ThinkingDelta(delta.to_string()));
                }
            }
            "response.output_item.done" => {
                if let Some(item) = value.get("item") {
                    match item.get("type").and_then(|t| t.as_str()) {
                        Some("function_call") => self.close_function_call(),
                        Some("reasoning") => {
                            self.close_thinking();
                            let id = item
                                .get("id")
                                .and_then(|v| v.as_str())
                                .or(self.reasoning_id.as_deref())
                                .unwrap_or_default()
                                .to_string();
                            let summary = item
                                .get("summary")
                                .and_then(|v| v.as_array())
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|e| {
                                            e.get("text")
                                                .and_then(|t| t.as_str())
                                                .map(str::to_string)
                                        })
                                        .collect::<Vec<_>>()
                                })
                                .unwrap_or_default();
                            let encrypted_content = item
                                .get("encrypted_content")
                                .and_then(|v| v.as_str())
                                .map(str::to_string);
                            self.pending.push_back(StreamEvent::OpenAIReasoning {
                                id,
                                summary,
                                encrypted_content,
                                status: item
                                    .get("status")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string),
                            });
                            self.reasoning_id = None;
                        }
                        _ => {}
                    }
                }
            }
            "response.completed" => {
                self.close_function_call();
                self.close_thinking();
                if let Some(response) = value.get("response") {
                    let (input_tokens, output_tokens) = usage(response);
                    if input_tokens.is_some() || output_tokens.is_some() {
                        self.pending.push_back(StreamEvent::TokenUsage {
                            input_tokens,
                            output_tokens,
                            cache_read_input_tokens: None,
                            cache_creation_input_tokens: None,
                        });
                    }
                    let status = response
                        .get("status")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    self.pending.push_back(StreamEvent::MessageEnd {
                        stop_reason: status,
                    });
                    self.message_end_emitted = true;
                }
            }
            "response.failed" | "response.incomplete" => {
                self.close_function_call();
                self.close_thinking();
                let message = value
                    .get("response")
                    .and_then(|r| r.get("error"))
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .or_else(|| {
                        value
                            .get("error")
                            .and_then(|e| e.get("message"))
                            .and_then(|m| m.as_str())
                    })
                    .unwrap_or("Responses stream reported failure");
                anyhow::bail!("{message}");
            }
            "error" => {
                let message = value
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("Responses stream error");
                anyhow::bail!("{message}");
            }
            _ => {}
        }
        Ok(())
    }
}

fn truncate(data: &str) -> String {
    const MAX: usize = 240;
    if data.len() <= MAX {
        data.to_string()
    } else {
        format!("{}…", &data[..MAX])
    }
}

fn usage(response: &Value) -> (Option<u64>, Option<u64>) {
    let usage = response.get("usage");
    (
        usage
            .and_then(|u| u.get("input_tokens"))
            .and_then(|v| v.as_u64()),
        usage
            .and_then(|u| u.get("output_tokens"))
            .and_then(|v| v.as_u64()),
    )
}

impl Stream for ResponsesStream {
    type Item = Result<StreamEvent>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        if let Some(event) = self.pending.pop_front() {
            return Poll::Ready(Some(Ok(event)));
        }
        match self.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(bytes))) => {
                let decoded = self.utf8.decode(&bytes);
                self.buffer.push_str(&decoded);
                while let Some(event) = take_sse_event(&mut self.buffer) {
                    if let Err(e) = self.handle_event(&event) {
                        return Poll::Ready(Some(Err(e)));
                    }
                    if !self.pending.is_empty() {
                        break;
                    }
                }
                if let Some(event) = self.pending.pop_front() {
                    Poll::Ready(Some(Ok(event)))
                } else {
                    // No complete event yet; keep polling the transport.
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e.into()))),
            Poll::Ready(None) => {
                // Transport closed: drain any fully-buffered events first,
                // then flush a final MessageEnd if the server never sent
                // `response.completed` (e.g. truncated stream).
                while let Some(event) = take_sse_event(&mut self.buffer) {
                    if let Err(e) = self.handle_event(&event) {
                        return Poll::Ready(Some(Err(e)));
                    }
                }
                if let Some(event) = self.pending.pop_front() {
                    Poll::Ready(Some(Ok(event)))
                } else if !self.message_end_emitted {
                    self.close_function_call();
                    self.close_thinking();
                    self.message_end_emitted = true;
                    Poll::Ready(Some(Ok(StreamEvent::MessageEnd { stop_reason: None })))
                } else {
                    Poll::Ready(None)
                }
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

pub(super) async fn run_responses_stream_with_retries(
    client: Client,
    api_base: String,
    auth: ProviderAuth,
    request: Value,
    tx: mpsc::Sender<Result<StreamEvent>>,
    model: String,
) {
    let mut last_error = None;
    let mut next_retry_delay = None;
    let config = jcode_base::config::config();
    let max_retries = config.provider.max_retries.max(1);
    let retry_backoff_cap =
        std::time::Duration::from_secs(config.provider.retry_backoff_cap_secs.max(1));

    for attempt in 0..max_retries {
        if attempt > 0 {
            let delay = jcode_provider_core::retry_after::retry_delay(
                attempt,
                RESPONSES_RETRY_BASE_DELAY_MS,
                next_retry_delay.take(),
            )
            .min(retry_backoff_cap);
            tokio::time::sleep(delay).await;
            jcode_base::logging::info(&format!(
                "Retrying Responses API request using {} (attempt {}/{})",
                auth.label(),
                attempt + 1,
                max_retries
            ));
        }

        jcode_base::logging::info(&format!(
            "Responses API stream attempt {}/{} over HTTPS transport (model: {}, endpoint: {}, auth: {})",
            attempt + 1,
            max_retries,
            model,
            api_base,
            auth.label()
        ));

        let (attempt_tx, attempt_guard) =
            jcode_provider_core::attempt_tracker::track_attempt_output(tx.clone());

        let attempt_client = if attempt == 0 {
            client.clone()
        } else {
            jcode_provider_core::fresh_transport_client()
        };

        match stream_responses_response(
            attempt_client,
            api_base.clone(),
            auth.clone(),
            request.clone(),
            attempt_tx,
            model.clone(),
        )
        .await
        {
            Ok(()) => {
                let _ = attempt_guard.finish().await;
                return;
            }
            Err(e) => {
                let saw_output = attempt_guard.finish().await;
                let error_str = format!("{e:#}").to_lowercase();
                if is_responses_retryable_error(&error_str) && attempt + 1 < max_retries {
                    if saw_output {
                        jcode_base::logging::warn(&format!(
                            "Transient Responses API error after partial output; rolling back partial attempt and retrying: {}",
                            e
                        ));
                        let _ = tx
                            .send(Ok(StreamEvent::RetryRollback {
                                attempt: attempt + 2,
                                max: max_retries,
                            }))
                            .await;
                    } else {
                        jcode_base::logging::info(&format!(
                            "Transient Responses API error, will retry: {}",
                            e
                        ));
                    }
                    next_retry_delay = jcode_provider_core::retry_after::retry_after_from_error(&e);
                    last_error = Some(e);
                    continue;
                }

                let _ = tx.send(Err(e)).await;
                return;
            }
        }
    }

    if let Some(e) = last_error {
        let _ = tx
            .send(Err(anyhow::anyhow!(
                "Failed after {} retries: {}",
                max_retries,
                e
            )))
            .await;
    }
}

/// Retry transient transport faults and 429/5xx, but never client errors
/// (400/401/403/404/422) — same policy as the chat path.
fn is_responses_retryable_error(error_str: &str) -> bool {
    if error_str.contains("429")
        || error_str.contains("rate limit")
        || error_str.contains("too many requests")
    {
        return error_str.contains("retry") || !error_str.contains("quota");
    }
    if let Some(status) = parsed_responses_http_status(error_str) {
        return status == 408 || status == 425 || status == 429 || status >= 500;
    }
    error_str.contains("timed out")
        || error_str.contains("timeout")
        || error_str.contains("connection")
        || error_str.contains("network")
        || error_str.contains("eof")
        || error_str.contains("reset by peer")
}

fn parsed_responses_http_status(error_str: &str) -> Option<u16> {
    let lower = error_str.to_ascii_lowercase();
    let marker = lower.find("http")?;
    lower[marker..]
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| part.len() == 3)
        .filter_map(|part| part.parse::<u16>().ok())
        .find(|status| (100..600).contains(status))
}

async fn stream_responses_response(
    client: Client,
    api_base: String,
    auth: ProviderAuth,
    request: Value,
    tx: mpsc::Sender<Result<StreamEvent>>,
    model: String,
) -> Result<()> {
    use jcode_message_types::ConnectionPhase;
    let _ = tx
        .send(Ok(StreamEvent::ConnectionPhase {
            phase: ConnectionPhase::SendingRequest,
        }))
        .await;
    let connect_start = std::time::Instant::now();
    let stream_idle_timeout = jcode_base::provider::stream_idle_timeout();

    let url = format!("{}/responses", api_base.trim_end_matches('/'));
    let req = auth
        .apply(
            client
                .post(&url)
                .header("Content-Type", "application/json")
                .header("Accept-Encoding", "identity"),
        )
        .await?;

    let response = jcode_provider_core::transport::send_with_initial_response_timeout(
        req.json(&request),
        stream_idle_timeout,
    )
    .await
    .with_context(|| {
        format!(
            "Failed to send Responses API request\n  endpoint: {}\n  model: {}\n  auth: {}\nHint: check network connectivity, DNS/TLS, that the base URL includes the API version (usually /v1), and that the model exists on the provider.",
            url,
            model,
            auth.label(),
        )
    })?;

    let connect_ms = connect_start.elapsed().as_millis();
    jcode_base::logging::info(&format!(
        "HTTP connection established in {}ms (status={})",
        connect_ms,
        response.status()
    ));

    if !response.status().is_success() {
        let status = response.status();
        let retry_after = jcode_provider_core::retry_after::retry_after(response.headers());
        let body = jcode_base::util::http_error_body(response, "HTTP error").await;
        return Err(jcode_provider_core::retry_after::error_with_retry_after(
            format!(
                "Responses API request failed\n  endpoint: {}\n  model: {}\n  auth: {}\n  status: {}\n  response: {}",
                url,
                model,
                auth.label(),
                status,
                body,
            ),
            retry_after,
        ));
    }

    let _ = tx
        .send(Ok(StreamEvent::ConnectionPhase {
            phase: ConnectionPhase::WaitingForResponse,
        }))
        .await;

    let mut stream = ResponsesStream::new(response.bytes_stream(), model.clone());
    let idle_timeout_secs = stream_idle_timeout.as_secs();

    loop {
        let event = match tokio::time::timeout(stream_idle_timeout, stream.next()).await {
            Ok(Some(Ok(event))) => event,
            Ok(Some(Err(e))) => anyhow::bail!(
                "Responses API stream error\n  endpoint: {}\n  model: {}\n  auth: {}\n  error: {}",
                url,
                model,
                auth.label(),
                e
            ),
            Ok(None) => break,
            Err(_) => {
                jcode_base::logging::warn(&format!(
                    "Responses SSE stream timed out (no data for {}s)",
                    idle_timeout_secs
                ));
                anyhow::bail!(
                    "Responses API stream timed out (no data for {}s)\n  endpoint: {}\n  model: {}\n  auth: {}",
                    idle_timeout_secs,
                    url,
                    model,
                    auth.label(),
                );
            }
        };
        if tx.send(Ok(event)).await.is_err() {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sse_chunks(payloads: &[&str]) -> Vec<Result<Bytes, reqwest::Error>> {
        payloads
            .iter()
            .map(|p| Ok(Bytes::from(p.to_string())))
            .collect()
    }

    async fn collect_events(chunks: Vec<Result<Bytes, reqwest::Error>>) -> Vec<StreamEvent> {
        use futures::StreamExt;
        let stream = futures::stream::iter(chunks);
        let mut parser = ResponsesStream::new(stream, "test-model".to_string());
        let mut events = Vec::new();
        while let Some(item) = parser.next().await {
            events.push(item.expect("parser event"));
        }
        events
    }

    fn event_kinds(events: &[StreamEvent]) -> Vec<String> {
        events
            .iter()
            .map(|e| match e {
                StreamEvent::TextDelta(_) => "text",
                StreamEvent::ToolUseStart { .. } => "tool-start",
                StreamEvent::ToolInputDelta(_) => "tool-delta",
                StreamEvent::ToolUseEnd => "tool-end",
                StreamEvent::TokenUsage { .. } => "usage",
                StreamEvent::MessageEnd { .. } => "end",
                StreamEvent::ThinkingStart => "thinking-start",
                StreamEvent::ThinkingDelta(_) => "thinking-delta",
                StreamEvent::ThinkingEnd => "thinking-end",
                StreamEvent::OpenAIReasoning { .. } => "reasoning",
                other => panic!("unexpected event: {other:?}"),
            })
            .map(str::to_string)
            .collect()
    }

    #[tokio::test]
    async fn text_stream_parses_deltas_usage_and_end() {
        let chunks = sse_chunks(&[
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello\"}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\" world\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":9,\"output_tokens\":20}}}\n\n",
        ]);
        let events = collect_events(chunks).await;
        assert_eq!(event_kinds(&events), vec!["text", "text", "usage", "end"]);
        match (&events[0], &events[1]) {
            (StreamEvent::TextDelta(a), StreamEvent::TextDelta(b)) => {
                assert_eq!(format!("{a}{b}"), "Hello world");
            }
            _ => panic!("expected text deltas"),
        }
        match &events[2] {
            StreamEvent::TokenUsage {
                input_tokens,
                output_tokens,
                ..
            } => {
                assert_eq!(*input_tokens, Some(9));
                assert_eq!(*output_tokens, Some(20));
            }
            _ => panic!("expected usage"),
        }
    }

    #[tokio::test]
    async fn function_call_assembles_start_deltas_end() {
        let chunks = sse_chunks(&[
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"bash\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"delta\":\"{\\\"cmd\\\":\"}\n\ndata: {\"type\":\"response.function_call_arguments.delta\",\"delta\":\" \\\"ls\\\"}\"}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"bash\",\"arguments\":\"{\\\"cmd\\\": \\\"ls\\\"}\"}}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n",
        ]);
        let events = collect_events(chunks).await;
        assert_eq!(
            event_kinds(&events),
            vec!["tool-start", "tool-delta", "tool-delta", "tool-end", "end"]
        );
        match &events[0] {
            StreamEvent::ToolUseStart { id, name } => {
                assert_eq!(id, "call_1");
                assert_eq!(name, "bash");
            }
            _ => panic!("expected tool start"),
        }
        let mut args = String::new();
        for e in &events[1..3] {
            match e {
                StreamEvent::ToolInputDelta(d) => args.push_str(d),
                _ => panic!("expected tool delta"),
            }
        }
        assert_eq!(args, "{\"cmd\": \"ls\"}");
    }

    #[tokio::test]
    async fn reasoning_item_emits_native_replay_event() {
        let chunks = sse_chunks(&[
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"reasoning\",\"id\":\"rs_1\"}}\n\n",
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"thinking\"}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"reasoning\",\"id\":\"rs_1\",\"summary\":[{\"type\":\"summary_text\",\"text\":\"s\"}],\"encrypted_content\":\"enc\"}}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n",
        ]);
        let events = collect_events(chunks).await;
        assert_eq!(
            event_kinds(&events),
            vec![
                "thinking-start",
                "thinking-delta",
                "thinking-end",
                "reasoning",
                "end"
            ]
        );
        match &events[3] {
            StreamEvent::OpenAIReasoning {
                id,
                summary,
                encrypted_content,
                ..
            } => {
                assert_eq!(id, "rs_1");
                assert_eq!(summary, &vec!["s".to_string()]);
                assert_eq!(encrypted_content.as_deref(), Some("enc"));
            }
            _ => panic!("expected reasoning event"),
        }
    }

    #[tokio::test]
    async fn failed_response_surfaces_error() {
        let chunks = sse_chunks(&[
            "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"boom\"}}}\n\n",
        ]);
        use futures::StreamExt;
        let stream = futures::stream::iter(chunks);
        let mut parser = ResponsesStream::new(stream, "m".to_string());
        let first = parser.next().await.expect("item").expect_err("error");
        assert!(format!("{first}").contains("boom"));
    }

    #[test]
    fn client_errors_are_not_retryable() {
        for status in [400, 401, 403, 404, 422] {
            assert!(
                !is_responses_retryable_error(&format!("HTTP {status} bad request")),
                "status {status}"
            );
        }
    }

    #[test]
    fn server_errors_and_rate_limits_are_retryable() {
        assert!(is_responses_retryable_error(
            "HTTP 500 internal server error"
        ));
        assert!(is_responses_retryable_error(
            "HTTP 429 too many requests, retry later"
        ));
        assert!(is_responses_retryable_error("connection reset by peer"));
    }
}
