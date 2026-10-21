import type { DocumentIdentity, JsonObject, PartitionKeyComponent } from "@tabularis/service-contracts/types";
import type { RpcHandler, RpcRequestContext } from "../runtime/contracts.js";
import type { ClientProvider } from "../connection/index.js";
import { getContainerMetadata } from "../connection/metadata.js";
import { assertRequestActive, DriverError, mapSdkError, safeError, serviceResponse } from "../runtime/errors.js";
import { jsonObject, validateOperationInput } from "../runtime/validation.js";
import { MetricsAccumulator, withRequestMetrics } from "../runtime/metrics.js";
import { assertReadOnlyFields, extractPartitionComponents, fullPartitionKey, samePartitionKey, validateDocumentId, writableDocument } from "./identity.js";

export interface DocumentScope { database: string; container: string }
export interface ReadDocumentInput extends DocumentScope { identity: DocumentIdentity }
export interface CreateDocumentInput extends DocumentScope { document: JsonObject; partition_key: PartitionKeyComponent[] }
export interface ReplaceDocumentInput extends ReadDocumentInput { if_match: string; document: JsonObject }
export interface DeleteDocumentInput extends ReadDocumentInput { if_match: string }
export interface DocumentHandlers { read_document: RpcHandler; create_document: RpcHandler; replace_document: RpcHandler; delete_document: RpcHandler }
function writeAllowed(context: RpcRequestContext): void {
  if (context.read_only !== false) throw new DriverError("WRITE_NOT_ALLOWED", "This connection does not allow document writes.");
  assertRequestActive(context);
}
function documentData(document: unknown, identity: DocumentIdentity): JsonObject {
  if (!jsonObject(document)) throw new DriverError("DRIVER_EXITED", "The service did not return a document.", "unknown");
  return { document, identity: identity as unknown as JsonObject, etag: typeof document._etag === "string" ? document._etag : null };
}
export function createDocumentHandlers(clients: ClientProvider): DocumentHandlers {
  function boundary(action: RpcHandler, write: boolean): RpcHandler {
    return async (input, context) => {
      let dispatched = false;
      const tracked: RpcRequestContext = { ...context, metrics: context.metrics ?? new MetricsAccumulator(), markWriteDispatched: () => { dispatched = true; context.markWriteDispatched?.(); }, write_dispatched: () => dispatched || context.write_dispatched?.() === true };
      try {
        if (tracked.session_handle !== undefined) throw new DriverError("UNSUPPORTED_OPERATION", "Cosmos SQL sessions are unsupported.");
        assertRequestActive(tracked);
        return await withRequestMetrics(tracked, () => action(input, tracked));
      } catch (error) {
        if (!(error instanceof DriverError)) tracked.metrics?.add(error);
        return serviceResponse(tracked, null, safeError(mapSdkError(error, tracked, { write, document: true })));
      }
    };
  }
  return {
    read_document: boundary(async (value, context) => {
      const input = validateOperationInput("document.read", value);
      validateDocumentId(input.identity.id);
      const { container, metadata } = await getContainerMetadata(clients, context, input.database, input.container);
      const item = container.item(input.identity.id, fullPartitionKey(input.identity.partition_key, metadata.partition_key_paths));
      const response = await item.read({ abortSignal: context.signal });
      context.metrics?.add(response);
      if (!response.resource) throw new DriverError("DOCUMENT_NOT_FOUND", "The document was not found.", "not_applied");
      if (response.resource.id !== input.identity.id || !samePartitionKey(extractPartitionComponents(response.resource, metadata.partition_key_paths), input.identity.partition_key)) throw new DriverError("DRIVER_EXITED", "The service returned a mismatched document identity.", "not_applied");
      assertRequestActive(context);
      return serviceResponse(context, documentData(response.resource, input.identity));
    }, false),
    create_document: boundary(async (value, context) => {
      const input = validateOperationInput("document.create", value);
      writeAllowed(context);
      const document = writableDocument(input.document);
      const { container, metadata } = await getContainerMetadata(clients, context, input.database, input.container);
      fullPartitionKey(input.partition_key, metadata.partition_key_paths);
      if (metadata.system_key) throw new DriverError("UNSUPPORTED_TYPE", "System partition-key containers are unsupported for writes.");
      if (!samePartitionKey(extractPartitionComponents(document, metadata.partition_key_paths), input.partition_key)) throw new DriverError("INVALID_ARGUMENT", "The document does not match its full partition key.");
      writeAllowed(context);
      context.markWriteDispatched?.();
      const response = await container.items.create(document, { abortSignal: context.signal, disableAutomaticIdGeneration: true });
      context.metrics?.add(response);
      return serviceResponse(context, documentData(response.resource, { id: document.id as string, partition_key: input.partition_key }));
    }, true),
    replace_document: boundary(async (value, context) => {
      const input = validateOperationInput("document.replace", value);
      writeAllowed(context);
      validateDocumentId(input.identity.id);
      const document = writableDocument(input.document);
      if (document.id !== input.identity.id) throw new DriverError("PARTITION_KEY_IMMUTABLE", "The document id is immutable.");
      const { container, metadata } = await getContainerMetadata(clients, context, input.database, input.container);
      const key = fullPartitionKey(input.identity.partition_key, metadata.partition_key_paths);
      if (metadata.system_key) throw new DriverError("UNSUPPORTED_TYPE", "System partition-key containers are unsupported for writes.");
      if (!samePartitionKey(extractPartitionComponents(document, metadata.partition_key_paths), input.identity.partition_key)) throw new DriverError("PARTITION_KEY_IMMUTABLE", "The partition key is immutable.");
      const item = container.item(input.identity.id, key);
      const snapshotResponse = await item.read({ abortSignal: context.signal });
      context.metrics?.add(snapshotResponse);
      const snapshot = snapshotResponse.resource;
      if (!jsonObject(snapshot)) throw new DriverError("DOCUMENT_NOT_FOUND", "The document was not found.", "not_applied");
      if (snapshot.id !== input.identity.id || !samePartitionKey(extractPartitionComponents(snapshot, metadata.partition_key_paths), input.identity.partition_key)) throw new DriverError("PARTITION_KEY_IMMUTABLE", "The fetched document identity does not match the replacement.");
      if (snapshot._etag !== input.if_match) throw new DriverError("ETAG_CONFLICT", "The document ETag has changed.", "not_applied");
      assertReadOnlyFields(input.document, snapshot);
      writeAllowed(context);
      context.markWriteDispatched?.();
      const response = await item.replace(document, { abortSignal: context.signal, accessCondition: { type: "IfMatch", condition: input.if_match } });
      context.metrics?.add(response);
      return serviceResponse(context, documentData(response.resource, input.identity));
    }, true),
    delete_document: boundary(async (value, context) => {
      const input = validateOperationInput("document.delete", value);
      writeAllowed(context);
      validateDocumentId(input.identity.id);
      const { container, metadata } = await getContainerMetadata(clients, context, input.database, input.container);
      const item = container.item(input.identity.id, fullPartitionKey(input.identity.partition_key, metadata.partition_key_paths));
      const snapshotResponse = await item.read({ abortSignal: context.signal });
      context.metrics?.add(snapshotResponse);
      if (!jsonObject(snapshotResponse.resource)) throw new DriverError("DOCUMENT_NOT_FOUND", "The document was not found.", "not_applied");
      if (snapshotResponse.resource.id !== input.identity.id || !samePartitionKey(extractPartitionComponents(snapshotResponse.resource, metadata.partition_key_paths), input.identity.partition_key)) throw new DriverError("PARTITION_KEY_IMMUTABLE", "The fetched document identity does not match the delete target.");
      if (snapshotResponse.resource._etag !== input.if_match) throw new DriverError("ETAG_CONFLICT", "The document ETag has changed.", "not_applied");
      writeAllowed(context);
      context.markWriteDispatched?.();
      const response = await item.delete({ abortSignal: context.signal, accessCondition: { type: "IfMatch", condition: input.if_match } });
      context.metrics?.add(response);
      return serviceResponse(context, { identity: input.identity as unknown as JsonObject, deleted: true });
    }, true),
  };
}
