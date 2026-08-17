//! Fuzzy model search for the picker popover (F8.3).
//!
//! Ranking is deliberately blunt and explainable: exact match, then exact prefix, then
//! word prefix, then substring, then subsequence. Within a band, a tighter match (more of
//! the target consumed by the query) wins, and ties fall back to catalog order so the list
//! never reshuffles between keystrokes for no reason.

use serde::{Deserialize, Serialize};

use super::{load, Model, Provider};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelHit {
    pub provider_id: String,
    pub provider_name: String,
    pub model_id: String,
    pub model_name: String,
    pub family: String,
    pub deprecated: bool,
    pub score: f64,
}

/// Weighted fields. Model identity dominates; provider and family only break ties or catch
/// queries like "groq" or "gemini".
const FIELD_WEIGHTS: [f64; 6] = [1.0, 1.0, 0.95, 0.6, 0.6, 0.5];

pub fn search(q: &str) -> Vec<ModelHit> {
    let query = q.trim().to_ascii_lowercase();
    let mut hits: Vec<ModelHit> = Vec::new();

    for (provider, model) in load().models() {
        let score = if query.is_empty() {
            0.0
        } else {
            match score_model(provider, model, &query) {
                Some(s) => s,
                None => continue,
            }
        };
        hits.push(ModelHit {
            provider_id: provider.id.clone(),
            provider_name: provider.name.clone(),
            model_id: model.id.clone(),
            model_name: model.name.clone(),
            family: model.family.clone(),
            deprecated: model.deprecated,
            score,
        });
    }

    // Stable sort: equal scores keep catalog order.
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    hits
}

fn score_model(provider: &Provider, model: &Model, query: &str) -> Option<f64> {
    let full_ref = format!("{}/{}", provider.id, model.id);
    let fields = [
        model.name.to_ascii_lowercase(),
        model.id.to_ascii_lowercase(),
        full_ref.to_ascii_lowercase(),
        provider.name.to_ascii_lowercase(),
        provider.id.to_ascii_lowercase(),
        model.family.to_ascii_lowercase(),
    ];

    let best = fields
        .iter()
        .zip(FIELD_WEIGHTS)
        .filter_map(|(field, weight)| score_field(field, query).map(|s| s * weight))
        .fold(f64::NEG_INFINITY, f64::max);

    if best == f64::NEG_INFINITY {
        return None;
    }
    // Deprecated models stay findable by name but never lead the list.
    Some(if model.deprecated { best * 0.75 } else { best })
}

fn score_field(field: &str, query: &str) -> Option<f64> {
    if field.is_empty() {
        return None;
    }
    let tightness = query.len() as f64 / field.len() as f64;

    if field == query {
        return Some(1000.0);
    }
    if field.starts_with(query) {
        return Some(800.0 + 100.0 * tightness);
    }
    if word_starts_with(field, query) {
        return Some(600.0 + 50.0 * tightness);
    }
    if field.contains(query) {
        return Some(400.0 + 50.0 * tightness);
    }
    subsequence_span(field, query).map(|span| {
        let density = query.chars().count() as f64 / span as f64;
        200.0 + 100.0 * density
    })
}

fn is_boundary(c: char) -> bool {
    c == '-' || c == '_' || c == '/' || c == ' ' || c == '.' || c == ':'
}

/// Does any word inside `field` start with `query`? "GPT-OSS 120B" matches "oss".
fn word_starts_with(field: &str, query: &str) -> bool {
    field
        .split(is_boundary)
        .any(|word| word.starts_with(query) && !word.is_empty())
}

/// Length of the shortest window of `field` containing `query`'s characters in order,
/// scanning greedily from each possible start. `None` when it is not a subsequence.
fn subsequence_span(field: &str, query: &str) -> Option<usize> {
    let field: Vec<char> = field.chars().collect();
    let query: Vec<char> = query.chars().collect();
    if query.is_empty() || query.len() > field.len() {
        return None;
    }
    let mut best: Option<usize> = None;
    for start in 0..field.len() {
        if field[start] != query[0] {
            continue;
        }
        let mut qi = 0;
        for (i, c) in field.iter().enumerate().skip(start) {
            if *c == query[qi] {
                qi += 1;
                if qi == query.len() {
                    let span = i - start + 1;
                    best = Some(best.map_or(span, |b: usize| b.min(span)));
                    break;
                }
            }
        }
    }
    best
}
