import { deleteKratosIdentity } from "@apps/hash-api/src/auth/ory-kratos";
import { ensureSystemGraphIsInitialized } from "@apps/hash-api/src/graph/ensure-system-graph-is-initialized";
import { createEntity } from "@apps/hash-api/src/graph/knowledge/primitive/entity";
import type { Block } from "@apps/hash-api/src/graph/knowledge/system-types/block";
import { createBlock } from "@apps/hash-api/src/graph/knowledge/system-types/block";
import {
  createComment,
  getCommentAuthor,
  getCommentParent,
  getCommentText,
} from "@apps/hash-api/src/graph/knowledge/system-types/comment";
import type { Page } from "@apps/hash-api/src/graph/knowledge/system-types/page";
import {
  createPage,
  getPageBlocks,
} from "@apps/hash-api/src/graph/knowledge/system-types/page";
import type { User } from "@apps/hash-api/src/graph/knowledge/system-types/user";
import type { WebId } from "@blockprotocol/type-system";
import { Logger } from "@local/hash-backend-utils/logger";
import { systemEntityTypes } from "@local/hash-isomorphic-utils/ontology-type-ids";
import type { Text } from "@local/hash-isomorphic-utils/system-types/shared";
import { beforeAll, describe, expect, it } from "vitest";

import { resetGraph } from "../../../test-server";
import {
  createTestImpureGraphContext,
  createTestUser,
  waitForAfterHookTriggerToComplete,
} from "../../../util";

const logger = new Logger({
  environment: "test",
  level: "debug",
  serviceName: "integration-tests",
});

const graphContext = createTestImpureGraphContext();

describe("Comment", () => {
  let testUser: User;
  let testBlock: Block;
  let testPage: Page;

  beforeAll(async () => {
    await ensureSystemGraphIsInitialized({
      logger,
      context: graphContext,
      seedSystemPolicies: true,
    });

    testUser = await createTestUser(graphContext, "commentTest", logger);
    const authentication = { actorId: testUser.accountId };

    const initialBlock = await createBlock(
      graphContext,
      { actorId: testUser.accountId },
      {
        webId: testUser.accountId as WebId,
        componentId: "text",
        blockData: await createEntity<Text>(
          graphContext,
          { actorId: testUser.accountId },
          {
            webId: testUser.accountId as WebId,
            entityTypeIds: [systemEntityTypes.text.entityTypeId],
            properties: {
              value: {
                "https://blockprotocol.org/@blockprotocol/types/property-type/textual-content/":
                  { value: [] },
              },
            },
          },
        ),
      },
    );

    testPage = await createPage(graphContext, authentication, {
      initialBlocks: [initialBlock],
      webId: testUser.accountId as WebId,
      title: "test page",
      type: "document",
    });

    const pageBlocks = await getPageBlocks(graphContext, authentication, {
      pageEntityId: testPage.entity.metadata.recordId.entityId,
      type: "document",
    });

    testBlock = pageBlocks[0]!.rightEntity;

    return async () => {
      await deleteKratosIdentity({
        kratosIdentityId: testUser.kratosIdentityId,
      });

      await resetGraph();
    };
  });

  it("createComment method can create a comment", async () => {
    const authentication = { actorId: testUser.accountId };

    const comment = await createComment(graphContext, authentication, {
      webId: testUser.accountId as WebId,
      parentEntityId: testBlock.entity.metadata.recordId.entityId,
      textualContent: [],
      author: testUser,
    });

    /**
     * Notifications are created after the request is resolved, so we need to wait
     * before trying to get the notification.
     *
     * @todo: consider adding retry logic instead of relying on a timeout
     */
    await waitForAfterHookTriggerToComplete();

    const commentEntityId = comment.entity.metadata.recordId.entityId;

    const hasText = await getCommentText(graphContext, authentication, {
      commentEntityId,
    });
    expect(hasText.textualContent).toEqual([]);

    const commentAuthor = await getCommentAuthor(graphContext, authentication, {
      commentEntityId,
    });
    expect(commentAuthor.entity).toEqual(testUser.entity);

    const parentBlock = await getCommentParent(graphContext, authentication, {
      commentEntityId,
    });
    expect(parentBlock).toEqual(testBlock.entity);
  });
});
