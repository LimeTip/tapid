import { createHash } from "node:crypto";
import {
  accessSync,
  constants,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { gzipSync } from "node:zlib";

export const root = dirname(dirname(fileURLToPath(import.meta.url)));
export const read = (path: string) => readFileSync(path, "utf8");
export const json = (path: string) => JSON.parse(read(path));
export const write = (path: string, value: string | Buffer) => {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, value);
};
export const writeJson = (path: string, value: unknown) =>
  write(path, JSON.stringify(value));
export const sha256 = (value: string | Buffer) =>
  createHash("sha256").update(value).digest("hex");
export const quote = (value: string) => `'${value.replaceAll("'", "'\\''")}'`;
export const nodeArgs = (script: string, args: string[] = []) => [
  "--experimental-strip-types",
  join(root, script),
  ...args,
];
export const hasCommand = (command: string) =>
  (process.env.PATH ?? "").split(delimiter).some((directory) => {
    const suffixes =
      process.platform === "win32"
        ? ["", ...(process.env.PATHEXT ?? ".EXE;.CMD;.BAT").split(";")]
        : [""];
    return suffixes.some((suffix) => {
      const path = join(directory, command + suffix);
      try {
        accessSync(path, constants.X_OK);
        return statSync(path).isFile();
      } catch {
        return false;
      }
    });
  });
export async function temporary<T>(
  body: (path: string) => T | Promise<T>,
): Promise<T> {
  const path = mkdtempSync(join(tmpdir(), "tapid-script-test-"));
  try {
    return await body(path);
  } finally {
    rmSync(path, { recursive: true, force: true });
  }
}

// One USTAR member, used only for offline fixtures. No external archive tool or registry.
export function fixtureArchive(
  name: string,
  contents: Buffer,
  mode = 0o644,
): Buffer {
  const header = Buffer.alloc(512);
  const octal = (offset: number, width: number, value: number) =>
    header.write(
      value.toString(8).padStart(width - 1, "0") + "\0",
      offset,
      width,
      "ascii",
    );
  header.write(name, 0, 100, "utf8");
  octal(100, 8, mode);
  octal(108, 8, 0);
  octal(116, 8, 0);
  octal(124, 12, contents.length);
  octal(136, 12, 0);
  header.fill(32, 148, 156);
  header[156] = 48;
  header.write("ustar\0", 257, 6, "ascii");
  header.write("00", 263, 2, "ascii");
  const checksum = header.reduce((sum, byte) => sum + byte, 0);
  header.write(checksum.toString(8).padStart(6, "0") + "\0 ", 148, 8, "ascii");
  return gzipSync(
    Buffer.concat([
      header,
      contents,
      Buffer.alloc(((512 - (contents.length % 512)) % 512) + 1024),
    ]),
  );
}
