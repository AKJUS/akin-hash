//! Web routes for CRU operations on Data Types.

use alloc::sync::Arc;
use std::collections::{HashMap, HashSet};

use axum::{
    Extension, Router,
    response::Response,
    routing::{post, put},
};
use error_stack::{Report, ResultExt as _};
use hash_graph_authorization::policies::principal::actor::AuthenticatedActor;
use hash_graph_postgres_store::{
    ontology::patch_id_and_parse,
    store::error::{OntologyVersionDoesNotExist, VersionedUrlAlreadyExists},
};
use hash_graph_store::{
    data_type::{
        ArchiveDataTypeParams, CreateDataTypeParams, DataTypeConversionTargets, DataTypeQueryToken,
        DataTypeStore, GetDataTypeConversionTargetsParams, GetDataTypeConversionTargetsResponse,
        GetDataTypeSubgraphParams, GetDataTypesParams, GetDataTypesResponse,
        HasPermissionForDataTypesParams, UnarchiveDataTypeParams, UpdateDataTypeEmbeddingParams,
        UpdateDataTypesParams,
    },
    entity_type::ClosedDataTypeDefinition,
    pool::StorePool,
    query::ConflictBehavior,
};
use hash_status::Status;
use hash_temporal_client::TemporalClient;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use type_system::{
    ontology::{
        DataTypeWithMetadata, OntologyTemporalMetadata, OntologyTypeMetadata,
        OntologyTypeReference,
        data_type::{
            ConversionDefinition, ConversionExpression, ConversionValue, Conversions, DataType,
            DataTypeMetadata, Operator, Variable,
        },
        id::{BaseUrl, OntologyTypeVersion, VersionedUrl},
        json_schema::{DomainValidator, JsonSchemaValueType, ValidateOntologyType as _},
        provenance::{OntologyOwnership, ProvidedOntologyEditionProvenance},
    },
    principal::actor_group::WebId,
};
use utoipa::{OpenApi, ToSchema};

use crate::rest::{
    AuthenticatedUserHeader, OpenApiQuery, QueryLogger, RestApiStore,
    json::Json,
    status::{report_to_response, status_to_response},
    utoipa_typedef::{ListOrValue, MaybeListOfDataType, subgraph::Subgraph},
};

#[derive(OpenApi)]
#[openapi(
    paths(
        has_permission_for_data_types,

        create_data_type,
        load_external_data_type,
        get_data_types,
        get_data_type_subgraph,
        get_data_type_conversion_targets,
        update_data_type,
        update_data_types,
        update_data_type_embeddings,
        archive_data_type,
        unarchive_data_type,
    ),
    components(
        schemas(
            DataTypeWithMetadata,
            HasPermissionForDataTypesParams,

            CreateDataTypeRequest,
            LoadExternalDataTypeRequest,
            UpdateDataTypeRequest,
            UpdateDataTypeEmbeddingParams,
            DataTypeQueryToken,
            GetDataTypesParams,
            GetDataTypesResponse,
            GetDataTypeSubgraphParams,
            GetDataTypeSubgraphResponse,
            GetDataTypeConversionTargetsParams,
            GetDataTypeConversionTargetsResponse,
            DataTypeConversionTargets,
            ArchiveDataTypeParams,
            UnarchiveDataTypeParams,
            ClosedDataTypeDefinition,
            JsonSchemaValueType,

            ConversionDefinition,
            ConversionExpression,
            ConversionValue,
            Conversions,
            Operator,
            Variable,
        )
    ),
    tags(
        (name = "DataType", description = "Data Type management API")
    )
)]
pub(crate) struct DataTypeResource;

impl DataTypeResource {
    /// Create routes for interacting with data types.
    pub(crate) fn routes<S>() -> Router
    where
        S: StorePool + Send + Sync + 'static,
        for<'pool> S::Store<'pool>: RestApiStore,
    {
        // TODO: The URL format here is preliminary and will have to change.
        Router::new().nest(
            "/data-types",
            Router::new()
                .route("/", post(create_data_type::<S>).put(update_data_type::<S>))
                .route("/bulk", put(update_data_types::<S>))
                .route("/permissions", post(has_permission_for_data_types::<S>))
                .nest(
                    "/query",
                    Router::new()
                        .route("/", post(get_data_types::<S>))
                        .route("/subgraph", post(get_data_type_subgraph::<S>))
                        .route("/conversions", post(get_data_type_conversion_targets::<S>)),
                )
                .route("/load", post(load_external_data_type::<S>))
                .route("/archive", put(archive_data_type::<S>))
                .route("/unarchive", put(unarchive_data_type::<S>))
                .route("/embeddings", post(update_data_type_embeddings::<S>)),
        )
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateDataTypeRequest {
    #[schema(inline)]
    schema: MaybeListOfDataType,
    web_id: WebId,
    provenance: ProvidedOntologyEditionProvenance,
    conversions: HashMap<BaseUrl, Conversions>,
}

#[utoipa::path(
    post,
    path = "/data-types",
    request_body = CreateDataTypeRequest,
    tag = "DataType",
    params(
        ("X-Authenticated-User-Actor-Id" = ActorEntityUuid, Header, description = "The ID of the actor which is used to authorize the request"),
    ),
    responses(
        (status = 200, content_type = "application/json", description = "The metadata of the created data type", body = MaybeListOfDataTypeMetadata),
        (status = 422, content_type = "text/plain", description = "Provided request body is invalid"),

        (status = 409, description = "Unable to create data type in the store as the base data type URL already exists"),
        (status = 500, description = "Store error occurred"),
    ),
)]
#[tracing::instrument(level = "info", skip(store_pool, temporal_client, domain_validator))]
async fn create_data_type<S>(
    AuthenticatedUserHeader(actor_id): AuthenticatedUserHeader,
    store_pool: Extension<Arc<S>>,
    temporal_client: Extension<Option<Arc<TemporalClient>>>,
    domain_validator: Extension<DomainValidator>,
    body: Json<CreateDataTypeRequest>,
) -> Result<Json<ListOrValue<DataTypeMetadata>>, Response>
where
    S: StorePool + Send + Sync,
    for<'pool> S::Store<'pool>: RestApiStore,
{
    let mut store = store_pool
        .acquire(temporal_client.0)
        .await
        .map_err(report_to_response)?;

    let Json(CreateDataTypeRequest {
        schema,
        web_id,
        provenance,
        conversions,
    }) = body;

    let is_list = matches!(&schema, ListOrValue::List(_));

    let mut metadata = store
        .create_data_types(
            actor_id,
            schema
                .into_iter()
                .map(|schema| {
                    domain_validator
                        .validate(&schema)
                        .map_err(report_to_response)?;

                    Ok(CreateDataTypeParams {
                        schema,
                        ownership: OntologyOwnership::Local { web_id },
                        conflict_behavior: ConflictBehavior::Fail,
                        provenance: provenance.clone(),
                        conversions: conversions.clone(),
                    })
                })
                .collect::<Result<Vec<_>, Response>>()?,
        )
        .await
        .map_err(report_to_response)?;

    if is_list {
        Ok(Json(ListOrValue::List(metadata)))
    } else {
        Ok(Json(ListOrValue::Value(
            metadata.pop().expect("metadata does not contain a value"),
        )))
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, untagged)]
enum LoadExternalDataTypeRequest {
    #[serde(rename_all = "camelCase")]
    Fetch { data_type_id: VersionedUrl },
    #[serde(rename_all = "camelCase")]
    Create {
        schema: Box<DataType>,
        provenance: Box<ProvidedOntologyEditionProvenance>,
        conversions: HashMap<BaseUrl, Conversions>,
    },
}

#[utoipa::path(
    post,
    path = "/data-types/load",
    request_body = LoadExternalDataTypeRequest,
    tag = "DataType",
    params(
        ("X-Authenticated-User-Actor-Id" = ActorEntityUuid, Header, description = "The ID of the actor which is used to authorize the request"),
    ),
    responses(
        (status = 200, content_type = "application/json", description = "The metadata of the loaded data type", body = DataTypeMetadata),
        (status = 422, content_type = "text/plain", description = "Provided request body is invalid"),

        (status = 409, description = "Unable to load data type in the store as the base data type ID already exists"),
        (status = 500, description = "Store error occurred"),
    ),
)]
#[tracing::instrument(level = "info", skip(store_pool, temporal_client, domain_validator))]
async fn load_external_data_type<S>(
    AuthenticatedUserHeader(actor_id): AuthenticatedUserHeader,
    store_pool: Extension<Arc<S>>,
    temporal_client: Extension<Option<Arc<TemporalClient>>>,
    domain_validator: Extension<DomainValidator>,
    Json(request): Json<LoadExternalDataTypeRequest>,
) -> Result<Json<DataTypeMetadata>, Response>
where
    S: StorePool + Send + Sync,
    for<'pool> S::Store<'pool>: RestApiStore,
{
    let mut store = store_pool
        .acquire(temporal_client.0)
        .await
        .map_err(report_to_response)?;

    match request {
        LoadExternalDataTypeRequest::Fetch { data_type_id } => {
            let OntologyTypeMetadata::DataType(metadata) = store
                .load_external_type(
                    actor_id,
                    &domain_validator,
                    OntologyTypeReference::DataTypeReference((&data_type_id).into()),
                )
                .await?
            else {
                // TODO: Make the type fetcher typed
                panic!("`load_external_type` should have returned a `DataTypeMetadata`");
            };
            Ok(Json(metadata))
        }
        LoadExternalDataTypeRequest::Create {
            schema,
            provenance,
            conversions,
        } => {
            if domain_validator.validate_url(schema.id.base_url.as_str()) {
                let error = "Ontology type is not external".to_owned();
                tracing::error!(id=%schema.id, error);
                return Err(status_to_response(Status::<()>::new(
                    hash_status::StatusCode::InvalidArgument,
                    Some(error),
                    vec![],
                )));
            }

            Ok(Json(
                store
                    .create_data_type(
                        actor_id,
                        CreateDataTypeParams {
                            schema: *schema,
                            ownership: OntologyOwnership::Remote {
                                fetched_at: OffsetDateTime::now_utc(),
                            },
                            conflict_behavior: ConflictBehavior::Fail,
                            provenance: *provenance,
                            conversions: conversions.clone(),
                        },
                    )
                    .await
                    .map_err(report_to_response)?,
            ))
        }
    }
}

#[utoipa::path(
    post,
    path = "/data-types/query",
    request_body = GetDataTypesParams,
    tag = "DataType",
    params(
        ("X-Authenticated-User-Actor-Id" = ActorEntityUuid, Header, description = "The ID of the actor which is used to authorize the request"),
    ),
    responses(
        (
            status = 200,
            content_type = "application/json",
            body = GetDataTypesResponse,
            description = "Gets a a list of data types that satisfy the given query.",
        ),

        (status = 422, content_type = "text/plain", description = "Provided query is invalid"),
        (status = 500, description = "Store error occurred"),
    )
)]
#[tracing::instrument(level = "info", skip(store_pool, temporal_client, request))]
async fn get_data_types<S>(
    AuthenticatedUserHeader(actor_id): AuthenticatedUserHeader,
    store_pool: Extension<Arc<S>>,
    temporal_client: Extension<Option<Arc<TemporalClient>>>,
    mut query_logger: Option<Extension<QueryLogger>>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<GetDataTypesResponse>, Response>
where
    S: StorePool + Send + Sync,
{
    if let Some(query_logger) = &mut query_logger {
        query_logger.capture(actor_id, OpenApiQuery::GetDataTypes(&request));
    }

    let store = store_pool
        .acquire(temporal_client.0)
        .await
        .map_err(report_to_response)?;

    let response = store
        .get_data_types(
            actor_id,
            // Manually deserialize the query from a JSON value to allow borrowed deserialization
            // and better error reporting.
            GetDataTypesParams::deserialize(&request)
                .map_err(Report::from)
                .map_err(report_to_response)?,
        )
        .await
        .map_err(report_to_response)
        .map(Json);
    if let Some(query_logger) = &mut query_logger {
        query_logger.send().await.map_err(report_to_response)?;
    }
    response
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct GetDataTypeSubgraphResponse {
    subgraph: Subgraph,
    cursor: Option<VersionedUrl>,
}

#[utoipa::path(
    post,
    path = "/data-types/query/subgraph",
    request_body = GetDataTypeSubgraphParams,
    tag = "DataType",
    params(
        ("X-Authenticated-User-Actor-Id" = ActorEntityUuid, Header, description = "The ID of the actor which is used to authorize the request"),
    ),
    responses(
        (
            status = 200,
            content_type = "application/json",
            body = GetDataTypeSubgraphResponse,
            description = "Gets a subgraph rooted at all data types that satisfy the given query, each resolved to the requested depth.",
        ),

        (status = 422, content_type = "text/plain", description = "Provided query is invalid"),
        (status = 500, description = "Store error occurred"),
    )
)]
#[tracing::instrument(level = "info", skip(store_pool, temporal_client, request))]
async fn get_data_type_subgraph<S>(
    AuthenticatedUserHeader(actor_id): AuthenticatedUserHeader,
    store_pool: Extension<Arc<S>>,
    temporal_client: Extension<Option<Arc<TemporalClient>>>,
    mut query_logger: Option<Extension<QueryLogger>>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<GetDataTypeSubgraphResponse>, Response>
where
    S: StorePool + Send + Sync,
{
    if let Some(query_logger) = &mut query_logger {
        query_logger.capture(actor_id, OpenApiQuery::GetDataTypeSubgraph(&request));
    }

    let store = store_pool
        .acquire(temporal_client.0)
        .await
        .map_err(report_to_response)?;

    let response = store
        .get_data_type_subgraph(
            actor_id,
            // Manually deserialize the query from a JSON value to allow borrowed deserialization
            // and better error reporting.
            GetDataTypeSubgraphParams::deserialize(&request)
                .map_err(Report::from)
                .map_err(report_to_response)?,
        )
        .await
        .map_err(report_to_response)
        .map(|response| {
            Json(GetDataTypeSubgraphResponse {
                subgraph: Subgraph::from(response.subgraph),
                cursor: response.cursor,
            })
        });
    if let Some(query_logger) = &mut query_logger {
        query_logger.send().await.map_err(report_to_response)?;
    }
    response
}

#[utoipa::path(
    post,
    path = "/data-types/query/conversions",
    request_body = GetDataTypeConversionTargetsParams,
    tag = "DataType",
    params(
        ("X-Authenticated-User-Actor-Id" = ActorEntityUuid, Header, description = "The ID of the actor which is used to authorize the request"),
    ),
    responses(
        (
            status = 200,
            content_type = "application/json",
            body = GetDataTypeConversionTargetsResponse,
        ),
        (status = 500, description = "Store error occurred"),
    )
)]
#[tracing::instrument(level = "info", skip(store_pool, temporal_client, request))]
async fn get_data_type_conversion_targets<S>(
    AuthenticatedUserHeader(actor_id): AuthenticatedUserHeader,
    store_pool: Extension<Arc<S>>,
    temporal_client: Extension<Option<Arc<TemporalClient>>>,
    Json(request): Json<GetDataTypeConversionTargetsParams>,
) -> Result<Json<GetDataTypeConversionTargetsResponse>, Response>
where
    S: StorePool + Send + Sync,
{
    store_pool
        .acquire(temporal_client.0)
        .await
        .map_err(report_to_response)?
        .get_data_type_conversion_targets(actor_id, request)
        .await
        .map_err(report_to_response)
        .map(Json)
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateDataTypeRequest {
    #[schema(value_type = UpdateDataType)]
    schema: serde_json::Value,
    type_to_update: VersionedUrl,
    provenance: ProvidedOntologyEditionProvenance,
    conversions: HashMap<BaseUrl, Conversions>,
}

#[utoipa::path(
    put,
    path = "/data-types",
    tag = "DataType",
    params(
        ("X-Authenticated-User-Actor-Id" = ActorEntityUuid, Header, description = "The ID of the actor which is used to authorize the request"),
    ),
    responses(
        (status = 200, content_type = "application/json", description = "The metadata of the updated data type", body = DataTypeMetadata),
        (status = 422, content_type = "text/plain", description = "Provided request body is invalid"),

        (status = 404, description = "Base data type ID was not found"),
        (status = 500, description = "Store error occurred"),
    ),
    request_body = UpdateDataTypeRequest,
)]
#[tracing::instrument(level = "info", skip(store_pool, temporal_client))]
async fn update_data_type<S>(
    AuthenticatedUserHeader(actor_id): AuthenticatedUserHeader,
    store_pool: Extension<Arc<S>>,
    temporal_client: Extension<Option<Arc<TemporalClient>>>,
    body: Json<UpdateDataTypeRequest>,
) -> Result<Json<DataTypeMetadata>, Response>
where
    S: StorePool + Send + Sync,
{
    let Json(UpdateDataTypeRequest {
        schema,
        mut type_to_update,
        provenance,
        conversions,
    }) = body;

    type_to_update.version = OntologyTypeVersion::new(type_to_update.version.inner() + 1);

    let data_type = patch_id_and_parse(&type_to_update, schema).map_err(report_to_response)?;

    let mut store = store_pool
        .acquire(temporal_client.0)
        .await
        .map_err(report_to_response)?;

    store
        .update_data_type(
            actor_id,
            UpdateDataTypesParams {
                schema: data_type,
                provenance,
                conversions,
            },
        )
        .await
        .map_err(report_to_response)
        .map(Json)
}

#[utoipa::path(
    put,
    path = "/data-types/bulk",
    tag = "DataType",
    params(
        ("X-Authenticated-User-Actor-Id" = ActorEntityUuid, Header, description = "The ID of the actor which is used to authorize the request"),
    ),
    responses(
        (status = 200, content_type = "application/json", description = "The metadata of the updated data types", body = [DataTypeMetadata]),
        (status = 422, content_type = "text/plain", description = "Provided request body is invalid"),

        (status = 404, description = "Base data types ID were not found"),
        (status = 500, description = "Store error occurred"),
    ),
    request_body = [UpdateDataTypeRequest],
)]
#[tracing::instrument(level = "info", skip(store_pool, temporal_client))]
async fn update_data_types<S>(
    AuthenticatedUserHeader(actor_id): AuthenticatedUserHeader,
    store_pool: Extension<Arc<S>>,
    temporal_client: Extension<Option<Arc<TemporalClient>>>,
    bodies: Json<Vec<UpdateDataTypeRequest>>,
) -> Result<Json<Vec<DataTypeMetadata>>, Response>
where
    S: StorePool + Send + Sync,
{
    let mut store = store_pool
        .acquire(temporal_client.0)
        .await
        .map_err(report_to_response)?;

    let params = bodies
        .0
        .into_iter()
        .map(
            |UpdateDataTypeRequest {
                 schema,
                 mut type_to_update,
                 provenance,
                 conversions,
             }| {
                type_to_update.version =
                    OntologyTypeVersion::new(type_to_update.version.inner() + 1);

                Ok(UpdateDataTypesParams {
                    schema: patch_id_and_parse(&type_to_update, schema)
                        .map_err(report_to_response)?,
                    provenance,
                    conversions,
                })
            },
        )
        .collect::<Result<Vec<_>, Response>>()?;
    store
        .update_data_types(actor_id, params)
        .await
        .map_err(report_to_response)
        .map(Json)
}

#[utoipa::path(
    post,
    path = "/data-types/embeddings",
    tag = "DataType",
    params(
        ("X-Authenticated-User-Actor-Id" = ActorEntityUuid, Header, description = "The ID of the actor which is used to authorize the request"),
    ),
    responses(
        (status = 204, content_type = "application/json", description = "The embeddings were created"),

        (status = 403, description = "Insufficient permissions to update the data type"),
        (status = 500, description = "Store error occurred"),
    ),
    request_body = UpdateDataTypeEmbeddingParams,
)]
#[tracing::instrument(level = "info", skip(store_pool, temporal_client, body))]
async fn update_data_type_embeddings<S>(
    AuthenticatedUserHeader(actor_id): AuthenticatedUserHeader,
    store_pool: Extension<Arc<S>>,
    temporal_client: Extension<Option<Arc<TemporalClient>>>,
    Json(body): Json<serde_json::Value>,
) -> Result<(), Response>
where
    S: StorePool + Send + Sync,
{
    // Manually deserialize the request from a JSON value to allow borrowed deserialization and
    // better error reporting.
    let params = UpdateDataTypeEmbeddingParams::deserialize(body)
        .attach(hash_status::StatusCode::InvalidArgument)
        .map_err(report_to_response)?;

    let mut store = store_pool
        .acquire(temporal_client.0)
        .await
        .map_err(report_to_response)?;

    store
        .update_data_type_embeddings(actor_id, params)
        .await
        .map_err(report_to_response)
}

#[utoipa::path(
    put,
    path = "/data-types/archive",
    tag = "DataType",
    params(
        ("X-Authenticated-User-Actor-Id" = ActorEntityUuid, Header, description = "The ID of the actor which is used to authorize the request"),
    ),
    responses(
        (status = 200, content_type = "application/json", description = "The metadata of the updated data type", body = OntologyTemporalMetadata),
        (status = 422, content_type = "text/plain", description = "Provided request body is invalid"),

        (status = 404, description = "Data type ID was not found"),
        (status = 409, description = "Data type ID is already archived"),
        (status = 500, description = "Store error occurred"),
    ),
    request_body = ArchiveDataTypeParams,
)]
#[tracing::instrument(level = "info", skip(store_pool, temporal_client))]
async fn archive_data_type<S>(
    AuthenticatedUserHeader(actor_id): AuthenticatedUserHeader,
    store_pool: Extension<Arc<S>>,
    temporal_client: Extension<Option<Arc<TemporalClient>>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<OntologyTemporalMetadata>, Response>
where
    S: StorePool + Send + Sync,
{
    // Manually deserialize the request from a JSON value to allow borrowed deserialization and
    // better error reporting.
    let params = ArchiveDataTypeParams::deserialize(body)
        .attach(hash_status::StatusCode::InvalidArgument)
        .map_err(report_to_response)?;

    let mut store = store_pool
        .acquire(temporal_client.0)
        .await
        .map_err(report_to_response)?;

    store
        .archive_data_type(actor_id, params)
        .await
        .map_err(|mut report| {
            if report.contains::<OntologyVersionDoesNotExist>() {
                report = report.attach(hash_status::StatusCode::NotFound);
            }
            if report.contains::<VersionedUrlAlreadyExists>() {
                report = report.attach(hash_status::StatusCode::AlreadyExists);
            }
            report_to_response(report)
        })
        .map(Json)
}

#[utoipa::path(
    put,
    path = "/data-types/unarchive",
    tag = "DataType",
    params(
        ("X-Authenticated-User-Actor-Id" = ActorEntityUuid, Header, description = "The ID of the actor which is used to authorize the request"),
    ),
    responses(
        (status = 200, content_type = "application/json", description = "The temporal metadata of the updated data type", body = OntologyTemporalMetadata),
        (status = 422, content_type = "text/plain", description = "Provided request body is invalid"),

        (status = 404, description = "Data type ID was not found"),
        (status = 409, description = "Data type ID already exists and is not archived"),
        (status = 500, description = "Store error occurred"),
    ),
    request_body = UnarchiveDataTypeParams,
)]
#[tracing::instrument(level = "info", skip(store_pool, temporal_client))]
async fn unarchive_data_type<S>(
    AuthenticatedUserHeader(actor_id): AuthenticatedUserHeader,
    store_pool: Extension<Arc<S>>,
    temporal_client: Extension<Option<Arc<TemporalClient>>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<OntologyTemporalMetadata>, Response>
where
    S: StorePool + Send + Sync,
{
    // Manually deserialize the request from a JSON value to allow borrowed deserialization and
    // better error reporting.
    let params = UnarchiveDataTypeParams::deserialize(body)
        .attach(hash_status::StatusCode::InvalidArgument)
        .map_err(report_to_response)?;

    let mut store = store_pool
        .acquire(temporal_client.0)
        .await
        .map_err(report_to_response)?;

    store
        .unarchive_data_type(actor_id, params)
        .await
        .map_err(|mut report| {
            if report.contains::<OntologyVersionDoesNotExist>() {
                report = report.attach(hash_status::StatusCode::NotFound);
            }
            if report.contains::<VersionedUrlAlreadyExists>() {
                report = report.attach(hash_status::StatusCode::AlreadyExists);
            }
            report_to_response(report)
        })
        .map(Json)
}

#[utoipa::path(
    post,
    path = "/data-types/permissions",
    tag = "DataType",
    request_body = HasPermissionForDataTypesParams,
    params(
        ("X-Authenticated-User-Actor-Id" = ActorEntityUuid, Header, description = "The ID of the actor which is used to authorize the request"),
    ),
    responses(
        (status = 200, body = Vec<VersionedUrl>, description = "Information if the actor has the permission for the data types"),

        (status = 500, description = "Internal error occurred"),
    )
)]
#[tracing::instrument(level = "info", skip(store_pool, temporal_client))]
async fn has_permission_for_data_types<S>(
    AuthenticatedUserHeader(actor): AuthenticatedUserHeader,
    temporal_client: Extension<Option<Arc<TemporalClient>>>,
    store_pool: Extension<Arc<S>>,
    Json(params): Json<HasPermissionForDataTypesParams<'static>>,
) -> Result<Json<HashSet<VersionedUrl>>, Response>
where
    S: StorePool + Send + Sync,
    for<'p> S::Store<'p>: DataTypeStore,
{
    store_pool
        .acquire(temporal_client.0)
        .await
        .map_err(report_to_response)?
        .has_permission_for_data_types(AuthenticatedActor::from(actor), params)
        .await
        .map(Json)
        .map_err(report_to_response)
}
