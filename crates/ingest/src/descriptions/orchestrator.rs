//! Description-extraction Pass 2 orchestrator.
//!
//! Dispatches extraction over a bounded thread pool. Uses blocking HTTP
//! (via `ureq` in providers) so integration with the rest of the sync
//! ingest pipeline is straightforward — no tokio runtime required.
//!
//! Concurrency model:
//! - `cfg.concurrency` worker threads process from a bounded `crossbeam_channel`-
//!   free fallback: std `mpsc` + a semaphore-ish `Arc<AtomicUsize>` cap.
//! - Per-call retry with exponential backoff, capped at 3 attempts.
//! - Per-run call cap via `cfg.max_desc_calls` — exceeded stops enqueueing
//!   further candidates and marks `cost_cap_reached`.
//!
//! The orchestrator NEVER catches and ignores a non-retryable provider
//! error — each outcome maps to a specific counter in `Report`.

use crate::descriptions::config::DescriptionsConfig;
use crate::descriptions::provider::{DescriptionProvider, ProviderError, Snippet};
use crate::descriptions::redactor::redact;
use crate::descriptions::report::Report;
use crate::descriptions::schema::Description;
use anyhow::Result;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::Duration;

/// Input to the orchestrator — a batch of candidate entities to describe.
#[derive(Clone, Debug)]
pub struct ExtractionInputs {
    pub entities: Vec<CandidateEntity>,
}

/// A single entity under consideration for description extraction.
#[derive(Clone, Debug)]
pub struct CandidateEntity {
    pub id: String,
    pub name: String,
    pub entity_type: String,
    pub is_public: bool,
    pub doc_comment: String,
    /// First ~10 lines of the entity's body (already extracted upstream).
    pub body_head: String,
}

impl CandidateEntity {
    /// Trivial delegates (empty body or body that's just a function call
    /// with no branching) are skipped — they add graph noise. An entity
    /// is non-trivial if either its body_head OR its doc_comment has
    /// more than one non-empty line worth of substance.
    pub fn is_trivial(&self) -> bool {
        let combined = format!("{}\n{}", self.doc_comment.trim(), self.body_head.trim());
        let combined = combined.trim();
        if combined.is_empty() {
            return true;
        }
        let non_empty_lines = combined.lines().filter(|l| !l.trim().is_empty()).count();
        non_empty_lines <= 1
    }
}

/// Output from the orchestrator — descriptions keyed by entity id.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ExtractionOutput {
    pub descriptions: Vec<EntityDescription>,
    pub report: Report,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct EntityDescription {
    pub entity_id: String,
    pub description: Description,
}

/// Top-level entry point. Probes, dispatches, collects.
pub fn extract_descriptions(
    cfg: &DescriptionsConfig,
    provider: Arc<dyn DescriptionProvider>,
    inputs: ExtractionInputs,
) -> Result<ExtractionOutput> {
    let report = Arc::new(Mutex::new(Report {
        provider: provider.label().to_string(),
        model: provider.model().to_string(),
        ..Default::default()
    }));
    let descriptions = Arc::new(Mutex::new(Vec::<EntityDescription>::new()));
    let calls_made = Arc::new(AtomicU64::new(0));

    // Pre-filter: privacy + triviality happen single-threaded so counters
    // are easy to reason about.
    let filtered: Vec<CandidateEntity> = inputs
        .entities
        .into_iter()
        .filter(|e| {
            let mut r = report.lock().expect("report lock");
            r.candidates += 1;
            if !cfg.include_private && !e.is_public {
                r.skipped_private += 1;
                return false;
            }
            if e.is_trivial() {
                r.skipped_trivial += 1;
                return false;
            }
            true
        })
        .collect();

    // Skip provider short-circuit — mark remaining as skipped_disabled.
    if provider.label() == "skip" {
        {
            let mut r = report.lock().expect("report lock");
            r.skipped_disabled += filtered.len() as u64;
        }
        let descriptions = Arc::into_inner(descriptions)
            .expect("descriptions arc unique")
            .into_inner()
            .expect("descriptions mutex");
        let report = Arc::into_inner(report)
            .expect("report arc unique")
            .into_inner()
            .expect("report mutex");
        return Ok(ExtractionOutput {
            descriptions,
            report,
        });
    }

    // Bounded parallel dispatch. We use a fixed worker pool + an mpsc
    // channel for candidate distribution. No tokio.
    let (tx, rx) = std::sync::mpsc::sync_channel::<CandidateEntity>(cfg.concurrency * 2);
    let rx = Arc::new(Mutex::new(rx));

    let mut handles = Vec::with_capacity(cfg.concurrency);
    for _ in 0..cfg.concurrency {
        let rx = Arc::clone(&rx);
        let provider = Arc::clone(&provider);
        let report = Arc::clone(&report);
        let descriptions = Arc::clone(&descriptions);
        let calls_made = Arc::clone(&calls_made);
        let max_calls = cfg.max_desc_calls;
        let min_conf = cfg.min_confidence;
        let handle = thread::spawn(move || loop {
            let cand = {
                let guard = rx.lock().expect("rx lock");
                match guard.recv() {
                    Ok(c) => c,
                    Err(_) => return, // channel closed
                }
            };

            if calls_made.load(Ordering::Relaxed) >= max_calls {
                let mut r = report.lock().expect("report lock");
                r.cost_cap_reached = true;
                continue; // drain without doing work
            }
            calls_made.fetch_add(1, Ordering::Relaxed);

            let (doc_red, doc_count) = redact(&cand.doc_comment);
            let (body_red, body_count) = redact(&cand.body_head);
            let total_red = doc_count + body_count;
            {
                let mut r = report.lock().expect("report lock");
                r.redactions_applied += total_red as u64;
            }
            let snippet = Snippet {
                entity_id: cand.id.clone(),
                entity_name: cand.name.clone(),
                entity_type: cand.entity_type.clone(),
                doc: doc_red,
                body: body_red,
            };

            let result = call_with_retry(provider.as_ref(), &snippet, &report);
            match result {
                Ok(mut desc) => {
                    if desc.confidence < min_conf {
                        let mut r = report.lock().expect("report lock");
                        r.skipped_low_confidence += 1;
                    } else {
                        // Stamp the actual redaction count into provenance.
                        desc.provenance.redactions = total_red;
                        let mut d = descriptions.lock().expect("descs lock");
                        d.push(EntityDescription {
                            entity_id: cand.id,
                            description: desc,
                        });
                        let mut r = report.lock().expect("report lock");
                        r.extracted += 1;
                    }
                }
                Err(e) => record_error(&report, e),
            }
        });
        handles.push(handle);
    }

    // Send candidates.
    let call_cap = cfg.max_desc_calls;
    for cand in filtered {
        if calls_made.load(Ordering::Relaxed) >= call_cap {
            break;
        }
        if tx.send(cand).is_err() {
            break;
        }
    }
    drop(tx); // close channel; workers will exit

    for h in handles {
        if let Err(e) = h.join() {
            // Worker panic — rare but account for it.
            let mut r = report.lock().expect("report lock");
            r.transport_failures += 1;
            eprintln!("[descriptions] worker panicked: {e:?}");
        }
    }

    let descriptions = Arc::into_inner(descriptions)
        .expect("descriptions arc unique")
        .into_inner()
        .expect("descriptions mutex");
    let report = Arc::into_inner(report)
        .expect("report arc unique")
        .into_inner()
        .expect("report mutex");

    Ok(ExtractionOutput {
        descriptions,
        report,
    })
}

/// Retry wrapper with exponential backoff + jitter. Caps at 3 attempts.
fn call_with_retry(
    provider: &dyn DescriptionProvider,
    snippet: &Snippet,
    report: &Arc<Mutex<Report>>,
) -> Result<Description, ProviderError> {
    const MAX_ATTEMPTS: u32 = 3;
    let mut delay_ms: u64 = 100;
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        match provider.extract(snippet) {
            Ok(d) => return Ok(d),
            Err(e) if e.is_retryable() && attempt < MAX_ATTEMPTS => {
                let mut r = report.lock().expect("report lock");
                r.retries_total += 1;
                drop(r);
                thread::sleep(Duration::from_millis(delay_ms));
                delay_ms = delay_ms.saturating_mul(2);
            }
            Err(e) => return Err(e),
        }
    }
}

fn record_error(report: &Arc<Mutex<Report>>, e: ProviderError) {
    let mut r = report.lock().expect("report lock");
    match e {
        ProviderError::MalformedResponse(_) => r.malformed_responses += 1,
        ProviderError::PromptLeak => r.prompt_leaks_detected += 1,
        ProviderError::Timeout(_) => r.timeouts += 1,
        ProviderError::RateLimited(_) => r.rate_limited += 1,
        _ => r.transport_failures += 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptions::providers::mock::{MockBehavior, MockProvider};

    fn mk_candidate(id: &str, body: &str, is_public: bool) -> CandidateEntity {
        CandidateEntity {
            id: id.into(),
            name: id.into(),
            entity_type: "fn".into(),
            is_public,
            doc_comment: String::new(),
            body_head: body.into(),
        }
    }

    #[test]
    fn trivial_detector() {
        let t = mk_candidate("a", "", true);
        assert!(t.is_trivial());
        let t = mk_candidate("a", "    ", true);
        assert!(t.is_trivial());
        let t = mk_candidate("a", "foo()", true);
        assert!(t.is_trivial());
        let nt = mk_candidate("a", "let x = 1;\nlet y = x + 1;", true);
        assert!(!nt.is_trivial());
    }

    #[test]
    fn private_skipped_by_default() {
        let cfg = DescriptionsConfig::default();
        let provider = Arc::new(MockProvider::always_ok("hi"));
        let out = extract_descriptions(
            &cfg,
            provider,
            ExtractionInputs {
                entities: vec![
                    mk_candidate("pub", "let a = 1;\nlet b = 2;", true),
                    mk_candidate("priv", "let a = 1;\nlet b = 2;", false),
                ],
            },
        )
        .unwrap();
        assert_eq!(out.descriptions.len(), 1);
        assert_eq!(out.report.skipped_private, 1);
        assert_eq!(out.report.extracted, 1);
    }

    #[test]
    fn skip_provider_short_circuits() {
        let mut cfg = DescriptionsConfig::default();
        cfg.provider = crate::descriptions::config::Provider::Skip;
        let provider: Arc<dyn DescriptionProvider> =
            Arc::new(crate::descriptions::providers::skip::SkipProvider);
        let out = extract_descriptions(
            &cfg,
            provider,
            ExtractionInputs {
                entities: vec![mk_candidate("a", "let a = 1;\nlet b = 2;", true)],
            },
        )
        .unwrap();
        assert!(out.descriptions.is_empty());
        assert_eq!(out.report.skipped_disabled, 1);
    }

    #[test]
    fn retry_succeeds_after_rate_limit() {
        let cfg = DescriptionsConfig::default();
        let provider = Arc::new(MockProvider::scripted(vec![
            MockBehavior::RateLimit,
            MockBehavior::RateLimit,
            MockBehavior::Ok("eventual success".into()),
        ]));
        let out = extract_descriptions(
            &cfg,
            provider,
            ExtractionInputs {
                entities: vec![mk_candidate("a", "let a = 1;\nlet b = 2;", true)],
            },
        )
        .unwrap();
        assert_eq!(out.descriptions.len(), 1);
        assert_eq!(out.report.retries_total, 2);
    }

    #[test]
    fn retry_gives_up_and_records_rate_limit() {
        let cfg = DescriptionsConfig::default();
        let provider = Arc::new(MockProvider::scripted(vec![
            MockBehavior::RateLimit,
            MockBehavior::RateLimit,
            MockBehavior::RateLimit,
        ]));
        let out = extract_descriptions(
            &cfg,
            provider,
            ExtractionInputs {
                entities: vec![mk_candidate("a", "let a = 1;\nlet b = 2;", true)],
            },
        )
        .unwrap();
        assert_eq!(out.descriptions.len(), 0);
        assert_eq!(out.report.rate_limited, 1);
        assert_eq!(out.report.retries_total, 2);
    }

    #[test]
    fn prompt_leak_counted() {
        let cfg = DescriptionsConfig::default();
        let provider = Arc::new(MockProvider::scripted(vec![MockBehavior::PromptLeak]));
        let out = extract_descriptions(
            &cfg,
            provider,
            ExtractionInputs {
                entities: vec![mk_candidate("a", "let a = 1;\nlet b = 2;", true)],
            },
        )
        .unwrap();
        assert_eq!(out.report.prompt_leaks_detected, 1);
        assert!(out.descriptions.is_empty());
    }

    #[test]
    fn cost_cap_honored() {
        let mut cfg = DescriptionsConfig::default();
        cfg.max_desc_calls = 2;
        let provider = Arc::new(MockProvider::always_ok("ok"));
        let entities = (0..10)
            .map(|i| mk_candidate(&format!("e{i}"), "let a = 1;\nlet b = 2;", true))
            .collect();
        let out = extract_descriptions(&cfg, provider, ExtractionInputs { entities }).unwrap();
        assert!(out.descriptions.len() <= 2);
    }

    #[test]
    fn report_is_accounted() {
        let cfg = DescriptionsConfig::default();
        let provider = Arc::new(MockProvider::always_ok("a valid description here"));
        let entities = (0..5)
            .map(|i| mk_candidate(&format!("e{i}"), "let a = 1;\nlet b = 2;", i % 2 == 0))
            .collect();
        let out = extract_descriptions(&cfg, provider, ExtractionInputs { entities }).unwrap();
        assert!(
            out.report.ensure_accounted(),
            "report accounting failed: {:?}",
            out.report
        );
    }
}
