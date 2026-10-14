mod common;

use common::question;
use ocs_ai_answerer::answer::normalize::{
    AnswerValue, format_for_ocs, parse_model_output,
};
use ocs_ai_answerer::answer::ocs;
use ocs_ai_answerer::answer::prompt::PromptBuilder;
use ocs_ai_answerer::answer::{Question, QuestionType};
use ocs_ai_answerer::config::{AnswerConfig, ChoiceFormat};
use ocs_ai_answerer::error::AnswerError;

const LANG_OPTS: &str = "A. HTML\nB. Rust\nC. CSS\nD. SQL";
const CITY_OPTS: &str = "A. 北京\nB. 上海\nC. 东京\nD. 广州";

fn answer(raw: &str, q: &Question) -> Result<String, AnswerError> {
    let cfg = AnswerConfig::default();
    parse_model_output(raw, q).map(|p| format_for_ocs(&p, q, &cfg))
}

fn choices(raw: &str, q: &Question) -> Vec<usize> {
    match parse_model_output(raw, q).unwrap().value {
        AnswerValue::Choices(c) => c,
        other => panic!("expected choices, got {other:?}"),
    }
}

/// The returned answer must make the simulated OCS resolver pick exactly `expected`.
fn assert_ocs_selects(answer: &str, q: &Question, expected: &[usize]) {
    let raw = q.raw_options();
    if q.qtype == QuestionType::Multiple {
        let mut got = ocs::resolve_multiple(answer, &raw);
        got.sort_unstable();
        assert_eq!(got, expected, "OCS would select {got:?} for {answer:?}");
    } else {
        assert_eq!(
            ocs::resolve_single(answer, &raw),
            Some(expected[0]),
            "OCS mismatch for {answer:?}"
        );
    }
}

// ---------- question parsing ----------

#[test]
fn options_are_parsed_from_newline_string() {
    let q = question("t", "single", LANG_OPTS);
    let texts: Vec<_> = q.options.iter().map(|o| o.text.as_str()).collect();
    assert_eq!(texts, ["HTML", "Rust", "CSS", "SQL"]);
}

#[test]
fn options_without_labels_and_array_input() {
    let q = Question::new("t", "single", vec!["HTML".into(), "Rust".into()]);
    assert_eq!(q.options[1].text, "Rust");
    let q = question("t", "single", "I think so\nNo way");
    assert_eq!(q.options[0].text, "I think so");
}

#[test]
fn split_label_lines_are_merged() {
    let q = question("t", "single", "A\nHTML\nB\nRust");
    let texts: Vec<_> = q.options.iter().map(|o| o.text.as_str()).collect();
    assert_eq!(texts, ["HTML", "Rust"]);
}

#[test]
fn unknown_type_is_inferred() {
    assert_eq!(question("t", "", "").qtype, QuestionType::Completion);
    assert_eq!(
        question("t", "unknown", "A. 正确\nB. 错误").qtype,
        QuestionType::Judgement
    );
    // Not silently treated as single choice.
    assert_eq!(question("t", "", LANG_OPTS).qtype, QuestionType::Unknown);
    assert_eq!(question("t", "多选题", LANG_OPTS).qtype, QuestionType::Multiple);
}

// ---------- prompt ----------

#[test]
fn prompt_contains_question_options_and_format() {
    let q = question("哪种语言用于系统编程？", "single", LANG_OPTS);
    let req = PromptBuilder::new(&AnswerConfig::default()).build(&q);
    assert!(req.user.contains("哪种语言用于系统编程？"));
    assert!(req.user.contains("B. Rust"));
    assert!(req.user.contains("单选题"));
    assert!(req.user.contains("\"answer\""));
    assert!(req.user.contains("A-D"));
    assert!(!req.system.is_empty());
}

#[test]
fn custom_prompts_override_defaults() {
    let cfg = AnswerConfig {
        system_prompt: "CUSTOM SYSTEM".into(),
        multiple_prompt: "CUSTOM MULTI".into(),
        ..AnswerConfig::default()
    };
    let q = question("t", "multiple", CITY_OPTS);
    let req = PromptBuilder::new(&cfg).build(&q);
    assert_eq!(req.system, "CUSTOM SYSTEM");
    assert!(req.user.starts_with("CUSTOM MULTI"));
    // The output format is always appended.
    assert!(req.user.contains("[\"A\", \"C\"]"));
}

// ---------- single ----------

#[test]
fn single_standard_json() {
    let q = question("t", "single", LANG_OPTS);
    let a = answer(r#"{"answer": "B", "explanation": "Rust 是系统语言"}"#, &q).unwrap();
    assert_eq!(a, "Rust");
    assert_ocs_selects(&a, &q, &[1]);
    let p = parse_model_output(r#"{"answer": "B", "explanation": "x"}"#, &q).unwrap();
    assert_eq!(p.explanation.as_deref(), Some("x"));
}

#[test]
fn single_markdown_code_block() {
    let q = question("t", "single", LANG_OPTS);
    assert_eq!(choices("```json\n{\"answer\": \"B\"}\n```", &q), [1]);
    assert_eq!(choices("好的，结果如下：\n{\"answer\": \"b\"} 希望有帮助", &q), [1]);
}

#[test]
fn single_plain_text_variants() {
    let q = question("t", "single", LANG_OPTS);
    for raw in [
        "B",
        "B.",
        "答案：B",
        "答案: B",
        "正确答案是 B",
        "正确答案是B。",
        "最终答案为：(B)",
        "B. Rust",
        "B、Rust",
        "**B**",
        "Rust",
        "分析：HTML 是标记语言，CSS 是样式语言。\n所以答案是 B",
        "Answer: B",
    ] {
        assert_eq!(choices(raw, &q), [1], "input {raw:?}");
    }
}

#[test]
fn single_option_text_instead_of_letter() {
    let q = question("t", "single", LANG_OPTS);
    assert_eq!(choices(r#"{"answer": "Rust"}"#, &q), [1]);
    assert_eq!(choices(r#"{"answer": "B. Rust"}"#, &q), [1]);
}

#[test]
fn single_letter_text_conflict_is_rejected() {
    let q = question("t", "single", LANG_OPTS);
    assert!(parse_model_output(r#"{"answer": "C. Rust"}"#, &q).is_err());
}

#[test]
fn single_with_multiple_letters_is_rejected() {
    let q = question("t", "single", LANG_OPTS);
    let err = parse_model_output(r#"{"answer": "BC"}"#, &q).unwrap_err();
    assert!(err.to_string().contains("单选题返回了多个选项"));
}

#[test]
fn ordinary_english_letters_are_not_options() {
    let q = question("t", "single", LANG_OPTS);
    for raw in ["A lot of languages exist", "I don't know", "CPU", "Maybe"] {
        assert!(parse_model_output(raw, &q).is_err(), "input {raw:?}");
    }
}

#[test]
fn out_of_range_letter_is_rejected() {
    let q = question("t", "single", LANG_OPTS);
    assert!(parse_model_output(r#"{"answer": "F"}"#, &q).is_err());
}

#[test]
fn single_options_with_similar_text_fall_back_to_letter() {
    // "计算机" is contained in "计算机网络", so returning the text could select the wrong option.
    let q = question("t", "single", "A. 计算机网络\nB. 计算机\nC. 网络\nD. 互联网");
    let a = answer(r#"{"answer":"B"}"#, &q).unwrap();
    assert_ocs_selects(&a, &q, &[1]);
}

#[test]
fn letter_format_config() {
    let q = question("t", "multiple", CITY_OPTS);
    let cfg = AnswerConfig {
        choice_format: ChoiceFormat::Letter,
        ..AnswerConfig::default()
    };
    let p = parse_model_output(r#"{"answer":["A","D"]}"#, &q).unwrap();
    assert_eq!(format_for_ocs(&p, &q, &cfg), "A#D");
}

// ---------- multiple ----------

#[test]
fn multiple_json_array() {
    let q = question("t", "multiple", CITY_OPTS);
    let a = answer(r#"{"answer": ["D", "A", "B", "A"]}"#, &q).unwrap();
    assert_eq!(a, "北京#上海#广州");
    assert_ocs_selects(&a, &q, &[0, 1, 3]);
}

#[test]
fn multiple_letter_formats() {
    let q = question("t", "multiple", CITY_OPTS);
    for raw in [
        "ABD",
        "A,B,D",
        "A, B, D",
        "A、B、D",
        "A，B，D",
        "A B D",
        "A#B#D",
        "答案：ABD",
        "正确答案是 A、B、D",
        r#"{"answer": "ABD"}"#,
        r#"{"answer": "A,B,D"}"#,
        r#"{"answer": ["北京", "上海", "广州"]}"#,
        "北京#上海#广州",
    ] {
        assert_eq!(choices(raw, &q), [0, 1, 3], "input {raw:?}");
    }
}

#[test]
fn multiple_single_correct_option_still_selects_it() {
    let q = question("t", "multiple", CITY_OPTS);
    let a = answer(r#"{"answer": ["C"]}"#, &q).unwrap();
    assert_ocs_selects(&a, &q, &[2]);
}

#[test]
fn multiple_options_containing_separator_use_letters() {
    // Option text contains '#', which OCS would split on.
    let q = question("t", "multiple", "A. C#\nB. Java\nC. F#\nD. Go");
    let a = answer(r#"{"answer": ["A", "C"]}"#, &q).unwrap();
    assert_ocs_selects(&a, &q, &[0, 2]);
}

#[test]
fn multiple_similar_options_are_disambiguated_by_letter_fallback() {
    let q = question(
        "t",
        "multiple",
        "A. 计算机网络\nB. 计算机网络原理\nC. 操作系统\nD. 数据结构",
    );
    let a = answer(r#"{"answer": ["A", "B"]}"#, &q).unwrap();
    assert_ocs_selects(&a, &q, &[0, 1]);
}

#[test]
fn multiple_json_never_returned_as_array() {
    let q = question("t", "multiple", CITY_OPTS);
    let a = answer(r#"{"answer": ["A", "B"]}"#, &q).unwrap();
    assert!(!a.starts_with('['));
}

// ---------- judgement ----------

#[test]
fn judgement_boolean_json() {
    let q = question("t", "judgement", "");
    assert_eq!(answer(r#"{"answer": true}"#, &q).unwrap(), "正确");
    assert_eq!(answer(r#"{"answer": false}"#, &q).unwrap(), "错误");
}

#[test]
fn judgement_synonyms() {
    let q = question("t", "judgement", "");
    for (raw, expect) in [
        ("正确", true),
        ("对", true),
        ("True", true),
        ("T", true),
        ("√", true),
        ("是", true),
        ("yes", true),
        ("错误", false),
        ("错", false),
        ("False", false),
        ("F", false),
        ("×", false),
        ("不正确", false),
        ("否", false),
        (r#"{"answer": "对"}"#, true),
        (r#"{"answer": "错误"}"#, false),
        (r#"{"answer": "true"}"#, true),
        ("答案：错误", false),
        ("该说法不正确，因为……", false),
        ("这个说法是正确的。", true),
    ] {
        let p = parse_model_output(raw, &q).unwrap();
        assert_eq!(p.value, AnswerValue::Judgement(expect), "input {raw:?}");
    }
}

#[test]
fn judgement_maps_to_option_wording() {
    let q = question("t", "judgement", "A. 对\nB. 错");
    let a = answer(r#"{"answer": false}"#, &q).unwrap();
    assert_eq!(ocs::resolve_judgement(&a, &q.raw_options()), Some(1));

    let q = question("t", "judgement", "√\n×");
    let a = answer(r#"{"answer": true}"#, &q).unwrap();
    assert_eq!(ocs::resolve_judgement(&a, &q.raw_options()), Some(0));
}

#[test]
fn judgement_by_option_letter() {
    let q = question("t", "judgement", "A. 正确\nB. 错误");
    let p = parse_model_output(r#"{"answer": "B"}"#, &q).unwrap();
    assert_eq!(p.value, AnswerValue::Judgement(false));
}

#[test]
fn judgement_ambiguous_is_rejected() {
    let q = question("t", "judgement", "");
    assert!(parse_model_output("不确定", &q).is_err());
}

// ---------- completion ----------

#[test]
fn completion_json_array_keeps_order() {
    let q = question("____是中国的首都，____是最大城市。", "completion", "");
    assert_eq!(
        answer(r#"{"answer": ["北京", "上海"]}"#, &q).unwrap(),
        "北京#上海"
    );
}

#[test]
fn completion_single_string() {
    let q = question("t", "completion", "");
    assert_eq!(answer(r#"{"answer": "光合作用"}"#, &q).unwrap(), "光合作用");
    assert_eq!(answer(r#"{"answer": 42}"#, &q).unwrap(), "42");
}

#[test]
fn completion_strips_explanation() {
    let q = question("t", "completion", "");
    assert_eq!(
        answer("答案：光合作用\n\n解析：植物利用光能……", &q).unwrap(),
        "光合作用"
    );
    assert_eq!(
        answer("光合作用（解析：植物利用光能）", &q).unwrap(),
        "光合作用"
    );
}

#[test]
fn completion_numbered_lines() {
    let q = question("t", "completion", "");
    assert_eq!(
        answer("第1空：北京\n第2空：上海", &q).unwrap(),
        "北京#上海"
    );
    assert_eq!(answer("1. 北京\n2. 上海", &q).unwrap(), "北京#上海");
}

#[test]
fn completion_result_is_split_by_ocs_in_order() {
    let q = question("t", "completion", "");
    let a = answer(r#"{"answer": ["x+1", "y"]}"#, &q).unwrap();
    assert_eq!(ocs::split_answer(&a), ["x+1", "y"]);
}

// ---------- unknown ----------

#[test]
fn unknown_with_options_accepts_one_or_many() {
    let q = question("t", "", CITY_OPTS);
    assert_eq!(choices(r#"{"answer": ["B"]}"#, &q), [1]);
    assert_eq!(choices(r#"{"answer": ["A", "C"]}"#, &q), [0, 2]);
}

// ---------- failure modes ----------

#[test]
fn empty_output_is_error() {
    let q = question("t", "single", LANG_OPTS);
    assert!(matches!(
        parse_model_output("   ", &q),
        Err(AnswerError::EmptyOutput)
    ));
    assert!(matches!(
        parse_model_output("<think>long reasoning</think>", &q),
        Err(AnswerError::EmptyOutput)
    ));
}

#[test]
fn think_block_is_ignored() {
    let q = question("t", "single", LANG_OPTS);
    assert_eq!(
        choices("<think>A 不对，C 也不对</think>\n{\"answer\":\"B\"}", &q),
        [1]
    );
}

#[test]
fn truncated_json_is_recovered() {
    let q = question("t", "single", LANG_OPTS);
    assert_eq!(
        choices(r#"{"answer": "B", "explanation": "Rust 是一门系统编"#, &q),
        [1]
    );
}

#[test]
fn empty_answer_field_is_error() {
    let q = question("t", "multiple", CITY_OPTS);
    assert!(parse_model_output(r#"{"answer": []}"#, &q).is_err());
    let q = question("t", "completion", "");
    assert!(parse_model_output(r#"{"answer": ""}"#, &q).is_err());
}

#[test]
fn unmappable_answer_is_error() {
    let q = question("t", "single", LANG_OPTS);
    let err = parse_model_output(r#"{"answer": "Python"}"#, &q).unwrap_err();
    assert!(matches!(err, AnswerError::Unrecognized { .. }));
}
