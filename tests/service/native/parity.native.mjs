import test from "node:test";
import assert from "node:assert/strict";
import path from "node:path";
import {
  NativeHarnessError, createNativeFixture, explicitNativeEnvironment, loadNativeInputs,
  connectFixture, request, queryInput, querySource, succeeded, requireCondition,
  runArtifactSave, readMcpArtifact, sha256,
} from "../support/native-process.mjs";

function semanticEnvelope(response, { revision = false, artifact = false } = {}) {
  const copy = structuredClone(response);
  for (const name of ["request_id", "job_id", "result_id"]) {
    requireCondition(copy[name] === null || typeof copy[name] === "string" && copy[name].length > 0, "DYNAMIC_RESPONSE_ID_INVALID");
    if (copy[name] !== null) copy[name] = `<dynamic:${name}>`;
  }
  copy.metrics.elapsed_ms = "<elapsed_ms>";
  if (copy.page.next_token !== null) copy.page.next_token = "<opaque_next_token>";
  if (revision) {
    requireCondition(typeof copy.data.result_revision === "string" && copy.data.result_revision.length > 0, "RESULT_REVISION_MISSING");
    copy.data.result_revision = "<result_revision>";
  }
  if (artifact) {
    requireCondition(/^artifact-[a-f0-9-]{36}$/.test(copy.data.artifact_id), "ARTIFACT_ID_INVALID");
    copy.data.artifact_id = "<artifact_id>";
  }
  return copy;
}

function expectedQueryPresentation(response) {
  const copy = structuredClone(response);
  copy.data.rows_count = copy.data.rows.length;
  copy.data.rows_sample = copy.data.rows;
  delete copy.data.rows;
  copy.presentation_truncated = false;
  return copy;
}

function expectedSpatialPresentation(response) {
  const copy = structuredClone(response);
  copy.data.features_count = copy.data.features.length;
  copy.data.features_sample = copy.data.features.map((feature) => ({ ...feature, geometry: { type: "Point" } }));
  delete copy.data.features;
  copy.presentation_truncated = true;
  return copy;
}

async function parityScenario(fixture) {
  const evidence = { scenario: "cli_mcp_parity", native_execution: true, database_writes: 0 };
  let actual;
  try {
    const cli = fixture.start("cli");
    const mcp = fixture.start("mcp");
    const initialized = await mcp.rpc("initialize", { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "tabularis-native-verification", version: "1" } });
    const tools = await mcp.rpc("tools/list");
    const connectionId = await connectFixture(fixture, cli);
    const context = { connection_id: connectionId };
    const cliQuery = succeeded(await cli.call(request("query.execute", queryInput(), context)));
    const mcpQuery = succeeded(await mcp.call(request("query.execute", queryInput(), context)));
    requireCondition(cliQuery.page.has_more && mcpQuery.page.has_more, "QUERY_CONTINUATION_REQUIRED");
    const get = (id) => ({ result_id: id, result_set_index: 0, offset: 0, limit: 1000 });
    const cliSnapshot = succeeded(await cli.call(request("result.get", get(cliQuery.result_id), context)));
    const recoveredMcpSnapshot = succeeded(await cli.call(request("result.get", get(mcpQuery.result_id), context)));
    const mcpFirstSnapshot = succeeded(await cli.call(request("result.get", { ...get(mcpQuery.result_id), limit: 1 }, context)));
    const cliNext = succeeded(await cli.call(request("query.next", { next_token: cliQuery.page.next_token }, context)));
    const mcpNext = succeeded(await mcp.call(request("query.next", { next_token: mcpQuery.page.next_token }, context)));
    const recoveredMcpNext = succeeded(await cli.call(request("result.get", { ...get(mcpQuery.result_id), offset: mcpNext.data.offset, limit: 1 }, context)));
    const cliCatalog = succeeded(await cli.call(request("catalog.describe", { table: { database: "spatial_fixture", schema: "public", table: "spatial_ref_sys" } }, context)));
    const mcpCatalog = succeeded(await mcp.call(request("catalog.describe", { table: { database: "spatial_fixture", schema: "public", table: "spatial_ref_sys" } }, context)));
    const spatialInput = { result_id: cliQuery.result_id, result_set_index: 0, column_index: 2, source_srid: 4326, longitude_mode: "preserve", skip_invalid: false, page_size: 1000 };
    const cliSpatial = succeeded(await cli.call(request("spatial.query_result", spatialInput, context)));
    const mcpSpatial = succeeded(await mcp.call(request("spatial.query_result", spatialInput, context)));
    const exportInput = { source: querySource(cliQuery.result_id), format: "geojson", filename: "native-한글.geojson" };
    const cliExport = succeeded(await cli.call(request("spatial.export", exportInput, context)));
    const mcpExport = succeeded(await mcp.call(request("spatial.export", exportInput, context)));
    const saved = await runArtifactSave(fixture, cliExport.data.artifact_id, path.join(fixture.evidence, "cli.geojson"));
    const streamed = await readMcpArtifact(mcp, mcpExport.data.artifact_id);
    const deniedInput = { table: { database: "spatial_fixture", schema: "public", table: "spatial_ref_sys" }, identity: { columns: ["srid"], values: [4326] }, changes: { auth_name: "native-denied" } };
    const cliInvalid = await cli.call(request("row.update", deniedInput, context));
    const mcpInvalid = await mcp.call(request("row.update", deniedInput, context));
    const createdMap = succeeded(await cli.call(request("map.create", { name: "Headless fixture" })));
    const headless = await cli.call(request("map.open", { gui_instance_id: "native-verification-unavailable" }, { map_id: createdMap.data.map.map_id }));
    actual = { initialized, tools, connectionId, cliQuery, mcpQuery, cliSnapshot, recoveredMcpSnapshot,
      mcpFirstSnapshot, cliNext, mcpNext, recoveredMcpNext, cliCatalog, mcpCatalog, cliSpatial, mcpSpatial,
      cliExport, mcpExport, saved, streamed, cliInvalid, mcpInvalid, headless };
    evidence.snapshot_recovered_across_processes = { cli_pid: cli.child.pid, mcp_pid: mcp.child.pid,
      mcp_result_id: mcpQuery.result_id, recovered_result_id: recoveredMcpSnapshot.result_id };
    evidence.artifact_checksums = { cli: sha256(saved.bytes), mcp: sha256(streamed.bytes), chunks: streamed.chunkCount };
    evidence.permission_denial = { operations: ["row.update", "row.update"], required_code: "WRITE_NOT_ALLOWED", dispatch_boundary: "connection.allow_writes:false before driver dispatch" };
    evidence.schema_error_contract = "MCP invalid-schema decode uses JSON-RPC error.data; it is not a service response envelope.";
  } catch (error) {
    evidence.failure = error instanceof NativeHarnessError ? error.code : "NATIVE_PARITY_FAILED";
    throw new NativeHarnessError(evidence.failure, fixture.root);
  } finally {
    const cleanup = await fixture.finish(evidence);
    if (actual) actual.cleanup = cleanup;
    console.log(JSON.stringify({ evidence_root: cleanup.evidenceRoot, scenario: evidence.scenario }));
  }
  return actual;
}

test("실제 CLI와 MCP는 원본 스냅샷과 공간 내보내기를 보존하고 요약의 좌표 생략을 명시한다", { timeout: 10 * 60 * 1000 }, async () => {
  // given
  const inputs = loadNativeInputs(explicitNativeEnvironment());
  const fixture = createNativeFixture(inputs);
  // when
  const actual = await parityScenario(fixture);
  // then
  assert.equal(actual.initialized.protocolVersion, "2025-06-18");
  assert.ok(["query.execute", "query.next", "result.get", "catalog.describe", "spatial.query_result", "spatial.export"].every((name) => actual.tools.tools.some((tool) => tool.name === name)));
  assert.deepEqual(semanticEnvelope(actual.recoveredMcpSnapshot, { revision: true }), semanticEnvelope(actual.cliSnapshot, { revision: true }));
  assert.equal(actual.recoveredMcpSnapshot.result_id, actual.mcpQuery.result_id);
  assert.deepEqual(actual.cliSnapshot.data.rows.map((row) => row.slice(0, 2)), [[1, "native-한글"], [2, "native-한글"], [3, "native-한글"]]);
  assert.deepEqual(actual.cliSnapshot.data.columns.map((column) => column.name), ["id", "note", "geom"]);
  assert.ok(actual.cliSnapshot.data.rows.every((row) => row[2].kind === "spatial" && row[2].srid === 4326 && row[2].encoding === "ewkb-base64"));
  assert.deepEqual(semanticEnvelope(actual.mcpQuery, { revision: true }), semanticEnvelope(expectedQueryPresentation(actual.cliQuery), { revision: true }));
  assert.deepEqual(actual.mcpQuery.data.rows_sample, actual.mcpFirstSnapshot.data.rows);
  assert.equal(actual.mcpQuery.data.rows_count, 1);
  assert.equal(actual.mcpQuery.presentation_truncated, false);
  assert.deepEqual(semanticEnvelope(actual.mcpNext, { revision: true }), semanticEnvelope(expectedQueryPresentation(actual.cliNext), { revision: true }));
  assert.deepEqual(actual.mcpNext.data.rows_sample, actual.recoveredMcpNext.data.rows);
  assert.equal(actual.cliNext.data.offset, 1);
  assert.equal(actual.mcpNext.data.offset, 1);
  assert.equal(actual.cliNext.data.result_revision, actual.cliQuery.data.result_revision);
  assert.equal(actual.mcpNext.data.result_revision, actual.mcpQuery.data.result_revision);
  assert.deepEqual(semanticEnvelope(actual.mcpCatalog), { ...semanticEnvelope(actual.cliCatalog), presentation_truncated: false });
  assert.deepEqual(semanticEnvelope(actual.mcpSpatial), semanticEnvelope(expectedSpatialPresentation(actual.cliSpatial)));
  assert.ok(actual.cliSpatial.data.features.every((feature) => feature.geometry.type === "Point" && feature.geometry.coordinates[0] === 10 && feature.geometry.coordinates[1] === 20));
  assert.equal(actual.cliSpatial.data.features.length, 3);
  assert.equal(actual.mcpSpatial.data.features_count, 3);
  assert.equal(actual.mcpSpatial.presentation_truncated, true);
  assert.ok(actual.mcpSpatial.data.features_sample.every((feature) => !Object.hasOwn(feature.geometry, "coordinates")));
  assert.deepEqual(actual.mcpSpatial.data.row_references, actual.cliSpatial.data.row_references);
  assert.deepEqual(semanticEnvelope(actual.mcpExport, { artifact: true }), { ...semanticEnvelope(actual.cliExport, { artifact: true }), presentation_truncated: false });
  assert.deepEqual(actual.streamed.bytes, actual.saved.bytes);
  assert.equal(actual.streamed.metadata.checksum, actual.saved.metadata.checksum);
  assert.equal(actual.streamed.metadata.feature_count, 3);
  assert.equal(actual.saved.metadata.coordinate_count, 3);
  assert.equal(JSON.parse(actual.saved.bytes.toString("utf8")).type, "FeatureCollection");
  assert.deepEqual(JSON.parse(actual.saved.bytes.toString("utf8")).features, actual.cliSpatial.data.features);
  assert.deepEqual(semanticEnvelope(actual.mcpInvalid), { ...semanticEnvelope(actual.cliInvalid), presentation_truncated: false });
  assert.equal(actual.cliInvalid.error.code, "WRITE_NOT_ALLOWED");
  assert.equal(actual.cliInvalid.error.outcome, "not_started");
  assert.equal(actual.headless.error.code, "GUI_UNAVAILABLE");
  assert.equal(actual.headless.error.outcome, "applied");
  assert.deepEqual(actual.headless.error.details.instances, []);
  assert.equal(actual.headless.data.gui_applied, false);
  assert.deepEqual(actual.cleanup.cleanupErrors, []);
});
