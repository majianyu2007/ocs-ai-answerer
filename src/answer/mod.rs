//! Question model and the answering pipeline: prompt → provider → parse → OCS-formatted answer.

pub mod normalize;
pub mod ocs;
pub mod prompt;

use std::collections::HashSet;
use std::sync::LazyLock;
use std::time::Instant;

use regex::Regex;

use crate::config::{AnswerConfig, ProviderKind};
use crate::error::AppError;
use crate::provider::AiClient;
use normalize::{format_for_ocs, parse_model_output};
use prompt::PromptBuilder;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionType {
    Single,
    Multiple,
    Judgement,
    Completion,
    Unknown,
}

impl QuestionType {
    /// Accepts the OCS type names plus common aliases; anything else is `Unknown`.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "single" | "radio" | "single_choice" | "单选" | "单选题" => Self::Single,
            "multiple" | "multi" | "checkbox" | "multiple_choice" | "多选" | "多选题" => {
                Self::Multiple
            }
            "judgement" | "judgment" | "judge" | "truefalse" | "true_false" | "判断" | "判断题" => {
                Self::Judgement
            }
            "completion" | "blank" | "fill" | "fill_blank" | "填空" | "填空题" => {
                Self::Completion
            }
            _ => Self::Unknown,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Multiple => "multiple",
            Self::Judgement => "judgement",
            Self::Completion => "completion",
            Self::Unknown => "unknown",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Single => "单选题",
            Self::Multiple => "多选题",
            Self::Judgement => "判断题",
            Self::Completion => "填空题",
            Self::Unknown => "未知题型",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionOption {
    /// The option as OCS sees it (one line of `${options}`), used to simulate OCS matching.
    pub raw: String,
    /// Option content with any leading label (`A.`, `B、`, …) removed.
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct Question {
    pub title: String,
    pub qtype: QuestionType,
    /// The type as sent by the client, before inference.
    pub requested_type: QuestionType,
    pub options: Vec<QuestionOption>,
}

static OPTION_LABEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*[(（]?([A-Za-z])\s*(?:[.．、:：)）]|\s)\s*(.*)$").unwrap());
static BARE_LABEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*[(（]?([A-Z])\s*[.．、:：)）]?\s*$").unwrap());

impl Question {
    pub fn new(title: &str, qtype: &str, options: Vec<String>) -> Self {
        let options = parse_options(options);
        let requested_type = QuestionType::parse(qtype);
        let qtype = match requested_type {
            QuestionType::Unknown => infer_type(&options),
            t => t,
        };
        Self {
            title: title.trim().to_string(),
            qtype,
            requested_type,
            options,
        }
    }

    /// Option letter by index, matching OCS's `charCodeAt(0) - 65` fallback.
    pub fn letter(index: usize) -> String {
        if index < 26 {
            ((b'A' + index as u8) as char).to_string()
        } else {
            format!("#{}", index + 1)
        }
    }

    pub fn raw_options(&self) -> Vec<String> {
        self.options.iter().map(|o| o.raw.clone()).collect()
    }

    pub fn is_choice(&self) -> bool {
        !self.options.is_empty()
            && matches!(
                self.qtype,
                QuestionType::Single | QuestionType::Multiple | QuestionType::Unknown
            )
    }
}

/// Splits `${options}` into options. OCS joins option elements with `\n`; when an element's
/// `innerText` itself spans two lines (`A\nHTML`) the bare label line is merged back.
pub fn parse_options(input: Vec<String>) -> Vec<QuestionOption> {
    let lines: Vec<String> = input
        .iter()
        .flat_map(|s| s.lines().map(str::to_string).collect::<Vec<_>>())
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    let mut merged: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let line = &lines[i];
        if BARE_LABEL.is_match(line)
            && let Some(next) = lines.get(i + 1)
            && !BARE_LABEL.is_match(next)
        {
            merged.push(format!("{line} {next}"));
            i += 2;
            continue;
        }
        merged.push(line.clone());
        i += 1;
    }

    let mut seen = HashSet::new();
    merged
        .into_iter()
        .filter(|l| seen.insert(l.clone()))
        .enumerate()
        .map(|(i, raw)| {
            // Only strip a label that matches the option position, so "I think so" stays intact.
            let text = OPTION_LABEL
                .captures(&raw)
                .filter(|c| c[1].eq_ignore_ascii_case(&Question::letter(i)))
                .map(|c| c[2].trim().to_string())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| raw.clone());
            QuestionOption { raw, text }
        })
        .collect()
}

fn infer_type(options: &[QuestionOption]) -> QuestionType {
    if options.is_empty() {
        return QuestionType::Completion;
    }
    let texts: Vec<String> = options.iter().map(|o| o.raw.clone()).collect();
    if ocs::is_judgement_options(&texts) {
        return QuestionType::Judgement;
    }
    // Choice question whose cardinality is unknown; the prompt asks for a letter array.
    QuestionType::Unknown
}

/// Result of answering one question.
#[derive(Debug, Clone)]
pub struct AnswerOutcome {
    pub question: String,
    pub answer: String,
    pub qtype: QuestionType,
    pub explanation: Option<String>,
    pub provider: ProviderKind,
    pub model: String,
    pub elapsed_ms: u64,
}

#[derive(Clone)]
pub struct Answerer {
    client: AiClient,
    prompts: PromptBuilder,
    cfg: AnswerConfig,
}

impl Answerer {
    pub fn new(client: AiClient, cfg: &AnswerConfig) -> Self {
        Self {
            client,
            prompts: PromptBuilder::new(cfg),
            cfg: cfg.clone(),
        }
    }

    pub fn client(&self) -> &AiClient {
        &self.client
    }

    pub async fn answer(&self, question: &Question) -> Result<AnswerOutcome, AppError> {
        if question.title.is_empty() {
            return Err(AppError::BadRequest("缺少题目内容 title".into()));
        }
        let started = Instant::now();
        let request = self.prompts.build(question);
        tracing::debug!(prompt = %request.user, "构造模型请求");

        let response = self.client.complete(&request).await?;
        tracing::debug!(raw = %response.content, "模型原始输出");

        let parsed = parse_model_output(&response.content, question).inspect_err(|e| {
            tracing::warn!(error = %e, "模型输出解析失败");
        })?;
        let answer = format_for_ocs(&parsed, question, &self.cfg);

        Ok(AnswerOutcome {
            question: question.title.clone(),
            answer,
            qtype: question.qtype,
            explanation: parsed.explanation,
            provider: self.client.kind(),
            model: response
                .model
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| self.client.model().to_string()),
            elapsed_ms: started.elapsed().as_millis() as u64,
        })
    }
}
