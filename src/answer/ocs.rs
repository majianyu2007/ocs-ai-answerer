//! A faithful port of the parts of OCS's answer matching that decide which option gets clicked
//! (`packages/core/src/core/worker/resolvers/*` and `core/utils/string.ts` in ocsjs).
//!
//! It is used to verify, before replying, that an answer string will make OCS select exactly the
//! options the model chose. When it would not (duplicate/similar option texts, separators inside
//! option texts, …) the formatter falls back to plain letters, which OCS maps by option index.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

/// OCS default `answerSeparators`, in the order `splitAnswer` tries them.
pub const DEFAULT_SEPARATORS: [&str; 7] = ["===", "#", "---", "###", "|", ";", "；"];

/// `CORRECT_WORDS` from `resolvers/judgement.ts`.
pub const CORRECT_WORDS: [&str; 13] = [
    "是",
    "对",
    "正确",
    "确定",
    "√",
    "对的",
    "是的",
    "正确的",
    "true",
    "True",
    "T",
    "yes",
    "1",
];

/// `INCORRECT_WORDS` from `resolvers/judgement.ts`.
pub const INCORRECT_WORDS: [&str; 17] = [
    "非",
    "否",
    "错",
    "错误",
    "×",
    "X",
    "错的",
    "不对",
    "不正确的",
    "不正确",
    "不是",
    "不是的",
    "false",
    "False",
    "F",
    "no",
    "0",
];

const CJK: &str = "\u{2E80}-\u{9FFF}";

static REDUNDANT_LABEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("^[A-Z][^A-Za-z0-9{CJK}]+([A-Za-z0-9{CJK}]+)")).unwrap());
static REDUNDANT_LABEL_CJK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("^[A-Z]([{CJK}][A-Za-z0-9{CJK}]*)")).unwrap());

/// `removeRedundant`: strips a leading option label such as `A.` / `A、` / `A选项`.
pub fn remove_redundant(s: &str) -> String {
    let s = s.trim();
    let s = REDUNDANT_LABEL.replace(s, "$1");
    REDUNDANT_LABEL_CJK.replace(&s, "$1").into_owned()
}

fn is_cjk(c: char) -> bool {
    ('\u{2E80}'..='\u{9FFF}').contains(&c)
}

/// `clearString`: lowercase and keep only ASCII letters/digits, CJK and the excluded chars.
pub fn clear_string(s: &str, exclude: &[char]) -> String {
    const ALWAYS: [char; 9] = ['①', '②', '③', '④', '⑤', '⑥', '⑦', '⑧', '⑨'];
    s.trim()
        .to_lowercase()
        .chars()
        .filter(|&c| {
            c.is_ascii_alphanumeric() || is_cjk(c) || exclude.contains(&c) || ALWAYS.contains(&c)
        })
        .collect()
}

/// `normalizeString`: drop punctuation/whitespace, full-width → half-width, then `clearString`.
pub fn normalize_string(s: &str) -> String {
    const DROP: &str = "，。！？；：\"\"''“”‘’、（）【】《》,.-!?;:'()[]<>";
    let converted: String = s
        .chars()
        .filter(|c| !c.is_whitespace() && !DROP.contains(*c))
        .map(|c| match c {
            'ａ'..='ｚ' | 'Ａ'..='Ｚ' | '０'..='９' => {
                char::from_u32(c as u32 - 0xFEE0).unwrap_or(c)
            }
            '％' => '%',
            other => other,
        })
        .collect();
    clear_string(&converted, &[])
}

/// `compareTwoStrings` from the `string-similarity` package (Sørensen–Dice on bigrams).
pub fn compare_two_strings(first: &str, second: &str) -> f64 {
    let a: Vec<char> = first.chars().filter(|c| !c.is_whitespace()).collect();
    let b: Vec<char> = second.chars().filter(|c| !c.is_whitespace()).collect();
    if a == b {
        return 1.0;
    }
    if a.len() < 2 || b.len() < 2 {
        return 0.0;
    }
    let mut bigrams: HashMap<(char, char), usize> = HashMap::new();
    for w in a.windows(2) {
        *bigrams.entry((w[0], w[1])).or_default() += 1;
    }
    let mut intersection = 0usize;
    for w in b.windows(2) {
        if let Some(count) = bigrams.get_mut(&(w[0], w[1]))
            && *count > 0
        {
            *count -= 1;
            intersection += 1;
        }
    }
    (2.0 * intersection as f64) / ((a.len() + b.len() - 2) as f64)
}

/// `findBestMatch(main, targets).bestMatch.rating` (first maximum wins).
fn best_rating(main: &str, targets: &[String]) -> f64 {
    targets
        .iter()
        .map(|t| compare_two_strings(main, t))
        .fold(0.0, f64::max)
}

/// `splitAnswer` with OCS's default separators (program-code detection is not ported,
/// which only makes the simulation stricter for answers containing `;` or `|`).
pub fn split_answer(answer: &str) -> Vec<String> {
    let answer = answer.trim();
    if answer.is_empty() {
        return Vec::new();
    }
    match serde_json::from_str::<serde_json::Value>(answer) {
        Ok(serde_json::Value::Array(items)) => {
            return items
                .iter()
                .map(|v| match v {
                    serde_json::Value::String(s) => s.trim().to_string(),
                    other => other.to_string(),
                })
                .filter(|s| !s.is_empty())
                .collect();
        }
        Ok(_) => return vec![answer.to_string()],
        Err(_) => {}
    }
    for sep in DEFAULT_SEPARATORS {
        if answer.contains(sep) {
            return answer
                .split(sep)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
        }
    }
    vec![answer.to_string()]
}

/// `isPlainAnswer` + `resolvePlainAnswer`: `"ACD"`, `"A,C,D"`, `"A#C"` → `"ACD"`.
pub fn resolve_plain_answer(answer: &str) -> Option<String> {
    let resolved: String = answer
        .trim()
        .chars()
        .filter(|c| !matches!(c, ',' | '，' | '.' | '。' | '．' | '、' | ' ' | '#'))
        .collect();
    let resolved = resolved.trim();
    if resolved.chars().count() > 8 || !resolved.chars().any(|c| c.is_ascii_uppercase()) {
        return None;
    }
    let mut prev = 0u32;
    let mut seen = std::collections::HashSet::new();
    for c in resolved.chars() {
        if (c as u32) < prev || !seen.insert(c) {
            return None;
        }
        prev = c as u32;
    }
    Some(resolved.to_string())
}

/// `answerNormalizedMatch`: indices of options contained in (or equal to) an answer,
/// exact matches first.
fn normalized_match(answers: &[String], options: &[String]) -> Vec<usize> {
    let answers: Vec<String> = answers
        .iter()
        .map(|a| normalize_string(&remove_redundant(a)))
        .collect();
    if answers.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<(usize, u8)> = options
        .iter()
        .enumerate()
        .filter_map(|(i, opt)| {
            let opt = normalize_string(&remove_redundant(opt));
            if opt.is_empty() {
                return None;
            }
            let mut level = 0;
            for ans in &answers {
                if *ans == opt {
                    level = 2;
                    break;
                } else if ans.contains(&opt) {
                    level = 1;
                }
            }
            (level > 0).then_some((i, level))
        })
        .collect();
    hits.sort_by_key(|h| std::cmp::Reverse(h.1));
    hits.into_iter().map(|(i, _)| i).collect()
}

/// `answerSimilar`: for every option, the best Dice rating against all answers.
fn similar_ratings(answers: &[String], options: &[String]) -> Vec<f64> {
    let answers: Vec<String> = answers
        .iter()
        .map(|a| clear_string(&remove_redundant(a), &[]))
        .collect();
    options
        .iter()
        .map(|opt| {
            let opt = clear_string(&remove_redundant(opt), &[]);
            if answers.is_empty() || opt.trim().is_empty() {
                0.0
            } else {
                best_rating(&opt, &answers)
            }
        })
        .collect()
}

fn judgement_matches(target: &str, words: &[&str]) -> bool {
    let target = clear_string(&remove_redundant(target), &['√', '×']);
    words
        .iter()
        .any(|w| clear_string(&remove_redundant(w), &['√', '×']) == target)
}

pub fn is_correct_word(s: &str) -> bool {
    judgement_matches(s, &CORRECT_WORDS)
}

pub fn is_incorrect_word(s: &str) -> bool {
    judgement_matches(s, &INCORRECT_WORDS)
}

/// `isJudgementOptions`: OCS routes such "single" questions through the judgement resolver.
pub fn is_judgement_options(options: &[String]) -> bool {
    options.len() == 2
        && options.iter().any(|o| is_correct_word(o.trim()))
        && options.iter().any(|o| is_incorrect_word(o.trim()))
}

/// `resolveJudgement` for a single answer string; returns the selected option index.
pub fn resolve_judgement(answer: &str, options: &[String]) -> Option<usize> {
    let correct = is_correct_word(answer);
    let incorrect = is_incorrect_word(answer);
    if !correct && !incorrect {
        return None;
    }
    options
        .iter()
        .position(|o| (correct && is_correct_word(o)) || (incorrect && is_incorrect_word(o)))
}

/// The single-choice handler: judgement detection, then `resolveSingle`.
pub fn resolve_single(answer: &str, options: &[String]) -> Option<usize> {
    let trimmed: Vec<String> = options.iter().map(|o| o.trim().to_string()).collect();
    if is_judgement_options(&trimmed) {
        return resolve_judgement(answer, options);
    }
    resolve_single_inner(&split_answer(answer), options, true)
}

fn resolve_single_inner(all: &[String], options: &[String], allow_merge: bool) -> Option<usize> {
    let option_strings: Vec<String> = options.iter().map(|o| remove_redundant(o)).collect();

    if let Some(&first) = normalized_match(all, &option_strings).first() {
        let target = &option_strings[first];
        if let Some(i) = option_strings.iter().position(|o| o == target) {
            return Some(i);
        }
    }

    let ratings = similar_ratings(all, &option_strings);
    let mut best = None;
    let mut best_rating = 0.0;
    for (i, r) in ratings.iter().enumerate() {
        if *r > best_rating {
            best_rating = *r;
            best = Some(i);
        }
    }
    if best.is_some() && best_rating > 0.6 {
        return best;
    }

    for a in all {
        let nowrap: String = a.chars().filter(|c| *c != '\n' && *c != '\r').collect();
        if let Some(plain) = resolve_plain_answer(nowrap.trim())
            && plain.chars().count() == 1
        {
            let idx = (plain.chars().next().unwrap() as u32 - 'A' as u32) as usize;
            if idx < options.len() {
                return Some(idx);
            }
        }
    }

    if allow_merge && all.len() > 1 {
        return resolve_single_inner(&[all.concat()], options, false);
    }
    None
}

/// `disambiguateSimilarOptions`: among near-identical candidates keep the better rated one.
fn disambiguate(indices: &[usize], ratings: &[f64], options: &[String]) -> Vec<usize> {
    if indices.len() <= 1 {
        return indices.to_vec();
    }
    let normalized: Vec<String> = indices
        .iter()
        .map(|&i| normalize_string(&remove_redundant(&options[i])))
        .collect();
    let mut eliminated = vec![false; indices.len()];
    for i in 0..indices.len() {
        if eliminated[i] {
            continue;
        }
        for j in (i + 1)..indices.len() {
            if eliminated[j] {
                continue;
            }
            if compare_two_strings(&normalized[i], &normalized[j]) > 0.6 {
                if ratings[i] >= ratings[j] {
                    eliminated[j] = true;
                } else {
                    eliminated[i] = true;
                    break;
                }
            }
        }
    }
    indices
        .iter()
        .zip(eliminated)
        .filter(|(_, e)| !e)
        .map(|(&i, _)| i)
        .collect()
}

/// `resolveMultiple` for a single answer string; returns the selected option indices.
pub fn resolve_multiple(answer: &str, options: &[String]) -> Vec<usize> {
    let option_strings: Vec<String> = options.iter().map(|o| remove_redundant(o)).collect();
    let answers = split_answer(answer.trim());

    // Stage 1: normalized containment (answer ⊇ option).
    let mut norm_idx = Vec::new();
    let mut norm_ratings = Vec::new();
    for opt_i in normalized_match(&answers, &option_strings) {
        let no = normalize_string(&remove_redundant(&option_strings[opt_i]));
        let matched = answers.iter().find(|a| {
            let na = normalize_string(&remove_redundant(a));
            na == no || na.contains(&no)
        });
        if let Some(a) = matched {
            let na = normalize_string(&remove_redundant(a));
            norm_idx.push(opt_i);
            norm_ratings.push(if na == no { 1.0 } else { 0.8 });
        }
    }
    let norm_kept = disambiguate(&norm_idx, &norm_ratings, options);
    let norm_sum: f64 = norm_kept
        .iter()
        .map(|k| norm_ratings[norm_idx.iter().position(|i| i == k).unwrap()])
        .sum();

    // Stage 2: similarity above 0.6.
    let ratings = similar_ratings(&answers, &option_strings);
    let sim_idx: Vec<usize> = (0..ratings.len()).filter(|&j| ratings[j] > 0.6).collect();
    let sim_ratings: Vec<f64> = sim_idx.iter().map(|&j| ratings[j]).collect();
    let sim_kept = disambiguate(&sim_idx, &sim_ratings, options);
    let sim_sum: f64 = sim_kept.iter().map(|&j| ratings[j]).sum();

    let score = |kept: &[usize], sum: f64| kept.len() as f64 * 100.0 + sum;
    let best = if score(&sim_kept, sim_sum) > score(&norm_kept, norm_sum) {
        sim_kept
    } else {
        norm_kept
    };
    if !best.is_empty() {
        return best;
    }

    // Stage 3: plain letters.
    let nowrap: String = answer
        .chars()
        .filter(|c| *c != '\n' && *c != '\r')
        .collect();
    let mut out = Vec::new();
    if let Some(plain) = resolve_plain_answer(nowrap.trim()) {
        for c in plain.chars() {
            let idx = (c as u32).wrapping_sub('A' as u32) as usize;
            if idx < options.len() && !out.contains(&idx) {
                out.push(idx);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn remove_redundant_matches_ocs() {
        assert_eq!(remove_redundant("A. HTML"), "HTML");
        // `、` (U+3001) falls inside the JS range ⺀-鿿, so OCS keeps it too.
        assert_eq!(remove_redundant("B、Rust"), "、Rust");
        assert_eq!(remove_redundant("C选项"), "选项");
        assert_eq!(remove_redundant("CPU"), "CPU");
        assert_eq!(remove_redundant("TCP协议"), "TCP协议");
    }

    #[test]
    fn dice_coefficient() {
        assert_eq!(compare_two_strings("abc", "abc"), 1.0);
        assert_eq!(compare_two_strings("a", "b"), 0.0);
        assert!((compare_two_strings("night", "nacht") - 0.25).abs() < 1e-9);
    }

    #[test]
    fn split_answer_uses_first_matching_separator() {
        assert_eq!(split_answer("A#B#C"), vec!["A", "B", "C"]);
        assert_eq!(split_answer("x===y#z"), vec!["x", "y#z"]);
        assert_eq!(split_answer("[\"a\",\"b\"]"), vec!["a", "b"]);
        assert_eq!(split_answer("123"), vec!["123"]);
    }

    #[test]
    fn single_text_and_letter() {
        let o = opts(&["A. HTML", "B. Rust", "C. CSS", "D. SQL"]);
        assert_eq!(resolve_single("Rust", &o), Some(1));
        assert_eq!(resolve_single("B", &o), Some(1));
    }

    #[test]
    fn multiple_text_and_letters() {
        let o = opts(&["A. 北京", "B. 上海", "C. 东京", "D. 广州"]);
        assert_eq!(resolve_multiple("北京#上海#广州", &o), vec![0, 1, 3]);
        assert_eq!(resolve_multiple("A#B#D", &o), vec![0, 1, 3]);
        assert_eq!(resolve_multiple("ABD", &o), vec![0, 1, 3]);
    }

    #[test]
    fn judgement_words() {
        let o = opts(&["对", "错"]);
        assert_eq!(resolve_judgement("正确", &o), Some(0));
        assert_eq!(resolve_judgement("错误", &o), Some(1));
        assert!(is_judgement_options(&opts(&["A. 正确", "B. 错误"])));
    }
}
