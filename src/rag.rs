use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::OnceLock;
use tokio::fs;
use tracing::info;

const EMBEDDING_API_URL: &str = "https://api.openai.com/v1/embeddings";

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Document {
    pub title: String,
    pub content: String,
    pub embedding: Option<Vec<f64>>,
}

// In-Memory Vector Store
static KNOWLEDGE_BASE: OnceLock<Vec<Document>> = OnceLock::new();

pub async fn init_knowledge_base() {
    let api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
    if api_key.is_empty() {
        return;
    }

    let file_content = fs::read_to_string("knowledge.json")
        .await
        .unwrap_or_else(|_| "[]".to_string());
    let mut docs: Vec<Document> = serde_json::from_str(&file_content).unwrap_or_default();

    let client = reqwest::Client::new();
    let mut updated = false;

    for doc in docs.iter_mut() {
        if doc.embedding.is_none() {
            info!("[RAG] Generating vector embedding for: {}", doc.title);
            let req_body = json!({
                "input": doc.content,
                "model": "text-embedding-3-small"
            });

            if let Ok(res) = client
                .post(EMBEDDING_API_URL)
                .bearer_auth(&api_key)
                .json(&req_body)
                .send()
                .await
            {
                if let Ok(json_res) = res.json::<Value>().await {
                    if let Some(vec) = json_res["data"][0]["embedding"].as_array() {
                        doc.embedding = Some(vec.iter().filter_map(|v| v.as_f64()).collect());
                        updated = true;
                    }
                }
            }
        }
    }

    if updated {
        let _ = fs::write(
            "knowledge.json",
            serde_json::to_string_pretty(&docs).unwrap(),
        )
        .await;
    }

    KNOWLEDGE_BASE.set(docs).unwrap();
    info!("[RAG] Vector Knowledge Base initialized successfully.");
}

// High-performance pure-Rust Cosine Similarity
fn cosine_similarity(a: &[f64], b: &[f64]) -> f64 {
    let dot_product: f64 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f64 = a.iter().map(|x| x * x).sum::<f64>().sqrt();
    let norm_b: f64 = b.iter().map(|x| x * x).sum::<f64>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot_product / (norm_a * norm_b)
    }
}

// The Function called by the AI Agent
pub async fn search_kaspa_docs(query: &str) -> String {
    let api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
    let docs = match KNOWLEDGE_BASE.get() {
        Some(d) => d,
        None => return "Error: Knowledge base not initialized.".to_string(),
    };

    let client = reqwest::Client::new();
    let req_body = json!({ "input": query, "model": "text-embedding-3-small" });

    let query_embedding: Vec<f64> = match client
        .post(EMBEDDING_API_URL)
        .bearer_auth(&api_key)
        .json(&req_body)
        .send()
        .await
    {
        Ok(res) => {
            let json_res: Value = res.json().await.unwrap_or_default();
            json_res["data"][0]["embedding"]
                .as_array()
                .unwrap_or(&vec![])
                .iter()
                .filter_map(|v| v.as_f64())
                .collect()
        }
        Err(_) => return "Failed to embed query.".to_string(),
    };

    if query_embedding.is_empty() {
        return "Failed to process search query.".to_string();
    }

    let mut scored_docs: Vec<(&Document, f64)> = docs
        .iter()
        .filter_map(|d| {
            d.embedding
                .as_ref()
                .map(|emb| (d, cosine_similarity(&query_embedding, emb)))
        })
        .collect();

    scored_docs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // Return the top 2 most relevant documents to the AI
    let top_docs: Vec<String> = scored_docs
        .into_iter()
        .take(2)
        .map(|(d, score)| {
            format!(
                "--- Document (Relevance: {:.2}) ---\nTitle: {}\nContent: {}",
                score, d.title, d.content
            )
        })
        .collect();

    if top_docs.is_empty() {
        "No relevant official documentation found for this query.".to_string()
    } else {
        top_docs.join("\n\n")
    }
}
