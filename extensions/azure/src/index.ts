import { createClientProvider } from "./connection/index.js";
import { createQueryHandlers } from "./query/index.js";
import { createCosmosRuntime, runStdio } from "./runtime/index.js";

const clients = createClientProvider();
const { query_page, execute_query } = createQueryHandlers(clients);
const runtime = createCosmosRuntime({ clients, handlers: { query_page, execute_query } });
await runStdio(process.stdin, process.stdout, runtime);
