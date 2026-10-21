import http from "node:http";
import https from "node:https";
import net from "node:net";
import tls from "node:tls";
import { syncBuiltinESMExports } from "node:module";

let attempts = 0;
function denied() { attempts++; throw new Error("Synthetic production network guard."); }
http.request = denied;
https.request = denied;
net.connect = denied;
tls.connect = denied;
globalThis.fetch = denied;
syncBuiltinESMExports();
process.on("exit", () => { process.send?.({ type: "network_attempts", count: attempts }); });
