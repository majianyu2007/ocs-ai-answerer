use crate::config::AnswerConfig;
use crate::provider::ChatRequest;

use super::{Question, QuestionType};

pub const DEFAULT_SYSTEM_PROMPT: &str = "你是一名知识全面、严谨细致的答题助手，负责为在线课程的题目给出正确答案。\
请先在内部认真分析题干与每个选项，再给出最终答案。\
你的回复必须是一个 JSON 对象，不要输出 JSON 以外的任何文字，也不要使用 Markdown 代码块。";

pub const DEFAULT_SINGLE_PROMPT: &str =
    "这是一道单选题，只有一个选项是正确的。请选出唯一正确的选项。";
pub const DEFAULT_MULTIPLE_PROMPT: &str = "这是一道多选题，可能有两个或更多正确选项。\
请逐一判断每个选项，选出全部正确的选项，不要遗漏，也不要多选，不要默认选择固定数量。";
pub const DEFAULT_JUDGEMENT_PROMPT: &str = "这是一道判断题，请判断题干中的说法是否正确。";
pub const DEFAULT_COMPLETION_PROMPT: &str = "这是一道填空题，请给出每个空应填写的内容。\
答案要简洁准确，只填写空格处的词语、短语、数字或公式，不要复述题干，不要附带解释。\
题目中有多个空时，按空出现的先后顺序依次作答。";
pub const DEFAULT_UNKNOWN_PROMPT: &str = "题目类型未知，请根据题干和选项自行判断题型并作答：\
有选项时选出全部正确的选项（可能只有一个，也可能有多个）；没有选项时直接给出应填写的答案。";

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

    pub fn system(&self) -> &str {
        non_empty(&self.cfg.system_prompt).unwrap_or(DEFAULT_SYSTEM_PROMPT)
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

    pub fn build(&self, q: &Question) -> ChatRequest {
        let qtype = q.qtype;
        let mut user = String::new();
        user.push_str(self.instruction(qtype));
        user.push_str("\n\n【题型】");
        user.push_str(qtype.label());
        user.push_str("\n【题目】\n");
        user.push_str(q.title.trim());
        if !q.options.is_empty() {
            user.push_str("\n【选项】");
            for (i, opt) in q.options.iter().enumerate() {
                user.push('\n');
                user.push_str(&Question::letter(i));
                user.push_str(". ");
                user.push_str(&opt.text);
            }
        }
        user.push_str("\n\n【输出格式】\n");
        user.push_str(&self.format_instruction(q));
        ChatRequest {
            system: self.system().to_string(),
            user,
        }
    }

    fn format_instruction(&self, q: &Question) -> String {
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
                "只输出 JSON：{{\"answer\": \"B\"{explanation}}}\nanswer 为唯一正确选项的字母（A-{last}），只能是一个字母。"
            ),
            QuestionType::Multiple => format!(
                "只输出 JSON：{{\"answer\": [\"A\", \"C\"]{explanation}}}\nanswer 为全部正确选项字母组成的数组（A-{last}），按字母顺序排列。"
            ),
            QuestionType::Judgement => format!(
                "只输出 JSON：{{\"answer\": true{explanation}}}\n说法正确时 answer 为 true，错误时为 false。"
            ),
            QuestionType::Completion => format!(
                "只输出 JSON：{{\"answer\": [\"第1空答案\", \"第2空答案\"]{explanation}}}\nanswer 为字符串数组，每个元素对应一个空，只有一个空时数组只含一个元素。"
            ),
            QuestionType::Unknown if q.options.is_empty() => format!(
                "只输出 JSON：{{\"answer\": [\"答案\"]{explanation}}}\nanswer 为字符串数组，每个元素对应一个需要填写的答案。"
            ),
            QuestionType::Unknown => format!(
                "只输出 JSON：{{\"answer\": [\"A\"]{explanation}}}\nanswer 为全部正确选项字母组成的数组（A-{last}），只有一个正确选项时数组只含一个字母。"
            ),
        }
    }
}

fn non_empty(s: &str) -> Option<&str> {
    let t = s.trim();
    (!t.is_empty()).then_some(t)
}
