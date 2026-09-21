/*
 * Copyright 2026 young1lin
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! The zai-vision adapter - `@z_ai/mcp-server` (Z.AI / Zhipu GLM vision MCP) ported native,
//! the third way in: compiled into the binary instead of a Node child, replacing the
//! `proc` def that used to spawn `node .../@z_ai/mcp-server/build/index.js`.
//!
//! What the upstream server actually is: eight tools, each a fixed system prompt plus ONE
//! multimodal chat-completions call against an OpenAI-compatible endpoint (`{base}chat/completions`,
//! ZHIPU mode = open.bigmodel.cn, ZAI mode = api.z.ai), Bearer-authed with the API key the def
//! carries as a `${...}` reference. Images ride as `image_url` content (URLs pass through, local
//! files become base64 data URLs), video as `video_url`. No state, no streaming, no OAuth - so
//! the port is an [`Engine`] with a lazily-built HTTP client and zero idle cost.
//!
//! Behavior parity is deliberate: tool names, descriptions, JSON schemas, the optional-argument
//! prompt weaving (`<language_hint>` etc.) and the system prompts (see `zai_prompts.rs`,
//! extracted verbatim) all match the deployed 0.1.5 build. The one improvement over upstream:
//! retries skip non-429 4xx (a 400 will still be a 400 a second later).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use serde_json::{json, Value};
use swiss_host::config::ServerDef;

use super::direct::Lazy;
use super::proxy::{assert_proxy_url, proxied_client};
use super::tool_server::{Engine, ServerMeta, ToolDef};
use super::zai_prompts;

/// The model the deployed 0.1.5 build defaults to (`Z_AI_VISION_MODEL || "glm-5.3-flash"`).
const DEFAULT_MODEL: &str = "glm-5.3-flash";
/// ZHIPU mode (this machine's `Z_AI_MODE=ZHIPU` def) - the open platform endpoint.
const ZHIPU_BASE: &str = "https://open.bigmodel.cn/api/paas/v4/";
/// ZAI mode - the z.ai international endpoint. Same wire shape, different key account.
const ZAI_BASE: &str = "https://api.z.ai/api/paas/v4/";
/// Upstream `Z_AI_TIMEOUT` default; vision generations are slow, this is not a typo.
const DEFAULT_TIMEOUT_MS: u64 = 300_000;
/// One attempt plus two retries, one second apart - upstream `withRetry(fn, 2, 1000)`.
const MAX_ATTEMPTS: u32 = 3;
const RETRY_DELAY: Duration = Duration::from_secs(1);
/// Upstream `MAX_IMAGE_SIZE_MB = 5`, formats jpg/jpeg/png (its validator rejects the rest).
const IMAGE_MAX_BYTES: u64 = 5 * 1024 * 1024;
/// Upstream `MAX_VIDEO_SIZE_MB = 8`. The extension is NOT validated upstream (the mime table
/// covers more than the advertised mp4/mov/m4v); the port keeps that tolerance.
const VIDEO_MAX_BYTES: u64 = 8 * 1024 * 1024;
/// Sampling defaults from `getVisionConfig` - kept identical so answers do not drift between
/// the child process and the port.
const TEMPERATURE: f64 = 0.8;
const TOP_P: f64 = 0.6;
const MAX_TOKENS: u64 = 131_072;

/// The engine a [`super::direct::DirectAdapter`] wraps: eight fixed tools, one shared HTTP
/// client behind a `Lazy` (proxied when the def names a proxy), nothing to open or close.
pub struct ZaiEngine {
    def: ServerDef,
    /// The MCP's registry name, followed across renames - the call log is filed under the
    /// engine's view of this cell, read fresh at call time.
    name: Arc<std::sync::RwLock<String>>,
    /// Already resolved (`resolve_def_checked` ran in `make_adapter`), so the literal never
    /// touches the persisted def - only this copy in memory.
    api_key: String,
    base_url: String,
    model: String,
    timeout_ms: u64,
    client: Arc<Lazy<reqwest::Client>>,
}

impl ZaiEngine {
    /// `def` must be the resolve_def() clone make_adapter hands over: an empty apiKey is a
    /// configuration error, refused here rather than at first call.
    pub fn new(def: &ServerDef, name: &str) -> Result<Self, String> {
        let api_key = def.get_str("apiKey").unwrap_or("").trim().to_string();
        if api_key.is_empty() {
            return Err(
                "apiKey is required (a ${ENV_VAR} or ${secret://name} reference)".to_string(),
            );
        }
        // An explicit baseUrl wins (self-hosted GLM endpoints, and the test fake); otherwise
        // the mode picks one of the two official bases, ZHIPU when unset.
        let base_url = match def.get_str("baseUrl") {
            Some(url) if !url.trim().is_empty() => {
                let url = url.trim().to_string();
                if !(url.starts_with("http://") || url.starts_with("https://")) {
                    return Err(format!("baseUrl must be http(s)://, got {url:?}"));
                }
                url
            }
            _ => {
                let mode = def
                    .get_str("mode")
                    .unwrap_or("ZHIPU")
                    .trim()
                    .to_ascii_uppercase();
                match mode.as_str() {
                    "ZHIPU" => ZHIPU_BASE.to_string(),
                    "ZAI" | "Z_AI" => ZAI_BASE.to_string(),
                    other => {
                        return Err(format!("unknown mode {other:?} (supported: ZHIPU | ZAI)"))
                    }
                }
            }
        };
        let model = def
            .get_str("model")
            .filter(|m| !m.trim().is_empty())
            .unwrap_or(DEFAULT_MODEL)
            .to_string();
        let timeout_ms = def
            .get("timeoutMs")
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && *v > 0.0)
            .map(|v| v as u64)
            .unwrap_or(DEFAULT_TIMEOUT_MS);
        // Validated here, used per request - a bad proxy is a start error, same as rest.
        let proxy_url = def
            .get_str("proxy")
            .filter(|p| !p.is_empty())
            .map(assert_proxy_url)
            .transpose()?;
        let client = Arc::new(Lazy::new(move || {
            let proxy_url = proxy_url.clone();
            Box::pin(async move {
                match &proxy_url {
                    Some(url) => proxied_client(url),
                    None => reqwest::Client::builder()
                        .build()
                        .map_err(|err| err.to_string()),
                }
            }) as super::direct::BoxFut<Result<reqwest::Client, String>>
        }));
        Ok(Self {
            def: def.clone(),
            name: Arc::new(std::sync::RwLock::new(name.to_string())),
            api_key,
            base_url,
            model,
            timeout_ms,
            client,
        })
    }

    /// One multimodal chat completion. Returns the message text; every tool result is that
    /// text, exactly as upstream `visionCompletions` returned `choices[0].message.content`.
    /// `system` is None for analyze_video, which upstream sends with no system message.
    async fn vision(
        &self,
        system: Option<&str>,
        user: &str,
        parts: Vec<Value>,
    ) -> Result<String, String> {
        let url = format!("{}chat/completions", self.base_url);
        // json! has no spread syntax; the text part is appended to the media parts by hand.
        let mut content = parts;
        content.push(json!({ "type": "text", "text": user }));
        let user_message = json!({ "role": "user", "content": content });
        let messages: Vec<Value> = match system {
            Some(system) => vec![json!({ "role": "system", "content": system }), user_message],
            None => vec![user_message],
        };
        let body = json!({
            "model": self.model,
            "messages": messages,
            "thinking": { "type": "enabled" },
            "stream": false,
            "temperature": TEMPERATURE,
            "top_p": TOP_P,
            "max_tokens": MAX_TOKENS,
        });
        let client = self.client.get().await?;
        let mut last_err = String::new();
        for attempt in 1..=MAX_ATTEMPTS {
            let outcome = client
                .post(&url)
                .timeout(Duration::from_millis(self.timeout_ms))
                .bearer_auth(&self.api_key)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .json(&body)
                .send()
                .await;
            // Every arm that could still succeed returns out of the loop; what falls through
            // is the error message for this attempt.
            last_err = match outcome {
                Ok(response) => {
                    let status = response.status();
                    let text = response.text().await.unwrap_or_default();
                    if !status.is_success() {
                        format!("HTTP {}: {text}", status.as_u16())
                    } else {
                        match serde_json::from_str::<Value>(&text) {
                            Ok(parsed) => match parsed.pointer("/choices/0/message/content").and_then(Value::as_str) {
                                Some(content) => return Ok(content.to_string()),
                                None => return Err(format!("invalid API response: missing choices[0].message.content: {parsed}")),
                            },
                            Err(err) => format!("reading the API response failed: {err}"),
                        }
                    }
                }
                Err(err) => format!("API call failed: {err}"),
            };
            // A 4xx other than 429 will fail identically on the next attempt - upstream
            // retried everything; this port does not spend the round trips.
            let retryable = !last_err.starts_with("HTTP 4") || last_err.starts_with("HTTP 429");
            if attempt < MAX_ATTEMPTS && retryable {
                tokio::time::sleep(RETRY_DELAY).await;
                continue;
            }
            break;
        }
        Err(last_err)
    }

    /// One `image_url` content part: http(s) URLs pass through untouched; a local path is
    /// validated (exists, <=5 MB, jpg/jpeg/png) and encoded as a base64 data URL - the same
    /// two shapes the deployed child process produced.
    async fn image_part(source: &str) -> Result<Value, String> {
        let url = if is_url(source) {
            source.to_string()
        } else {
            let ext = std::path::Path::new(source)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if !matches!(ext.as_str(), "jpg" | "jpeg" | "png") {
                return Err(format!(
                    "Unsupported image format: .{ext}. Supported formats: .jpg, .jpeg, .png"
                ));
            }
            let meta = tokio::fs::metadata(source)
                .await
                .map_err(|_| format!("Image file not found: {source}"))?;
            if meta.len() > IMAGE_MAX_BYTES {
                return Err(format!(
                    "Image file too large: {:.2}MB. Maximum allowed: 5MB",
                    meta.len() as f64 / (1024.0 * 1024.0)
                ));
            }
            let bytes = tokio::fs::read(source)
                .await
                .map_err(|err| format!("reading {source} failed: {err}"))?;
            let mime = if ext == "png" {
                "image/png"
            } else {
                "image/jpeg"
            };
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            format!("data:{mime};base64,{b64}")
        };
        Ok(json!({ "type": "image_url", "image_url": { "url": url } }))
    }

    /// One `video_url` content part - same passthrough/encode split, video limits and mime
    /// table (unknown extensions read as video/mp4, as upstream's fallback does).
    async fn video_part(source: &str) -> Result<Value, String> {
        let url = if is_url(source) {
            source.to_string()
        } else {
            let meta = tokio::fs::metadata(source)
                .await
                .map_err(|_| format!("Video file not found: {source}"))?;
            if meta.len() > VIDEO_MAX_BYTES {
                return Err(format!(
                    "Video file size ({:.2}MB) exceeds maximum allowed size (8MB)",
                    meta.len() as f64 / (1024.0 * 1024.0)
                ));
            }
            let bytes = tokio::fs::read(source)
                .await
                .map_err(|err| format!("reading {source} failed: {err}"))?;
            let ext = std::path::Path::new(source)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let mime = match ext.as_str() {
                "avi" => "video/x-msvideo",
                "mov" => "video/quicktime",
                "wmv" => "video/x-ms-wmv",
                "m4v" => "video/x-m4v",
                _ => "video/mp4",
            };
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            format!("data:{mime};base64,{b64}")
        };
        Ok(json!({ "type": "video_url", "video_url": { "url": url } }))
    }
}

/// Upstream `isUrl`: http/https scheme, nothing else counts as remote.
fn is_url(source: &str) -> bool {
    source.starts_with("http://") || source.starts_with("https://")
}

/// A required string argument, non-empty after trimming (upstream `nonEmptyString`).
fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    let value = args
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{key} is required"))?;
    if value.trim().is_empty() {
        return Err(format!("{key} must not be empty"));
    }
    Ok(value)
}

/// An optional string argument that counts only when non-empty (upstream's `if (x && x.trim())`).
fn arg_opt<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

/// The eight tool declarations, ported from the upstream `server.tool(...)` calls: names,
/// descriptions and JSON Schemas verbatim - a client written against the child process
/// must not see anything move.
fn tool_defs() -> Vec<ToolDef> {
    let image_source =
        || json!({ "type": "string", "description": "Local file path or remote URL to the image" });
    vec![
        ToolDef {
            name: "ui_to_artifact".into(),
            description: "Convert UI screenshots into various artifacts: code, prompts, design specifications, or descriptions.\n\nUse this tool ONLY when the user wants to:\n- Generate frontend code from UI design (output_type='code')\n- Create AI prompts for UI generation (output_type='prompt')\n- Extract design specifications (output_type='spec')\n- Get natural language description of the UI (output_type='description')\n\nDo NOT use for: screenshots containing text/code to extract, error messages, diagrams, or data visualizations.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "image_source": image_source(),
                    "output_type": { "type": "string", "enum": ["code", "prompt", "spec", "description"], "description": "Type of output to generate. Options: 'code' (generate frontend code), 'prompt' (generate AI prompt for recreating this UI), 'spec' (generate design specification document), 'description' (natural language description of the UI)." },
                    "prompt": { "type": "string", "description": "Detailed instructions describing what to generate from this UI image. Should clearly state the desired output and any specific requirements." },
                },
                "required": ["image_source", "output_type", "prompt"],
            }),
        },
        ToolDef {
            name: "extract_text_from_screenshot".into(),
            description: "Extract and recognize text from screenshots using advanced OCR capabilities.\n\nUse this tool ONLY when the user has a screenshot containing text and wants to extract it.\nThis tool specializes in OCR for code, terminal output, documentation, and general text extraction.\n\nDo NOT use for: UI design conversion, error diagnosis, or diagram understanding.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "image_source": image_source(),
                    "prompt": { "type": "string", "description": "Instructions for text extraction. Specify what type of text to extract and any formatting requirements." },
                    "programming_language": { "type": "string", "description": "Optional: specify the programming language if the screenshot contains code (e.g., 'python', 'javascript', 'java'). Leave empty for auto-detection or non-code text." },
                },
                "required": ["image_source", "prompt"],
            }),
        },
        ToolDef {
            name: "diagnose_error_screenshot".into(),
            description: "Diagnose and analyze error messages, stack traces, and exception screenshots.\n\nUse this tool ONLY when the user has an error screenshot and needs help understanding or fixing it.\nThis tool specializes in error analysis and provides actionable solutions.\n\nDo NOT use for: code extraction, UI analysis, or diagram understanding.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "image_source": image_source(),
                    "prompt": { "type": "string", "description": "Description of what you need help with regarding this error. Include any relevant context about when it occurred." },
                    "context": { "type": "string", "description": "Optional: additional context about when the error occurred (e.g., 'during npm install', 'when running the app', 'after deployment'). Helps with more accurate diagnosis." },
                },
                "required": ["image_source", "prompt"],
            }),
        },
        ToolDef {
            name: "understand_technical_diagram".into(),
            description: "Analyze and explain technical diagrams including architecture diagrams, flowcharts, UML, ER diagrams, and system design diagrams.\n\nUse this tool ONLY when the user has a technical diagram and wants to understand its structure or components.\nThis tool specializes in interpreting visual technical documentation.\n\nDo NOT use for: UI screenshots, error messages, or data visualizations/charts.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "image_source": image_source(),
                    "prompt": { "type": "string", "description": "What you want to understand or extract from this diagram." },
                    "diagram_type": { "type": "string", "description": "Optional: specify the diagram type if known (e.g., 'architecture', 'flowchart', 'uml', 'er-diagram', 'sequence'). Leave empty for auto-detection." },
                },
                "required": ["image_source", "prompt"],
            }),
        },
        ToolDef {
            name: "analyze_data_visualization".into(),
            description: "Analyze data visualizations, charts, graphs, and dashboards to extract insights and trends.\n\nUse this tool ONLY when the user has a data visualization image and wants to understand the data patterns or metrics.\nThis tool specializes in interpreting visual data representations.\n\nDo NOT use for: UI mockups, error messages, or technical architecture diagrams.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "image_source": image_source(),
                    "prompt": { "type": "string", "description": "What insights or information you want to extract from this visualization." },
                    "analysis_focus": { "type": "string", "description": "Optional: specify what to focus on (e.g., 'trends', 'anomalies', 'comparisons', 'performance metrics'). Leave empty for comprehensive analysis." },
                },
                "required": ["image_source", "prompt"],
            }),
        },
        ToolDef {
            name: "ui_diff_check".into(),
            description: "Compare two UI screenshots to identify visual differences and implementation discrepancies.\n\nUse this tool ONLY when the user wants to compare an expected/reference UI with an actual implementation.\nThis tool is specialized for UI quality assurance and design-to-implementation verification.\n\nDo NOT use for: general image comparison, error diagnosis, or analyzing single UIs.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "expected_image_source": { "type": "string", "description": "Local file path or remote URL to the image" },
                    "actual_image_source": { "type": "string", "description": "Local file path or remote URL to the image" },
                    "prompt": { "type": "string", "description": "Instructions for the comparison. Specify what aspects to focus on or what level of detail is needed." },
                },
                "required": ["expected_image_source", "actual_image_source", "prompt"],
            }),
        },
        ToolDef {
            name: "analyze_image".into(),
            description: "General-purpose image analysis for scenarios not covered by specialized tools.\n\nUse this tool as a FALLBACK when none of the other specialized tools (ui_to_artifact, extract_text_from_screenshot, diagnose_error_screenshot, understand_technical_diagram, analyze_data_visualization, ui_diff_check) fit the user's need.\nThis tool provides flexible image understanding for any visual content.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "image_source": image_source(),
                    "prompt": { "type": "string", "description": "Detailed description of what you want to analyze, extract, or understand from the image. Be specific about your requirements." },
                },
                "required": ["image_source", "prompt"],
            }),
        },
        ToolDef {
            name: "analyze_video".into(),
            description: "Analyze video content using advanced AI vision models.\n\nUse this tool when the user wants to:\n- Understand what happens in a video\n- Extract key moments or actions from a video\n- Analyze video content, scenes, or sequences\n- Get descriptions of video footage\n- Identify objects, people, or activities in video\n\nSupports both local files and remote URL. Maximum file size: 8MB. Supports MP4, MOV, M4V formats.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "video_source": { "type": "string", "description": "Local file path or remote URL to the video (supports MP4, MOV, M4V)" },
                    "prompt": { "type": "string", "description": "Detailed text prompt describing what to analyze, extract, or understand from the video" },
                },
                "required": ["video_source", "prompt"],
            }),
        },
    ]
}

#[async_trait]
impl Engine for ZaiEngine {
    fn kind(&self) -> &'static str {
        "zai-vision"
    }

    fn tools(&self) -> Vec<ToolDef> {
        tool_defs()
    }

    async fn call(&self, tool: &str, args: &Value) -> Result<Value, String> {
        match tool {
            "ui_to_artifact" => {
                let output_type = arg_str(args, "output_type")?.to_ascii_lowercase();
                let system = match output_type.as_str() {
                    "code" => zai_prompts::UI_TO_ARTIFACT_CODE,
                    "prompt" => zai_prompts::UI_TO_ARTIFACT_PROMPT,
                    "spec" => zai_prompts::UI_TO_ARTIFACT_SPEC,
                    "description" => zai_prompts::UI_TO_ARTIFACT_DESCRIPTION,
                    other => {
                        return Err(format!(
                            "Invalid output_type '{other}'. Must be one of: code, prompt, spec, description"
                        ))
                    }
                };
                let part = Self::image_part(arg_str(args, "image_source")?).await?;
                let text = self
                    .vision(Some(system), arg_str(args, "prompt")?, vec![part])
                    .await?;
                Ok(Value::String(text))
            }
            "extract_text_from_screenshot" => {
                let mut user = arg_str(args, "prompt")?.to_string();
                if let Some(lang) = arg_opt(args, "programming_language") {
                    user.push_str(&format!(
                        "\n\n<language_hint>The code is in {lang}.</language_hint>"
                    ));
                }
                let part = Self::image_part(arg_str(args, "image_source")?).await?;
                let text = self
                    .vision(Some(zai_prompts::TEXT_EXTRACTION), &user, vec![part])
                    .await?;
                Ok(Value::String(text))
            }
            "diagnose_error_screenshot" => {
                let mut user = arg_str(args, "prompt")?.to_string();
                if let Some(context) = arg_opt(args, "context") {
                    user.push_str(&format!(
                        "\n\n<error_context>This error occurred {context}.</error_context>"
                    ));
                }
                let part = Self::image_part(arg_str(args, "image_source")?).await?;
                let text = self
                    .vision(Some(zai_prompts::ERROR_DIAGNOSIS), &user, vec![part])
                    .await?;
                Ok(Value::String(text))
            }
            "understand_technical_diagram" => {
                let mut user = arg_str(args, "prompt")?.to_string();
                if let Some(diagram_type) = arg_opt(args, "diagram_type") {
                    user.push_str(&format!(
                        "\n\n<diagram_type_hint>This is a {diagram_type} diagram.</diagram_type_hint>"
                    ));
                }
                let part = Self::image_part(arg_str(args, "image_source")?).await?;
                let text = self
                    .vision(Some(zai_prompts::DIAGRAM), &user, vec![part])
                    .await?;
                Ok(Value::String(text))
            }
            "analyze_data_visualization" => {
                let mut user = arg_str(args, "prompt")?.to_string();
                if let Some(focus) = arg_opt(args, "analysis_focus") {
                    user.push_str(&format!(
                        "\n\n<analysis_focus>Focus particularly on: {focus}.</analysis_focus>"
                    ));
                }
                let part = Self::image_part(arg_str(args, "image_source")?).await?;
                let text = self
                    .vision(Some(zai_prompts::DATA_VIZ), &user, vec![part])
                    .await?;
                Ok(Value::String(text))
            }
            "ui_diff_check" => {
                // The images arrive in declaration order; the prefix tells the model which is
                // which - ported verbatim from upstream's enhancedPrompt.
                let user = format!(
                    "<images>\nThe first image is the EXPECTED/REFERENCE design (the target).\nThe second image is the ACTUAL/CURRENT implementation (what needs to be checked).\n</images>\n\n{}",
                    arg_str(args, "prompt")?
                );
                let expected = Self::image_part(arg_str(args, "expected_image_source")?).await?;
                let actual = Self::image_part(arg_str(args, "actual_image_source")?).await?;
                let text = self
                    .vision(Some(zai_prompts::UI_DIFF), &user, vec![expected, actual])
                    .await?;
                Ok(Value::String(text))
            }
            "analyze_image" => {
                let part = Self::image_part(arg_str(args, "image_source")?).await?;
                let text = self
                    .vision(
                        Some(zai_prompts::GENERAL_IMAGE),
                        arg_str(args, "prompt")?,
                        vec![part],
                    )
                    .await?;
                Ok(Value::String(text))
            }
            "analyze_video" => {
                // No system message upstream: the video service posts the bare multimodal
                // user message (video-analysis.js has no prompt template of its own).
                let part = Self::video_part(arg_str(args, "video_source")?).await?;
                let text = self
                    .vision(None, arg_str(args, "prompt")?, vec![part])
                    .await?;
                Ok(Value::String(text))
            }
            other => Err(format!("unknown tool: {other}")),
        }
    }

    fn meta(&self) -> ServerMeta {
        let name = self.name.read().ok().map(|g| g.clone());
        ServerMeta {
            name,
            description: self.def.get_str("description").map(str::to_string),
            target: Some(self.base_url.clone()),
            limits: None,
        }
    }

    // Deliberately NO ping (the Engine default stays): every tool call spends real tokens on a
    // metered API - the registry's health probe must not. "unknown" is the honest state.

    fn rename(&self, name: &str) {
        if let Ok(mut current) = self.name.write() {
            *current = name.to_string();
        }
    }
}
#[cfg(test)]
mod tests {
    //! The port is pinned against a fake OpenAI-compatible chat-completions server, the same
    //! way rest.rs drives its engine against a real local listener: the request SHAPE (auth,
    //! model, messages, content parts) and the parity decisions (no system message for video,
    //! hint weaving, retry policy) are what these tests lock.
    use super::*;

    fn def(v: Value) -> ServerDef {
        match v {
            Value::Object(o) => ServerDef(o),
            other => panic!("a server def is an object, got {other}"),
        }
    }

    /// One captured request: the bearer token and the parsed JSON body.
    #[derive(Clone)]
    struct Captured {
        auth: String,
        body: Value,
    }

    /// A fake chat-completions endpoint that replies with `script` in order (one entry per
    /// request) and records everything it saw.
    async fn fake_chat(
        script: Vec<(u16, &'static str)>,
    ) -> (String, Arc<std::sync::Mutex<Vec<Captured>>>) {
        use axum::body::Body;
        use axum::extract::Request;
        use axum::http::StatusCode;

        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let script = Arc::new(script);
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let state = (seen.clone(), script.clone(), calls.clone());
        let app = axum::Router::new().route(
            "/chat/completions",
            axum::routing::post(move |req: Request<Body>| {
                let (seen, script, calls) = state.clone();
                async move {
                    let auth = req
                        .headers()
                        .get(reqwest::header::AUTHORIZATION)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_string();
                    let bytes = axum::body::to_bytes(req.into_body(), 32 * 1024 * 1024)
                        .await
                        .unwrap_or_default();
                    let body = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
                    seen.lock().expect("seen").push(Captured { auth, body });
                    let i = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let (status, payload) = script.get(i).cloned().unwrap_or((200, "{}"));
                    let text = format!(
                        "{{\"choices\":[{{\"message\":{{\"content\":{payload:?}}}}}]}}",
                        payload = payload
                    );
                    (
                        StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
                        [(reqwest::header::CONTENT_TYPE, "application/json")],
                        text,
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://127.0.0.1:{port}/"), seen)
    }

    fn engine_against(base: &str) -> ZaiEngine {
        ZaiEngine::new(
            &def(json!({ "type": "zai-vision", "apiKey": "test-key", "baseUrl": base })),
            "zai-test",
        )
        .expect("engine")
    }

    #[test]
    fn the_eight_upstream_tools_survive_with_their_schemas() {
        let engine = engine_against("http://127.0.0.1:9/");
        let tools = engine.tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "ui_to_artifact",
                "extract_text_from_screenshot",
                "diagnose_error_screenshot",
                "understand_technical_diagram",
                "analyze_data_visualization",
                "ui_diff_check",
                "analyze_image",
                "analyze_video",
            ],
            "the deployed child's clients see exactly these names"
        );
        let ui = tools[0].input_schema.clone();
        assert_eq!(
            ui["properties"]["output_type"]["enum"],
            json!(["code", "prompt", "spec", "description"])
        );
    }

    #[test]
    fn the_constructor_decides_the_endpoint_and_refuses_a_keyless_def() {
        let zhipu = ZaiEngine::new(
            &def(json!({ "type": "zai-vision", "apiKey": "k", "mode": "ZHIPU" })),
            "m",
        )
        .expect("zhipu");
        assert!(zhipu.base_url.starts_with("https://open.bigmodel.cn"));
        let zai = ZaiEngine::new(
            &def(json!({ "type": "zai-vision", "apiKey": "k", "mode": "ZAI" })),
            "m",
        )
        .expect("zai");
        assert!(zai.base_url.starts_with("https://api.z.ai"));
        // No mode at all = ZHIPU, the upstream default.
        let default = ZaiEngine::new(&def(json!({ "type": "zai-vision", "apiKey": "k" })), "m")
            .expect("default");
        assert_eq!(default.model, "glm-5.3-flash");
        assert!(ZaiEngine::new(
            &def(json!({ "type": "zai-vision", "apiKey": "k", "mode": "nope" })),
            "m"
        )
        .err()
        .expect("bad mode")
        .contains("unknown mode"));
        assert!(ZaiEngine::new(&def(json!({ "type": "zai-vision" })), "m")
            .err()
            .expect("keyless")
            .contains("apiKey is required"));
    }

    #[tokio::test]
    async fn analyze_image_posts_the_upstream_body_shape() {
        let (base, seen) = fake_chat(vec![(200, "pong")]).await;
        let engine = engine_against(&base);
        let out = engine
            .call(
                "analyze_image",
                &json!({ "image_source": "https://example.com/a.png", "prompt": "what is this" }),
            )
            .await
            .expect("call");
        assert_eq!(out, json!("pong"));
        let seen = seen.lock().expect("seen");
        let captured = seen.last().expect("one request");
        assert_eq!(captured.auth, "Bearer test-key");
        assert_eq!(captured.body["model"], json!("glm-5.3-flash"));
        assert_eq!(captured.body["thinking"]["type"], json!("enabled"));
        assert_eq!(captured.body["stream"], json!(false));
        assert_eq!(captured.body["temperature"], json!(0.8));
        assert_eq!(captured.body["top_p"], json!(0.6));
        assert_eq!(captured.body["max_tokens"], json!(131072));
        // System message is the verbatim general-image prompt; content carries the image part
        // first and the text part last, upstream's order.
        assert_eq!(captured.body["messages"][0]["role"], json!("system"));
        assert_eq!(
            captured.body["messages"][0]["content"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(40)
                .collect::<String>(),
            zai_prompts::GENERAL_IMAGE
                .chars()
                .take(40)
                .collect::<String>()
        );
        let content = &captured.body["messages"][1]["content"];
        assert_eq!(content[0]["type"], json!("image_url"));
        assert_eq!(
            content[0]["image_url"]["url"],
            json!("https://example.com/a.png")
        );
        assert_eq!(content[1]["type"], json!("text"));
        assert_eq!(content[1]["text"], json!("what is this"));
    }

    #[tokio::test]
    async fn a_local_image_becomes_a_base64_data_url() {
        let dir = std::env::temp_dir().join(format!("zai-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("shot.png");
        let magic: &[u8] = &[0x89, b'P', b'N', b'G', 1, 2, 3, 4];
        std::fs::write(&path, magic).expect("write");
        let (base, seen) = fake_chat(vec![(200, "ok")]).await;
        let engine = engine_against(&base);
        engine
            .call(
                "analyze_image",
                &json!({ "image_source": path.to_string_lossy(), "prompt": "describe" }),
            )
            .await
            .expect("call");
        // Scoped guard: the assertions below await again, and a std Mutex must not ride an
        // await point.
        let url = {
            let seen = seen.lock().expect("seen");
            seen.last().expect("request").body["messages"][1]["content"][0]["image_url"]["url"]
                .as_str()
                .expect("url")
                .to_string()
        };
        let (head, b64) = url.split_once(',').expect("data url");
        assert_eq!(head, "data:image/png;base64");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .expect("base64");
        assert_eq!(decoded, magic);
        // A wrong extension never reaches the API.
        let bad = dir.join("shot.gif");
        std::fs::write(&bad, magic).expect("write");
        let err = engine
            .call(
                "analyze_image",
                &json!({ "image_source": bad.to_string_lossy(), "prompt": "x" }),
            )
            .await
            .unwrap_err();
        assert!(err.contains("Unsupported image format"), "{err}");
        let missing = engine
            .call(
                "analyze_image",
                &json!({ "image_source": dir.join("nope.png").to_string_lossy(), "prompt": "x" }),
            )
            .await
            .unwrap_err();
        assert!(missing.contains("not found"), "{missing}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn optional_hints_are_woven_into_the_user_prompt() {
        let (base, seen) = fake_chat(vec![(200, "ok")]).await;
        let engine = engine_against(&base);
        engine
            .call(
                "extract_text_from_screenshot",
                &json!({
                    "image_source": "https://example.com/s.png",
                    "prompt": "extract the failing line",
                    "programming_language": "rust"
                }),
            )
            .await
            .expect("call");
        let seen = seen.lock().expect("seen");
        let captured = seen.last().expect("request");
        let text = captured.body["messages"][1]["content"]
            .as_array()
            .expect("content")
            .last()
            .expect("text part")["text"]
            .as_str()
            .expect("text");
        assert_eq!(
            text,
            "extract the failing line\n\n<language_hint>The code is in rust.</language_hint>"
        );
    }

    #[tokio::test]
    async fn ui_diff_sends_two_images_in_order_behind_the_prefix() {
        let (base, seen) = fake_chat(vec![(200, "ok")]).await;
        let engine = engine_against(&base);
        engine
            .call(
                "ui_diff_check",
                &json!({
                    "expected_image_source": "https://example.com/expected.png",
                    "actual_image_source": "https://example.com/actual.png",
                    "prompt": "find regressions"
                }),
            )
            .await
            .expect("call");
        let seen = seen.lock().expect("seen");
        let content: Vec<Value> = seen.last().expect("request").body["messages"][1]["content"]
            .as_array()
            .expect("content")
            .clone();
        assert_eq!(
            content[0]["image_url"]["url"],
            json!("https://example.com/expected.png")
        );
        assert_eq!(
            content[1]["image_url"]["url"],
            json!("https://example.com/actual.png")
        );
        let text = content.last().expect("text")["text"]
            .as_str()
            .expect("text");
        assert!(text.starts_with("<images>"), "{text}");
        assert!(text.contains("EXPECTED/REFERENCE"), "{text}");
        assert!(text.ends_with("find regressions"), "{text}");
    }

    #[tokio::test]
    async fn video_posts_no_system_message_and_a_video_url_part() {
        let (base, seen) = fake_chat(vec![(200, "ok")]).await;
        let engine = engine_against(&base);
        engine
            .call(
                "analyze_video",
                &json!({ "video_source": "https://example.com/clip.mp4", "prompt": "what happens" }),
            )
            .await
            .expect("call");
        let seen = seen.lock().expect("seen");
        let body = seen.last().expect("request").body.clone();
        // Upstream's video service posts the bare user message - no system prompt at all.
        assert_eq!(body["messages"].as_array().expect("messages").len(), 1);
        assert_eq!(body["messages"][0]["role"], json!("user"));
        let content = body["messages"][0]["content"].as_array().expect("content");
        assert_eq!(content[0]["type"], json!("video_url"));
        assert_eq!(
            content[0]["video_url"]["url"],
            json!("https://example.com/clip.mp4")
        );
    }

    #[tokio::test]
    async fn a_4xx_propagates_immediately_and_a_5xx_is_retried() {
        let (base, seen) = fake_chat(vec![(401, "bad key")]).await;
        let engine = engine_against(&base);
        let err = engine
            .call(
                "analyze_image",
                &json!({ "image_source": "https://example.com/a.png", "prompt": "x" }),
            )
            .await
            .unwrap_err();
        assert!(err.contains("HTTP 401") && err.contains("bad key"), "{err}");
        assert_eq!(seen.lock().expect("seen").len(), 1, "no retry on a 4xx");

        let (base, seen) = fake_chat(vec![(500, "boom"), (200, "recovered")]).await;
        let engine = engine_against(&base);
        let out = engine
            .call(
                "analyze_image",
                &json!({ "image_source": "https://example.com/a.png", "prompt": "x" }),
            )
            .await
            .expect("retried");
        assert_eq!(out, json!("recovered"));
        assert_eq!(seen.lock().expect("seen").len(), 2, "one retry after a 5xx");
    }

    #[test]
    fn ui_to_artifact_output_type_selects_the_system_prompt() {
        // The four variants differ from their first line on; picking by output_type is the
        // whole feature. Checked without a server: the mapping is compiled in.
        assert!(zai_prompts::UI_TO_ARTIFACT_CODE.starts_with("You are a senior frontend engineer"));
        assert!(zai_prompts::UI_TO_ARTIFACT_DESCRIPTION.starts_with("You are a UX writer"));
        assert_ne!(
            zai_prompts::UI_TO_ARTIFACT_CODE,
            zai_prompts::UI_TO_ARTIFACT_PROMPT
        );
    }
}
