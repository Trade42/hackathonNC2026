// Loads Ferry's single root .env. The process environment always wins, even
// when a variable is set to the empty string, and an empty value means unset.

import { existsSync, readFileSync, statSync } from "node:fs";
import { dirname, join, resolve } from "node:path";

export type Env = Readonly<Record<string, string>>;

/** Parses dotenv text: KEY=VALUE lines, `#` comments, optional `export ` and quotes. */
export function parseDotenv(text: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.trim();
    if (line === "" || line.startsWith("#")) continue;
    const match = /^(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*)$/.exec(line);
    if (!match) continue;
    const key = match[1] as string;
    let value = match[2] as string;
    const quote = value[0];
    if ((quote === '"' || quote === "'") && value.length >= 2 && value.endsWith(quote)) {
      value = value.slice(1, -1);
      if (quote === '"') value = value.replace(/\\n/g, "\n").replace(/\\"/g, '"');
    } else {
      // Unquoted values may carry a trailing comment.
      value = value.replace(/\s+#.*$/, "").trim();
    }
    out[key] = value;
  }
  return out;
}

/**
 * Finds the root .env: FERRY_ENV_FILE when set, otherwise the first directory
 * at or above `startDir` that holds both `.env` and `contract/`.
 */
export function findEnvFile(startDir: string, ferryEnvFile: string | undefined): string | undefined {
  if (ferryEnvFile) return resolve(ferryEnvFile);
  let dir = resolve(startDir);
  for (;;) {
    const candidate = join(dir, ".env");
    if (existsSync(candidate) && isDir(join(dir, "contract"))) return candidate;
    const parent = dirname(dir);
    if (parent === dir) return undefined;
    dir = parent;
  }
}

function isDir(path: string): boolean {
  try {
    return statSync(path).isDirectory();
  } catch {
    return false;
  }
}

/**
 * Merges the file over nothing and the process over the file, key by key.
 * A key present in `processEnv` wins even when empty; empty results are dropped.
 */
export function mergeEnv(
  processEnv: Readonly<Record<string, string | undefined>>,
  fileVars: Readonly<Record<string, string>>,
): Env {
  const out: Record<string, string> = {};
  const keys = new Set([...Object.keys(fileVars), ...Object.keys(processEnv)]);
  for (const key of keys) {
    const value = Object.hasOwn(processEnv, key) && processEnv[key] !== undefined ? processEnv[key] : fileVars[key];
    if (value !== undefined && value !== "") out[key] = value;
  }
  return out;
}

export interface LoadedEnv {
  env: Env;
  /** The .env file that was read, if any. */
  file: string | undefined;
}

export function loadEnv(
  processEnv: Readonly<Record<string, string | undefined>> = process.env,
  startDir: string = process.cwd(),
): LoadedEnv {
  const ferryEnvFile = processEnv.FERRY_ENV_FILE || undefined;
  const file = findEnvFile(startDir, ferryEnvFile);
  let fileVars: Record<string, string> = {};
  if (file !== undefined) {
    if (existsSync(file)) {
      fileVars = parseDotenv(readFileSync(file, "utf8"));
    } else if (ferryEnvFile) {
      throw new Error(`FERRY_ENV_FILE points to a missing file: ${file}`);
    }
  }
  return { env: mergeEnv(processEnv, fileVars), file };
}
