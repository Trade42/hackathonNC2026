// The bridge and the internal API on Telegram, with the fake Transport.

import { afterAll, beforeEach, describe, expect, test } from "bun:test";
import { Bridge } from "../src/bridge";
import { CoreClient } from "../src/core-client";
import { createHandler } from "../src/server";
import { ChatMigratedError, NotAdminError, RecipientUnreachableError, type Channel, type InboundPayload } from "../src/types";
import { captureLogs, FakeSpace, FakeTransport, message, until, type MessageInit } from "./helpers";

const SECRET = "localtest";
const AUTH = { "x-ferry-secret": SECRET };
const USER = "987654321";
const GROUP = "-1001234567890";

// A stub ferry-core on an OS-assigned port. Each test queues its responses.
const received: InboundPayload[] = [];
let responders: (() => Response)[] = [];
const core = Bun.serve({
  port: 0,
  hostname: "127.0.0.1",
  fetch: async (req) => {
    received.push((await req.json()) as InboundPayload);
    return responders.shift()?.() ?? Response.json({ replies: [] });
  },
});
afterAll(() => core.stop(true));

let transport: FakeTransport | undefined;

function makeBridge(channels: Channel[] = ["telegram"], echo = false): Bridge {
  return new Bridge({
    core: new CoreClient({ coreUrl: `http://127.0.0.1:${core.port}`, sharedSecret: SECRET }),
    sidecarUrl: "http://127.0.0.1:8120",
    echo,
    agentPhone: undefined,
    channels,
    transport: () => transport,
  });
}

/** A Telegram inbound event from Maya. */
function tg(space: FakeSpace, init: MessageInit = {}) {
  return message(space, { channel: "telegram", sender: USER, senderName: "Maya", senderUsername: "maya_l", ...init });
}

function post(bridge: Bridge, path: string, body: unknown, channels: Channel[] = ["telegram"]): Promise<Response> {
  const handle = createHandler({ sharedSecret: SECRET, bridge, status: () => (transport ? "connected" : "disconnected"), channels });
  return handle(new Request(`http://sidecar${path}`, { method: "POST", headers: AUTH, body: JSON.stringify(body) }));
}

async function errorCode(res: Response): Promise<string> {
  return ((await res.json()) as { error: { code: string } }).error.code;
}

beforeEach(() => {
  received.length = 0;
  responders = [];
  transport = new FakeTransport();
});

describe("inbound payload on Telegram", () => {
  test("a DM: sender_phone is null, sender_id is the user id, names and kind are added", async () => {
    const space = (transport as FakeTransport).space(USER);
    await makeBridge().handleInbound(tg(space, { text: "/start Xy7_k2" }));
    expect(received[0]).toEqual({
      kind: "message",
      channel: "telegram",
      space_id: USER,
      is_group: false,
      participants: [USER],
      sender_phone: null,
      sender_id: USER,
      sender_name: "Maya",
      sender_username: "maya_l",
      chat_title: null,
      text: "/start Xy7_k2",
      attachments: [],
      received_at: "2026-10-10T22:00:00.000Z",
    });
  });

  test("a group: participants is the sender (the Bot API cannot list members), with the title", async () => {
    const space = (transport as FakeTransport).space(GROUP, true);
    await makeBridge().handleInbound(tg(space, { text: "heading to Sup Dogs at 6", chatTitle: "Hinton James 4th floor" }));
    expect(received[0]).toMatchObject({ is_group: true, space_id: GROUP, participants: [USER], chat_title: "Hinton James 4th floor" });
  });

  test("a button press: kind callback with callback_data and empty text", async () => {
    const space = (transport as FakeTransport).space(USER);
    await makeBridge().handleInbound(tg(space, { kind: "callback", text: "", callbackData: "invite:acc:42" }));
    expect(received[0]).toMatchObject({ kind: "callback", callback_data: "invite:acc:42", text: "", sender_id: USER });
  });

  test("membership: the bot added, made admin, or removed; and a group upgrade", async () => {
    const space = (transport as FakeTransport).space(GROUP, true);
    const bridge = makeBridge();
    await bridge.handleInbound(tg(space, { kind: "membership", text: "", membership: { inChat: true, isAdmin: false } }));
    await bridge.handleInbound(tg(space, { kind: "membership", text: "", membership: { inChat: true, isAdmin: true } }));
    await bridge.handleInbound(tg(space, { kind: "membership", text: "", membership: { inChat: false, isAdmin: false } }));
    await bridge.handleInbound(tg(space, { kind: "membership", text: "", membership: { inChat: true, isAdmin: null, migratedFrom: "-4001" } }));
    expect(received.map((p) => [p.kind, p.bot_in_chat, p.bot_is_admin, p.migrated_from_space_id])).toEqual([
      ["membership", true, false, undefined],
      ["membership", true, true, undefined],
      ["membership", false, false, undefined],
      ["membership", true, null, "-4001"],
    ]);
    expect("callback_data" in (received[0] as object)).toBe(false);
  });

  test("core's replies go out as plain sends into Telegram chats", async () => {
    const space = (transport as FakeTransport).space(GROUP, true);
    responders.push(() => Response.json({ replies: [{ space_id: GROUP, text: "Ferry to Sup Dogs: orders close 6:10" }, { space_id: USER, text: "You're in" }] }));
    await makeBridge().handleInbound(tg(space, { text: "heading to Sup Dogs at 6" }));
    expect(space.sent).toEqual(["Ferry to Sup Dogs: orders close 6:10"]);
    expect(transport?.spaces.get(USER)?.sent).toEqual(["You're in"]);
  });

  test("logs mask the user id and never carry names or usernames", async () => {
    const space = (transport as FakeTransport).space(USER);
    responders.push(() => new Response("", { status: 500 }));
    responders.push(() => new Response("", { status: 500 }));
    const logs = captureLogs();
    try {
      await makeBridge().handleInbound(tg(space));
    } finally {
      logs.restore();
    }
    expect(logs.lines.some((line) => line.includes("•••321"))).toBe(true);
    for (const line of logs.lines) {
      expect(line).not.toContain(USER);
      expect(line).not.toContain("Maya");
      expect(line).not.toContain("maya_l");
    }
  });
});

describe("button presses are answered at once", () => {
  test("dispatch answers the callback before the space's queue gets to it", async () => {
    const space = (transport as FakeTransport).space(USER);
    const order: string[] = [];
    let release: () => void = () => {};
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const bridge = new Bridge({
      core: {
        forward: async (payload: InboundPayload) => {
          order.push(`core:${payload.kind}`);
          if (payload.kind === "message") await gate;
          return [];
        },
      } as unknown as CoreClient,
      sidecarUrl: "http://127.0.0.1:8120",
      echo: false,
      agentPhone: undefined,
      channels: ["telegram"],
      transport: () => transport,
    });
    bridge.dispatch(tg(space, { text: "slow one" }));
    await until(() => order.length === 1);
    bridge.dispatch(
      tg(space, {
        kind: "callback",
        callbackData: "inv:acc:1",
        acknowledge: async () => {
          order.push("ack");
        },
      }),
    );
    // Answered while the earlier message still holds the queue.
    await until(() => order.includes("ack"));
    expect(order).toEqual(["core:message", "ack"]);
    release();
    await bridge.idle();
    expect(order).toEqual(["core:message", "ack", "core:callback"]);
  });

  test("a failed answer is logged and the press still reaches core", async () => {
    const space = (transport as FakeTransport).space(USER);
    const bridge = makeBridge();
    const logs = captureLogs();
    try {
      bridge.dispatch(
        tg(space, {
          kind: "callback",
          callbackData: "inv:acc:1",
          acknowledge: async () => {
            throw new Error("query is too old");
          },
        }),
      );
      await bridge.idle();
      await until(() => logs.lines.some((line) => line.includes("callback answer failed")));
    } finally {
      logs.restore();
    }
    expect(received[0]?.kind).toBe("callback");
  });
});

describe("echo mode on Telegram", () => {
  test("a message is echoed with typing; a press names its data; a join, a promotion and an upgrade say so; a removal is only logged", async () => {
    const t = transport as FakeTransport;
    const dm = t.space(USER);
    const grp = t.space(GROUP, true);
    const bridge = makeBridge(["telegram"], true);
    await bridge.handleInbound(tg(dm, { text: "hi ferry" }));
    await bridge.handleInbound(tg(dm, { kind: "callback", callbackData: "inv:acc:1", text: "" }));
    await bridge.handleInbound(tg(grp, { kind: "membership", text: "", membership: { inChat: true, isAdmin: false } }));
    await bridge.handleInbound(tg(grp, { kind: "membership", text: "", membership: { inChat: true, isAdmin: true } }));
    await bridge.handleInbound(tg(grp, { kind: "membership", text: "", membership: { inChat: true, isAdmin: null, migratedFrom: "-4001" } }));
    await bridge.handleInbound(tg(grp, { kind: "membership", text: "", membership: { inChat: false, isAdmin: false } }));
    expect(dm.events).toEqual(["typing:start", "reply:echo: hi ferry", "typing:stop", "reply:echo: button inv:acc:1"]);
    expect(grp.sent).toEqual(["echo: Ferry joined this group", "echo: Ferry is an admin in this group", "echo: this group moved to a new chat id"]);
    expect(received).toEqual([]);
  });
});

describe("POST /send on Telegram", () => {
  test("space_id: a group chat id, with link and callback buttons", async () => {
    const bridge = makeBridge();
    const buttons = [
      { label: "Order", url: "https://ferry.example/r/run_1" },
      { label: "I'm in", callback_data: "run:join:run_1" },
    ];
    const res = await post(bridge, "/send", { space_id: GROUP, text: "Ferry to Sup Dogs is open", buttons });
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ space_id: GROUP });
    expect(transport?.spaces.get(GROUP)?.sent).toEqual(["Ferry to Sup Dogs is open"]);
    expect(transport?.spaces.get(GROUP)?.buttons).toEqual([buttons]);
  });

  test("to_user_ids: one user's DM, whose chat id is the user id (numbers accepted too)", async () => {
    const bridge = makeBridge();
    const res = await post(bridge, "/send", { to_user_ids: [USER], text: "Maya invited you to meet again" });
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ space_id: USER });
    expect(transport?.spaces.get(USER)?.sent).toEqual(["Maya invited you to meet again"]);
    expect((await post(bridge, "/send", { to_user_ids: [555000111], text: "hi" })).status).toBe(200);
    expect(transport?.spaces.get("555000111")?.sent).toEqual(["hi"]);
  });

  test("to_user_ids with several users is 501: bots cannot make group chats", async () => {
    const res = await post(makeBridge(), "/send", { to_user_ids: [USER, "555000111"], text: "hi" });
    expect(res.status).toBe(501);
    expect(await errorCode(res)).toBe("unsupported");
  });

  test("a user who never pressed Start (or blocked the bot) is 403 recipient_not_reachable, for to_user_ids and space_id", async () => {
    const bridge = makeBridge();
    (transport as FakeTransport).space(USER).failWith = new RecipientUnreachableError("Forbidden: bot can't initiate conversation with a user");
    const byUser = await post(bridge, "/send", { to_user_ids: [USER], text: "hi" });
    expect(byUser.status).toBe(403);
    expect(await errorCode(byUser)).toBe("recipient_not_reachable");
    const bySpace = await post(bridge, "/send", { space_id: USER, text: "hi" });
    expect(bySpace.status).toBe(403);
    expect(await errorCode(bySpace)).toBe("recipient_not_reachable");
  });

  test("a group that became a supergroup is 409 chat_migrated with the new id", async () => {
    (transport as FakeTransport).space("-4001", true).failWith = new ChatMigratedError("-1004001", "group chat was upgraded to a supergroup chat");
    const res = await post(makeBridge(), "/send", { space_id: "-4001", text: "hi" });
    expect(res.status).toBe(409);
    expect(await res.json()).toEqual({
      error: { code: "chat_migrated", message: "the group became a supergroup with a new chat id: use details.space_id", details: { space_id: "-1004001" } },
    });
  });

  test("to_phones on a Telegram-only sidecar is 400 phones_unsupported", async () => {
    const res = await post(makeBridge(), "/send", { to_phones: ["+19195550101"], text: "hi" });
    expect(res.status).toBe(400);
    expect(await errorCode(res)).toBe("phones_unsupported");
  });

  test("an iMessage space_id on a Telegram-only sidecar, or a Telegram one on an iMessage-only sidecar, is 400 channel_disabled", async () => {
    const tgOnly = await post(makeBridge(), "/send", { space_id: "any;-;+19195550101", text: "hi" });
    expect(tgOnly.status).toBe(400);
    expect(await errorCode(tgOnly)).toBe("channel_disabled");
    const imOnly = await post(makeBridge(["imessage"]), "/send", { to_user_ids: [USER], text: "hi" }, ["imessage"]);
    expect(imOnly.status).toBe(400);
    expect(await errorCode(imOnly)).toBe("channel_disabled");
  });

  test("with both providers, ids route by shape, and buttons on an iMessage target are 400", async () => {
    const both = makeBridge(["telegram", "imessage"]);
    expect((await post(both, "/send", { space_id: "any;+;chat9", text: "hi" }, ["telegram", "imessage"])).status).toBe(200);
    expect((await post(both, "/send", { space_id: GROUP, text: "hi" }, ["telegram", "imessage"])).status).toBe(200);
    expect(transport?.spaces.get("any;+;chat9")?.sent).toEqual(["hi"]);
    expect(transport?.spaces.get(GROUP)?.sent).toEqual(["hi"]);
    const withButtons = await post(both, "/send", { space_id: "any;+;chat9", text: "hi", buttons: [{ label: "Go", url: "https://x.example" }] });
    expect(withButtons.status).toBe(400);
    expect(await errorCode(withButtons)).toBe("buttons_unsupported");
  });

  test("text over Telegram's 4096 characters is 400", async () => {
    const res = await post(makeBridge(), "/send", { space_id: USER, text: "x".repeat(4097) });
    expect(res.status).toBe(400);
    expect(await errorCode(res)).toBe("invalid_request");
  });

  test("503 while Spectrum is not connected", async () => {
    transport = undefined;
    const res = await post(makeBridge(), "/send", { to_user_ids: [USER], text: "hi" });
    expect(res.status).toBe(503);
    expect(await errorCode(res)).toBe("spectrum_unavailable");
  });

  const bad: [string, unknown][] = [
    ["to_user_ids empty", { to_user_ids: [], text: "hi" }],
    ["to_user_ids not ids", { to_user_ids: ["@maya_l"], text: "hi" }],
    ["to_user_ids negative (a group)", { to_user_ids: ["-100123"], text: "hi" }],
    ["to_user_ids duplicates", { to_user_ids: [USER, Number(USER)], text: "hi" }],
    ["buttons empty", { space_id: USER, text: "hi", buttons: [] }],
    ["buttons not objects", { space_id: USER, text: "hi", buttons: ["Go"] }],
    ["a button without a label", { space_id: USER, text: "hi", buttons: [{ url: "https://x.example" }] }],
    ["a button with both url and callback_data", { space_id: USER, text: "hi", buttons: [{ label: "Go", url: "https://x.example", callback_data: "x" }] }],
    ["a button with neither", { space_id: USER, text: "hi", buttons: [{ label: "Go" }] }],
    ["a button url that is not http(s) or tg", { space_id: USER, text: "hi", buttons: [{ label: "Go", url: "javascript:alert(1)" }] }],
    ["callback_data over 64 bytes", { space_id: USER, text: "hi", buttons: [{ label: "Go", callback_data: "é".repeat(33) }] }],
    ["nine buttons", { space_id: USER, text: "hi", buttons: Array.from({ length: 9 }, (_, i) => ({ label: `b${i}`, callback_data: `${i}` })) }],
  ];
  for (const [name, body] of bad) {
    test(`${name} is 400 invalid_request`, async () => {
      const res = await post(makeBridge(), "/send", body);
      expect(res.status).toBe(400);
      expect(await errorCode(res)).toBe("invalid_request");
    });
  }
});

describe("POST /groups and POST /invite-links on Telegram", () => {
  test("/groups is 501: Telegram bots cannot create groups", async () => {
    const res = await post(makeBridge(), "/groups", { phones: ["+19195550101", "+19195550102"], name: "n", intro_text: "hi" });
    expect(res.status).toBe(501);
    expect(await errorCode(res)).toBe("unsupported");
  });

  test("/invite-links returns the link for a group the bot administers", async () => {
    const res = await post(makeBridge(), "/invite-links", { space_id: GROUP, name: "Hinton James" });
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ url: "https://t.me/+invite1" });
    expect(transport?.inviteLinks).toEqual([{ spaceId: GROUP, name: "Hinton James" }]);
  });

  test("/invite-links where the bot is not an admin is 403 not_admin", async () => {
    (transport as FakeTransport).inviteError = new NotAdminError("Bad Request: not enough rights to manage chat invite links");
    const res = await post(makeBridge(), "/invite-links", { space_id: GROUP });
    expect(res.status).toBe(403);
    expect(await errorCode(res)).toBe("not_admin");
  });

  test("/invite-links refuses a DM id, a bad name, iMessage-only sidecars, and a missing connection", async () => {
    const dmRes = await post(makeBridge(), "/invite-links", { space_id: USER });
    expect(dmRes.status).toBe(400);
    expect(await errorCode(dmRes)).toBe("invalid_request");
    expect((await post(makeBridge(), "/invite-links", { space_id: GROUP, name: "x".repeat(33) })).status).toBe(400);
    expect((await post(makeBridge(), "/invite-links", {})).status).toBe(400);
    const imOnly = await post(makeBridge(["imessage"]), "/invite-links", { space_id: GROUP }, ["imessage"]);
    expect(await errorCode(imOnly)).toBe("channel_disabled");
    transport = undefined;
    expect((await post(makeBridge(), "/invite-links", { space_id: GROUP })).status).toBe(503);
  });

  test("/healthz lists the providers", async () => {
    const handle = createHandler({ sharedSecret: SECRET, bridge: makeBridge(), status: () => "connected", channels: ["telegram"] });
    const res = await handle(new Request("http://sidecar/healthz", { headers: AUTH }));
    expect(await res.json()).toEqual({ status: "ok", spectrum: "connected", providers: ["telegram"] });
  });
});
