//! Retrieval-quality metrics over labeled fixtures (RM-AIM-P2 EVL-203):
//! recall@k, nDCG@k, and MRR for any ranked-id retriever.
//!
//! The agent-level suites grade *answers*; this harness grades the
//! *retriever* itself — given a query and a labeled set of relevant record
//! ids, how well does the ranked result list surface them? The metric
//! functions are pure (no clock/rng); the [`RankedRetriever`] trait keeps the
//! harness agnostic to what produces the ranking (the integration tests drive
//! the real `wovyr-memory` engine as a dev-dependency, keeping this crate's
//! library spine memory-free).
//!
//! Relevance is **binary** (a result id is in the labeled set or not) —
//! graded relevance is a later refinement if a real corpus ever needs it.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use wovyr_common::{Error, Result};

/// One labeled retrieval case: a query and the ids that count as relevant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RetrievalCase {
    /// Stable case id, unique within its suite.
    pub id: String,
    /// The query text handed to the retriever.
    pub query: String,
    /// The record ids a good retriever should surface (non-empty).
    pub relevant: Vec<String>,
}

/// A named set of labeled retrieval cases, loadable from YAML.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RetrievalSuite {
    pub name: String,
    /// The cutoff for recall@k / nDCG@k.
    pub k: usize,
    pub cases: Vec<RetrievalCase>,
}

impl RetrievalSuite {
    /// Parse a suite from YAML, failing closed on anything malformed — the
    /// same validate-on-load shape as [`EvalSuite`](crate::EvalSuite).
    pub fn from_yaml(yaml: &str) -> Result<Self> {
        let suite: Self = serde_yaml::from_str(yaml)
            .map_err(|e| Error::invalid(format!("invalid retrieval suite: {e}")))?;
        suite.validate()?;
        Ok(suite)
    }

    fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(Error::invalid("retrieval suite name must not be empty"));
        }
        if self.k == 0 {
            return Err(Error::invalid("retrieval suite k must be at least 1"));
        }
        if self.cases.is_empty() {
            return Err(Error::invalid(
                "retrieval suite must have at least one case",
            ));
        }
        for case in &self.cases {
            if case.id.trim().is_empty() {
                return Err(Error::invalid("every retrieval case must have an id"));
            }
            if case.query.trim().is_empty() {
                return Err(Error::invalid(format!(
                    "retrieval case `{}` must have a non-empty query",
                    case.id
                )));
            }
            if case.relevant.is_empty() {
                return Err(Error::invalid(format!(
                    "retrieval case `{}` must label at least one relevant id",
                    case.id
                )));
            }
        }
        Ok(())
    }
}

/// Anything that ranks record ids for a query, best-first. Implemented over
/// `wovyr_memory::MemoryEngine` in the integration tests; a scripted impl
/// works for unit-testing the harness itself.
#[async_trait]
pub trait RankedRetriever: Send + Sync {
    /// The ranked ids the retriever returns for `query`, best-first.
    async fn rank(&self, query: &str) -> Result<Vec<String>>;
}

/// One case's retrieval metrics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RetrievalCaseResult {
    pub id: String,
    /// Fraction of the labeled relevant set surfaced in the top k.
    pub recall_at_k: f64,
    /// Normalized discounted cumulative gain at k (binary relevance).
    pub ndcg_at_k: f64,
    /// Reciprocal rank of the first relevant result (0 when none appear).
    pub reciprocal_rank: f64,
    /// The ids actually returned, for diagnosis of a failing case.
    pub ranked: Vec<String>,
}

/// The aggregate result of evaluating a retriever against a labeled suite.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RetrievalReport {
    pub suite: String,
    pub k: usize,
    pub cases: Vec<RetrievalCaseResult>,
    pub mean_recall_at_k: f64,
    pub mean_ndcg_at_k: f64,
    /// Mean reciprocal rank across cases — MRR proper.
    pub mrr: f64,
}

/// Evaluate `retriever` against every case in `suite`. Cases run
/// sequentially; the metric math is pure, so the report is exactly as
/// reproducible as the retriever it measures.
pub async fn evaluate_retrieval(
    suite: &RetrievalSuite,
    retriever: &dyn RankedRetriever,
) -> Result<RetrievalReport> {
    let mut cases = Vec::with_capacity(suite.cases.len());
    for case in &suite.cases {
        let ranked = retriever.rank(&case.query).await?;
        let relevant: BTreeSet<&str> = case.relevant.iter().map(String::as_str).collect();
        cases.push(RetrievalCaseResult {
            id: case.id.clone(),
            recall_at_k: recall_at_k(&ranked, &relevant, suite.k),
            ndcg_at_k: ndcg_at_k(&ranked, &relevant, suite.k),
            reciprocal_rank: reciprocal_rank(&ranked, &relevant),
            ranked,
        });
    }
    let n = cases.len() as f64;
    let mean = |f: fn(&RetrievalCaseResult) -> f64| cases.iter().map(f).sum::<f64>() / n;
    Ok(RetrievalReport {
        suite: suite.name.clone(),
        k: suite.k,
        mean_recall_at_k: mean(|c| c.recall_at_k),
        mean_ndcg_at_k: mean(|c| c.ndcg_at_k),
        mrr: mean(|c| c.reciprocal_rank),
        cases,
    })
}

/// Fraction of `relevant` found in the top `k` of `ranked`. Pure.
pub fn recall_at_k(ranked: &[String], relevant: &BTreeSet<&str>, k: usize) -> f64 {
    if relevant.is_empty() {
        return 0.0;
    }
    let hits = ranked
        .iter()
        .take(k)
        .filter(|id| relevant.contains(id.as_str()))
        .count();
    hits as f64 / relevant.len() as f64
}

/// Normalized DCG at `k` with binary relevance: each relevant result at
/// (0-based) position `i` contributes `1 / log2(i + 2)`, normalized by the
/// ideal ordering's DCG (all relevant results packed at the top). Pure.
pub fn ndcg_at_k(ranked: &[String], relevant: &BTreeSet<&str>, k: usize) -> f64 {
    if relevant.is_empty() {
        return 0.0;
    }
    let dcg: f64 = ranked
        .iter()
        .take(k)
        .enumerate()
        .filter(|(_, id)| relevant.contains(id.as_str()))
        .map(|(i, _)| 1.0 / ((i + 2) as f64).log2())
        .sum();
    let ideal: f64 = (0..relevant.len().min(k))
        .map(|i| 1.0 / ((i + 2) as f64).log2())
        .sum();
    dcg / ideal
}

/// Reciprocal rank of the first relevant result (1-based); 0 when none
/// appear anywhere in the ranking. Pure.
pub fn reciprocal_rank(ranked: &[String], relevant: &BTreeSet<&str>) -> f64 {
    ranked
        .iter()
        .position(|id| relevant.contains(id.as_str()))
        .map(|i| 1.0 / (i + 1) as f64)
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn recall_counts_relevant_hits_within_k() {
        let ranked = ids(&["a", "b", "c", "d"]);
        let relevant: BTreeSet<&str> = ["a", "c"].into();
        assert_eq!(recall_at_k(&ranked, &relevant, 1), 0.5);
        assert_eq!(recall_at_k(&ranked, &relevant, 2), 0.5);
        assert_eq!(recall_at_k(&ranked, &relevant, 3), 1.0);
        // k beyond the list length is fine.
        assert_eq!(recall_at_k(&ranked, &relevant, 100), 1.0);
    }

    #[test]
    fn reciprocal_rank_is_one_over_first_hit_position() {
        let relevant: BTreeSet<&str> = ["c"].into();
        assert_eq!(reciprocal_rank(&ids(&["c", "a"]), &relevant), 1.0);
        assert_eq!(reciprocal_rank(&ids(&["a", "c"]), &relevant), 0.5);
        assert_eq!(reciprocal_rank(&ids(&["a", "b"]), &relevant), 0.0);
    }

    #[test]
    fn ndcg_rewards_relevant_results_ranked_higher() {
        let relevant: BTreeSet<&str> = ["a", "b"].into();
        // Perfect ordering: both relevant docs on top.
        assert!((ndcg_at_k(&ids(&["a", "b", "x"]), &relevant, 3) - 1.0).abs() < 1e-12);
        // Same hits, worse positions → strictly lower nDCG.
        let worse = ndcg_at_k(&ids(&["x", "a", "b"]), &relevant, 3);
        assert!(worse < 1.0);
        // Hand-computed: hits at positions 1,2 (0-based) →
        // DCG = 1/log2(3) + 1/log2(4); IDCG = 1/log2(2) + 1/log2(3).
        let expected = (1.0 / 3.0_f64.log2() + 0.5) / (1.0 + 1.0 / 3.0_f64.log2());
        assert!((worse - expected).abs() < 1e-12);
        // Nothing relevant retrieved → 0.
        assert_eq!(ndcg_at_k(&ids(&["x", "y"]), &relevant, 2), 0.0);
    }

    #[test]
    fn ndcg_ideal_accounts_for_k_smaller_than_the_relevant_set() {
        // 3 relevant, k=1: the best any ranking can do at k=1 is one hit, and
        // that must score 1.0 — the ideal is capped at k, not |relevant|.
        let relevant: BTreeSet<&str> = ["a", "b", "c"].into();
        assert!((ndcg_at_k(&ids(&["a"]), &relevant, 1) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn suites_validate_on_load() {
        let ok = "
name: kb-retrieval
k: 3
cases:
  - id: refund
    query: refund window
    relevant: [mem-kb-1, mem-kb-2]
";
        let suite = RetrievalSuite::from_yaml(ok).unwrap();
        assert_eq!(suite.k, 3);
        assert_eq!(suite.cases[0].relevant.len(), 2);

        for bad in [
            "name: \"\"\nk: 3\ncases:\n  - id: c\n    query: q\n    relevant: [a]\n",
            "name: s\nk: 0\ncases:\n  - id: c\n    query: q\n    relevant: [a]\n",
            "name: s\nk: 3\ncases: []\n",
            "name: s\nk: 3\ncases:\n  - id: c\n    query: q\n    relevant: []\n",
            "name: s\nk: 3\ncases:\n  - id: c\n    query: \"\"\n    relevant: [a]\n",
        ] {
            assert!(
                RetrievalSuite::from_yaml(bad).is_err(),
                "must reject: {bad}"
            );
        }
    }

    #[tokio::test]
    async fn evaluate_aggregates_means_across_cases() {
        struct Scripted;
        #[async_trait]
        impl RankedRetriever for Scripted {
            async fn rank(&self, query: &str) -> Result<Vec<String>> {
                Ok(if query == "perfect" {
                    ids(&["a", "b"])
                } else {
                    ids(&["x", "a"])
                })
            }
        }
        let suite = RetrievalSuite {
            name: "s".into(),
            k: 2,
            cases: vec![
                RetrievalCase {
                    id: "p".into(),
                    query: "perfect".into(),
                    relevant: vec!["a".into(), "b".into()],
                },
                RetrievalCase {
                    id: "q".into(),
                    query: "imperfect".into(),
                    relevant: vec!["a".into(), "b".into()],
                },
            ],
        };
        let report = evaluate_retrieval(&suite, &Scripted).await.unwrap();
        assert_eq!(report.cases[0].recall_at_k, 1.0);
        assert_eq!(report.cases[0].reciprocal_rank, 1.0);
        assert_eq!(report.cases[1].recall_at_k, 0.5);
        assert_eq!(report.cases[1].reciprocal_rank, 0.5);
        assert_eq!(report.mean_recall_at_k, 0.75);
        assert_eq!(report.mrr, 0.75);
        assert!(report.mean_ndcg_at_k > 0.0 && report.mean_ndcg_at_k < 1.0);
    }
}
