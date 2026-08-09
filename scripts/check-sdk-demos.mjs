import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import path from "node:path";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const contract = JSON.parse(
  await readFile(path.join(root, "docs", "sdk-demo-contract-v1.json"), "utf8"),
);
const failures = [];

for (const [client, demo] of Object.entries(contract.demos)) {
  const paths = demo.paths ?? [demo.path];
  let source = "";
  try {
    source = (
      await Promise.all(paths.map((relative) => readFile(path.join(root, relative), "utf8")))
    ).join("\n");
  } catch (error) {
    failures.push(`${client}: cannot read ${paths.join(", ")}: ${error.message}`);
    continue;
  }
  for (const marker of demo.markers) {
    if (!source.includes(marker)) failures.push(`${client}: missing ${marker}`);
  }
}

if (failures.length > 0) {
  throw new Error(`SDK demo contract v${contract.version} failed:\n${failures.join("\n")}`);
}

console.log(
  `SDK demo contract v${contract.version}: ${Object.keys(contract.demos).length} clients, ` +
    `${contract.scenarios.length} common scenarios verified`,
);
