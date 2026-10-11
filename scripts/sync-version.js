import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { valid } from "semver";

const pkg = JSON.parse(readFileSync(resolve("package.json"), "utf8"));
if (pkg.name !== "tabularis-extended" || !valid(pkg.version)) {
  throw new Error("Expected a valid Tabularis Extended package version.");
}
const version = pkg.version;
const configPath = resolve("src-tauri/tauri.conf.json");
const cargoPath = resolve("src-tauri/Cargo.toml");
const lockPath = resolve("src-tauri/Cargo.lock");
const config = JSON.parse(readFileSync(configPath, "utf8"));
const cargo = readFileSync(cargoPath, "utf8");
const lock = readFileSync(lockPath, "utf8");
const rootPackage = /(\[\[package\]\]\r?\nname = "tabularis-extended"\r?\nversion = ")[^"]+("(?=\r?\n))/g;
if ([...lock.matchAll(rootPackage)].length !== 1 || !/^name = "tabularis-extended"$/m.test(cargo)) {
  throw new Error("Expected exactly one fork package in the Cargo manifests.");
}
config.version = version;
writeFileSync(configPath, `${JSON.stringify(config, null, 2)}\n`);
writeFileSync(cargoPath, cargo.replace(/^version = ".*"/m, `version = "${version}"`));
writeFileSync(lockPath, lock.replace(rootPackage, (_, before, after) => `${before}${version}${after}`));
writeFileSync(resolve("src/version.ts"), `export const APP_VERSION = "${version}";\n`);
console.log(`Synchronized Tabularis Extended ${version}.`);
