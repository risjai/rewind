use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::get,
    Router,
};
use serde::{Deserialize, Serialize};

use crate::AppState;

pub fn routes(state: AppState) -> Router {
    Router::new()
        .route("/datasets", get(list_datasets))
        .route("/datasets/{name}", get(get_dataset))
        .route("/evaluators", get(list_evaluators))
        .route("/experiments", get(list_experiments))
        .route("/experiments/{id}", get(get_experiment))
        .route("/experiments/{id}/results", get(get_experiment_results))
        .route("/compare", get(compare))
        .with_state(state)
}

// ── Response types ───────────────────────────────────────────

#[derive(Serialize)]
struct DatasetDetailResponse {
    dataset: rewind_store::Dataset,
    examples: Vec<ResolvedExample>,
}

#[derive(Serialize)]
struct ResolvedExample {
    id: String,
    ordinal: u32,
    input: serde_json::Value,
    expected: serde_json::Value,
    metadata: serde_json::Value,
    source_session_id: Option<String>,
    source_step_id: Option<String>,
    created_at: String,
}

#[derive(Serialize)]
struct ExperimentDetailResponse {
    experiment: rewind_store::Experiment,
    dataset_name: Option<String>,
}

#[derive(Serialize)]
struct ResultWithScores {
    result: rewind_store::ExperimentResult,
    scores: Vec<rewind_store::ExperimentScore>,
}

// ── Query parameters ─────────────────────────────────────────

#[derive(Deserialize)]
struct ExperimentsQuery {
    dataset: Option<String>,
}

#[derive(Deserialize)]
struct CompareQuery {
    left: String,
    right: String,
}

// ── Handlers ─────────────────────────────────────────────────

async fn list_datasets(
    State(state): State<AppState>,
) -> Result<Json<Vec<rewind_store::Dataset>>, (StatusCode, String)> {
    let store = state.store.lock().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock error: {e}"))
    })?;
    let datasets = store.list_datasets().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;
    Ok(Json(datasets))
}

async fn get_dataset(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<DatasetDetailResponse>, (StatusCode, String)> {
    let store = state.store.lock().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock error: {e}"))
    })?;

    let dataset = store.get_dataset_by_name(&name).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?.ok_or_else(|| (StatusCode::NOT_FOUND, format!("Dataset not found: {name}")))?;

    let examples = store.get_dataset_examples(&dataset.id).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;

    let resolved: Vec<ResolvedExample> = examples.iter().map(|ex| {
        let input = if !ex.input_blob.is_empty() {
            store.blobs.get_json::<serde_json::Value>(&ex.input_blob)
                .unwrap_or(serde_json::Value::Null)
        } else {
            serde_json::Value::Null
        };
        let expected = if !ex.expected_blob.is_empty() {
            store.blobs.get_json::<serde_json::Value>(&ex.expected_blob)
                .unwrap_or(serde_json::Value::Null)
        } else {
            serde_json::Value::Null
        };
        ResolvedExample {
            id: ex.id.clone(),
            ordinal: ex.ordinal,
            input,
            expected,
            metadata: ex.metadata.clone(),
            source_session_id: ex.source_session_id.clone(),
            source_step_id: ex.source_step_id.clone(),
            created_at: ex.created_at.to_rfc3339(),
        }
    }).collect();

    Ok(Json(DatasetDetailResponse { dataset, examples: resolved }))
}

async fn list_evaluators(
    State(state): State<AppState>,
) -> Result<Json<Vec<rewind_store::Evaluator>>, (StatusCode, String)> {
    let store = state.store.lock().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock error: {e}"))
    })?;
    let evaluators = store.list_evaluators().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;
    Ok(Json(evaluators))
}

async fn list_experiments(
    State(state): State<AppState>,
    Query(query): Query<ExperimentsQuery>,
) -> Result<Json<Vec<rewind_store::Experiment>>, (StatusCode, String)> {
    let store = state.store.lock().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock error: {e}"))
    })?;

    let experiments = if let Some(ref dataset_name) = query.dataset {
        store.list_experiments_by_dataset(dataset_name)
    } else {
        store.list_experiments()
    }.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;

    Ok(Json(experiments))
}

async fn get_experiment(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ExperimentDetailResponse>, (StatusCode, String)> {
    let store = state.store.lock().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock error: {e}"))
    })?;

    let experiment = store.get_experiment(&id).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?.ok_or_else(|| (StatusCode::NOT_FOUND, format!("Experiment not found: {id}")))?;

    let dataset_name = store.get_dataset(&experiment.dataset_id)
        .ok()
        .flatten()
        .map(|d| d.name);

    Ok(Json(ExperimentDetailResponse { experiment, dataset_name }))
}

async fn get_experiment_results(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<ResultWithScores>>, (StatusCode, String)> {
    let store = state.store.lock().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock error: {e}"))
    })?;

    // Verify experiment exists
    store.get_experiment(&id).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?.ok_or_else(|| (StatusCode::NOT_FOUND, format!("Experiment not found: {id}")))?;

    let results = store.get_experiment_results(&id).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
    })?;

    let mut with_scores = Vec::with_capacity(results.len());
    for result in results {
        let scores = store.get_experiment_scores(&result.id).map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
        })?;
        with_scores.push(ResultWithScores { result, scores });
    }

    Ok(Json(with_scores))
}

async fn compare(
    State(state): State<AppState>,
    Query(query): Query<CompareQuery>,
) -> Result<Json<rewind_eval::ExperimentComparison>, (StatusCode, String)> {
    let store = state.store.lock().map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Lock error: {e}"))
    })?;

    let comparison = rewind_eval::compare_experiments(&store, &query.left, &query.right, false)
        .map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Compare error: {e}"))
        })?;

    Ok(Json(comparison))
}
