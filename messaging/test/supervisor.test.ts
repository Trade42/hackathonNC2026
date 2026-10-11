import { describe, expect, test } from "bun:test";
import { Supervisor, type Backoff } from "../src/supervisor";
import type { InboundMessage } from "../src/types";
import { captureLogs, FakeTransport, message, until } from "./helpers";

const FAST: Backoff = { initialMs: 5, maxMs: 20, stableMs: 10_000 };

describe("Supervisor", () => {
  test("disabled without a factory: run() returns at once and current() is empty", async () => {
    const supervisor = new Supervisor({ factory: undefined, onMessage: () => {} });
    expect(supervisor.status).toBe("disabled");
    await supervisor.run();
    expect(supervisor.status).toBe("disabled");
    expect(supervisor.current()).toBeUndefined();
  });

  test("reconnects when the stream ends, and when it throws", async () => {
    const transports: FakeTransport[] = [];
    const seen: InboundMessage[] = [];
    let disconnects = 0;
    const supervisor = new Supervisor({
      factory: async () => {
        const t = new FakeTransport();
        transports.push(t);
        return t;
      },
      onMessage: (m) => seen.push(m),
      onDisconnect: () => disconnects++,
      backoff: FAST,
    });
    const running = supervisor.run();

    await until(() => transports.length === 1 && supervisor.status === "connected");
    const first = transports[0] as FakeTransport;
    expect(supervisor.current()).toBe(first);
    first.queue.push(message(first.space("any;-;+19195550101")));
    await until(() => seen.length === 1);

    first.queue.end();
    await until(() => transports.length === 2 && supervisor.status === "connected");
    expect(first.stopped).toBe(true);

    (transports[1] as FakeTransport).queue.fail(new Error("stream reset for +19195550101"));
    await until(() => transports.length === 3 && supervisor.status === "connected");
    expect(disconnects).toBe(2);
    expect(supervisor.connects).toBe(3);

    await supervisor.stop();
    await running;
    expect(supervisor.status).toBe("disconnected");
    expect((transports[2] as FakeTransport).stopped).toBe(true);
  });

  test("backs off while connecting fails, reporting disconnected", async () => {
    let attempts = 0;
    const stamps: number[] = [];
    const supervisor = new Supervisor({
      factory: async () => {
        attempts++;
        stamps.push(performance.now());
        throw new Error("Photon unreachable");
      },
      onMessage: () => {},
      backoff: FAST,
    });
    const running = supervisor.run();
    await until(() => attempts >= 4);
    expect(supervisor.status).toBe("disconnected");
    expect(supervisor.current()).toBeUndefined();
    await supervisor.stop();
    await running;
    // Delays double: about 5, 10, 20 ms between attempts.
    const gaps = stamps.slice(1).map((t, i) => t - (stamps[i] as number));
    expect(gaps[2] as number).toBeGreaterThanOrEqual(15);
  });

  test("a connect that never finishes times out and is retried with backoff", async () => {
    let attempts = 0;
    const transport = new FakeTransport();
    const logs = captureLogs();
    const supervisor = new Supervisor({
      factory: () => {
        attempts++;
        return attempts === 1 ? new Promise<FakeTransport>(() => {}) : Promise.resolve(transport);
      },
      onMessage: () => {},
      backoff: FAST,
      connectTimeoutMs: 30,
    });
    const running = supervisor.run();
    try {
      await until(() => supervisor.status === "connected");
    } finally {
      logs.restore();
    }
    expect(attempts).toBe(2);
    expect(supervisor.current()).toBe(transport);
    expect(logs.lines.some((line) => line.includes("spectrum connection failed") && line.includes("spectrum connect timed out after 30 ms"))).toBe(true);
    await supervisor.stop();
    await running;
  });

  test("a connect that finishes after its timeout is stopped, never kept beside its replacement", async () => {
    const late = new FakeTransport();
    const replacement = new FakeTransport();
    let attempts = 0;
    const supervisor = new Supervisor({
      factory: async () => {
        attempts++;
        if (attempts > 1) return replacement;
        await Bun.sleep(100);
        return late;
      },
      onMessage: () => {},
      backoff: FAST,
      connectTimeoutMs: 30,
    });
    const running = supervisor.run();
    await until(() => supervisor.status === "connected");
    expect(supervisor.current()).toBe(replacement);
    await until(() => late.stopped);
    expect(supervisor.current()).toBe(replacement);
    expect(replacement.stopped).toBe(false);
    await supervisor.stop();
    await running;
  });

  test("stop() during a backoff wait ends run() promptly", async () => {
    const supervisor = new Supervisor({
      factory: async () => {
        throw new Error("down");
      },
      onMessage: () => {},
      backoff: { initialMs: 60_000, maxMs: 60_000, stableMs: 60_000 },
    });
    const running = supervisor.run();
    await Bun.sleep(20);
    const started = performance.now();
    await supervisor.stop();
    await running;
    expect(performance.now() - started).toBeLessThan(500);
  });
});
