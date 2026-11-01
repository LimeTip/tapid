// News-site adapter: keep application-specific exclusions outside the shared comparator.
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { parseArgs } from "node:util";
import { compare as compareConsumer } from "./compare-consumer-package-graphs.ts";
export const compare = (npmRoot: string, tapidRoot: string) =>
  compareConsumer(npmRoot, tapidRoot, (path) =>
    /[\\/]dist[\\/]compiled[\\/]/.test(path),
  );
function main(): number {
  const { values } = parseArgs({
    options: {
      "npm-root": { type: "string" },
      "tapid-root": { type: "string" },
      json: { type: "string" },
      text: { type: "string" },
    },
  });
  if (!values["npm-root"] || !values["tapid-root"])
    throw new Error("--npm-root and --tapid-root are required");
  try {
    const { report, text, code } = compare(
      values["npm-root"],
      values["tapid-root"],
    );
    const encoded = JSON.stringify(report, null, 2) + "\n";
    for (const [path, contents] of [
      [values.json, encoded],
      [values.text, text],
    ])
      if (path) {
        mkdirSync(dirname(path), { recursive: true });
        writeFileSync(path, contents);
      }
    if (!values.json) process.stdout.write(encoded);
    process.stderr.write(text);
    return code;
  } catch (error) {
    console.error(`graph comparison error: ${error}`);
    return 2;
  }
}
if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href
)
  process.exitCode = main();
