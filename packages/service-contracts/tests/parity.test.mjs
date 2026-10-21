import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { fixtureFile, validateRequest, validateResponse } from "../dist/index.js";

test("Ajv와 Rust가 동일한 모든 fixture를 허용하거나 거부한다", () => {
  // given
  const fixtures = JSON.parse(readFileSync(fixtureFile, "utf8"));
  const expected = fixtures.map((fixture) => (fixture.schema === "request" ? validateRequest : validateResponse)(fixture.value).valid);
  const manifest = fileURLToPath(new URL("../rust/Cargo.toml", import.meta.url));
  const options = { encoding: "utf8", timeout: 300_000, env: { PATH: process.env.PATH, HOME: process.env.HOME, TMPDIR: process.env.TMPDIR, CARGO_TARGET_DIR: process.env.CARGO_TARGET_DIR ?? "/tmp/tabularis-x0-target/host-contracts" } };

  // when
  const actual = JSON.parse(execFileSync("cargo", ["run", "--quiet", "--locked", "--jobs", "2", "--manifest-path", manifest, "--bin", "validate-fixtures"], options));

  // then
  assert.deepEqual(actual, expected);
  assert.deepEqual(actual, fixtures.map((fixture) => fixture.valid));
});
