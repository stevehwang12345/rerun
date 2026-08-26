use std::{
    collections::{BTreeMap, BTreeSet},
    convert::Infallible,
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse as _, Response, Sse, sse::Event},
    routing::{delete, get, post},
};
use serde_json::json;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::{
    AppState,
    domain::{
        AccessMode, CreateDataAssignmentRequest, CreateDeviceAssignmentRequest,
        CreateProjectRequest, DataAssignment, DataVisibility, DeviceAssignment, Project,
        ProjectWorkspace,
    },
    error::{ApiError, ApiResult},
    state::{Catalog, new_id, now_iso},
};

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/projects", get(list_projects).post(create_project))
        .route(
            "/api/v1/projects/{project_id}/workspace",
            get(get_workspace),
        )
        .route(
            "/api/v1/projects/{project_id}/recordings",
            get(list_recordings),
        )
        .route(
            "/api/v1/projects/{project_id}/events",
            get(workspace_events),
        )
        .route(
            "/api/v1/projects/{project_id}/device-assignments",
            post(create_device_assignment),
        )
        .route(
            "/api/v1/projects/{project_id}/device-assignments/{assignment_id}",
            delete(delete_device_assignment),
        )
        .route(
            "/api/v1/projects/{project_id}/data-assignments",
            post(create_data_assignment),
        )
        .route(
            "/api/v1/projects/{project_id}/data-assignments/{assignment_id}",
            delete(delete_data_assignment),
        )
}

async fn list_recordings(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> ApiResult<Json<Vec<crate::domain::Recording>>> {
    let catalog = state.catalog.read().await;
    if !catalog.projects.contains_key(&project_id) {
        return Err(ApiError::not_found("Project", &project_id));
    }
    Ok(Json(
        catalog
            .recordings
            .values()
            .filter(|recording| recording.project_id == project_id)
            .cloned()
            .collect(),
    ))
}

async fn workspace_events(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> ApiResult<Response> {
    let mut workspace_version = {
        let catalog = state.catalog.read().await;
        if !catalog.projects.contains_key(&project_id) {
            return Err(ApiError::not_found("Project", &project_id));
        }
        catalog.workspace_version.subscribe()
    };
    let (sender, receiver) = mpsc::channel::<Result<Event, Infallible>>(2);
    tokio::spawn(async move {
        loop {
            let snapshot_version = *workspace_version.borrow_and_update();
            let event = json!({
                "eventId": format!("workspace-event-{project_id}-{snapshot_version}"),
                "type": "project.workspace.changed",
                "occurredAt": now_iso(),
                "projectId": project_id,
                "snapshotVersion": snapshot_version
            });
            let event = Event::default()
                .event("project.workspace.changed")
                .data(event.to_string());
            if sender.send(Ok(event)).await.is_err() {
                break;
            }
            if workspace_version.changed().await.is_err() {
                break;
            }
        }
    });
    Ok(Sse::new(ReceiverStream::new(receiver))
        .keep_alive(
            axum::response::sse::KeepAlive::new()
                .interval(Duration::from_secs(10))
                .text("keep-alive"),
        )
        .into_response())
}

async fn list_projects(State(state): State<AppState>) -> Json<Vec<Project>> {
    let catalog = state.catalog.read().await;
    Json(catalog.projects.values().cloned().collect())
}

async fn create_project(
    State(state): State<AppState>,
    Json(request): Json<CreateProjectRequest>,
) -> ApiResult<(StatusCode, Json<Project>)> {
    if request.organization_id.trim().is_empty() || request.name.trim().is_empty() {
        return Err(ApiError::bad_request(
            "organizationId and name are required.",
        ));
    }

    let project = state
        .durable_catalog_mutation(move |catalog| {
            let device_ids = request.device_ids.into_iter().collect::<BTreeSet<_>>();
            let data_source_ids = request.data_source_ids.into_iter().collect::<BTreeSet<_>>();
            validate_project_resources(
                catalog,
                &request.organization_id,
                &device_ids,
                &data_source_ids,
            )?;

            let project_id = new_id("project");
            let resource_version = catalog.snapshot_version.saturating_add(1);
            let project = Project {
                id: project_id.clone(),
                organization_id: request.organization_id,
                name: request.name,
                description: request.description,
                status: request.status,
                device_count: 0,
                online_device_count: 0,
                created_at: now_iso(),
                resource_version,
            };
            catalog.projects.insert(project_id.clone(), project);
            for device_id in device_ids {
                insert_device_assignment(catalog, &project_id, device_id, AccessMode::Control)?;
            }
            for data_source_id in data_source_ids {
                insert_data_assignment(
                    catalog,
                    &project_id,
                    data_source_id,
                    DataVisibility::Operator,
                )?;
            }
            catalog.refresh_project_counts(&project_id);
            catalog.bump_version();
            catalog
                .projects
                .get(&project_id)
                .cloned()
                .ok_or_else(|| ApiError::not_found("Project", &project_id))
        })
        .await?;
    Ok((StatusCode::CREATED, Json(project)))
}

async fn get_workspace(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> ApiResult<Json<ProjectWorkspace>> {
    let catalog = state.catalog.read().await;
    let project = catalog
        .projects
        .get(&project_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("Project", &project_id))?;

    let device_assignments = catalog
        .device_assignments
        .values()
        .filter(|assignment| assignment.project_id == project_id && assignment.valid_to.is_none())
        .cloned()
        .collect::<Vec<_>>();
    let data_assignments = catalog
        .data_assignments
        .values()
        .filter(|assignment| assignment.project_id == project_id && assignment.valid_to.is_none())
        .cloned()
        .collect::<Vec<_>>();
    let devices = device_assignments
        .iter()
        .filter_map(|assignment| catalog.devices.get(&assignment.device_id).cloned())
        .collect::<Vec<_>>();
    let data_sources = data_assignments
        .iter()
        .filter_map(|assignment| {
            catalog
                .data_sources
                .get(&assignment.data_source_id)
                .cloned()
        })
        .collect::<Vec<_>>();
    let topics_by_data_source = data_sources
        .iter()
        .filter_map(|source| {
            catalog
                .topics_by_data_source
                .get(&source.id)
                .cloned()
                .map(|topics| (source.id.clone(), topics))
        })
        .collect::<BTreeMap<_, _>>();
    let recordings = catalog
        .recordings
        .values()
        .filter(|recording| recording.project_id == project_id)
        .cloned()
        .collect::<Vec<_>>();
    let topics_by_recording = recordings
        .iter()
        .filter_map(|recording| {
            catalog
                .topics_by_recording
                .get(&recording.id)
                .cloned()
                .map(|topics| (recording.id.clone(), topics))
        })
        .collect::<BTreeMap<_, _>>();

    Ok(Json(ProjectWorkspace {
        snapshot_version: catalog.snapshot_version,
        captured_at: now_iso(),
        project,
        device_assignments,
        data_assignments,
        devices,
        data_sources,
        recordings,
        topics_by_recording,
        topics_by_data_source,
    }))
}

async fn create_device_assignment(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    Json(request): Json<CreateDeviceAssignmentRequest>,
) -> ApiResult<(StatusCode, Json<DeviceAssignment>)> {
    let assignment = state
        .durable_catalog_mutation(move |catalog| {
            validate_assignment_project_and_device(catalog, &project_id, &request.device_id)?;
            let assignment = insert_device_assignment(
                catalog,
                &project_id,
                request.device_id,
                request.access_mode,
            )?;
            catalog.refresh_project_counts(&project_id);
            catalog.bump_version();
            Ok(assignment)
        })
        .await?;
    Ok((StatusCode::CREATED, Json(assignment)))
}

async fn delete_device_assignment(
    State(state): State<AppState>,
    Path((project_id, assignment_id)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    state
        .durable_catalog_mutation(move |catalog| {
            let assignment = catalog
                .device_assignments
                .get(&assignment_id)
                .cloned()
                .ok_or_else(|| ApiError::not_found("DeviceAssignment", &assignment_id))?;
            if assignment.project_id != project_id {
                return Err(ApiError::not_found("DeviceAssignment", &assignment_id));
            }
            if catalog.live_sessions.values().any(|session| {
                session.status == "open"
                    && session.project_id == project_id
                    && session.device_id == assignment.device_id
            }) {
                return Err(ApiError::conflict(
                    "Close the active LiveSession before deleting its DeviceAssignment.",
                ));
            }
            if catalog.recording_imports.values().any(|import| {
                matches!(import.status.as_str(), "uploading" | "processing")
                    && import.project_id == project_id
                    && import.device_id == assignment.device_id
            }) {
                return Err(ApiError::conflict(
                    "Cancel or finish the active RecordingImport before deleting its DeviceAssignment.",
                ));
            }
            catalog.device_assignments.remove(&assignment_id);
            catalog.refresh_project_counts(&project_id);
            catalog.bump_version();
            Ok(())
        })
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn create_data_assignment(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    Json(request): Json<CreateDataAssignmentRequest>,
) -> ApiResult<(StatusCode, Json<DataAssignment>)> {
    let assignment = state
        .durable_catalog_mutation(move |catalog| {
            validate_assignment_project_and_source(catalog, &project_id, &request.data_source_id)?;
            let source_device_id = catalog
                .data_sources
                .get(&request.data_source_id)
                .ok_or_else(|| ApiError::not_found("DataSource", &request.data_source_id))?
                .device_id
                .clone();
            let device_is_assigned = catalog.device_assignments.values().any(|assignment| {
                assignment.project_id == project_id
                    && assignment.device_id == source_device_id
                    && assignment.valid_to.is_none()
            });
            if !device_is_assigned {
                return Err(ApiError::conflict(
                    "Assign the DataSource Device to the Project first.",
                ));
            }
            let assignment = insert_data_assignment(
                catalog,
                &project_id,
                request.data_source_id,
                request.visibility,
            )?;
            catalog.bump_version();
            Ok(assignment)
        })
        .await?;
    Ok((StatusCode::CREATED, Json(assignment)))
}

async fn delete_data_assignment(
    State(state): State<AppState>,
    Path((project_id, assignment_id)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    state
        .durable_catalog_mutation(move |catalog| {
            let assignment = catalog
                .data_assignments
                .get(&assignment_id)
                .cloned()
                .ok_or_else(|| ApiError::not_found("DataAssignment", &assignment_id))?;
            if assignment.project_id != project_id {
                return Err(ApiError::not_found("DataAssignment", &assignment_id));
            }
            if catalog.live_sessions.values().any(|session| {
                session.status == "open"
                    && session.project_id == project_id
                    && session.data_source_id == assignment.data_source_id
            }) {
                return Err(ApiError::conflict(
                    "Close the active LiveSession before deleting its DataAssignment.",
                ));
            }
            if catalog.recording_imports.values().any(|import| {
                matches!(import.status.as_str(), "uploading" | "processing")
                    && import.project_id == project_id
                    && import.data_source_id == assignment.data_source_id
            }) {
                return Err(ApiError::conflict(
                    "Cancel or finish the active RecordingImport before deleting its DataAssignment.",
                ));
            }
            catalog.data_assignments.remove(&assignment_id);
            catalog.bump_version();
            Ok(())
        })
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

fn validate_project_resources(
    catalog: &Catalog,
    organization_id: &str,
    device_ids: &BTreeSet<String>,
    data_source_ids: &BTreeSet<String>,
) -> ApiResult<()> {
    for device_id in device_ids {
        let device = catalog
            .devices
            .get(device_id)
            .ok_or_else(|| ApiError::not_found("Device", device_id))?;
        if device.organization_id != organization_id {
            return Err(ApiError::conflict(
                "Project and assigned Device must belong to the same organization.",
            ));
        }
        ensure_control_assignment_available(catalog, device_id, None)?;
    }
    for source_id in data_source_ids {
        let source = catalog
            .data_sources
            .get(source_id)
            .ok_or_else(|| ApiError::not_found("DataSource", source_id))?;
        let device = catalog
            .devices
            .get(&source.device_id)
            .ok_or_else(|| ApiError::not_found("Device", &source.device_id))?;
        if device.organization_id != organization_id {
            return Err(ApiError::conflict(
                "Project and assigned DataSource must belong to the same organization.",
            ));
        }
    }
    Ok(())
}

fn validate_assignment_project_and_device(
    catalog: &Catalog,
    project_id: &str,
    device_id: &str,
) -> ApiResult<()> {
    let project = catalog
        .projects
        .get(project_id)
        .ok_or_else(|| ApiError::not_found("Project", project_id))?;
    let device = catalog
        .devices
        .get(device_id)
        .ok_or_else(|| ApiError::not_found("Device", device_id))?;
    if project.organization_id != device.organization_id {
        return Err(ApiError::conflict(
            "Project and Device must belong to the same organization.",
        ));
    }
    Ok(())
}

fn validate_assignment_project_and_source(
    catalog: &Catalog,
    project_id: &str,
    source_id: &str,
) -> ApiResult<()> {
    let project = catalog
        .projects
        .get(project_id)
        .ok_or_else(|| ApiError::not_found("Project", project_id))?;
    let source = catalog
        .data_sources
        .get(source_id)
        .ok_or_else(|| ApiError::not_found("DataSource", source_id))?;
    let device = catalog
        .devices
        .get(&source.device_id)
        .ok_or_else(|| ApiError::not_found("Device", &source.device_id))?;
    if project.organization_id != device.organization_id {
        return Err(ApiError::conflict(
            "Project and DataSource must belong to the same organization.",
        ));
    }
    Ok(())
}

fn insert_device_assignment(
    catalog: &mut Catalog,
    project_id: &str,
    device_id: String,
    access_mode: AccessMode,
) -> ApiResult<DeviceAssignment> {
    if catalog
        .device_assignments
        .values()
        .any(|assignment| assignment.project_id == project_id && assignment.device_id == device_id)
    {
        return Err(ApiError::conflict(
            "Device is already assigned to this Project.",
        ));
    }
    if access_mode == AccessMode::Control {
        ensure_control_assignment_available(catalog, &device_id, Some(project_id))?;
    }
    let assignment = DeviceAssignment {
        id: new_id("device-assignment"),
        project_id: project_id.to_owned(),
        device_id,
        access_mode,
        valid_from: now_iso(),
        valid_to: None,
        resource_version: catalog.snapshot_version.saturating_add(1),
    };
    catalog
        .device_assignments
        .insert(assignment.id.clone(), assignment.clone());
    Ok(assignment)
}

fn insert_data_assignment(
    catalog: &mut Catalog,
    project_id: &str,
    data_source_id: String,
    visibility: DataVisibility,
) -> ApiResult<DataAssignment> {
    if catalog.data_assignments.values().any(|assignment| {
        assignment.project_id == project_id && assignment.data_source_id == data_source_id
    }) {
        return Err(ApiError::conflict(
            "DataSource is already assigned to this Project.",
        ));
    }
    let assignment = DataAssignment {
        id: new_id("data-assignment"),
        project_id: project_id.to_owned(),
        data_source_id,
        visibility,
        valid_from: now_iso(),
        valid_to: None,
        resource_version: catalog.snapshot_version.saturating_add(1),
    };
    catalog
        .data_assignments
        .insert(assignment.id.clone(), assignment.clone());
    Ok(assignment)
}

fn ensure_control_assignment_available(
    catalog: &Catalog,
    device_id: &str,
    current_project_id: Option<&str>,
) -> ApiResult<()> {
    let conflict = catalog.device_assignments.values().any(|assignment| {
        assignment.device_id == device_id
            && assignment.access_mode == AccessMode::Control
            && current_project_id.is_none_or(|project_id| assignment.project_id != project_id)
    });
    if conflict {
        Err(ApiError::conflict(
            "Device already has a control assignment in another Project.",
        ))
    } else {
        Ok(())
    }
}
