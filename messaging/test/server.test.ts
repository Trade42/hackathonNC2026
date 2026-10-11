import { afterAll, beforeEach, describe, expect, test } from "bun:test";
import { Bridge } from "../src/bridge";
import { CoreClient } from "../src/core-client";
import { createHandler, MAX_TEXT, startServer } from "../src/server";
import type { SpectrumStatus } from "../src/supervisor";
import type { InboundAttachment } from "../src/types";
import { bytesAttachment, captureLogs, FakeTransport, message } from "./helpers";

const SECRET = "localtest";
const AUTH = { "x-ferry-secret": SECRET };
const PHONE = "+19195550123";

/** A handler whose every attachment id serves this one attachment. */
function attachmentHandler(attachment: InboundAttachment): (req: Request) => Promise<Response> {
  return createHandler({
    sharedSecret: SECRET,
    status: () => "connected",
    channels: ["imessage"],
    bridge: {
      send: async () => ({ space_id: "s" }),
      createGroup: async () => ({ space_id: "g" }),
      createInviteLink: async () => ({ url: "u" }),
      attachment: () => attachment,
    },
  });
}

/** Sends 3 bytes, then fails with the phone in its error, as a gRPC download error naming a chat might. */
function failingAttachment(phone: string): InboundAttachment {
  return {
    mimeType: "image/jpeg",
    name: "photo.jpg",
    stream: async () =>
      new ReadableStream<Uint8Array>({
        start: (controller) => controller.enqueue(new TextEncoder().encode("JPE")),
        pull: () => {
          throw new Error(`attachment download failed in chat any;-;${phone}`);
        },
      }),
  };
}

let transport: FakeTransport | undefined;
let status: SpectrumStatus;
let bridge: Bridge;
let handle: (req: Request) => Promise<Response>;

beforeEach(() => {
  transport = new FakeTransport();
  status = "connected";
  bridge = new Bridge({
    // Never called here: these tests do not forward inbound messages.
    core: new CoreClient({ coreUrl: "http://127.0.0.1:9", sharedSecret: SECRET }),
    sidecarUrl: "http://127.0.0.1:8120",
    echo: false,
    agentPhone: undefined,
    channels: ["imessage"],
    transport: () => transport,
  });
  handle = createHandler({ sharedSecret: SECRET, bridge, status: () => status, channels: ["imessage"] });
});

function call(method: string, path: string, body?: unknown, headers: Record<string, string> = AUTH): Promise<Response> {
  const init: RequestInit = { method, headers: { ...headers, "content-type": "application/json" } };
  if (body !== undefined) init.body = typeof body === "string" ? body : JSON.stringify(body);
  return handle(new Request(`http://sidecar${path}`, init));
}

describe("auth", () => {
  const routes: [string, string][] = [
    ["GET", "/healthz"],
    ["POST", "/send"],
    ["POST", "/groups"],
    ["GET", "/attachments/x"],
    ["GET", "/nope"],
  ];

  for (const [method, path] of routes) {
    test(`${method} ${path} without the header is 401`, async () => {
      const res = await call(method, path, method === "POST" ? { text: "hi", space_id: "s" } : undefined, {});
      expect(res.status).toBe(401);
      expect(((await res.json()) as { error: { code: string } }).error.code).toBe("unauthorized");
    });

    test(`${method} ${path} with a wrong secret is 401`, async () => {
      const res = await call(method, path, undefined, { "x-ferry-secret": "localtest-but-longer" });
      expect(res.status).toBe(401);
    });
  }

  test("a secret that differs only in its last byte is 401", async () => {
    expect((await call("GET", "/healthz", undefined, { "x-ferry-secret": "localtesT" })).status).toBe(401);
  });

  test("an unknown route with the secret is 404", async () => {
    expect((await call("GET", "/nope")).status).toBe(404);
  });
});

describe("GET /healthz", () => {
  test("reports the Spectrum state", async () => {
    for (const state of ["connected", "disconnected", "disabled"] as const) {
      status = state;
      const res = await call("GET", "/healthz");
      expect(res.status).toBe(200);
      expect(await res.json()).toEqual({ status: "ok", spectrum: state, providers: ["imessage"] });
    }
  });

  test("POST is 405", async () => {
    expect((await call("POST", "/healthz", {})).status).toBe(405);
  });
});

describe("POST /send", () => {
  const bad: [string, unknown][] = [
    ["not JSON", "{"],
    ["an array", ["hi"]],
    ["no text", { space_id: "any;-;+19195550101" }],
    ["empty text", { space_id: "any;-;+19195550101", text: "  " }],
    ["text not a string", { space_id: "any;-;+19195550101", text: 5 }],
    ["text too long", { space_id: "any;-;+19195550101", text: "x".repeat(MAX_TEXT + 1) }],
    ["neither target", { text: "hi" }],
    ["both targets", { text: "hi", space_id: "s", to_phones: ["+19195550101"] }],
    ["space_id and to_user_ids", { text: "hi", space_id: "s", to_user_ids: ["123456789"] }],
    ["empty space_id", { text: "hi", space_id: "" }],
    ["to_phones not an array", { text: "hi", to_phones: "+19195550101" }],
    ["to_phones empty", { text: "hi", to_phones: [] }],
    ["to_phones not E.164", { text: "hi", to_phones: ["919-555-0101"] }],
    ["to_phones duplicates", { text: "hi", to_phones: ["+19195550101", "+19195550101"] }],
  ];

  for (const [name, body] of bad) {
    test(`${name} is 400`, async () => {
      const res = await call("POST", "/send", body);
      expect(res.status).toBe(400);
      expect(((await res.json()) as { error: { code: string } }).error.code).toBe("invalid_request");
    });
  }

  test("sends into a known space_id", async () => {
    const res = await call("POST", "/send", { space_id: "any;+;chat9", text: "Ferry to Cosmic Cantina is open" });
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ space_id: "any;+;chat9" });
    expect(transport?.spaces.get("any;+;chat9")?.sent).toEqual(["Ferry to Cosmic Cantina is open"]);
  });

  test("to_phones with one phone opens a DM and returns its space_id", async () => {
    const res = await call("POST", "/send", { to_phones: ["+19195550101"], text: "welcome" });
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ space_id: "any;-;+19195550101" });
  });

  test("to_phones with several phones is 501 on a shared line", async () => {
    const res = await call("POST", "/send", { to_phones: ["+19195550101", "+19195550102"], text: "hi" });
    expect(res.status).toBe(501);
  });

  test("503 while Spectrum is not connected", async () => {
    transport = undefined;
    const res = await call("POST", "/send", { space_id: "s", text: "hi" });
    expect(res.status).toBe(503);
    expect(((await res.json()) as { error: { code: string } }).error.code).toBe("spectrum_unavailable");
  });

  test("502 when the platform send fails", async () => {
    (transport as FakeTransport).space("any;-;+19195550101").failSend = true;
    const res = await call("POST", "/send", { space_id: "any;-;+19195550101", text: "hi" });
    expect(res.status).toBe(502);
  });

  test("502 within the send timeout when Photon never answers", async () => {
    const t = transport as FakeTransport;
    const slow = new Bridge({
      core: new CoreClient({ coreUrl: "http://127.0.0.1:9", sharedSecret: SECRET }),
      sidecarUrl: "http://127.0.0.1:8120",
      echo: false,
      agentPhone: undefined,
      channels: ["imessage"],
      transport: () => t,
      sendTimeoutMs: 50,
    });
    const h = createHandler({ sharedSecret: SECRET, bridge: slow, status: () => "connected", channels: ["imessage"] });
    t.space("any;-;+19195550101").sendText = () => new Promise(() => {});
    const post = (path: string, body: unknown) =>
      h(new Request(`http://sidecar${path}`, { method: "POST", headers: AUTH, body: JSON.stringify(body) }));

    const started = performance.now();
    const res = await post("/send", { space_id: "any;-;+19195550101", text: "hi" });
    expect(res.status).toBe(502);
    expect(((await res.json()) as { error: { code: string } }).error.code).toBe("send_failed");
    expect(performance.now() - started).toBeLessThan(1_000);

    t.createSpace = () => new Promise(() => {});
    expect((await post("/send", { to_phones: ["+19195550102"], text: "hi" })).status).toBe(502);
    expect((await post("/groups", { phones: ["+19195550101", "+19195550102"], name: "n", intro_text: "hi" })).status).toBe(502);
  });
});

describe("POST /groups", () => {
  const good = { phones: ["+19195550101", "+19195550102"], name: "Davis Library crew", intro_text: "Hi all, Ferry here." };

  const bad: [string, unknown][] = [
    ["not JSON", "nope"],
    ["no phones", { name: "n", intro_text: "i" }],
    ["one phone", { ...good, phones: ["+19195550101"] }],
    ["a bad phone", { ...good, phones: ["+19195550101", "5550102"] }],
    ["no name", { phones: good.phones, intro_text: "i" }],
    ["empty name", { ...good, name: "" }],
    ["name too long", { ...good, name: "x".repeat(101) }],
    ["no intro_text", { phones: good.phones, name: "n" }],
  ];

  for (const [name, body] of bad) {
    test(`${name} is 400`, async () => {
      expect((await call("POST", "/groups", body)).status).toBe(400);
    });
  }

  test("501 when the line cannot create groups", async () => {
    const res = await call("POST", "/groups", good);
    expect(res.status).toBe(501);
    expect(((await res.json()) as { error: { code: string } }).error.code).toBe("unsupported");
  });

  test("creates, names and greets the group on a dedicated line", async () => {
    if (transport) transport.canCreateGroups = true;
    const res = await call("POST", "/groups", good);
    expect(res.status).toBe(200);
    const { space_id } = (await res.json()) as { space_id: string };
    const space = transport?.spaces.get(space_id);
    expect(space?.renamed).toBe("Davis Library crew");
    expect(space?.sent).toEqual(["Hi all, Ferry here."]);
  });

  test("503 while Spectrum is not connected", async () => {
    transport = undefined;
    expect((await call("POST", "/groups", good)).status).toBe(503);
  });
});

describe("GET /attachments/:id", () => {
  test("serves an attachment core was given, and 404s an unknown one", async () => {
    const t = transport as FakeTransport;
    const space = t.space("any;-;+19195550101");
    let forwardedUrl = "";
    const capture = new Bridge({
      core: {
        forward: async (payload: { attachments: { url: string }[] }) => {
          forwardedUrl = payload.attachments[0]?.url ?? "";
          return [];
        },
      } as unknown as CoreClient,
      sidecarUrl: "http://127.0.0.1:8120",
      echo: false,
      agentPhone: undefined,
      channels: ["imessage"],
      transport: () => t,
    });
    await capture.handleInbound(message(space, { text: "", attachments: [bytesAttachment("JPEGDATA")] }));
    expect(forwardedUrl).toStartWith("http://127.0.0.1:8120/attachments/");

    const h = createHandler({ sharedSecret: SECRET, bridge: capture, status: () => "connected", channels: ["imessage"] });
    const path = new URL(forwardedUrl).pathname;
    const res = await h(new Request(`http://sidecar${path}`, { headers: AUTH }));
    expect(res.status).toBe(200);
    expect(res.headers.get("content-type")).toBe("image/jpeg");
    expect(await res.text()).toBe("JPEGDATA");

    expect((await h(new Request("http://sidecar/attachments/unknown", { headers: AUTH }))).status).toBe(404);
    expect((await h(new Request(`http://sidecar${path}`))).status).toBe(401);
  });

  test("a stream that fails mid-body errors the response with a generic message and logs masked", async () => {
    const h = attachmentHandler(failingAttachment(PHONE));
    const logs = captureLogs();
    let readError: unknown;
    try {
      const res = await h(new Request("http://sidecar/attachments/x", { headers: AUTH }));
      expect(res.status).toBe(200);
      await res.text().catch((err) => {
        readError = err;
      });
    } finally {
      logs.restore();
    }
    expect(readError).toBeInstanceOf(Error);
    expect((readError as Error).message).toBe("attachment stream failed");
    expect((readError as Error).stack ?? "").not.toContain("5550123");
    expect(logs.lines.some((line) => line.includes("attachment stream failed") && line.includes("+1•••0123"))).toBe(true);
    for (const line of logs.lines) expect(line).not.toContain("9195550123");
  });

  test("a client that hangs up cancels the platform stream", async () => {
    let cancelled: unknown = "not cancelled";
    const h = attachmentHandler({
      mimeType: "image/jpeg",
      name: "photo.jpg",
      stream: async () =>
        new ReadableStream<Uint8Array>({
          pull: (controller) => controller.enqueue(new Uint8Array(8)),
          cancel: (reason) => {
            cancelled = reason;
          },
        }),
    });
    const res = await h(new Request("http://sidecar/attachments/x", { headers: AUTH }));
    const reader = (res.body as ReadableStream<Uint8Array>).getReader();
    await reader.read();
    await reader.cancel("client gone");
    expect(cancelled).toBe("client gone");
  });

  test("over real HTTP, a mid-body failure resets the connection and never prints the phone", async () => {
    // Bun prints a failed body stream's error to the process's own stderr, so the
    // server runs in a child process and the test reads everything it printed.
    const child = Bun.spawn([process.execPath, "--no-env-file", `${import.meta.dir}/fixtures/attachment-server.ts`, PHONE], {
      stdout: "pipe",
      stderr: "pipe",
    });
    const stdout = (child.stdout as ReadableStream<Uint8Array>).getReader();
    let printed = "";
    let clientSaw = "";
    try {
      while (!printed.includes("\n")) {
        const chunk = await stdout.read();
        if (chunk.done) break;
        printed += new TextDecoder().decode(chunk.value);
      }
      const { port } = JSON.parse(printed.split("\n")[0] ?? "") as { port: number };
      try {
        const res = await fetch(`http://127.0.0.1:${port}/attachments/broken`, { headers: AUTH });
        clientSaw = `status ${res.status}`;
        const body = await res.text();
        clientSaw += `, complete body of ${body.length} bytes`;
      } catch (err) {
        clientSaw += ` then ${(err as Error).name}`;
      }
      await Bun.sleep(100);
    } finally {
      child.kill();
      await child.exited;
    }
    for (let chunk = await stdout.read(); !chunk.done; chunk = await stdout.read()) printed += new TextDecoder().decode(chunk.value);
    const output = printed + (await new Response(child.stderr as ReadableStream<Uint8Array>).text());

    expect(clientSaw).not.toContain("complete body");
    expect(output).toContain("attachment stream failed");
    expect(output).toContain("+1•••0123");
    expect(output).not.toContain("9195550123");
  });
});

describe("Bun.serve wiring", () => {
  const server = startServer(
    {
      sharedSecret: SECRET,
      bridge: {
        send: async () => ({ space_id: "s" }),
        createGroup: async () => ({ space_id: "g" }),
        createInviteLink: async () => ({ url: "u" }),
        attachment: () => undefined,
      },
      status: () => "disabled",
      channels: [],
    },
    { port: 0, hostname: "127.0.0.1" },
  );
  afterAll(() => server.stop(true));

  test("healthz over real HTTP", async () => {
    const base = `http://127.0.0.1:${server.port}`;
    expect((await fetch(`${base}/healthz`)).status).toBe(401);
    const res = await fetch(`${base}/healthz`, { headers: AUTH });
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ status: "ok", spectrum: "disabled", providers: [] });
  });
});
