use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::get,
};
use serde::Deserialize;

use crate::{
    AppState,
    domain::{
        CreateDataSourceRequest, CreateDeviceRequest, CreateIntegrationRequest, DataSource, Device,
        Integration, Topic,
    },
    error::{ApiError, ApiResult},
    state::{new_id, now_iso, topics_for_source},
};

#[derive(Default, Deserialize)]
struct DataSourceQuery {
    device_id: Option<String>,
}

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/integrations",
            get(list_integrations).post(create_integration),
        )
        .route("/api/v1/devices", get(list_devices).post(create_device))
        .route(
            "/api/v1/data-sources",
            get(list_data_sources).post(create_data_source),
        )
        .route(
            "/api/v1/data-sources/{data_source_id}/topics",
            get(list_topics),
        )
}

async fn list_integrations(State(state): State<AppState>) -> Json<Vec<Integration>> {
    let catalog = state.catalog.read().await;
    Json(catalog.integrations.values().cloned().collect())
}

async fn create_integration(
    State(state): State<AppState>,
    Json(request): Json<CreateIntegrationRequest>,
) -> ApiResult<(StatusCode, Json<Integration>)> {
    validate_required("organizationId", &request.organization_id)?;
    validate_required("name", &request.name)?;
    validate_required("kind", &request.kind)?;
    validate_required("endpointLabel", &request.endpoint_label)?;

    let integration = state
        .durable_catalog_mutation(move |catalog| {
            let resource_version = catalog.bump_version();
            let now = now_iso();
            let integration = Integration {
                id: new_id("integration"),
                organization_id: request.organization_id,
                name: request.name,
                kind: request.kind,
                status: "testing".to_owned(),
                endpoint_label: request.endpoint_label,
                last_health_at: now.clone(),
                created_at: now,
                resource_version,
            };
            catalog
                .integrations
                .insert(integration.id.clone(), integration.clone());
            Ok(integration)
        })
        .await?;
    Ok((StatusCode::CREATED, Json(integration)))
}

async fn list_devices(State(state): State<AppState>) -> Json<Vec<Device>> {
    let catalog = state.catalog.read().await;
    Json(catalog.devices.values().cloned().collect())
}

async fn create_device(
    State(state): State<AppState>,
    Json(request): Json<CreateDeviceRequest>,
) -> ApiResult<(StatusCode, Json<Device>)> {
    validate_required("organizationId", &request.organization_id)?;
    validate_required("integrationId", &request.integration_id)?;
    validate_required("name", &request.name)?;
    validate_required("kind", &request.kind)?;

    let device = state
        .durable_catalog_mutation(move |catalog| {
            let integration = catalog
                .integrations
                .get(&request.integration_id)
                .ok_or_else(|| ApiError::not_found("Integration", &request.integration_id))?;
            if integration.organization_id != request.organization_id {
                return Err(ApiError::conflict(
                    "Integration and Device must belong to the same organization.",
                ));
            }
            let device_id = request.id.unwrap_or_else(|| new_id("device"));
            if catalog.devices.contains_key(&device_id) {
                return Err(ApiError::conflict("Device ID is already registered."));
            }
            let device = Device {
                id: device_id,
                organization_id: request.organization_id,
                integration_id: request.integration_id,
                name: request.name,
                kind: request.kind,
                status: request.status,
                health: request.health,
                operation_mode: request.operation_mode,
                battery_percent: request.battery_percent,
                task_name: request.task_name,
                task_progress: request.task_progress,
                last_seen_at: request.last_seen_at.unwrap_or_else(now_iso),
                state_version: 1,
            };
            catalog.devices.insert(device.id.clone(), device.clone());
            catalog.bump_version();
            Ok(device)
        })
        .await?;
    Ok((StatusCode::CREATED, Json(device)))
}

async fn list_data_sources(
    State(state): State<AppState>,
    Query(query): Query<DataSourceQuery>,
) -> Json<Vec<DataSource>> {
    let catalog = state.catalog.read().await;
    Json(
        catalog
            .data_sources
            .values()
            .filter(|source| {
                query
                    .device_id
                    .as_ref()
                    .is_none_or(|device_id| source.device_id == *device_id)
            })
            .cloned()
            .collect(),
    )
}

async fn create_data_source(
    State(state): State<AppState>,
    Json(request): Json<CreateDataSourceRequest>,
) -> ApiResult<(StatusCode, Json<DataSource>)> {
    validate_required("name", &request.name)?;
    validate_required("protocol", &request.protocol)?;
    validate_required("liveUrl", &request.live_url)?;

    let data_source = state
        .durable_catalog_mutation(move |catalog| {
            let integration = catalog
                .integrations
                .get(&request.integration_id)
                .ok_or_else(|| ApiError::not_found("Integration", &request.integration_id))?;
            let device = catalog
                .devices
                .get(&request.device_id)
                .ok_or_else(|| ApiError::not_found("Device", &request.device_id))?;
            if device.integration_id != integration.id {
                return Err(ApiError::conflict(
                    "Device and DataSource must use the same Integration.",
                ));
            }
            let source_id = request.id.unwrap_or_else(|| new_id("data-source"));
            if catalog.data_sources.contains_key(&source_id) {
                return Err(ApiError::conflict("DataSource ID is already registered."));
            }
            let mut data_source = DataSource {
                id: source_id,
                integration_id: request.integration_id,
                device_id: request.device_id,
                name: request.name,
                protocol: request.protocol,
                status: request.status,
                live_url: request.live_url,
                topic_ids: request.topic_ids,
                mapping_version: 1,
                last_data_at: request.last_data_at.unwrap_or_else(now_iso),
            };
            let topics = topics_for_source(
                &data_source.id,
                &data_source.device_id,
                &data_source.topic_ids,
            );
            data_source.topic_ids = topics.iter().map(|topic| topic.id.clone()).collect();
            catalog
                .topics_by_data_source
                .insert(data_source.id.clone(), topics);
            catalog
                .data_sources
                .insert(data_source.id.clone(), data_source.clone());
            catalog.bump_version();
            Ok(data_source)
        })
        .await?;
    Ok((StatusCode::CREATED, Json(data_source)))
}

async fn list_topics(
    State(state): State<AppState>,
    Path(data_source_id): Path<String>,
) -> ApiResult<Json<Vec<Topic>>> {
    let catalog = state.catalog.read().await;
    if !catalog.data_sources.contains_key(&data_source_id) {
        return Err(ApiError::not_found("DataSource", &data_source_id));
    }
    Ok(Json(
        catalog
            .topics_by_data_source
            .get(&data_source_id)
            .cloned()
            .unwrap_or_default(),
    ))
}

fn validate_required(field: &str, value: &str) -> ApiResult<()> {
    if value.trim().is_empty() {
        Err(ApiError::bad_request(format!("{field} is required.")))
    } else {
        Ok(())
    }
}
