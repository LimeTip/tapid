import { preflight, run, summarize } from "./install-benchmarks/run.ts";
try {
  const [operation, path, output, ...extra] = process.argv.slice(2);
  if (extra.length || !path)
    throw new Error(
      "usage: benchmark-installs.ts preflight SPEC | run SPEC RUN_DIRECTORY | summarize RUN_DIRECTORY",
    );
  if (operation === "preflight" && !output) preflight(path);
  else if (operation === "run" && output) await run(path, output);
  else if (operation === "summarize" && !output) summarize(path);
  else throw new Error("invalid operation or arguments");
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
}
