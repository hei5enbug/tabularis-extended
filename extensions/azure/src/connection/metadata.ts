import type { CosmosClient, Container, ContainerDefinition, Resource } from "@azure/cosmos";
import type { RpcRequestContext } from "../runtime/contracts.js";
import { assertRequestActive, DriverError, mapSdkError } from "../runtime/errors.js";
import { connectionContext, type ClientProvider } from "./index.js";

export interface ContainerMetadata { database: string; container: string; database_rid: string; container_rid: string; partition_key_paths: string[]; partition_key_kind: string; partition_key_version: number; system_key: boolean }
export function metadataFromResource(database: string, name: string, databaseRid: string, resource: ContainerDefinition & Resource): ContainerMetadata {
  const paths = resource.partitionKey?.paths;
  if (!resource._rid || !databaseRid || !paths || paths.length < 1 || paths.length > 3 || paths.some((path) => typeof path !== "string" || !path.startsWith("/"))) throw new DriverError("UNSUPPORTED_TYPE", "Complete container RID and partition-key metadata are required.", "not_applied");
  return { database, container: name, database_rid: databaseRid, container_rid: resource._rid, partition_key_paths: [...paths], partition_key_kind: resource.partitionKey?.kind ?? "Hash", partition_key_version: resource.partitionKey?.version ?? 1, system_key: resource.partitionKey?.systemKey ?? false };
}
export async function getContainerMetadata(clients: ClientProvider, context: RpcRequestContext, database: string, name: string): Promise<{ client: CosmosClient; container: Container; metadata: ContainerMetadata }> {
  const client = await clients.get(connectionContext(context));
  const db = client.database(database);
  let phase: "database" | "container" = "database";
  try {
    assertRequestActive(context);
    const databaseResponse = await db.read({ abortSignal: context.signal });
    context.metrics?.add(databaseResponse);
    if (!databaseResponse.resource?._rid) throw new DriverError("INVALID_ARGUMENT", "The database was not found.", "not_applied");
    assertRequestActive(context);
    const container = db.container(name);
    phase = "container";
    const response = await container.read({ abortSignal: context.signal });
    context.metrics?.add(response);
    if (!response.resource) throw new DriverError("INVALID_ARGUMENT", "The container was not found.", "not_applied");
    assertRequestActive(context);
    return { client, container, metadata: metadataFromResource(database, name, databaseResponse.resource._rid, response.resource) };
  } catch (error) {
    if (!(error instanceof DriverError)) context.metrics?.add(error);
    throw mapSdkError(error, context, { resource: phase });
  }
}
