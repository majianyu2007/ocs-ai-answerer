//! Self-consistency voting across several samples and/or models.
//!
//! Round 1 asks every voter once. If all valid replies agree (and `early_stop` is on) that answer
//! wins immediately, so the common easy case costs one request per voter. Otherwise the remaining
//! samples are requested and votes are counted on the normalized answer; a judge model resolves
//! low-agreement results.

use std::collections::HashMap;

use std::time::Instant;

use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;

use super::normalize::{AnswerValue, ParsedAnswer};
use super::ocs;
use super::question::Question;

/// One successful vote.
#[derive(Debug, Clone)]
pub struct Vote {
    pub parsed: ParsedAnswer,
    pub voter: String,
    pub model: String,
}

/// Canonical form used to decide whether two answers are the same.
pub fn vote_key(value: &AnswerValue) -> String {
    match value {
        AnswerValue::Choices(idx) => {
            let mut idx = idx.clone();
            idx.sort_unstable();
            idx.dedup();
            format!(
                "c:{}",
                idx.iter()
                    .map(|&i| Question::letter(i))
                    .collect::<Vec<_>>()
                    .join("")
            )
        }
        AnswerValue::Judgement(b) => format!("j:{b}"),
        AnswerValue::Blanks(blanks) => format!(
            "b:{}",
            blanks
                .iter()
                .map(|b| ocs::normalize_string(b))
                .collect::<Vec<_>>()
                .join("|")
        ),
    }
}

/// Human-readable answer used in the judge prompt.
pub fn describe(value: &AnswerValue, q: &Question) -> String {
    match value {
        AnswerValue::Choices(idx) => idx
            .iter()
            .map(|&i| format!("{}. {}", Question::letter(i), q.options[i].display))
            .collect::<Vec<_>>()
            .join("；"),
        AnswerValue::Judgement(true) => "正确".into(),
        AnswerValue::Judgement(false) => "错误".into(),
        AnswerValue::Blanks(b) => b.join(" / "),
    }
}

#[derive(Debug, Clone)]
pub struct Tally {
    /// Groups ordered by votes (desc), ties broken by first appearance.
    pub groups: Vec<Group>,
    pub total: usize,
}

#[derive(Debug, Clone)]
pub struct Group {
    pub key: String,
    pub votes: Vec<Vote>,
}

impl Tally {
    pub fn new(votes: Vec<Vote>) -> Self {
        let total = votes.len();
        let mut order: Vec<String> = Vec::new();
        let mut map: HashMap<String, Vec<Vote>> = HashMap::new();
        for v in votes {
            let key = vote_key(&v.parsed.value);
            if !map.contains_key(&key) {
                order.push(key.clone());
            }
            map.entry(key).or_default().push(v);
        }
        let mut groups: Vec<Group> = order
            .into_iter()
            .map(|key| Group {
                votes: map.remove(&key).unwrap_or_default(),
                key,
            })
            .collect();
        // Stable sort keeps first-seen order among ties.
        groups.sort_by_key(|g| std::cmp::Reverse(g.votes.len()));
        Self { groups, total }
    }

    pub fn leader(&self) -> Option<&Group> {
        self.groups.first()
    }

    pub fn agreement(&self) -> f64 {
        match (self.leader(), self.total) {
            (Some(g), t) if t > 0 => g.votes.len() as f64 / t as f64,
            _ => 0.0,
        }
    }

    pub fn unanimous(&self) -> bool {
        self.groups.len() == 1
    }
}

/// Runs `make(voter_index, sample_index)` for every pending pair concurrently and keeps whatever
/// finished before `deadline`; unfinished calls are cancelled instead of losing earlier votes.
pub async fn collect_until<F, Fut>(pairs: &[(usize, u32)], deadline: Instant, make: F) -> Vec<Vote>
where
    F: Fn(usize, u32) -> Fut,
    Fut: std::future::Future<Output = Option<Vote>>,
{
    let mut pending: FuturesUnordered<Fut> = pairs.iter().map(|&(v, s)| make(v, s)).collect();
    let mut out = Vec::new();
    loop {
        match tokio::time::timeout_at(deadline.into(), pending.next()).await {
            Ok(Some(Some(v))) => out.push(v),
            Ok(Some(None)) => {}
            Ok(None) => break,
            Err(_) => {
                if !pending.is_empty() {
                    tracing::warn!(
                        unfinished = pending.len(),
                        "投票时间预算用尽，使用已收到的结果"
                    );
                }
                break;
            }
        }
    }
    out
}
