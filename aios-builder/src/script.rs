//! Natural-language to EasyLang script generation with deterministic validation.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use aios_llm::{LlmEngine, LlmRequest};
use aios_security::capability::Capability;

use crate::compiler::WorkflowCompiler;
use crate::easylang::EasyLangParser;
use crate::manifest::AutoManifestGenerator;
use crate::workflow::Workflow;

/// Strict instruction block used for every LLM generation attempt.
pub const SYSTEM_PROMPT: &str = "You translate user requests into EasyLang workflow code.\n\
Output ONLY executable EasyLang lines, one command per line.\n\
No markdown fences, no explanations, no prose, no blank lines.\n\
Allowed commands: net_check, browse, console_cmd, notify, spawn, kill, load, unload, query, compact, status, timer.\n\
Optional label syntax: `label: command arguments`.\n\
Use `net_check \"url\"` to validate a URL, `browse \"url\"` to request navigation,\n\
`console_cmd \"command\"` to request a console command and `notify \"text\"` to notify the user.\n\
Russian and English requests are equivalent. Keyword hints:\n\
RU: проверь/проверка сайта или ссылки -> net_check; открой/открыть сайт или страницу -> browse;\n\
выполни/запусти команду -> console_cmd; уведоми/сообщи/напомни -> notify.\n\
EN: check/validate a site or URL -> net_check; open a site or page -> browse;\n\
run/execute a command -> console_cmd; notify/tell/remind me -> notify.\n\
Always emit EasyLang commands regardless of the request language.\n\
Example:\nnet_check \"https://example.com\"\nnotify \"check complete\"";

const ALLOWED_COMMANDS: &[&str] = &[
    "net_check",
    "browse",
    "console_cmd",
    "notify",
    "spawn",
    "kill",
    "load",
    "unload",
    "query",
    "compact",
    "status",
    "timer",
];

/// Text generator used by [`ScriptGenerator`]; object-safe for mocking.
pub trait ScriptLlm: Send + Sync {
    /// Generate EasyLang code from a natural-language prompt.
    fn generate<'a>(
        &'a self,
        prompt: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>>;
}

/// Adapter that drives a real [`LlmEngine`] with the strict code-only prompt.
pub struct LlmScriptLlm {
    engine: LlmEngine,
}

impl LlmScriptLlm {
    /// Wrap an LLM engine for script generation.
    pub fn new(engine: LlmEngine) -> Self {
        Self { engine }
    }
}

/// Adapter that borrows an [`LlmEngine`] held by a long-lived service.
pub struct BorrowedLlmScriptLlm<'a> {
    engine: &'a LlmEngine,
}

impl<'a> BorrowedLlmScriptLlm<'a> {
    /// Wrap a borrowed LLM engine for one generation call.
    pub fn new(engine: &'a LlmEngine) -> Self {
        Self { engine }
    }
}

impl ScriptLlm for BorrowedLlmScriptLlm<'_> {
    fn generate<'a>(
        &'a self,
        prompt: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>> {
        Box::pin(async move {
            let request = LlmRequest {
                system_prompt: SYSTEM_PROMPT.to_string(),
                user_prompt: prompt.to_string(),
                max_tokens: 512,
                temperature: 0.1,
            };
            let response = self
                .engine
                .query(&request)
                .await
                .map_err(|e| format!("LLM error: {e}"))?;
            Ok(response.text)
        })
    }
}

impl ScriptLlm for LlmScriptLlm {
    fn generate<'a>(
        &'a self,
        prompt: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>> {
        Box::pin(async move {
            let request = LlmRequest {
                system_prompt: SYSTEM_PROMPT.to_string(),
                user_prompt: prompt.to_string(),
                max_tokens: 512,
                temperature: 0.1,
            };
            let response = self
                .engine
                .query(&request)
                .await
                .map_err(|e| format!("LLM error: {e}"))?;
            Ok(response.text)
        })
    }
}

/// Deterministic mock with queued responses, one response per generation attempt.
pub struct MockScriptLlm {
    responses: Mutex<VecDeque<String>>,
    fallback: Option<String>,
}

impl MockScriptLlm {
    /// Create a mock that always returns the same response.
    pub fn once(response: impl Into<String>) -> Self {
        let response = response.into();
        Self {
            responses: Mutex::new(VecDeque::from([response.clone()])),
            fallback: Some(response),
        }
    }

    /// Create a mock that returns responses in order, repeating the last one.
    pub fn with_responses(responses: Vec<String>) -> Self {
        Self {
            fallback: responses.last().cloned(),
            responses: Mutex::new(responses.into()),
        }
    }
}

impl ScriptLlm for MockScriptLlm {
    fn generate<'a>(
        &'a self,
        _prompt: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>> {
        Box::pin(async move {
            let mut responses = self
                .responses
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match responses.pop_front().or_else(|| self.fallback.clone()) {
                Some(response) => Ok(response),
                None => Err("mock LLM has no queued responses".to_string()),
            }
        })
    }
}

/// Offline generator used when no LLM backend is configured.
pub struct OfflineScriptLlm;

impl ScriptLlm for OfflineScriptLlm {
    fn generate<'a>(
        &'a self,
        prompt: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>> {
        Box::pin(async move { Ok(offline_script(prompt)) })
    }
}

/// Build a deterministic EasyLang script without a remote or local model.
pub fn offline_script(text: &str) -> String {
    let lower = text.to_lowercase();
    let mut lines = Vec::new();
    if lower.contains("browse") || lower.contains("open") || lower.contains("site") {
        lines.push(format!(
            "browse \"{}\"",
            extract_url(text).unwrap_or("https://example.com")
        ));
    } else if lower.contains("net")
        || lower.contains("network")
        || lower.contains("http")
        || lower.contains("url")
        || lower.contains("internet")
    {
        lines.push(format!(
            "net_check \"{}\"",
            extract_url(text).unwrap_or("https://example.com")
        ));
    }
    if lower.contains("console")
        || lower.contains("command")
        || lower.contains("terminal")
        || lower.contains("run")
    {
        lines.push("console_cmd \"status\"".to_string());
    }
    if lower.contains("notify") || lower.contains("notification") || lower.contains("alert") {
        lines.push("notify \"Voice command completed\"".to_string());
    }
    if lines.is_empty() {
        lines.push("status".to_string());
    }
    lines.join("\n")
}

fn extract_url(text: &str) -> Option<&str> {
    let start = text.find("http://").or_else(|| text.find("https://"))?;
    let rest = &text[start..];
    let end = rest
        .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ')' | ',' | ';'))
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

/// Compiled output produced from a natural-language request.
pub struct GeneratedScript {
    /// Validated EasyLang source returned by the generator.
    pub script: String,
    /// Parsed workflow representation.
    pub workflow: Workflow,
    /// Compiled WASM module ready for capability analysis and execution.
    pub wasm: Vec<u8>,
    /// Capabilities inferred from the generated module.
    pub capabilities: Vec<Capability>,
    /// Number of generation attempts used before success.
    pub attempts: usize,
}

/// Parses, validates and compiles generated EasyLang with bounded retries.
pub struct ScriptGenerator<'a> {
    llm: &'a dyn ScriptLlm,
    max_attempts: usize,
}

impl<'a> ScriptGenerator<'a> {
    /// Create a generator with an explicit retry limit.
    pub fn new(llm: &'a dyn ScriptLlm, max_attempts: usize) -> Self {
        Self {
            llm,
            max_attempts: max_attempts.max(1),
        }
    }

    /// Generate and compile a script from a voice transcript.
    pub async fn generate(
        &self,
        text: &str,
        workflow_name: &str,
    ) -> Result<GeneratedScript, String> {
        let mut last_error = String::new();
        for attempt in 1..=self.max_attempts {
            let prompt = if attempt == 1 {
                text.to_string()
            } else {
                format!("{text}\nPrevious output failed validation: {last_error}\nReturn only valid EasyLang lines.")
            };
            match self.llm.generate(&prompt).await {
                Ok(script) => match compile_script(&script, workflow_name) {
                    Ok(mut generated) => {
                        generated.attempts = attempt;
                        return Ok(generated);
                    }
                    Err(e) => last_error = e,
                },
                Err(e) => last_error = e,
            }
        }
        Err(format!(
            "script generation failed after {} attempts: {last_error}",
            self.max_attempts
        ))
    }
}

fn compile_script(script: &str, workflow_name: &str) -> Result<GeneratedScript, String> {
    validate_code_only(script)?;
    let workflow = EasyLangParser::parse(script, workflow_name)
        .map_err(|errors| format!("invalid EasyLang: {errors:?}"))?;
    workflow
        .validate()
        .map_err(|errors| format!("invalid workflow: {errors:?}"))?;
    let wasm = WorkflowCompiler::compile_to_wasm(&workflow)?;
    let capabilities = AutoManifestGenerator::new()
        .from_wasm_binary(&wasm)
        .unwrap_or_default();
    Ok(GeneratedScript {
        script: script.trim().to_string(),
        workflow,
        wasm,
        capabilities,
        attempts: 1,
    })
}

fn validate_code_only(script: &str) -> Result<(), String> {
    if script.trim().is_empty() {
        return Err("empty script".to_string());
    }
    if script.contains("```") {
        return Err("markdown fences are not allowed".to_string());
    }
    for line in script.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("//") || line.starts_with('#') {
            continue;
        }
        let command_line = line
            .find(':')
            .filter(|pos| {
                let before = &line[..*pos];
                !before.is_empty()
                    && !before.chars().any(char::is_whitespace)
                    && !line[*pos..].starts_with("://")
            })
            .map(|pos| line[pos + 1..].trim())
            .unwrap_or(line);
        let command = command_line
            .split_whitespace()
            .next()
            .ok_or_else(|| "empty command".to_string())?
            .to_ascii_lowercase();
        if !ALLOWED_COMMANDS.contains(&command.as_str()) {
            return Err(format!("unsupported command: {command}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_prompt_contains_bilingual_keyword_hints() {
        for hint in [
            "проверь",
            "открой",
            "выполни",
            "уведоми",
            "check/validate",
            "open a site",
            "run/execute",
            "notify/tell",
            "regardless of the request language",
        ] {
            assert!(SYSTEM_PROMPT.contains(hint), "missing prompt hint: {hint}");
        }
    }

    #[tokio::test]
    async fn test_generator_compiles_mock_script() {
        let llm = MockScriptLlm::once("net_check \"https://example.com\"\nnotify \"done\"");
        let generator = ScriptGenerator::new(&llm, 2);
        let generated = generator
            .generate("check network then notify", "voice")
            .await
            .unwrap();
        assert_eq!(generated.attempts, 1);
        assert!(generated.capabilities.contains(&Capability::NetConnect));
        assert_eq!(&generated.wasm[0..4], b"\x00asm");
    }

    #[tokio::test]
    async fn test_generator_retries_invalid_output() {
        let llm = MockScriptLlm::with_responses(vec![
            "Sure, here is the script".to_string(),
            "browse \"https://example.com\"".to_string(),
        ]);
        let generator = ScriptGenerator::new(&llm, 3);
        let generated = generator.generate("open example", "voice").await.unwrap();
        assert_eq!(generated.attempts, 2);
        assert!(generated.script.starts_with("browse"));
    }

    #[tokio::test]
    async fn test_generator_fails_closed() {
        let llm = MockScriptLlm::once("prose only");
        let generator = ScriptGenerator::new(&llm, 1);
        assert!(generator.generate("hello", "voice").await.is_err());
    }

    #[test]
    fn test_offline_script_mapping() {
        let script = offline_script("please browse https://example.com and notify me");
        assert!(script.contains("browse \"https://example.com\""));
        assert!(script.contains("notify"));
    }
}
