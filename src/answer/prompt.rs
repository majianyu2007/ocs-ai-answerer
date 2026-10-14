use crate::config::AnswerConfig;
use crate::provider::{ChatRequest, Message};

use super::question::{Question, QuestionType};

pub const DEFAULT_SYSTEM_PROMPT: &str = "你是一名知识全面、严谨细致的答题专家，负责为在线课程（大学及职业教育各学科）的题目给出正确答案。\
请依据学科教材中的标准表述与通行观点作答；涉及计算时务必逐步验算；注意题干中的「不」「错误」「不正确」「不包括」「除外」等否定词，\
以及「最」「首要」「根本」等限定词。";

pub const DEFAULT_SINGLE_PROMPT: &str = "这是一道单选题，只有一个选项是正确的。\
请逐一检查每个选项，排除错误项后选出唯一最符合题意的选项。";
pub const DEFAULT_MULTIPLE_PROMPT: &str = "这是一道多选题，正确选项可能有两个或更多，也可能全部正确。\
请对每个选项独立判断正误，选出全部正确的选项：不要遗漏，也不要多选，不要默认选择固定数量。";
pub const DEFAULT_JUDGEMENT_PROMPT: &str = "这是一道判断题，请判断题干中的说法是否正确。\
注意说法中任何一处错误（如绝对化表述、概念偷换、数据错误）都会使整句判为错误。";
pub const DEFAULT_COMPLETION_PROMPT: &str = "这是一道填空题，请给出每个空应填写的内容。\
答案要简洁准确，优先使用教材中的标准术语，只填写空格处的词语、短语、数字或公式，不要复述题干，不要附带解释。\
题目中有多个空时，按空出现的先后顺序依次作答。";
pub const DEFAULT_UNKNOWN_PROMPT: &str = "题目类型未知，请根据题干和选项自行判断题型并作答：\
有选项时选出全部正确的选项（可能只有一个，也可能有多个）；没有选项时直接给出应填写的答案。";

const IMAGE_NOTE: &str = "题目或选项中的「[图片N]」表示附带的第 N 张图片，请结合图片内容作答。";

/// Builds provider-agnostic prompts. Type instructions are configurable; the output format block
/// is not, because the normalizer depends on it.
#[derive(Debug, Clone)]
pub struct PromptBuilder {
    cfg: AnswerConfig,
}

impl PromptBuilder {
    pub fn new(cfg: &AnswerConfig) -> Self {
        Self { cfg: cfg.clone() }
    }

    pub fn system(&self) -> String {
        let base = non_empty(&self.cfg.system_prompt).unwrap_or(DEFAULT_SYSTEM_PROMPT);
        let tail = if self.cfg.step_by_step {
            "回复格式：先进行简明的分析，最后另起一行输出一个 JSON 对象作为最终答案，JSON 之后不要再输出任何内容。"
        } else {
            "回复格式：只输出一个 JSON 对象，不要输出 JSON 以外的任何文字，也不要使用 Markdown 代码块。"
        };
        format!("{base}\n{tail}")
    }

    fn instruction(&self, qtype: QuestionType) -> &str {
        let (custom, default) = match qtype {
            QuestionType::Single => (&self.cfg.single_prompt, DEFAULT_SINGLE_PROMPT),
            QuestionType::Multiple => (&self.cfg.multiple_prompt, DEFAULT_MULTIPLE_PROMPT),
            QuestionType::Judgement => (&self.cfg.judgement_prompt, DEFAULT_JUDGEMENT_PROMPT),
            QuestionType::Completion => (&self.cfg.completion_prompt, DEFAULT_COMPLETION_PROMPT),
            QuestionType::Unknown => (&self.cfg.unknown_prompt, DEFAULT_UNKNOWN_PROMPT),
        };
        non_empty(custom).unwrap_or(default)
    }

    /// The question block shared by answer and judge prompts.
    pub fn question_block(&self, q: &Question) -> String {
        let mut out = String::new();
        out.push_str("【题型】");
        out.push_str(q.qtype.label());
        out.push_str("\n【题目】\n");
        out.push_str(q.display_title());
        if !q.options.is_empty() {
            out.push_str("\n【选项】");
            for (i, opt) in q.options.iter().enumerate() {
                out.push('\n');
                out.push_str(&Question::letter(i));
                out.push_str(". ");
                out.push_str(&opt.display);
            }
        }
        out
    }

    pub fn build(&self, q: &Question) -> ChatRequest {
        let mut user = String::new();
        user.push_str(self.instruction(q.qtype));
        if !q.images.is_empty() {
            user.push('\n');
            user.push_str(IMAGE_NOTE);
        }
        user.push_str("\n\n");
        user.push_str(&self.question_block(q));
        user.push_str("\n\n【输出格式】\n");
        user.push_str(&self.format_instruction(q));
        ChatRequest {
            system: self.system(),
            messages: vec![Message {
                images: q.images.clone(),
                ..Message::user(user)
            }],
            temperature: None,
        }
    }

    /// Asks a judge model to decide between disagreeing candidate answers.
    pub fn build_judge(&self, q: &Question, candidates: &[(String, usize)]) -> ChatRequest {
        let mut user = String::from(
            "多位答题者对下面这道题给出了不同的答案。请独立、仔细地重新分析题目，判断哪个答案正确；\
如果都不正确，请给出你认为正确的答案。",
        );
        if !q.images.is_empty() {
            user.push('\n');
            user.push_str(IMAGE_NOTE);
        }
        user.push_str("\n\n");
        user.push_str(&self.question_block(q));
        user.push_str("\n\n【候选答案】");
        for (answer, votes) in candidates {
            user.push_str(&format!("\n- {answer}（{votes} 票）"));
        }
        user.push_str("\n\n【输出格式】\n");
        user.push_str(&self.format_instruction(q));
        ChatRequest {
            system: self.system(),
            messages: vec![Message {
                images: q.images.clone(),
                ..Message::user(user)
            }],
            temperature: None,
        }
    }

    /// Follow-up turn asking the model to restate an unparseable reply in the required format.
    pub fn repair_message(&self, q: &Question, reason: &str) -> String {
        format!(
            "你的上一条回复无法被程序解析（{reason}）。请不要重新分析，直接按以下要求只输出一个 JSON 对象：\n{}",
            self.json_spec(q)
        )
    }

    fn format_instruction(&self, q: &Question) -> String {
        let spec = self.json_spec(q);
        if self.cfg.step_by_step {
            format!("先简要分析，最后一行只输出 JSON：{spec}")
        } else {
            format!("只输出 JSON：{spec}")
        }
    }

    fn json_spec(&self, q: &Question) -> String {
        let explanation = if self.cfg.include_explanation {
            ", \"explanation\": \"不超过 50 字的简要理由\""
        } else {
            ""
        };
        let last = q
            .options
            .len()
            .checked_sub(1)
            .map(Question::letter)
            .unwrap_or_else(|| "D".into());
        match q.qtype {
            QuestionType::Single => format!(
                "{{\"answer\": \"B\"{explanation}}}\nanswer 为唯一正确选项的字母（A-{last}），只能是一个字母。"
            ),
            QuestionType::Multiple => format!(
                "{{\"answer\": [\"A\", \"C\"]{explanation}}}\nanswer 为全部正确选项字母组成的数组（A-{last}），按字母顺序排列。"
            ),
            QuestionType::Judgement => format!(
                "{{\"answer\": true{explanation}}}\n说法正确时 answer 为 true，错误时为 false。"
            ),
            QuestionType::Completion => format!(
                "{{\"answer\": [\"第1空答案\", \"第2空答案\"]{explanation}}}\nanswer 为字符串数组，每个元素对应一个空，只有一个空时数组只含一个元素。"
            ),
            QuestionType::Unknown if q.options.is_empty() => format!(
                "{{\"answer\": [\"答案\"]{explanation}}}\nanswer 为字符串数组，每个元素对应一个需要填写的答案。"
            ),
            QuestionType::Unknown => format!(
                "{{\"answer\": [\"A\"]{explanation}}}\nanswer 为全部正确选项字母组成的数组（A-{last}），只有一个正确选项时数组只含一个字母。"
            ),
        }
    }
}

fn non_empty(s: &str) -> Option<&str> {
    let t = s.trim();
    (!t.is_empty()).then_some(t)
}
