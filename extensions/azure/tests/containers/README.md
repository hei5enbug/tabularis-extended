# Cosmos emulator tests

Run from `extensions/azure` with Docker available:

```sh
corepack pnpm install --frozen-lockfile
corepack pnpm test:containers
```

Testcontainers starts its own Cosmos vNext emulator, waits for `/ready`, and removes it after the suite.
The image and Testcontainers version are pinned. The tests create disposable databases and use the
emulator's public development key. They never load saved application connections or Azure credentials.
The suite allows 300 seconds for the emulator to start. A first run that has to pull the image can exceed it;
run `docker pull` on the pinned image from `cosmos-emulator.test.mjs` first if it times out.

The production runtime, handlers, SDK transport, and client provider execute the tests. The existing
client-factory test seam supplies only the owned emulator endpoint. Production Azure endpoint validation
and TLS requirements remain unchanged; HTTP is confined to this test fixture.

Nine scenarios cover document CRUD, partition identity, ETag conflicts, local read-only enforcement,
parameterized queries, typed JSON, empty results, and complete snapshots when no continuation token is
returned. The pinned emulator returned all five query rows without a native token; the driver preserved
all five in its snapshot while exposing a two-row first page.

This does not validate native cloud continuation, Entra authentication, Azure permissions, RU costs, or
index behavior. The [Microsoft emulator support table](https://learn.microsoft.com/en-us/azure/cosmos-db/emulator-linux#feature-support)
lists the emulator's limitations. Controlled protocol and SDK fixtures cover these driver paths separately.
