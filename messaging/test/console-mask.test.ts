import { describe, expect, test } from "bun:test";
import { installConsoleMask, maskConsoleArgs, type ConsoleLike } from "../src/console-mask";

const PHONE = "+19195550123";

describe("maskConsoleArgs", () => {
  test("masks strings, format arguments and objects", () => {
    expect(maskConsoleArgs([`texting ${PHONE}`])).toBe("texting +1•••0123");
    expect(maskConsoleArgs(["%s sent %d messages", PHONE, 2])).toBe("+1•••0123 sent 2 messages");
    expect(maskConsoleArgs(["chat", { guid: `any;-;${PHONE}`, email: "maya.lopez@icloud.com" }])).not.toMatch(/9195550123|maya\.lopez/);
    expect(maskConsoleArgs([])).toBe("");
  });

  test("masks an Error's message, stack, fields and cause, keeping the stack frames", () => {
    const err = Object.assign(new Error(`chat any;-;${PHONE} not found`), {
      details: `no chat for ${PHONE}`,
      cause: new Error(`upstream rejected ${PHONE}`),
    });
    const line = maskConsoleArgs(["[spectrum.imessage]", "WARN", "send failed", err]);
    expect(line).not.toContain("9195550123");
    expect(line).toContain("chat any;-;+1•••0123 not found");
    expect(line).toContain("no chat for +1•••0123");
    expect(line).toContain("upstream rejected +1•••0123");
    expect(line).toContain("console-mask.test.ts");
    // The caller's Error is not changed.
    expect(err.message).toBe(`chat any;-;${PHONE} not found`);
  });

  test("an already masked line passes through unchanged", () => {
    const line = "2026-10-10T22:00:00.000Z WARN send failed space=any;-;+1•••0123 progress=100%";
    expect(maskConsoleArgs([line])).toBe(line);
  });
});

describe("installConsoleMask", () => {
  test("wraps log, info, warn, error and debug, and restore() puts them back", () => {
    const printed: [string, unknown[]][] = [];
    const record = (method: string) => (...args: unknown[]) => void printed.push([method, args]);
    const target: ConsoleLike = { log: record("log"), info: record("info"), warn: record("warn"), error: record("error"), debug: record("debug") };
    const originalWarn = target.warn;

    const restore = installConsoleMask(target);
    for (const method of ["log", "info", "warn", "error", "debug"] as const) target[method]("[sdk]", new Error(`failed for ${PHONE}`));
    expect(printed.map(([method]) => method)).toEqual(["log", "info", "warn", "error", "debug"]);
    for (const [, args] of printed) {
      expect(args).toHaveLength(1);
      expect(args[0] as string).toContain("[sdk] Error: failed for +1•••0123");
      expect(args[0] as string).not.toContain("9195550123");
    }

    restore();
    expect(target.warn).toBe(originalWarn);
  });

  test("index.ts installs it before anything else loads", async () => {
    const source = await Bun.file(`${import.meta.dir}/../src/index.ts`).text();
    const firstImport = source.split("\n").find((line) => line.startsWith("import "));
    expect(firstImport).toBe('import "./install-console-mask";');
  });
});

describe("the Spectrum SDK's own logger, in a child process", () => {
  async function run(mode: "masked" | "raw"): Promise<string> {
    const child = Bun.spawn([process.execPath, "--no-env-file", `${import.meta.dir}/fixtures/sdk-logger.ts`, mode, PHONE], {
      stdout: "pipe",
      stderr: "pipe",
    });
    const [out, err] = await Promise.all([
      new Response(child.stdout as ReadableStream<Uint8Array>).text(),
      new Response(child.stderr as ReadableStream<Uint8Array>).text(),
      child.exited,
    ]);
    return out + err;
  }

  test("without the mask it prints the raw number (the leak this closes)", async () => {
    const output = await run("raw");
    expect(output).toContain("failed to share contact card");
    expect(output).toContain(PHONE);
  });

  test("with the mask nothing it prints holds the number", async () => {
    const output = await run("masked");
    expect(output).toContain("failed to share contact card");
    expect(output).toContain("send failed");
    expect(output).toContain("chat any;-;+1•••0123 not found");
    expect(output).toContain("upstream rejected +1•••0123");
    expect(output).not.toContain("9195550123");
  });
});
