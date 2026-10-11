import { afterAll, beforeEach, describe, expect, test } from "bun:test";
import { Bridge } from "../src/bridge";
import { CoreClient } from "../src/core-client";
import type { InboundPayload } from "../src/types";
import { bytesAttachment, captureLogs, FakeTransport, message, until } from "./helpers";

const SECRET = "localtest";

// A stub ferry-core on an OS-assigned port. Each test queues its responses.
type Responder = () => Response | Promise<Response>;
const received: { secret: string | null; path: string; body: InboundPayload }[] = [];
let responders: Responder[] = [];
const core = Bun.serve({
  port: 0,
  hostname: "127.0.0.1",
  fetch: async (req) => {
    received.push({ secret: req.headers.get("x-ferry-secret"), path: new URL(req.url).pathname, body: (await req.json()) as InboundPayload });
    const next = responders.shift();
    return next ? next() : Response.json({ replies: [] });
  },
});
afterAll(() => core.stop(true));

let transport: FakeTransport;
let bridge: Bridge;

function makeBridge(timeoutMs = 3_000): Bridge {
  return new Bridge({
    core: new CoreClient({ coreUrl: `http://127.0.0.1:${core.port}`, sharedSecret: SECRET, timeoutMs }),
    sidecarUrl: "http://127.0.0.1:8120",
    echo: false,
    agentPhone: "+19195550100",
    channels: ["imessage"],
    transport: () => transport,
  });
}

beforeEach(() => {
  received.length = 0;
  responders = [];
  transport = new FakeTransport();
  bridge = makeBridge();
});

describe("inbound forwarding", () => {
  test("posts the exact spec body with the secret, then sends core's reply", async () => {
    const space = transport.space("any;-;+19195550101");
    responders.push(() => Response.json({ replies: [{ space_id: "any;-;+19195550101", text: "Your burrito is 4 min away." }] }));

    await bridge.handleInbound(message(space, { text: "how long until my food gets here", sender: "+19195550101" }));

    expect(received).toHaveLength(1);
    const req = received[0];
    expect(req?.path).toBe("/internal/messages/inbound");
    expect(req?.secret).toBe(SECRET);
    // Every v1 field keeps its name and meaning; the Telegram-era fields are added.
    expect(Object.keys(req?.body ?? {}).sort()).toEqual(
      [
        "attachments",
        "channel",
        "chat_title",
        "is_group",
        "kind",
        "participants",
        "received_at",
        "sender_id",
        "sender_name",
        "sender_phone",
        "sender_username",
        "space_id",
        "text",
      ].sort(),
    );
    expect(req?.body).toEqual({
      kind: "message",
      channel: "imessage",
      space_id: "any;-;+19195550101",
      is_group: false,
      participants: ["+19195550101"],
      sender_phone: "+19195550101",
      sender_id: "+19195550101",
      sender_name: null,
      sender_username: null,
      chat_title: null,
      text: "how long until my food gets here",
      attachments: [],
      received_at: "2026-10-10T22:00:00.000Z",
    });
    expect(space.sent).toEqual(["Your burrito is 4 min away."]);
    // No typing indicator on the core path: core often stays silent in groups.
    expect(space.events).not.toContain("typing:start");
  });

  test("a group sends its members without the agent, plus location and attachments", async () => {
    const space = transport.space("any;+;chat1", true, ["+19195550101", "+19195550102", "+19195550100"]);
    await bridge.handleInbound(
      message(space, {
        text: "",
        sender: "+19195550102",
        attachments: [bytesAttachment("x", "image/heic", "IMG_1.heic")],
        location: { lat: 35.909152, lng: -79.046905 },
      }),
    );
    const body = received[0]?.body;
    expect(body?.is_group).toBe(true);
    expect(body?.participants).toEqual(["+19195550101", "+19195550102"]);
    expect(body?.location).toEqual({ lat: 35.909152, lng: -79.046905 });
    expect(body?.attachments).toHaveLength(1);
    expect(body?.attachments[0]?.type).toBe("image/heic");
    expect(body?.attachments[0]?.url).toMatch(/^http:\/\/127\.0\.0\.1:8120\/attachments\/[0-9a-f-]{36}$/);
  });

  test("a dedicated line's own number is dropped from participants, as is AGENT_PHONE_E164", async () => {
    // makeBridge sets AGENT_PHONE_E164 to +19195550100.
    const space = transport.space("any;+;chat4", true, ["+19195550101", "+19195550199", "+19195550100", "+19195550102"]);
    space.ownHandle = "+19195550199";
    await bridge.handleInbound(message(space, { sender: "+19195550102" }));
    expect(received[0]?.body.participants).toEqual(["+19195550101", "+19195550102"]);
  });

  test("on the shared pool the roster goes to core as Spectrum lists it, pool number included", async () => {
    // Spectrum cannot say which pool number serves the chat, so ownHandle is unset.
    const space = transport.space("any;+;chat5", true, ["+19195550101", "+19195550177"]);
    await bridge.handleInbound(message(space, { sender: "+19195550101" }));
    expect(received[0]?.body.participants).toEqual(["+19195550101", "+19195550177"]);
  });

  test("a sender or member whose handle is an email is forwarded unchanged", async () => {
    const dm = transport.space("any;-;maya.lopez@icloud.com");
    await bridge.handleInbound(message(dm, { sender: "maya.lopez@icloud.com" }));
    expect(received[0]?.body.sender_phone).toBe("maya.lopez@icloud.com");
    expect(received[0]?.body.participants).toEqual(["maya.lopez@icloud.com"]);

    const group = transport.space("any;+;chat6", true, ["+19195550101", "maya.lopez@icloud.com"]);
    await bridge.handleInbound(message(group, { sender: "maya.lopez@icloud.com" }));
    expect(received[1]?.body.sender_phone).toBe("maya.lopez@icloud.com");
    expect(received[1]?.body.participants).toEqual(["+19195550101", "maya.lopez@icloud.com"]);
  });

  test("a group whose members cannot be listed falls back to the sender", async () => {
    const space = transport.space("any;+;chat2", true);
    space.memberIds = async () => {
      throw new Error("unsupported");
    };
    await bridge.handleInbound(message(space, { sender: "+19195550103" }));
    expect(received[0]?.body.participants).toEqual(["+19195550103"]);
  });

  test("replies can target another space, known or not", async () => {
    const dm = transport.space("any;-;+19195550101");
    const group = transport.space("any;+;chat3", true, ["+19195550101"]);
    await bridge.handleInbound(message(group, { text: "ferry open a run" }));
    responders.push(() =>
      Response.json({
        replies: [
          { space_id: "any;+;chat3", text: "Run opened" },
          { space_id: "any;-;+19195550109", text: "You were added" },
          { space_id: "", text: "dropped" },
        ],
      }),
    );
    await bridge.handleInbound(message(dm, { text: "go" }));
    expect(group.sent).toEqual(["Run opened"]);
    expect(transport.spaces.get("any;-;+19195550109")?.sent).toEqual(["You were added"]);
  });

  test("retries once after a 5xx", async () => {
    const space = transport.space("any;-;+19195550101");
    responders.push(() => new Response("boom", { status: 503 }));
    responders.push(() => Response.json({ replies: [{ space_id: space.id, text: "ok" }] }));
    await bridge.handleInbound(message(space));
    expect(received).toHaveLength(2);
    expect(space.sent).toEqual(["ok"]);
  });

  test("retries once after a timeout", async () => {
    bridge = makeBridge(100);
    const space = transport.space("any;-;+19195550101");
    responders.push(async () => {
      await Bun.sleep(300);
      return Response.json({ replies: [{ space_id: space.id, text: "late" }] });
    });
    responders.push(() => Response.json({ replies: [{ space_id: space.id, text: "on time" }] }));
    await bridge.handleInbound(message(space));
    expect(received).toHaveLength(2);
    expect(space.sent).toEqual(["on time"]);
  });

  test("two failures send nothing and log masked", async () => {
    const space = transport.space("any;-;+19195550101");
    responders.push(() => new Response("", { status: 500 }));
    responders.push(() => new Response("", { status: 500 }));
    const logs = captureLogs();
    try {
      await bridge.handleInbound(message(space, { sender: "+19195550101" }));
    } finally {
      logs.restore();
    }
    expect(received).toHaveLength(2);
    expect(space.sent).toEqual([]);
    expect(logs.lines.some((line) => line.includes("core unavailable"))).toBe(true);
    for (const line of logs.lines) expect(line).not.toContain("5550101");
  });

  test("a 4xx is not retried", async () => {
    const space = transport.space("any;-;+19195550101");
    responders.push(() => new Response("", { status: 401 }));
    await bridge.handleInbound(message(space));
    expect(received).toHaveLength(1);
    expect(space.sent).toEqual([]);
  });

  test("core unreachable sends nothing", async () => {
    const offline = new Bridge({
      core: new CoreClient({ coreUrl: "http://127.0.0.1:9", sharedSecret: SECRET, timeoutMs: 200 }),
      sidecarUrl: "http://127.0.0.1:8120",
      echo: false,
      agentPhone: undefined,
      channels: ["imessage"],
      transport: () => transport,
    });
    const space = transport.space("any;-;+19195550101");
    await offline.handleInbound(message(space));
    expect(space.sent).toEqual([]);
  });
});

describe("dispatch", () => {
  test("returns at once and keeps per-space order", async () => {
    const space = transport.space("any;-;+19195550101");
    let release: () => void = () => {};
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    responders.push(async () => {
      await gate;
      return Response.json({ replies: [{ space_id: space.id, text: "first" }] });
    });
    responders.push(() => Response.json({ replies: [{ space_id: space.id, text: "second" }] }));

    const started = performance.now();
    bridge.dispatch(message(space, { text: "one" }));
    bridge.dispatch(message(space, { text: "two" }));
    expect(performance.now() - started).toBeLessThan(50);

    await until(() => received.length === 1);
    release();
    await bridge.idle();
    expect(received.map((r) => r.body.text)).toEqual(["one", "two"]);
    expect(space.sent).toEqual(["first", "second"]);
  });

  test("a task that hangs is cut off by its timeout", async () => {
    const hung = new Bridge({
      core: { forward: () => new Promise(() => {}) } as unknown as CoreClient,
      sidecarUrl: "http://127.0.0.1:8120",
      echo: false,
      agentPhone: undefined,
      channels: ["imessage"],
      transport: () => transport,
      taskTimeoutMs: 50,
    });
    const space = transport.space("any;-;+19195550101");
    hung.dispatch(message(space));
    await hung.idle();
    expect(space.sent).toEqual([]);
  });

  test("a task cut off by its timeout sends none of core's late replies, even after the next message's", async () => {
    const space = transport.space("any;-;+19195550101");
    const slow = new Bridge({
      core: {
        forward: async (payload: InboundPayload) => {
          if (payload.text === "one") {
            await Bun.sleep(150);
            return [{ space_id: space.id, text: "late" }];
          }
          return [{ space_id: space.id, text: "second" }];
        },
      } as unknown as CoreClient,
      sidecarUrl: "http://127.0.0.1:8120",
      echo: false,
      agentPhone: undefined,
      channels: ["imessage"],
      transport: () => transport,
      taskTimeoutMs: 50,
    });
    slow.dispatch(message(space, { text: "one" }));
    slow.dispatch(message(space, { text: "two" }));
    await slow.idle();
    await Bun.sleep(200); // past the moment core's late answer arrives
    expect(space.sent).toEqual(["second"]);
  });

  test("a timeout between two replies stops the replies not yet sent", async () => {
    const space = transport.space("any;-;+19195550101");
    const sendText = space.sendText.bind(space);
    space.sendText = async (text) => {
      await Bun.sleep(100);
      await sendText(text);
    };
    const cut = new Bridge({
      core: {
        forward: async () => [
          { space_id: space.id, text: "r1" },
          { space_id: space.id, text: "r2" },
        ],
      } as unknown as CoreClient,
      sidecarUrl: "http://127.0.0.1:8120",
      echo: false,
      agentPhone: undefined,
      channels: ["imessage"],
      transport: () => transport,
      taskTimeoutMs: 50,
    });
    const logs = captureLogs();
    try {
      cut.dispatch(message(space));
      await cut.idle();
      await Bun.sleep(250);
    } finally {
      logs.restore();
    }
    // r1 was already in flight when the timeout hit; r2 never starts.
    expect(space.sent).toEqual(["r1"]);
    expect(logs.lines.some((line) => line.includes("replies not sent") && line.includes("skipped=1"))).toBe(true);
  });

  test("the timeout also cancels the call to core, so it is not retried", async () => {
    const space = transport.space("any;-;+19195550101");
    responders.push(async () => {
      await Bun.sleep(300);
      return new Response("", { status: 503 });
    });
    responders.push(() => Response.json({ replies: [{ space_id: space.id, text: "too late" }] }));
    const cut = new Bridge({
      core: new CoreClient({ coreUrl: `http://127.0.0.1:${core.port}`, sharedSecret: SECRET }),
      sidecarUrl: "http://127.0.0.1:8120",
      echo: false,
      agentPhone: undefined,
      channels: ["imessage"],
      transport: () => transport,
      taskTimeoutMs: 100,
    });
    cut.dispatch(message(space));
    await cut.idle();
    await Bun.sleep(500); // past the moment core's 503 would have led to a retry
    expect(received).toHaveLength(1);
    expect(space.sent).toEqual([]);
  });

  test("a space queues at most 20 waiting messages and drops the oldest, logged masked", async () => {
    const space = transport.space("any;-;+19195550101");
    let release: () => void = () => {};
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const forwarded: string[] = [];
    const gated = new Bridge({
      core: {
        forward: async (payload: InboundPayload) => {
          forwarded.push(payload.text);
          if (payload.text === "m0") await gate;
          return [];
        },
      } as unknown as CoreClient,
      sidecarUrl: "http://127.0.0.1:8120",
      echo: false,
      agentPhone: undefined,
      channels: ["imessage"],
      transport: () => transport,
    });
    const other = transport.space("any;-;+19195550102");
    const logs = captureLogs();
    try {
      gated.dispatch(message(space, { text: "m0" }));
      await until(() => forwarded.length === 1);
      for (let i = 1; i <= 24; i++) gated.dispatch(message(space, { text: `m${i}`, sender: "+19195550101" }));
      // Another space is not held up by the full one.
      gated.dispatch(message(other, { text: "elsewhere" }));
      await until(() => forwarded.includes("elsewhere"));
      release();
      await gated.idle();
    } finally {
      logs.restore();
    }
    expect(forwarded.filter((text) => text !== "elsewhere")).toEqual(["m0", ...Array.from({ length: 20 }, (_, i) => `m${i + 5}`)]);
    expect(logs.lines.filter((line) => line.includes("dropped the oldest"))).toHaveLength(4);
    for (const line of logs.lines) expect(line).not.toContain("9195550101");
  });
});
