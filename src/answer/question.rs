use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

use super::ocs;
use crate::provider::ImageInput;

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
    /// What the model sees: `text`, or its image-placeholder version (`[图片1]`).
    pub display: String,
}

#[derive(Debug, Clone)]
pub struct Question {
    pub title: String,
    pub qtype: QuestionType,
    /// The type as sent by the client, before inference.
    pub requested_type: QuestionType,
    pub options: Vec<QuestionOption>,
    /// Title with image URLs replaced by `[图片N]`, when OCS image optimization is on.
    pub suggestion_title: Option<String>,
    pub images: Vec<ImageInput>,
}

/// Raw question fields as received from OCS.
#[derive(Debug, Clone, Default)]
pub struct QuestionInput {
    pub title: String,
    pub qtype: String,
    pub options: Vec<String>,
    pub images: Vec<String>,
    pub suggestion_title: String,
    pub suggestion_options: String,
}

static OPTION_LABEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*[(（]?([A-Za-z])\s*(?:[.．、:：)）]|\s)\s*(.*)$").unwrap());
static BARE_LABEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*[(（]?([A-Z])\s*[.．、:：)）]?\s*$").unwrap());

impl Question {
    pub fn new(title: &str, qtype: &str, options: Vec<String>) -> Self {
        Self::from_input(QuestionInput {
            title: title.to_string(),
            qtype: qtype.to_string(),
            options,
            ..Default::default()
        })
    }

    pub fn from_input(input: QuestionInput) -> Self {
        let mut options = parse_options(input.options);
        let requested_type = QuestionType::parse(&input.qtype);
        let qtype = match requested_type {
            QuestionType::Unknown => infer_type(&options),
            t => t,
        };
        let images: Vec<ImageInput> = input
            .images
            .iter()
            .filter_map(|s| ImageInput::parse(s))
            .collect();

        let mut suggestion_title = None;
        if !images.is_empty() {
            let t = input.suggestion_title.trim();
            if !t.is_empty() {
                suggestion_title = Some(t.to_string());
            }
            // Placeholder options line up with the original ones only when the counts match.
            let suggested = parse_options(vec![input.suggestion_options]);
            if suggested.len() == options.len() {
                for (o, s) in options.iter_mut().zip(suggested) {
                    o.display = s.text;
                }
            }
        }

        Self {
            title: input.title.trim().to_string(),
            qtype,
            requested_type,
            options,
            suggestion_title,
            images,
        }
    }

    /// Title shown to the model.
    pub fn display_title(&self) -> &str {
        self.suggestion_title.as_deref().unwrap_or(&self.title)
    }

    /// Stable identity of the question, used as cache key and for in-flight de-duplication.
    pub fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(self.qtype.as_str());
        h.update([0]);
        h.update(ocs::normalize_string(&self.title));
        for o in &self.options {
            h.update([1]);
            h.update(ocs::normalize_string(&o.text));
        }
        for img in &self.images {
            h.update([2]);
            h.update(img.to_data_url());
        }
        let digest = h.finalize();
        digest.iter().map(|b| format!("{b:02x}")).collect()
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
            QuestionOption {
                raw,
                display: text.clone(),
                text,
            }
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
