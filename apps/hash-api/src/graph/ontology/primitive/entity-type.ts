import type { EntityTypeRootType, Subgraph } from "@blockprotocol/graph";
import type {
  EntityType,
  EntityTypeMetadata,
  EntityTypeWithMetadata,
  OntologyTemporalMetadata,
  OntologyTypeRecordId,
  ProvidedOntologyEditionProvenance,
  VersionedUrl,
  WebId,
} from "@blockprotocol/type-system";
import {
  ENTITY_TYPE_META_SCHEMA,
  ontologyTypeRecordIdToVersionedUrl,
} from "@blockprotocol/type-system";
import { NotFoundError } from "@local/hash-backend-utils/error";
import { publicUserAccountId } from "@local/hash-backend-utils/public-user-account-id";
import type { TemporalClient } from "@local/hash-backend-utils/temporal";
import type {
  ArchiveEntityTypeParams,
  ClosedMultiEntityTypeMap,
  GetClosedMultiEntityTypesParams,
  GetClosedMultiEntityTypesResponse as GetClosedMultiEntityTypesResponseGraphApi,
  GetEntityTypesParams,
  GetEntityTypeSubgraphParams,
  UnarchiveEntityTypeParams,
  UpdateEntityTypeRequest,
} from "@local/hash-graph-client";
import type { UserPermissionsOnEntityType } from "@local/hash-graph-sdk/authorization";
import { hasPermissionForEntityTypes } from "@local/hash-graph-sdk/entity-type";
import type {
  ClosedEntityTypeWithMetadata,
  ConstructEntityTypeParams,
  EntityTypeResolveDefinitions,
} from "@local/hash-graph-sdk/ontology";
import { currentTimeInstantTemporalAxes } from "@local/hash-isomorphic-utils/graph-queries";
import { blockProtocolEntityTypes } from "@local/hash-isomorphic-utils/ontology-type-ids";
import { generateTypeId } from "@local/hash-isomorphic-utils/ontology-types";
import {
  mapGraphApiClosedEntityTypesToClosedEntityTypes,
  mapGraphApiEntityTypeResolveDefinitionsToEntityTypeResolveDefinitions,
  mapGraphApiEntityTypesToEntityTypes,
  mapGraphApiSubgraphToSubgraph,
} from "@local/hash-isomorphic-utils/subgraph-mapping";

import type { ImpureGraphFunction } from "../../context-types";
import { rewriteSemanticFilter } from "../../shared/rewrite-semantic-filter";
import { getWebShortname, isExternalTypeId } from "./util";

export const checkPermissionsOnEntityType: ImpureGraphFunction<
  { entityTypeId: VersionedUrl },
  Promise<UserPermissionsOnEntityType>
> = async (graphContext, { actorId }, params) => {
  const { entityTypeId } = params;

  const isPublicUser = actorId === publicUserAccountId;

  const [canUpdate, canInstantiateEntities] = await Promise.all([
    isPublicUser
      ? false
      : await hasPermissionForEntityTypes(
          graphContext.graphApi,
          { actorId },
          {
            entityTypeIds: [entityTypeId],
            action: "updateEntityType",
          },
        ).then((permitted) => permitted.includes(entityTypeId)),
    isPublicUser
      ? false
      : await hasPermissionForEntityTypes(
          graphContext.graphApi,
          { actorId },
          {
            entityTypeIds: [entityTypeId],
            action: "instantiate",
          },
        ).then((permitted) => permitted.includes(entityTypeId)),
  ]);

  return {
    edit: canUpdate,
    instantiate: canInstantiateEntities,
    view: true,
  };
};

/**
 * Create an entity type.
 *
 * @param params.webId - the id of the account who owns the entity type
 * @param [params.webShortname] – the shortname of the web that owns the entity type, if the web entity does not yet
 *   exist.
 *    - Only for seeding purposes. Caller is responsible for ensuring the webShortname is correct for the webId.
 * @param params.schema - the `EntityType`
 * @param params.actorId - the id of the account that is creating the entity type
 */
export const createEntityType: ImpureGraphFunction<
  {
    webId: WebId;
    schema: ConstructEntityTypeParams;
    webShortname?: string;
    provenance?: ProvidedOntologyEditionProvenance;
  },
  Promise<EntityTypeWithMetadata>
> = async (ctx, authentication, params) => {
  const { webId, webShortname } = params;

  const shortname =
    webShortname ??
    (await getWebShortname(ctx, authentication, {
      accountOrAccountGroupId: webId,
    }));

  const entityTypeId = generateTypeId({
    kind: "entity-type",
    title: params.schema.title,
    webShortname: shortname,
  });

  const schema = {
    $schema: ENTITY_TYPE_META_SCHEMA,
    kind: "entityType" as const,
    $id: entityTypeId,
    ...params.schema,
  };

  const { graphApi } = ctx;

  const { data: metadata } = await graphApi.createEntityType(
    authentication.actorId,
    {
      webId,
      schema,
      provenance: {
        ...ctx.provenance,
        ...params.provenance,
      },
    },
  );

  // TODO: Avoid casting through `unknown` when new codegen is in place
  //   see https://linear.app/hash/issue/H-4463/utilize-new-codegen-and-replace-custom-defined-node-types
  return { schema, metadata: metadata as unknown as EntityTypeMetadata };
};

/**
 * Get entity types by a structural query.
 *
 * @param params.query the structural query to filter entity types by.
 */
export const getEntityTypeSubgraph: ImpureGraphFunction<
  Omit<GetEntityTypeSubgraphParams, "includeDrafts">,
  Promise<Subgraph<EntityTypeRootType>>
> = async ({ graphApi, temporalClient }, { actorId }, request) => {
  await rewriteSemanticFilter(request.filter, temporalClient);

  return await graphApi
    .getEntityTypeSubgraph(actorId, { includeDrafts: false, ...request })
    .then(({ data: response }) => {
      const subgraph = mapGraphApiSubgraphToSubgraph<EntityTypeRootType>(
        response.subgraph,
        actorId,
      );

      return subgraph;
    });
};

export const getEntityTypes: ImpureGraphFunction<
  Omit<GetEntityTypesParams, "includeDrafts" | "includeEntityTypes">,
  Promise<EntityTypeWithMetadata[]>
> = async ({ graphApi, temporalClient }, { actorId }, request) => {
  await rewriteSemanticFilter(request.filter, temporalClient);

  return await graphApi
    .getEntityTypes(actorId, {
      includeDrafts: false,
      ...request,
    })
    .then(({ data: response }) =>
      mapGraphApiEntityTypesToEntityTypes(response.entityTypes),
    );
};

export const getClosedEntityTypes: ImpureGraphFunction<
  Omit<GetEntityTypesParams, "includeDrafts" | "includeEntityTypes"> & {
    temporalClient?: TemporalClient;
  },
  Promise<ClosedEntityTypeWithMetadata[]>
> = async ({ graphApi, temporalClient }, { actorId }, request) => {
  await rewriteSemanticFilter(request.filter, temporalClient);

  const { data: response } = await graphApi.getEntityTypes(actorId, {
    includeDrafts: false,
    includeEntityTypes: "closed",
    ...request,
  });
  const entityTypes = mapGraphApiEntityTypesToEntityTypes(response.entityTypes);
  const closedEntityTypes = mapGraphApiClosedEntityTypesToClosedEntityTypes(
    response.closedEntityTypes!,
  );
  return closedEntityTypes.map((schema, idx) => ({
    schema,
    metadata: entityTypes[idx]!.metadata,
  }));
};

export type GetClosedMultiEntityTypesResponse = Omit<
  GetClosedMultiEntityTypesResponseGraphApi,
  "entityTypes" | "definitions"
> & {
  closedMultiEntityTypes: Record<string, ClosedMultiEntityTypeMap>;
  definitions?: EntityTypeResolveDefinitions;
};

export const getClosedMultiEntityTypes: ImpureGraphFunction<
  GetClosedMultiEntityTypesParams,
  Promise<GetClosedMultiEntityTypesResponse>
> = async ({ graphApi }, { actorId }, request) =>
  graphApi
    .getClosedMultiEntityTypes(actorId, request)
    .then(({ data: response }) => ({
      closedMultiEntityTypes: response.entityTypes,
      definitions: response.definitions
        ? mapGraphApiEntityTypeResolveDefinitionsToEntityTypeResolveDefinitions(
            response.definitions,
          )
        : undefined,
    }));

/**
 * Get an entity type by its versioned URL.
 *
 * @param params.entityTypeId the unique versioned URL for an entity type.
 */
export const getEntityTypeById: ImpureGraphFunction<
  {
    entityTypeId: VersionedUrl;
  },
  Promise<EntityTypeWithMetadata>
> = async (context, authentication, params) => {
  const { entityTypeId } = params;

  const [entityType] = await getEntityTypes(context, authentication, {
    filter: {
      equal: [{ path: ["versionedUrl"] }, { parameter: entityTypeId }],
    },
    temporalAxes: currentTimeInstantTemporalAxes,
  });

  if (!entityType) {
    throw new NotFoundError(
      `Could not find entity type with ID "${entityTypeId}"`,
    );
  }

  return entityType;
};

/**
 * Get an entity type rooted subgraph by its versioned URL.
 *
 * If the type does not already exist within the Graph, and is an externally-hosted type, this will also load the type
 * into the Graph.
 */
export const getEntityTypeSubgraphById: ImpureGraphFunction<
  Omit<GetEntityTypeSubgraphParams, "filter" | "includeDrafts"> & {
    entityTypeId: VersionedUrl;
  },
  Promise<Subgraph<EntityTypeRootType>>
> = async (context, authentication, params) => {
  const { graphResolveDepths, temporalAxes, entityTypeId } = params;

  const request: GetEntityTypeSubgraphParams = {
    filter: {
      equal: [{ path: ["versionedUrl"] }, { parameter: entityTypeId }],
    },
    graphResolveDepths,
    temporalAxes,
    includeDrafts: false,
  };

  let subgraph = await getEntityTypeSubgraph(context, authentication, request);

  if (subgraph.roots.length === 0 && isExternalTypeId(entityTypeId)) {
    await context.graphApi.loadExternalEntityType(authentication.actorId, {
      entityTypeId,
    });

    subgraph = await getEntityTypeSubgraph(context, authentication, request);
  }

  return subgraph;
};

type UpdateEntityTypeParams = {
  entityTypeId: VersionedUrl;
  schema: ConstructEntityTypeParams;
  provenance?: ProvidedOntologyEditionProvenance;
};

/**
 * Update an entity type.
 *
 * @param params.entityTypeId - the id of the entity type that's being updated
 * @param params.schema - the updated `EntityType`
 * @param params.actorId - the id of the account that is updating the entity type
 */
export const updateEntityType: ImpureGraphFunction<
  UpdateEntityTypeParams,
  Promise<EntityTypeWithMetadata>
> = async (ctx, authentication, params) => {
  const { entityTypeId, schema } = params;
  const updateArguments: UpdateEntityTypeRequest = {
    typeToUpdate: entityTypeId,
    schema: {
      kind: "entityType",
      $schema: ENTITY_TYPE_META_SCHEMA,
      ...schema,
    },
    provenance: {
      ...ctx.provenance,
      ...params.provenance,
    },
  };

  const { data: metadata } = await ctx.graphApi.updateEntityType(
    authentication.actorId,
    updateArguments,
  );

  const newEntityTypeId = ontologyTypeRecordIdToVersionedUrl(
    // TODO: Avoid casting through `unknown` when new codegen is in place
    //   see https://linear.app/hash/issue/H-4463/utilize-new-codegen-and-replace-custom-defined-node-types
    metadata.recordId as unknown as OntologyTypeRecordId,
  );

  return {
    schema: {
      kind: "entityType",
      $schema: ENTITY_TYPE_META_SCHEMA,
      ...schema,
      $id: newEntityTypeId,
    },
    // TODO: Avoid casting through `unknown` when new codegen is in place
    //   see https://linear.app/hash/issue/H-4463/utilize-new-codegen-and-replace-custom-defined-node-types
    metadata: metadata as unknown as EntityTypeMetadata,
  };
};

/**
 * Updates multiple entity types.
 *
 * @param params.entityTypeId - the id of the entity type that's being updated
 * @param params.schema - the updated `EntityType`
 * @param params.actorId - the id of the account that is updating the entity type
 */
export const updateEntityTypes: ImpureGraphFunction<
  {
    entityTypeUpdates: UpdateEntityTypeParams[];
  },
  Promise<EntityTypeWithMetadata[]>
> = async (ctx, authentication, params) => {
  const { entityTypeUpdates } = params;
  const updateArguments: UpdateEntityTypeRequest[] = entityTypeUpdates.map(
    ({ entityTypeId, schema, provenance }) => ({
      typeToUpdate: entityTypeId,
      schema: {
        kind: "entityType",
        $schema: ENTITY_TYPE_META_SCHEMA,
        ...schema,
      },
      provenance: {
        ...ctx.provenance,
        ...provenance,
      },
    }),
  );

  const { data: metadatas } = await ctx.graphApi.updateEntityTypes(
    authentication.actorId,
    updateArguments,
  );

  return metadatas.map((metadata, index) => {
    const input = entityTypeUpdates[index];

    if (!input) {
      throw new Error(
        `Entity type update metadata index ${index} not present in input array`,
      );
    }

    return {
      schema: {
        kind: "entityType",
        $schema: ENTITY_TYPE_META_SCHEMA,
        ...input.schema,
        $id: ontologyTypeRecordIdToVersionedUrl(
          // TODO: Avoid casting through `unknown` when new codegen is in place
          //   see https://linear.app/hash/issue/H-4463/utilize-new-codegen-and-replace-custom-defined-node-types
          metadata.recordId as unknown as OntologyTypeRecordId,
        ),
      },
      // TODO: Avoid casting through `unknown` when new codegen is in place
      //   see https://linear.app/hash/issue/H-4463/utilize-new-codegen-and-replace-custom-defined-node-types
      metadata: metadata as unknown as EntityTypeMetadata,
    };
  });
};

// Return true if any type in the provided entity type's ancestors is a link entity type
export const isEntityTypeLinkEntityType: ImpureGraphFunction<
  Pick<EntityType, "allOf">,
  Promise<boolean>
> = async (context, authentication, params) => {
  const { allOf } = params;

  if (
    allOf?.some(
      ({ $ref }) => $ref === blockProtocolEntityTypes.link.entityTypeId,
    )
  ) {
    return true;
  }

  const parentTypes = await Promise.all(
    (allOf ?? []).map(async ({ $ref }) =>
      getEntityTypeById(context, authentication, {
        entityTypeId: $ref,
      }),
    ),
  );

  return new Promise((resolve) => {
    const promises = parentTypes.map((parent) =>
      isEntityTypeLinkEntityType(context, authentication, parent.schema).then(
        (isLinkType) => {
          if (isLinkType) {
            // Resolve as soon as we have encountered a link type, instead of waiting for all parent types to be checked
            resolve(true);
          }
        },
      ),
    );

    void Promise.all(promises).then(() =>
      // If we haven't resolved yet, then none of the parent types are link types. If we have resolved this is a no-op.
      resolve(false),
    );
  });
};

/**
 * Archives a data type
 *
 * @param params.entityTypeId - the id of the entity type that's being archived
 * @param params.actorId - the id of the account that is archiving the entity type
 */
export const archiveEntityType: ImpureGraphFunction<
  ArchiveEntityTypeParams,
  Promise<OntologyTemporalMetadata>
> = async ({ graphApi }, { actorId }, params) => {
  const { data: temporalMetadata } = await graphApi.archiveEntityType(
    actorId,
    params,
  );

  return temporalMetadata as OntologyTemporalMetadata;
};

/**
 * Unarchives a data type
 *
 * @param params.entityTypeId - the id of the entity type that's being unarchived
 * @param params.actorId - the id of the account that is unarchiving the entity type
 */
export const unarchiveEntityType: ImpureGraphFunction<
  Omit<UnarchiveEntityTypeParams, "provenance">,
  Promise<OntologyTemporalMetadata>
> = async ({ graphApi, provenance }, { actorId }, params) => {
  const { data: temporalMetadata } = await graphApi.unarchiveEntityType(
    actorId,
    { ...params, provenance },
  );

  return temporalMetadata as OntologyTemporalMetadata;
};
