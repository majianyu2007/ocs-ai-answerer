//! Turns raw model output into a typed answer, then into the string OCS expects.
//!
//! Parsing order: structured JSON (`{"answer": ...}`, also inside code fences or surrounded by
//! text, also truncated) → explicit textual fallbacks ("答案：B", leading option labels, exact
//! option text). Anything that cannot be mapped reliably is an error, never a guess.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::ocs;
use super::question::{Question, QuestionType};
use crate::config::{AnswerConfig, ChoiceFormat};
use crate::error::AnswerError;

#[derive(Debug, Clone, PartialEq)]
pub enum AnswerValue {
    /// Zero-based option indices, sorted and de-duplicated.
    Choices(Vec<usize>),
    Judgement(bool),
    /// One entry per blank.
    Blanks(Vec<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedAnswer {
    pub value: AnswerValue,
    pub explanation: Option<String>,
}

static THINK_BLOCK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<think(?:ing)?>.*?</think(?:ing)?>").unwrap());
static CODE_FENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)```[A-Za-z0-9_-]*\s*\n?(.*?)```").unwrap());
static ANSWER_FIELD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)"(?:answer|answers|答案)"\s*:\s*("(?:[^"\\]|\\.)*"|\[[^\]]*\]|true|false|-?\d+(?:\.\d+)?)"#)
        .unwrap()
});
static ANSWER_KEYWORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:最终答案|正确答案|参考答案|正确选项|答案|final answer|answer)\s*(?:应该是|应当是|应选|应为|就是|是|为|选)?\s*[:：]?\s*",
    )
    .unwrap()
});
static LABELED_OPTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[(（\[【]?([A-Z])\s*[.．、:：)）\]】]\s*(\S.*)$").unwrap());
static BLANK_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:第\s*[0-9一二三四五六七八九十]+\s*[空个]\s*[:：]?|[(（]?\s*[0-9]+\s*[)）.．、:：]|空\s*[0-9]+\s*[:：])\s*")
        .unwrap()
});
static EXPLANATION_MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:\n\s*\n|[(（]?\s*(?:解析|解释|理由|原因|说明|分析|explanation|reason)\s*[:：])",
    )
    .unwrap()
});

const WRAP_CHARS: &[char] = &[
    '"', '\'', '“', '”', '‘', '’', '`', '「', '」', '『', '』', '*', ' ', '\t',
];
const TRAILING_PUNCT: &[char] = &['。', '.', '．', '，', ',', '；', ';', '！', '!', ' '];

/// Parses the model output for the given question.
pub fn parse_model_output(raw: &str, q: &Question) -> Result<ParsedAnswer, AnswerError> {
    let cleaned = THINK_BLOCK.replace_all(raw, "");
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        return Err(AnswerError::EmptyOutput);
    }

    if let Some((answer, explanation)) = extract_structured(cleaned) {
        let value = interpret_value(&answer, q, cleaned)?;
        return Ok(ParsedAnswer { value, explanation });
    }

    let value = interpret_text(cleaned, q)?;
    Ok(ParsedAnswer {
        value,
        explanation: None,
    })
}

/// Finds a JSON `answer` field in the output; returns the answer value and optional explanation.
fn extract_structured(text: &str) -> Option<(Value, Option<String>)> {
    if let Some(found) = parse_json_with_answer(text.trim()) {
        return Some(found);
    }
    // With step-by-step answers the final JSON comes last, so later candidates win.
    let fences: Vec<&str> = CODE_FENCE
        .captures_iter(text)
        .filter_map(|c| c.get(1).map(|m| m.as_str()))
        .collect();
    for candidate in fences.iter().rev() {
        if let Some(found) = parse_json_with_answer(candidate.trim()) {
            return Some(found);
        }
    }
    let starts: Vec<usize> = text.match_indices('{').map(|(i, _)| i).collect();
    for &i in starts.iter().rev() {
        let mut stream = serde_json::Deserializer::from_str(&text[i..]).into_iter::<Value>();
        if let Some(Ok(v)) = stream.next()
            && let Some(found) = answer_from_json(v)
        {
            return Some(found);
        }
    }
    // Truncated JSON, e.g. cut off by max_tokens inside "explanation".
    ANSWER_FIELD
        .captures_iter(text)
        .last()
        .and_then(|c| serde_json::from_str::<Value>(&c[1]).ok())
        .map(|v| (v, None))
}

fn parse_json_with_answer(s: &str) -> Option<(Value, Option<String>)> {
    serde_json::from_str::<Value>(s)
        .ok()
        .and_then(answer_from_json)
}

fn answer_from_json(v: Value) -> Option<(Value, Option<String>)> {
    let Value::Object(map) = v else { return None };
    let answer = ["answer", "answers", "答案", "result"]
        .iter()
        .find_map(|k| map.get(*k).cloned())?;
    let explanation = ["explanation", "reason", "analysis", "解析"]
        .iter()
        .find_map(|k| map.get(*k).and_then(Value::as_str))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    Some((answer, explanation))
}

fn value_to_strings(v: &Value) -> Vec<String> {
    match v {
        Value::Null => Vec::new(),
        Value::String(s) => vec![s.clone()],
        Value::Bool(b) => vec![b.to_string()],
        Value::Number(n) => vec![n.to_string()],
        Value::Array(items) => items.iter().flat_map(value_to_strings).collect(),
        Value::Object(_) => Vec::new(),
    }
}

fn interpret_value(v: &Value, q: &Question, raw: &str) -> Result<AnswerValue, AnswerError> {
    match q.qtype {
        QuestionType::Judgement => {
            if let Value::Bool(b) = v {
                return Ok(AnswerValue::Judgement(*b));
            }
            let parts = value_to_strings(v);
            match parts.as_slice() {
                [one] => judgement_from_text(one, q)
                    .map(AnswerValue::Judgement)
                    .ok_or_else(|| AnswerError::unrecognized("无法判断对错", raw)),
                [] => Err(AnswerError::unrecognized("answer 字段为空", raw)),
                _ => Err(AnswerError::unrecognized("判断题返回了多个答案", raw)),
            }
        }
        QuestionType::Completion => {
            let blanks: Vec<String> = value_to_strings(v)
                .iter()
                .map(|s| clean_blank(s))
                .filter(|s| !s.is_empty())
                .collect();
            if blanks.is_empty() {
                Err(AnswerError::unrecognized("answer 字段为空", raw))
            } else {
                Ok(AnswerValue::Blanks(blanks))
            }
        }
        QuestionType::Unknown if q.options.is_empty() => {
            let blanks: Vec<String> = value_to_strings(v)
                .iter()
                .map(|s| clean_blank(s))
                .filter(|s| !s.is_empty())
                .collect();
            if blanks.is_empty() {
                Err(AnswerError::unrecognized("answer 字段为空", raw))
            } else {
                Ok(AnswerValue::Blanks(blanks))
            }
        }
        _ => {
            let parts = value_to_strings(v);
            if parts.is_empty() {
                return Err(AnswerError::unrecognized("answer 字段为空", raw));
            }
            let mut indices = Vec::new();
            for part in &parts {
                let mapped = choices_from_text(part, q).ok_or_else(|| {
                    AnswerError::unrecognized(format!("\"{part}\" 无法对应到任何选项"), raw)
                })?;
                indices.extend(mapped);
            }
            finish_choices(indices, q, raw)
        }
    }
}

fn interpret_text(text: &str, q: &Question) -> Result<AnswerValue, AnswerError> {
    match q.qtype {
        QuestionType::Judgement => judgement_from_text(text, q)
            .map(AnswerValue::Judgement)
            .ok_or_else(|| AnswerError::unrecognized("无法判断对错", text)),
        QuestionType::Completion => blanks_from_text(text)
            .map(AnswerValue::Blanks)
            .ok_or_else(|| AnswerError::unrecognized("未找到填空答案", text)),
        QuestionType::Unknown if q.options.is_empty() => blanks_from_text(text)
            .map(AnswerValue::Blanks)
            .ok_or_else(|| AnswerError::unrecognized("未找到答案", text)),
        _ => {
            let indices = choices_from_text(text, q)
                .ok_or_else(|| AnswerError::unrecognized("无法对应到任何选项", text))?;
            finish_choices(indices, q, text)
        }
    }
}

fn finish_choices(
    mut indices: Vec<usize>,
    q: &Question,
    raw: &str,
) -> Result<AnswerValue, AnswerError> {
    indices.sort_unstable();
    indices.dedup();
    if indices.is_empty() {
        return Err(AnswerError::unrecognized("未选择任何选项", raw));
    }
    if q.qtype == QuestionType::Single && indices.len() > 1 {
        let letters: Vec<String> = indices.iter().map(|&i| Question::letter(i)).collect();
        return Err(AnswerError::unrecognized(
            format!("单选题返回了多个选项 {}", letters.join("")),
            raw,
        ));
    }
    Ok(AnswerValue::Choices(indices))
}

fn strip_wrapping(s: &str) -> &str {
    s.trim()
        .trim_matches(WRAP_CHARS)
        .trim_start_matches(['[', '【'])
        .trim_end_matches([']', '】'])
        .trim()
}

/// Maps a piece of text to option indices. Tried in order of reliability; `None` means
/// the text cannot be mapped without guessing.
pub fn choices_from_text(text: &str, q: &Question) -> Option<Vec<usize>> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if let Some(found) = choices_from_fragment(text, q) {
        return Some(found);
    }
    // "…正确答案是 B。" — the last answer keyword usually carries the conclusion.
    let after_keyword = ANSWER_KEYWORD
        .find_iter(text)
        .last()
        .map(|m| &text[m.end()..]);
    if let Some(rest) = after_keyword {
        let line = rest.lines().next().unwrap_or("").trim();
        if let Some(found) = choices_from_fragment(line, q).or_else(|| leading_letters(line, q)) {
            return Some(found);
        }
    }
    // A bare conclusion on the last line ("……\n\nB"), then the first line ("B，因为……").
    let last_line = text.lines().map(str::trim).rfind(|l| !l.is_empty())?;
    if let Some(found) = choices_from_fragment(last_line, q) {
        return Some(found);
    }
    let first_line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    choices_from_fragment(first_line, q).or_else(|| leading_letters(first_line, q))
}

/// Maps a short answer fragment such as `B`, `ACD`, `A,C,D`, `B. Rust`, `Rust` or `Rust#CSS`.
fn choices_from_fragment(fragment: &str, q: &Question) -> Option<Vec<usize>> {
    let s = strip_wrapping(fragment)
        .trim_end_matches(TRAILING_PUNCT)
        .trim();
    let s = strip_wrapping(s);
    if s.is_empty() {
        return None;
    }

    // Letters first: the prompt asks for letters, and option texts like "C#" normalize to "c".
    if let Some(letters) = letters_only(s, q) {
        return Some(letters);
    }
    if let Some(i) = exact_option(s, q) {
        return Some(vec![i]);
    }
    if let Some(c) = LABELED_OPTION.captures(s) {
        let idx = letter_index(c[1].chars().next()?, q)?;
        let rest = strip_wrapping(&c[2]).trim_end_matches(TRAILING_PUNCT);
        // "B. Rust" must agree with option B if the text is an option at all.
        match exact_option(rest, q) {
            Some(other) if other != idx => return None,
            _ => return Some(vec![idx]),
        }
    }

    let pieces: Vec<&str> = s
        .split(['#', '|', ';', '；', '、', '，', ',', '\n'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if pieces.len() > 1 {
        let mut out = Vec::new();
        for piece in pieces {
            out.extend(choices_from_fragment(piece, q)?);
        }
        return Some(out);
    }
    None
}

fn letter_index(c: char, q: &Question) -> Option<usize> {
    let c = c.to_ascii_uppercase();
    if !c.is_ascii_uppercase() {
        return None;
    }
    let idx = (c as u8 - b'A') as usize;
    (idx < q.options.len()).then_some(idx)
}

/// Exactly one option whose normalized text equals `s`.
fn exact_option(s: &str, q: &Question) -> Option<usize> {
    let target = ocs::normalize_string(s);
    if target.is_empty() {
        return None;
    }
    let mut hits = q
        .options
        .iter()
        .enumerate()
        .filter(|(_, o)| ocs::normalize_string(&o.text) == target)
        .map(|(i, _)| i);
    let first = hits.next()?;
    hits.next().is_none().then_some(first)
}

/// `B`, `b`, `ACD`, `A,C,D`, `A、C、D`, `A C D`, `A和C`: only option letters and separators.
fn letters_only(s: &str, q: &Question) -> Option<Vec<usize>> {
    if s.chars().count() == 1 {
        return letter_index(s.chars().next()?, q).map(|i| vec![i]);
    }
    let mut out = Vec::new();
    let mut last_was_letter = false;
    for c in s.chars() {
        if c.is_ascii_uppercase() {
            out.push(letter_index(c, q)?);
            last_was_letter = true;
        } else if matches!(
            c,
            ',' | '，' | '、' | ' ' | '#' | '/' | ';' | '；' | '|' | '和' | '与' | '及' | '+'
        ) {
            if !last_was_letter {
                return None;
            }
            last_was_letter = false;
        } else {
            return None;
        }
    }
    (!out.is_empty() && last_was_letter).then_some(out)
}

/// A run of option letters at the start of a sentence: `B 是正确的`, `A、C 正确`, `C。`.
/// Rejects ordinary words such as `A lot of` or `CPU`.
fn leading_letters(s: &str, q: &Question) -> Option<Vec<usize>> {
    let s = s.trim_start_matches(['(', '（', '[', '【', '"', '“', '*', ' ']);
    let mut end = 0;
    let mut letters = Vec::new();
    let mut expect_letter = true;
    for (i, c) in s.char_indices() {
        if c.is_ascii_uppercase() && expect_letter {
            letters.push(c);
            end = i + c.len_utf8();
            expect_letter = false;
        } else if !expect_letter && matches!(c, ',' | '，' | '、' | '/' | '和' | '与' | '及') {
            expect_letter = true;
        } else {
            break;
        }
    }
    if letters.is_empty() {
        return None;
    }
    let rest = &s[end..];
    let next = rest.chars().next();
    let ok = match next {
        None => true,
        Some(c) if c.is_ascii_alphanumeric() => false,
        Some(c) if c.is_whitespace() => !rest
            .trim_start()
            .chars()
            .next()
            .is_some_and(|n| n.is_ascii_alphanumeric()),
        Some(_) => true,
    };
    if !ok {
        return None;
    }
    let indices: Option<Vec<usize>> = letters.into_iter().map(|c| letter_index(c, q)).collect();
    // "C. Rust" where Rust is option B: contradictory, do not guess.
    if let Some([idx]) = indices.as_deref() {
        let tail = rest.trim_start_matches(['.', '．', '、', ':', '：', ')', '）', ']', '】', ' ']);
        let tail = strip_wrapping(tail).trim_end_matches(TRAILING_PUNCT);
        if exact_option(tail, q).is_some_and(|other| other != *idx) {
            return None;
        }
    }
    indices
}

const TRUE_WORDS: &[&str] = &[
    "正确",
    "对",
    "是",
    "√",
    "✓",
    "✔",
    "true",
    "t",
    "yes",
    "y",
    "对的",
    "正确的",
    "是的",
    "right",
    "correct",
    "1",
];
const FALSE_WORDS: &[&str] = &[
    "错误",
    "错",
    "否",
    "×",
    "✗",
    "✘",
    "x",
    "false",
    "f",
    "no",
    "n",
    "错的",
    "错误的",
    "不对",
    "不正确",
    "不是",
    "wrong",
    "incorrect",
    "0",
];

fn classify_judgement_word(s: &str) -> Option<bool> {
    let s = strip_wrapping(s).trim_end_matches(TRAILING_PUNCT).trim();
    let lower = s.to_lowercase();
    if TRUE_WORDS.contains(&lower.as_str()) {
        return Some(true);
    }
    if FALSE_WORDS.contains(&lower.as_str()) {
        return Some(false);
    }
    if ocs::is_correct_word(s) {
        return Some(true);
    }
    if ocs::is_incorrect_word(s) {
        return Some(false);
    }
    None
}

/// Prefixes that decide a judgement sentence; negative forms first since `不正确` contains `正确`.
const FALSE_PREFIXES: &[&str] = &[
    "不正确",
    "不对",
    "错误",
    "错",
    "false",
    "×",
    "✗",
    "wrong",
    "incorrect",
];
const TRUE_PREFIXES: &[&str] = &["正确", "对", "true", "√", "✓", "right", "correct"];

pub fn judgement_from_text(text: &str, q: &Question) -> Option<bool> {
    let text = text.trim();
    if let Some(b) = classify_judgement_word(text) {
        return Some(b);
    }
    // A letter referring to one of the provided options ("A" → "正确").
    if !q.options.is_empty()
        && let Some(found) = choices_from_fragment(text, q)
        && let [idx] = found.as_slice()
    {
        return classify_judgement_word(&q.options[*idx].text);
    }

    let candidates = ANSWER_KEYWORD
        .find_iter(text)
        .last()
        .map(|m| text[m.end()..].trim())
        .into_iter()
        .chain(text.lines().map(str::trim).rfind(|l| !l.is_empty()))
        .chain(text.lines().map(str::trim).find(|l| !l.is_empty()));
    for candidate in candidates {
        let lower = candidate.to_lowercase();
        if FALSE_PREFIXES.iter().any(|p| lower.starts_with(p)) {
            return Some(false);
        }
        if TRUE_PREFIXES.iter().any(|p| lower.starts_with(p)) {
            return Some(true);
        }
        let head: String = candidate
            .chars()
            .take_while(|c| !matches!(c, '，' | ',' | '。' | '；' | ';' | '\n' | ' '))
            .collect();
        if let Some(b) = classify_judgement_word(&head) {
            return Some(b);
        }
    }

    // Last resort: exactly one polarity is mentioned anywhere ("该说法错误").
    let lower = text.to_lowercase();
    let negatives = ["不正确", "不对", "错误", "false", "×", "✗"];
    let mut rest = lower.clone();
    let mut has_false = false;
    for n in negatives {
        if rest.contains(n) {
            has_false = true;
            rest = rest.replace(n, " ");
        }
    }
    let has_true = ["正确", "true", "√", "✓"].iter().any(|p| rest.contains(p));
    match (has_true, has_false) {
        (true, false) => Some(true),
        (false, true) => Some(false),
        _ => None,
    }
}

fn clean_blank(s: &str) -> String {
    let s = BLANK_PREFIX.replace(s.trim(), "");
    strip_wrapping(&s)
        .trim_end_matches(['。', '；', ';'])
        .trim()
        .to_string()
}

fn blanks_from_text(text: &str) -> Option<Vec<String>> {
    let mut body = text.trim();
    if let Some(m) = ANSWER_KEYWORD.find_iter(body).last() {
        body = body[m.end()..].trim();
    }
    if let Some(m) = EXPLANATION_MARKER.find(body)
        && m.start() > 0
    {
        body = body[..m.start()].trim();
    }
    let lines: Vec<&str> = body
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let numbered = lines.len() > 1 && lines.iter().all(|l| BLANK_PREFIX.is_match(l));
    let blanks: Vec<String> = if numbered {
        lines.iter().map(|l| clean_blank(l)).collect()
    } else {
        let first = lines.first()?;
        first
            .split('#')
            .map(clean_blank)
            .filter(|s| !s.is_empty())
            .collect()
    };
    (!blanks.is_empty()).then_some(blanks)
}

/// Formats a parsed answer as the `answer` string returned to OCS.
pub fn format_for_ocs(parsed: &ParsedAnswer, q: &Question, cfg: &AnswerConfig) -> String {
    match &parsed.value {
        AnswerValue::Judgement(b) => format_judgement(*b, q, cfg),
        AnswerValue::Blanks(blanks) => blanks.join(&cfg.completion_separator),
        AnswerValue::Choices(indices) => format_choices(indices, q, cfg),
    }
}

fn format_judgement(value: bool, q: &Question, cfg: &AnswerConfig) -> String {
    let word = if value {
        &cfg.judgement_true
    } else {
        &cfg.judgement_false
    };
    if q.options.is_empty() {
        return word.clone();
    }
    // Prefer the option's own wording when OCS would resolve our word to that same option.
    let raw = q.raw_options();
    let target = q
        .options
        .iter()
        .position(|o| classify_judgement_word(&o.text) == Some(value));
    match target {
        Some(idx) if ocs::resolve_judgement(word, &raw) == Some(idx) => word.clone(),
        Some(idx) if ocs::resolve_judgement(&q.options[idx].text, &raw) == Some(idx) => {
            q.options[idx].text.clone()
        }
        _ => word.clone(),
    }
}

fn format_choices(indices: &[usize], q: &Question, cfg: &AnswerConfig) -> String {
    let raw = q.raw_options();
    let sep = cfg.multiple_separator.as_str();
    let is_single = indices.len() == 1 && q.qtype != QuestionType::Multiple;

    let text = indices
        .iter()
        .map(|&i| q.options[i].text.as_str())
        .collect::<Vec<_>>()
        .join(sep);
    let letters_sep = indices
        .iter()
        .map(|&i| Question::letter(i))
        .collect::<Vec<_>>()
        .join(sep);
    let letters_plain: String = indices.iter().map(|&i| Question::letter(i)).collect();

    let ocs_selects = |answer: &str| -> bool {
        if is_single {
            ocs::resolve_single(answer, &raw) == Some(indices[0])
        } else {
            let mut got = ocs::resolve_multiple(answer, &raw);
            got.sort_unstable();
            got == indices
        }
    };

    // `===` is the first separator OCS tries, so it still splits correctly when option texts
    // themselves contain `#` (e.g. "C#").
    let text_strict = indices
        .iter()
        .map(|&i| q.options[i].text.as_str())
        .collect::<Vec<_>>()
        .join("===");

    let candidates: Vec<&str> = match cfg.choice_format {
        ChoiceFormat::Letter => vec![&letters_sep, &letters_plain, &text, &text_strict],
        ChoiceFormat::Text | ChoiceFormat::Auto => {
            vec![&text, &text_strict, &letters_sep, &letters_plain]
        }
    };
    if let Some(found) = candidates.iter().find(|c| ocs_selects(c)) {
        return found.to_string();
    }
    tracing::warn!(
        answer = %candidates[0],
        "无法确认 OCS 能准确匹配该答案，仍按首选格式返回"
    );
    candidates[0].to_string()
}
