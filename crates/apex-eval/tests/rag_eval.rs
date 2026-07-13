//! RM-AIM-P2 EVL-203 acceptance: the harness evaluates the **RAG path**
//! (`run_suite_with_memory` → the real `run_agent_with_memory` loop over the
//! real `apex-memory` engine), honors the manifest's `spec.max_steps`
//! (AIC-103, proven at the harness level), and grades a **retriever** with
//! recall@k / nDCG@k / MRR against a labeled relevant set.
//!
//! Everything here is deterministic: the mock gateway's embeddings are
//! non-semantic, so the memory engine's BM25 keyword branch drives retrieval
//! precision (the documented offline stance), and the agent/judge providers
//! are scripted.

use apex_agent::{AgentDefinition, ContextRetriever, MemorySpec, RetrievedContext};
use apex_common::{Result, Usage};
use apex_eval::{
    EvalSuite, RankedRetriever, RetrievalSuite, Scorer, evaluate_retrieval, run_suite,
    run_suite_with_memory,
};
use apex_memory::{InMemoryStore, MemoryEngine, MemoryQuery, MemoryType, RetrievalStrategy};
use apex_provider::{
    AIProvider, ChatRequest, ChatResponse, Gateway, Message, MockProvider, Role, ToolCall,
};
use apex_tools::ToolRegistry;
use async_trait::async_trait;
use std::sync::Arc;

/// A knowledge base seeded with two refund facts and two distractors,
/// returning the engine plus the stored ids (the labels for the retrieval
/// suite — ids are store-assigned, so the fixture references real ones).
async fn seeded_engine() -> (Arc<MemoryEngine>, Vec<String>) {
    let engine = MemoryEngine::new(
        Gateway::new(Box::new(MockProvider::new())),
        Arc::new(InMemoryStore::new()),
    );
    let mut ids = Vec::new();
    for content in [
        "The refund window is 30 days from purchase.",
        "Refund requests need the original receipt.",
        "The office is located in Berlin near the station.",
        "Visitor parking is in the basement garage.",
    ] {
        ids.push(
            engine
                .remember("kb", content, MemoryType::Semantic, 0.5, vec![])
                .await
                .unwrap(),
        );
    }
    (Arc::new(engine), ids)
}

/// The eval-side `ContextRetriever` over the real engine — the same adapter
/// shape the CLI's `EngineRetriever` uses (keyword strategy: deterministic
/// offline, since mock embeddings are non-semantic).
struct EngineRetriever(Arc<MemoryEngine>);

#[async_trait]
impl ContextRetriever for EngineRetriever {
    async fn retrieve(&self, query: &str, spec: &MemorySpec) -> Result<Vec<RetrievedContext>> {
        let mut q = MemoryQuery::new(query);
        q.namespace = spec.namespace.clone();
        q.strategy = RetrievalStrategy::Keyword;
        q.limit = 4;
        Ok(self
            .0
            .query(&q)
            .await?
            .into_iter()
            .map(|hit| RetrievedContext {
                source: hit.record.id,
                content: hit.record.content,
                score: hit.score,
            })
            .collect())
    }
}

/// The same engine behind the retrieval-metrics harness: ranked record ids.
struct EngineRanker(Arc<MemoryEngine>);

#[async_trait]
impl RankedRetriever for EngineRanker {
    async fn rank(&self, query: &str) -> Result<Vec<String>> {
        let mut q = MemoryQuery::new(query);
        q.namespace = Some("kb".to_string());
        q.strategy = RetrievalStrategy::Keyword;
        q.limit = 4;
        Ok(self
            .0
            .query(&q)
            .await?
            .into_iter()
            .map(|hit| hit.record.id)
            .collect())
    }
}

/// Answers from the injected "Retrieved knowledge" block only: if the
/// grounding contains the 30-day fact it repeats it, otherwise it declines —
/// so a passing case proves retrieval actually reached the prompt.
struct GroundedProvider;

#[async_trait]
impl AIProvider for GroundedProvider {
    fn name(&self) -> &str {
        "grounded"
    }
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let grounding = request
            .messages
            .iter()
            .filter(|m| m.role == Role::System)
            .filter_map(|m| m.content.as_deref())
            .collect::<Vec<_>>()
            .join("\n");
        let text = if grounding.contains("refund window is 30 days") {
            "Per the knowledge base, the refund window is 30 days."
        } else {
            "I don't know."
        };
        Ok(ChatResponse {
            message: Message::assistant(text.to_string()),
            model: request.model,
            usage: Usage::new(5, 5, 0.0),
            finish_reason: "stop".to_string(),
        })
    }
}

fn refund_suite() -> EvalSuite {
    EvalSuite::from_yaml(
        "
name: rag-refund
cases:
  - id: refund-window
    input: How long is the refund window?
    expect:
      contains: 30 days
",
    )
    .unwrap()
}

fn rag_agent() -> AgentDefinition {
    AgentDefinition::from_yaml(
        "metadata:\n  name: rag-bot\nspec:\n  instructions: Answer only from retrieved knowledge.\n  memory:\n    enabled: true\n    namespace: kb\n",
    )
    .unwrap()
}

/// EVL-203 acceptance (RAG half): the memory-grounded eval path passes where
/// the memoryless one fails — retrieval demonstrably reached the model.
#[tokio::test]
async fn memory_grounded_suite_passes_where_the_memoryless_run_fails() {
    let (engine, _) = seeded_engine().await;
    let def = rag_agent();
    let gateway = Gateway::new(Box::new(GroundedProvider));
    let registry = ToolRegistry::with_builtins();
    let scorer = Scorer::exact_only();

    // Without a retriever: no grounding block ever mentions the fact.
    let ungrounded = run_suite(&refund_suite(), &def, &gateway, &registry)
        .await
        .unwrap();
    assert_eq!(
        ungrounded.pass_rate, 0.0,
        "the memoryless path must fail: {ungrounded:#?}"
    );

    // With the real engine behind the retriever: the fact is retrieved,
    // injected, and the answer passes.
    let retriever = EngineRetriever(engine);
    let grounded = run_suite_with_memory(
        &refund_suite(),
        &def,
        &gateway,
        &registry,
        &scorer,
        &retriever,
    )
    .await
    .unwrap();
    println!("{grounded:#?}");
    assert_eq!(
        grounded.pass_rate, 1.0,
        "the RAG path must pass: {grounded:#?}"
    );
}

/// EVL-203 acceptance (retrieval-metrics half): recall@k / nDCG@k / MRR
/// computed against a labeled relevant set, driven by the real engine.
#[tokio::test]
async fn retrieval_metrics_grade_the_real_engine_against_labeled_fixtures() {
    let (engine, ids) = seeded_engine().await;
    // Labels: the two refund records are relevant for the refund query.
    let suite = RetrievalSuite::from_yaml(&format!(
        "
name: kb-retrieval
k: 2
cases:
  - id: refund
    query: refund window receipt
    relevant: [{}, {}]
",
        ids[0], ids[1]
    ))
    .unwrap();

    let report = evaluate_retrieval(&suite, &EngineRanker(engine))
        .await
        .unwrap();
    println!("{report:#?}");

    // BM25 puts both refund docs in the top 2 (the distractors match nothing),
    // so the metrics are exact.
    let case = &report.cases[0];
    assert_eq!(case.recall_at_k, 1.0, "both relevant docs in the top 2");
    assert_eq!(case.reciprocal_rank, 1.0, "a relevant doc ranks first");
    assert!((case.ndcg_at_k - 1.0).abs() < 1e-12, "ideal ordering");
    assert_eq!(report.mrr, 1.0);
    assert_eq!(report.mean_recall_at_k, 1.0);

    // Same engine, harder labels: only the receipt doc counts as relevant.
    // The 30-day doc outranks it for this query, so rank-sensitive metrics
    // drop below 1.0 while recall@2 still catches it — the three metrics
    // measure genuinely different things.
    let harder = RetrievalSuite::from_yaml(&format!(
        "
name: kb-retrieval-harder
k: 2
cases:
  - id: receipt-only
    query: refund window
    relevant: [{}]
",
        ids[1]
    ))
    .unwrap();
    let report = evaluate_retrieval(&harder, &EngineRanker(seeded_engine().await.0))
        .await
        .unwrap();
    println!("{report:#?}");
    let case = &report.cases[0];
    assert_eq!(case.recall_at_k, 1.0, "found within k");
    assert!(
        case.reciprocal_rank < 1.0 && case.reciprocal_rank > 0.0,
        "but not ranked first: {}",
        case.reciprocal_rank
    );
    assert!(case.ndcg_at_k < 1.0 && case.ndcg_at_k > 0.0);
}

/// A provider that always demands another tool call — it can never finish.
struct ToolHungryProvider;

#[async_trait]
impl AIProvider for ToolHungryProvider {
    fn name(&self) -> &str {
        "tool-hungry"
    }
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        Ok(ChatResponse {
            message: Message {
                role: Role::Assistant,
                content: None,
                parts: Vec::new(),
                tool_calls: vec![ToolCall {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                    arguments: r#"{"text": "again"}"#.to_string(),
                }],
                tool_call_id: None,
                name: None,
            },
            model: request.model,
            usage: Usage::new(3, 3, 0.0),
            finish_reason: "tool_calls".to_string(),
        })
    }
}

/// EVL-203 (max_steps half): the eval runner honors the manifest's
/// `spec.max_steps` (AIC-103) — a tool-hungry agent capped at 2 steps errors
/// with the budget message instead of looping toward the built-in default.
#[tokio::test]
async fn the_eval_runner_honors_the_manifest_step_budget() {
    let def = AgentDefinition::from_yaml(
        "metadata:\n  name: capped\nspec:\n  instructions: Loop forever.\n  tools: [echo]\n  max_steps: 2\n",
    )
    .unwrap();
    let gateway = Gateway::new(Box::new(ToolHungryProvider));
    let registry = ToolRegistry::with_builtins();

    let err = run_suite(&refund_suite(), &def, &gateway, &registry)
        .await
        .expect_err("a 2-step budget must exhaust");
    assert!(
        err.to_string().contains("did not finish within 2 steps"),
        "the failure must be the manifest budget, got: {err}"
    );
}
