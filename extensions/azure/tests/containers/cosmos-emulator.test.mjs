import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { test } from "node:test";
import { CosmosClient } from "@azure/cosmos";
import { GenericContainer, Wait } from "testcontainers";
import { createClientProvider } from "../../dist/connection/index.js";
import { createQueryHandlers } from "../../dist/query/index.js";
import { createCosmosRuntime } from "../../dist/runtime/index.js";

const image = "mcr.microsoft.com/cosmosdb/linux/azure-cosmos-emulator:vnext-latest@sha256:4535ddbd27bba762f3e068f7585feb0022a2559ceccc9394b23aa38d91d77c38";
const syntheticEndpoint = "https://fixture-account.documents.azure.com";
const emulatorKey = "C2y6yDjf5/R+ob0N8A7Cgv30VRDJIWEHLM+4QDU5DE2nQ9nDuVTqobD4b8mGGyPMbIZnqyMsEcaGQy67XIw/Jw==";
const emulatorAuth = { kind: "account_key", account_key: emulatorKey, identity: "cosmos-emulator-fixture" };
let emulator;
let emulatorEndpoint;

async function startEmulator() {
  return await new GenericContainer(image)
    .withExposedPorts(8080, 8081)
    .withCommand(["--enable-explorer", "false", "--enable-otlp", "false", "--enable-telemetry", "false"])
    .withWaitStrategy(Wait.forHttp("/ready", 8080, { abortOnContainerExit: true }).forStatusCode(200).withStartupTimeout(300_000))
    .withStartupTimeout(300_000)
    .start();
}

async function createFixture(t) {
  const databaseId = `it-${randomUUID()}`;
  const containerId = "items";
  const connectionId = randomUUID();
  const settings = { endpoint: syntheticEndpoint, database: databaseId, auth_mode: "account_key" };
  const connection = { connection_id: connectionId, settings, auth: emulatorAuth };
  const clients = createClientProvider({
    createClient: (options) => new CosmosClient({ ...options, endpoint: emulatorEndpoint }),
  });
  const queryHandlers = createQueryHandlers(clients);
  const runtime = createCosmosRuntime({
    clients,
    handlers: { query_page: queryHandlers.query_page, execute_query: queryHandlers.execute_query },
  });
  let client;
  let databaseCreated = false;

  t.after(async () => {
    try {
      if (databaseCreated && client) await client.database(databaseId).delete();
    } finally {
      await runtime.close();
    }
  });

  client = await clients.get(connection);
  await client.databases.createIfNotExists({ id: databaseId });
  databaseCreated = true;
  const database = client.database(databaseId);
  const { container } = await database.containers.createIfNotExists({
    id: containerId,
    partitionKey: { paths: ["/tenant"], kind: "Hash" },
  });

  function request(method, input, { readOnly = false, id = randomUUID() } = {}) {
    return {
      jsonrpc: "2.0",
      id,
      method,
      params: {
        params: {
          driver: "cosmos-nosql",
          database: databaseId,
          extra: { endpoint: syntheticEndpoint, database: databaseId, auth_mode: "account_key" },
        },
        driver_context: {
          protocol_version: 1,
          connection_id: connectionId,
          request_id: id,
          deadline_ms: 120_000,
          read_only: readOnly,
          auth: emulatorAuth,
        },
        input,
      },
    };
  }

  return { client, database, container, databaseId, containerId, runtime, request };
}

async function seed(container, documents) {
  const created = [];
  for (const document of documents) created.push((await container.items.create(document)).resource);
  return created;
}

function queryInput(database, container, category, pageSize = 2) {
  return {
    language: "cosmos_sql",
    database,
    container,
    text: "SELECT VALUE c.payload FROM c WHERE c.category = @category ORDER BY c.position",
    parameters: [{ name: "@category", value: category }],
    page_size: pageSize,
    partition_key: [{ type: "string", value: "query" }],
  };
}

test("Cosmos emulator integration scenarios", { timeout: 300_000 }, async (suite) => {
  emulator = await startEmulator();
  emulatorEndpoint = `http://${emulator.getHost()}:${emulator.getMappedPort(8081)}`;
  suite.after(async () => {
    await emulator?.stop();
  });
  await suite.test("create_document stores the JSON document in the emulator", async (t) => {
    // given
    const f = await createFixture(t);
    const document = { id: "created", tenant: "create", enabled: true, amount: 12.5, detail: { labels: ["a", null] } };

    // when
    const actual = await f.runtime.dispatch(f.request("create_document", {
      database: f.databaseId,
      container: f.containerId,
      document,
      partition_key: [{ type: "string", value: "create" }],
    }));

    // then
    assert.equal(actual.result.status, "succeeded");
    assert.equal(actual.result.data.document.id, document.id);
    assert.equal(actual.result.data.document.tenant, document.tenant);
    assert.equal(actual.result.data.document.enabled, document.enabled);
    assert.equal(actual.result.data.document.amount, document.amount);
    assert.deepEqual(actual.result.data.document.detail, document.detail);
    assert.equal(typeof actual.result.data.etag, "string");
  });

  await suite.test("read_document tells documents with the same id apart by partition key", async (t) => {
    // given
    const f = await createFixture(t);
    await seed(f.container, [
      { id: "shared", tenant: "alpha", value: 1 },
      { id: "shared", tenant: "beta", value: 2 },
    ]);

    // when
    const actual = await f.runtime.dispatch(f.request("read_document", {
      database: f.databaseId,
      container: f.containerId,
      identity: { id: "shared", partition_key: [{ type: "string", value: "beta" }] },
    }));

    // then
    assert.equal(actual.result.status, "succeeded");
    assert.equal(actual.result.data.document.id, "shared");
    assert.equal(actual.result.data.document.tenant, "beta");
    assert.equal(actual.result.data.document.value, 2);
    assert.equal(actual.result.data.etag, actual.result.data.document._etag);
    assert.equal(actual.result.data.identity.partition_key[0].value, "beta");
  });

  await suite.test("replace_document stores the change when the ETag matches", async (t) => {
    // given
    const f = await createFixture(t);
    const [stored] = await seed(f.container, [{ id: "replace", tenant: "replace", value: 1 }]);
    const document = { ...stored, value: 2 };

    // when
    const actual = await f.runtime.dispatch(f.request("replace_document", {
      database: f.databaseId,
      container: f.containerId,
      identity: { id: stored.id, partition_key: [{ type: "string", value: stored.tenant }] },
      if_match: stored._etag,
      document,
    }));

    // then
    assert.equal(actual.result.status, "succeeded");
    assert.equal(actual.result.data.document.value, 2);
    assert.notEqual(actual.result.data.etag, stored._etag);
  });

  await suite.test("delete_document removes the document when the ETag matches", async (t) => {
    // given
    const f = await createFixture(t);
    const [stored] = await seed(f.container, [{ id: "delete", tenant: "delete", value: 1 }]);

    // when
    const actual = await f.runtime.dispatch(f.request("delete_document", {
      database: f.databaseId,
      container: f.containerId,
      identity: { id: stored.id, partition_key: [{ type: "string", value: stored.tenant }] },
      if_match: stored._etag,
    }));

    // then
    assert.equal(actual.result.status, "succeeded");
    assert.equal(actual.result.data.deleted, true);
  });

  await suite.test("replace_document reports a stale ETag as a conflict", async (t) => {
    // given
    const f = await createFixture(t);
    const [stored] = await seed(f.container, [{ id: "stale", tenant: "stale", value: 1 }]);

    // when
    const actual = await f.runtime.dispatch(f.request("replace_document", {
      database: f.databaseId,
      container: f.containerId,
      identity: { id: stored.id, partition_key: [{ type: "string", value: stored.tenant }] },
      if_match: "stale-etag",
      document: { ...stored, value: 2 },
    }));

    // then
    assert.equal(actual.result.status, "failed");
    assert.equal(actual.result.error.code, "ETAG_CONFLICT");
  });

  await suite.test("a read-only request is denied document creation", async (t) => {
    // given
    const f = await createFixture(t);

    // when
    const actual = await f.runtime.dispatch(f.request("create_document", {
      database: f.databaseId,
      container: f.containerId,
      document: { id: "denied", tenant: "readonly" },
      partition_key: [{ type: "string", value: "readonly" }],
    }, { readOnly: true }));

    // then
    assert.equal(actual.result.status, "failed");
    assert.equal(actual.result.error.code, "WRITE_NOT_ALLOWED");
  });

  await suite.test("a parameterized query preserves JSON types in the first page and the full snapshot", async (t) => {
    // given
    const f = await createFixture(t);
    const documents = Array.from({ length: 5 }, (_, position) => ({
      id: `query-${position}`,
      tenant: "query",
      category: "selected",
      position,
      payload: { number: position + 0.25, enabled: position % 2 === 0, optional: null, tags: ["x", position], nested: { rank: position } },
    }));
    documents.push({ id: "query-other", tenant: "query", category: "other", position: 99, payload: { number: 99, enabled: false } });
    await seed(f.container, documents);
    const input = queryInput(f.databaseId, f.containerId, "selected");

    // when
    const actual = await f.runtime.dispatch(f.request("query_page", { query: input }));

    // then
    assert.equal(actual.result.status, "succeeded");
    assert.deepEqual(actual.result.data.data.values, [documents[0].payload, documents[1].payload]);
    assert.deepEqual(actual.result.data.snapshot.sets[0].rows, documents.slice(0, 5).map((document) => document.payload));
    assert.equal(typeof actual.result.data.data.values[0].number, "number");
    assert.equal(typeof actual.result.data.data.values[0].enabled, "boolean");
    assert.equal(actual.result.data.data.values[0].optional, null);
    assert.deepEqual(actual.result.data.data.values[0].nested, { rank: 0 });
    assert.equal(actual.result.data.cursor, null);
  });

  await suite.test("the full query result stays in the snapshot when the emulator returns no continuation token", async (t) => {
    // given
    const f = await createFixture(t);
    const documents = Array.from({ length: 3 }, (_, position) => ({
      id: `resume-${position}`,
      tenant: "query",
      category: "resume",
      position,
      payload: { position, flag: position === 2, values: [position, null] },
    }));
    await seed(f.container, documents);
    const input = queryInput(f.databaseId, f.containerId, "resume");

    // when
    const actual = await f.runtime.dispatch(f.request("query_page", { query: input }));

    // then
    assert.equal(actual.result.status, "succeeded");
    assert.deepEqual(actual.result.data.data.values, [documents[0].payload, documents[1].payload]);
    assert.deepEqual(actual.result.data.snapshot.sets[0].rows, documents.map((document) => document.payload));
    assert.equal(actual.result.data.cursor, null);
  });

  await suite.test("a parameterized query without matches returns an empty array and no cursor", async (t) => {
    // given
    const f = await createFixture(t);
    await seed(f.container, [{ id: "empty", tenant: "query", category: "present", payload: { value: 1 } }]);

    // when
    const actual = await f.runtime.dispatch(f.request("query_page", {
      query: queryInput(f.databaseId, f.containerId, "missing"),
    }));

    // then
    assert.equal(actual.result.status, "succeeded");
    assert.deepEqual(actual.result.data.data.values, []);
    assert.equal(actual.result.data.cursor, null);
  });
});
