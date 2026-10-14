//! Answering pipeline: cache → prompt → provider(s) → parse (with self-repair) → vote → OCS format.

pub mod cache;
pub mod ensemble;
pub mod normalize;
pub mod ocs;
pub mod prompt;
pub mod question;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::broadcast;

pub use question::{Question, QuestionInput, QuestionOption, QuestionType, parse_options};

use crate::config::{AnswerConfig, EnsembleConfig};
use crate::error::{AnswerError, AppError};
use crate::provider::{AiClient, ChatRequest, Message};
use cache::{AnswerCache, CachedAnswer, now_secs};
use ensemble::{Tally, Vote, describe};
use normalize::{ParsedAnswer, format_for_ocs, parse_model_output};
use prompt::PromptBuilder;

/// Result of answering one question.
#[derive(Debug, Clone)]
pub struct AnswerOutcome {
    pub question: String,
    pub answer: String,
    pub qtype: QuestionType,
    pub explanation: Option<String>,
    pub provider: String,
    pub model: String,
    pub elapsed_ms: u64,
    pub cached: bool,
    /// Share of votes for the chosen answer when voting is enabled.
    pub confidence: Option<f64>,
    pub votes: Option<usize>,
}

type InflightResult = Result<AnswerOutcome, String>;

#[derive(Clone)]
pub struct Answerer {
    inner: Arc<Inner>,
}

struct Inner {
    /// Index 0 is the main `[ai]` model; the rest are `[ensemble].models`.
    voters: Vec<AiClient>,
    judge: Option<AiClient>,
    prompts: PromptBuilder,
    cfg: AnswerConfig,
    ensemble: EnsembleConfig,
    cache: Option<AnswerCache>,
    /// Total time available per question; voting rounds and the judge stay within it.
    time_budget: Duration,
    /// Identical questions arriving concurrently share one upstream call.
    inflight: Mutex<HashMap<String, broadcast::Sender<InflightResult>>>,
}

impl Answerer {
    pub fn new(client: AiClient, cfg: &AnswerConfig) -> Self {
        Self::builder(client, cfg).build()
    }

    pub fn builder(client: AiClient, cfg: &AnswerConfig) -> AnswererBuilder {
        AnswererBuilder {
            voters: vec![client],
            judge: None,
            cfg: cfg.clone(),
            ensemble: EnsembleConfig::default(),
            cache: None,
            time_budget: Duration::from_secs(55),
        }
    }

    pub fn client(&self) -> &AiClient {
        &self.inner.voters[0]
    }

    pub fn voters(&self) -> &[AiClient] {
        &self.inner.voters
    }

    pub fn judge(&self) -> Option<&AiClient> {
        self.inner.judge.as_ref()
    }

    pub fn cache(&self) -> Option<&AnswerCache> {
        self.inner.cache.as_ref()
    }

    pub fn ensemble_enabled(&self) -> bool {
        self.inner.ensemble.enabled
    }

    /// Answers a question. `no_cache` skips the cache lookup (the result is still stored).
    pub async fn answer(
        &self,
        question: &Question,
        no_cache: bool,
    ) -> Result<AnswerOutcome, AppError> {
        if question.title.is_empty() {
            return Err(AppError::BadRequest("缺少题目内容 title".into()));
        }
        let started = Instant::now();
        let key = question.fingerprint();

        if !no_cache
            && let Some(cache) = &self.inner.cache
            && let Some(hit) = cache.get(&key)
        {
            tracing::debug!(key = %key, "命中答案缓存");
            return Ok(AnswerOutcome {
                question: question.title.clone(),
                answer: hit.answer,
                qtype: question.qtype,
                explanation: hit.explanation,
                provider: hit.provider,
                model: hit.model,
                elapsed_ms: started.elapsed().as_millis() as u64,
                cached: true,
                confidence: hit.confidence,
                votes: None,
            });
        }

        // Join an identical request already in progress instead of paying twice.
        let rx = {
            let mut map = self
                .inner
                .inflight
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            match map.get(&key) {
                Some(tx) => Some(tx.subscribe()),
                None => {
                    let (tx, _) = broadcast::channel(1);
                    map.insert(key.clone(), tx);
                    None
                }
            }
        };
        if let Some(mut rx) = rx {
            tracing::debug!(key = %key, "相同题目正在作答，等待其结果");
            return match rx.recv().await {
                Ok(Ok(mut outcome)) => {
                    outcome.elapsed_ms = started.elapsed().as_millis() as u64;
                    Ok(outcome)
                }
                Ok(Err(msg)) => Err(AppError::Answer(AnswerError::Unrecognized {
                    reason: "同一题目的并发请求失败".into(),
                    raw: msg,
                })),
                // The leader was cancelled (client went away); answer on our own.
                Err(_) => Box::pin(self.answer(question, true)).await,
            };
        }

        let guard = InflightGuard {
            inner: &self.inner,
            key: key.clone(),
        };
        let result = self.solve(question, started).await;
        if let Some(tx) = guard.take() {
            let _ = tx.send(result.as_ref().map(Clone::clone).map_err(|e| e.to_string()));
        }

        let outcome = result?;
        if let Some(cache) = &self.inner.cache {
            cache.insert(CachedAnswer {
                key,
                question: question.title.clone(),
                qtype: question.qtype.as_str().into(),
                answer: outcome.answer.clone(),
                explanation: outcome.explanation.clone(),
                model: outcome.model.clone(),
                provider: outcome.provider.clone(),
                confidence: outcome.confidence,
                created_at: now_secs(),
            });
        }
        Ok(outcome)
    }

    async fn solve(&self, q: &Question, started: Instant) -> Result<AnswerOutcome, AppError> {
        let inner = &self.inner;
        if !inner.ensemble.enabled {
            let vote = self.ask(&inner.voters[0], q, None).await?;
            return Ok(self.outcome(q, vote.parsed, &vote.model, &inner.voters[0], started, None));
        }

        // Leave headroom for formatting and the HTTP response before the server-level timeout.
        let deadline = started + inner.time_budget.saturating_sub(Duration::from_secs(2));
        let samples = inner.ensemble.samples.max(1);
        let all_pairs: Vec<(usize, u32)> = (0..inner.voters.len())
            .flat_map(|v| (0..samples).map(move |s| (v, s)))
            .collect();
        let (first, rest): (Vec<_>, Vec<_>) = if inner.ensemble.early_stop {
            all_pairs.into_iter().partition(|&(_, s)| s == 0)
        } else {
            (all_pairs, Vec::new())
        };

        let run = |pairs: Vec<(usize, u32)>| async move {
            ensemble::collect_until(&pairs, deadline, |v, s| async move {
                // Extra samples use a higher temperature so they are not identical copies.
                let temperature = (s > 0).then_some(0.7);
                let voter = &self.inner.voters[v];
                self.ask(voter, q, temperature)
                    .await
                    .inspect_err(
                        |e| tracing::warn!(voter = %voter.name(), error = %e, "投票者作答失败"),
                    )
                    .ok()
            })
            .await
        };
        let round_started = Instant::now();
        let mut votes = run(first).await;
        let round_time = round_started.elapsed();

        let first_round_unanimous = !votes.is_empty() && Tally::new(votes.clone()).unanimous();
        let need_more = !rest.is_empty()
            && !(first_round_unanimous && votes.len() >= inner.voters.len().min(2));
        if need_more {
            // Only start another round if a similar-length round can still finish.
            if deadline.saturating_duration_since(Instant::now()) > round_time {
                votes.extend(run(rest).await);
            } else {
                tracing::info!("剩余时间不足以进行第二轮采样，直接使用第一轮投票");
            }
        }

        if votes.is_empty() {
            // Every voter failed; one more attempt on the main model yields a precise error.
            let v = self.ask(&inner.voters[0], q, None).await?;
            return Ok(self.outcome(q, v.parsed, &v.model, &inner.voters[0], started, None));
        }

        let tally = Tally::new(votes);
        let agreement = tally.agreement();
        let leader = tally.leader().expect("non-empty tally").clone();
        tracing::info!(
            total = tally.total,
            groups = tally.groups.len(),
            agreement = format!("{:.2}", agreement),
            "投票结果"
        );

        if agreement + 1e-9 < inner.ensemble.min_agreement
            && let Some(judge) = &inner.judge
        {
            let candidates: Vec<(String, usize)> = tally
                .groups
                .iter()
                .map(|g| (describe(&g.votes[0].parsed.value, q), g.votes.len()))
                .collect();
            let req = inner.prompts.build_judge(q, &candidates);
            let judged =
                tokio::time::timeout_at(deadline.into(), self.request_and_parse(judge, q, req))
                    .await
                    .unwrap_or(Err(AppError::Timeout(inner.time_budget.as_secs())));
            match judged {
                Ok(v) => {
                    tracing::info!(judge = %judge.name(), "票数分散，采用裁判模型的答案");
                    let support = tally
                        .groups
                        .iter()
                        .find(|g| g.key == ensemble::vote_key(&v.parsed.value))
                        .map(|g| g.votes.len())
                        .unwrap_or(0);
                    let conf = support as f64 / tally.total as f64;
                    return Ok(self.outcome(
                        q,
                        v.parsed,
                        &v.model,
                        judge,
                        started,
                        Some((conf, tally.total)),
                    ));
                }
                Err(e) => tracing::warn!(error = %e, "裁判模型失败，采用得票最多的答案"),
            }
        }

        let best = leader.votes[0].clone();
        let client = inner
            .voters
            .iter()
            .find(|c| c.name() == best.voter)
            .unwrap_or(&inner.voters[0]);
        Ok(self.outcome(
            q,
            best.parsed,
            &best.model,
            client,
            started,
            Some((agreement, tally.total)),
        ))
    }

    fn outcome(
        &self,
        q: &Question,
        parsed: ParsedAnswer,
        model: &str,
        client: &AiClient,
        started: Instant,
        vote: Option<(f64, usize)>,
    ) -> AnswerOutcome {
        AnswerOutcome {
            question: q.title.clone(),
            answer: format_for_ocs(&parsed, q, &self.inner.cfg),
            qtype: q.qtype,
            explanation: parsed.explanation,
            provider: client.kind().as_str().to_string(),
            model: model.to_string(),
            elapsed_ms: started.elapsed().as_millis() as u64,
            cached: false,
            confidence: vote.map(|v| v.0),
            votes: vote.map(|v| v.1),
        }
    }

    async fn ask(
        &self,
        client: &AiClient,
        q: &Question,
        temperature: Option<f64>,
    ) -> Result<Vote, AppError> {
        let mut req = self.inner.prompts.build(q);
        req.temperature = temperature;
        self.request_and_parse(client, q, req).await
    }

    /// Sends `req`, parses the reply and, if it cannot be parsed, asks the model to restate it.
    async fn request_and_parse(
        &self,
        client: &AiClient,
        q: &Question,
        mut req: ChatRequest,
    ) -> Result<Vote, AppError> {
        tracing::debug!(model = %client.name(), prompt = %req.user_text(), "构造模型请求");
        let mut attempts_left = self.inner.cfg.self_repair_attempts;
        loop {
            let response = client.complete(&req).await?;
            tracing::debug!(model = %client.name(), raw = %response.content, "模型原始输出");
            let model = response
                .model
                .clone()
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| client.model().to_string());
            match parse_model_output(&response.content, q) {
                Ok(parsed) => {
                    return Ok(Vote {
                        parsed,
                        voter: client.name().to_string(),
                        model,
                    });
                }
                Err(e) if attempts_left > 0 => {
                    attempts_left -= 1;
                    tracing::info!(model = %client.name(), error = %e, "模型输出无法解析，请求模型按格式重述");
                    let reason = match &e {
                        AnswerError::EmptyOutput => "回复为空".to_string(),
                        AnswerError::Unrecognized { reason, .. } => reason.clone(),
                    };
                    req.messages
                        .push(Message::assistant(response.content.clone()));
                    req.messages
                        .push(Message::user(self.inner.prompts.repair_message(q, &reason)));
                }
                Err(e) => {
                    tracing::warn!(model = %client.name(), error = %e, "模型输出解析失败");
                    return Err(e.into());
                }
            }
        }
    }
}

/// Removes the in-flight entry even if the leader future is dropped mid-way.
struct InflightGuard<'a> {
    inner: &'a Inner,
    key: String,
}

impl InflightGuard<'_> {
    fn take(self) -> Option<broadcast::Sender<InflightResult>> {
        let tx = self
            .inner
            .inflight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.key);
        std::mem::forget(self);
        tx
    }
}

impl Drop for InflightGuard<'_> {
    fn drop(&mut self) {
        self.inner
            .inflight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.key);
    }
}

pub struct AnswererBuilder {
    voters: Vec<AiClient>,
    judge: Option<AiClient>,
    cfg: AnswerConfig,
    ensemble: EnsembleConfig,
    cache: Option<AnswerCache>,
    time_budget: Duration,
}

impl AnswererBuilder {
    pub fn time_budget(mut self, budget: Duration) -> Self {
        self.time_budget = budget;
        self
    }

    pub fn ensemble(
        mut self,
        cfg: &EnsembleConfig,
        extra_voters: Vec<AiClient>,
        judge: Option<AiClient>,
    ) -> Self {
        self.ensemble = cfg.clone();
        self.voters.extend(extra_voters);
        self.judge = judge;
        self
    }

    pub fn cache(mut self, cache: Option<AnswerCache>) -> Self {
        self.cache = cache;
        self
    }

    pub fn build(self) -> Answerer {
        Answerer {
            inner: Arc::new(Inner {
                voters: self.voters,
                judge: self.judge,
                prompts: PromptBuilder::new(&self.cfg),
                cfg: self.cfg,
                ensemble: self.ensemble,
                cache: self.cache,
                time_budget: self.time_budget,
                inflight: Mutex::new(HashMap::new()),
            }),
        }
    }
}
