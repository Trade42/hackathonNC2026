// The Telegram path through the real spectrum-ts 12.10.1 SDK, offline: a
// Spectrum app built without Photon credentials (so it never connects), raw
// Telegram Updates delivered the way Photon's Fusor delivers them (a
// RawInboundEvent through app.webhook) with the webhook secret header Telegram
// adds, and a fake Bot API on a local port in place of api.telegram.org.
// Never the real bot.

import { RawInboundEvent } from "@photon-ai/proto/photon/fusor/v1/inbound";
import { afterAll, afterEach, beforeAll, describe, expect, test } from "bun:test";
import { Spectrum, type Message, type Space } from "spectrum-ts";
import { Bridge } from "../src/bridge";
import { CoreClient } from "../src/core-client";
import { maskConsoleArgs } from "../src/console-mask";
import { errorText } from "../src/log";
import { createHandler } from "../src/server";
import { botClient, spectrumProviders, telegramInbound, transportFromApp, type BotClient } from "../src/spectrum";
import { ChatMigratedError, NotAdminError, RecipientUnreachableError, UnsupportedSpaceError, type InboundMessage, type Transport } from "../src/types";
import { until } from "./helpers";

const TOKEN = "7000000001:TEST_fake_token_never_real";
const SECRET = "TEST_fake-webhook_secret_0123456789";
const maya = { id: 987654321, is_bot: false, first_name: "Maya", username: "maya_l" };
const dm = { id: 987654321, type: "private", first_name: "Maya" };
const group = { id: -1001234567890, type: "supergroup", title: "Hinton James 4th floor" };

// The fake Bot API. Each call is recorded; a test can queue one response per method.
const calls: { method: string; body: Record<string, unknown> }[] = [];
const queued = new Map<string, Response[]>();
const api = Bun.serve({
  port: 0,
  hostname: "127.0.0.1",
  fetch: async (req) => {
    const path = new URL(req.url).pathname;
    if (path.startsWith(`/file/bot${TOKEN}/`)) return new Response("JPEGDATA");
    const method = path.split("/").pop() ?? "";
    const text = await req.text();
    calls.push({ method, body: text ? (JSON.parse(text) as Record<string, unknown>) : {} });
    const next = queued.get(method)?.shift();
    if (next) return next;
    switch (method) {
      case "sendMessage":
        return Response.json({ ok: true, result: { message_id: 100 + calls.length, date: 1760133600, chat: { id: 1, type: "private" } } });
      case "getWebhookInfo":
        return Response.json({ ok: true, result: { url: "", has_custom_certificate: false, pending_update_count: 0 } });
      case "getFile":
        return Response.json({ ok: true, result: { file_id: "f1", file_unique_id: "u1", file_path: "photos/file_1.jpg" } });
      case "createChatInviteLink":
        return Response.json({
          ok: true,
          result: {
            invite_link: "https://t.me/+AbCdEf123",
            creator: { id: 7000000001, is_bot: true, first_name: "Ferry" },
            creates_join_request: false,
            is_primary: false,
            is_revoked: false,
          },
        });
      default:
        return Response.json({ ok: true, result: true });
    }
  },
});
const baseUrl = `http://127.0.0.1:${api.port}`;

function refuse(method: string, code: number, description: string, parameters?: Record<string, unknown>): void {
  const list = queued.get(method) ?? [];
  list.push(Response.json({ ok: false, error_code: code, description, ...(parameters ? { parameters } : {}) }, { status: code }));
  queued.set(method, list);
}

let app: Awaited<ReturnType<typeof Spectrum>>;
let bot: BotClient;
let transport: Transport;

beforeAll(async () => {
  app = await Spectrum({
    providers: spectrumProviders({ imessage: false, telegram: { botToken: TOKEN, webhookSecret: SECRET, baseUrl } }),
    options: { logLevel: "error" },
  });
  bot = botClient({ botToken: TOKEN, baseUrl });
  transport = transportFromApp(app, { imessage: false, telegram: true, bot });
});

afterAll(async () => {
  await app.stop();
  api.stop(true);
});

afterEach(() => {
  calls.length = 0;
  queued.clear();
});

let eventSeq = 0;

/**
 * Posts one raw Update to app.webhook as Fusor relays it: Telegram's HTTP
 * request, with its X-Telegram-Bot-Api-Secret-Token header when `secret` is
 * given. Whatever Spectrum delivers is pushed onto `got`.
 */
async function post(update: Record<string, unknown>, secret: string | undefined, got: [Space, Message][]): Promise<Response> {
  const header = secret === undefined ? "" : `X-Telegram-Bot-Api-Secret-Token: ${secret}\r\n`;
  const raw = new TextEncoder().encode(`POST /telegram HTTP/1.1\r\ncontent-type: application/json\r\n${header}\r\n${JSON.stringify(update)}`);
  const event = RawInboundEvent.encode(RawInboundEvent.fromPartial({ eventId: `e${++eventSeq}`, platform: "telegram", rawRequest: raw })).finish();
  const webhook = (app as unknown as { webhook: (req: Request, h: (s: Space, m: Message) => void) => Promise<Response> }).webhook;
  return webhook(new Request("http://sidecar/fusor", { method: "POST", body: event }), (space, message) => {
    got.push([space, message]);
  });
}

/**
 * Delivers one raw Update with the right secret and returns what Spectrum
 * hands the app, mapped to InboundMessages. Spectrum delivers after it
 * acknowledges, so this waits for `expected` deliveries; for 0 it waits a
 * fixed while instead.
 */
async function deliver(update: Record<string, unknown>, expected = 1): Promise<InboundMessage[]> {
  const got: [Space, Message][] = [];
  const res = await post(update, SECRET, got);
  expect(res.status).toBe(200);
  if (expected > 0) await until(() => got.length >= expected);
  else await Bun.sleep(100);
  return got.map(([space, message]) => telegramInbound(space, message, bot)).filter((m): m is InboundMessage => m !== undefined);
}

describe("inbound through Spectrum", () => {
  test("a DM's /start payload arrives as text, with the sender's id, first name and username; the reply threads to it", async () => {
    const [msg, ...rest] = await deliver({ update_id: 1, message: { message_id: 10, date: 1760133600, chat: dm, from: maya, text: "/start Xy7_k2" } });
    expect(rest).toEqual([]);
    expect(msg).toMatchObject({
      kind: "message",
      channel: "telegram",
      senderId: "987654321",
      senderName: "Maya",
      senderUsername: "maya_l",
      text: "/start Xy7_k2",
      attachments: [],
    });
    expect(msg?.space.id).toBe("987654321");
    expect(msg?.space.isGroup).toBe(false);
    expect(msg?.receivedAt.toISOString()).toBe("2025-10-10T22:00:00.000Z");

    await msg?.reply("echo: /start Xy7_k2");
    expect(calls).toEqual([
      { method: "sendMessage", body: { chat_id: "987654321", text: "echo: /start Xy7_k2", reply_parameters: { message_id: 10 } } },
    ]);
  });

  test("a group message carries the group's id and title", async () => {
    const [msg] = await deliver({ update_id: 2, message: { message_id: 11, date: 1760133600, chat: group, from: maya, text: "heading to Sup Dogs at 6" } });
    expect(msg?.space.id).toBe("-1001234567890");
    expect(msg?.space.isGroup).toBe(true);
    expect(msg?.chatTitle).toBe("Hinton James 4th floor");
    expect(msg?.text).toBe("heading to Sup Dogs at 6");
  });

  test("a photo with a caption gives the caption and an attachment that downloads lazily", async () => {
    const [msg] = await deliver({
      update_id: 3,
      message: {
        message_id: 12,
        date: 1760133600,
        chat: dm,
        from: maya,
        caption: "my receipt",
        photo: [
          { file_id: "small", file_unique_id: "s", width: 90, height: 90, file_size: 1000 },
          { file_id: "big", file_unique_id: "b", width: 1280, height: 1280, file_size: 90000 },
        ],
      },
    });
    expect(msg?.text).toBe("my receipt");
    expect(msg?.attachments).toHaveLength(1);
    expect(msg?.attachments[0]?.mimeType).toBe("image/jpeg");
    expect(calls).toEqual([]); // nothing downloaded until core asks
    expect(await new Response(await msg?.attachments[0]?.stream()).text()).toBe("JPEGDATA");
    expect(calls).toEqual([{ method: "getFile", body: { file_id: "big" } }]);
  });

  test("a button press arrives as kind callback; acknowledge answers it and reply posts in the chat", async () => {
    const [msg] = await deliver({
      update_id: 4,
      callback_query: { id: "cbq-1", from: maya, chat_instance: "ci", message: { message_id: 30, date: 1760133600, chat: dm, text: "Join?" }, data: "invite:acc:42" },
    });
    expect(msg).toMatchObject({ kind: "callback", channel: "telegram", senderId: "987654321", callbackData: "invite:acc:42", text: "" });
    expect(msg?.space.id).toBe("987654321");
    await msg?.acknowledge?.();
    await msg?.reply("echo: button invite:acc:42");
    expect(calls).toEqual([
      { method: "answerCallbackQuery", body: { callback_query_id: "cbq-1" } },
      { method: "sendMessage", body: { chat_id: "987654321", text: "echo: button invite:acc:42" } },
    ]);
  });

  test("the bot being added to a group arrives as kind membership", async () => {
    const [msg] = await deliver({
      update_id: 5,
      my_chat_member: {
        chat: group,
        from: maya,
        date: 1760133600,
        old_chat_member: { status: "left", user: { id: 7000000001, is_bot: true, first_name: "Ferry" } },
        new_chat_member: { status: "member", user: { id: 7000000001, is_bot: true, first_name: "Ferry" } },
      },
    });
    expect(msg).toMatchObject({ kind: "membership", senderId: "987654321", chatTitle: "Hinton James 4th floor", membership: { inChat: true, isAdmin: false } });
    expect(msg?.space.id).toBe("-1001234567890");
    expect(msg?.space.isGroup).toBe(true);
  });

  test("a location pin, which Spectrum's own mapping drops, arrives with its coordinates", async () => {
    const [msg] = await deliver({
      update_id: 6,
      message: { message_id: 13, date: 1760133600, chat: dm, from: maya, location: { latitude: 35.913211, longitude: -79.055094 } },
    });
    expect(msg).toMatchObject({ kind: "message", text: "", location: { lat: 35.913211, lng: -79.055094 } });
  });

  test("the bot's own messages and edits never arrive", async () => {
    const own = { id: 7000000001, is_bot: true, first_name: "Ferry" };
    expect(await deliver({ update_id: 7, message: { message_id: 14, date: 1, chat: group, from: own, text: "echo: hi" } }, 0)).toEqual([]);
    expect(await deliver({ update_id: 8, edited_message: { message_id: 15, date: 1, edit_date: 2, chat: dm, from: maya, text: "x" } }, 0)).toEqual([]);
  });
});

describe("the webhook secret: forged Updates are refused", () => {
  const start = { update_id: 50, message: { message_id: 50, date: 1760133600, chat: dm, from: maya, text: "/start Xy7_k2" } };

  for (const [name, secret, reason] of [
    ["without the secret header", undefined, "missing the secret token header"],
    ["with a wrong secret", "TEST_wrong-webhook_secret_01234567", "secret token mismatch"],
    ["with an empty secret", "", "missing the secret token header"],
    ["with the secret's prefix", SECRET.slice(0, -1), "secret token mismatch"],
  ] as const) {
    test(`an Update ${name} is answered 400 and never reaches the sidecar`, async () => {
      const got: [Space, Message][] = [];
      const res = await post(start, secret, got);
      expect(res.status).toBe(400);
      expect(await res.text()).toContain(reason);
      await Bun.sleep(100);
      expect(got).toEqual([]);
      expect(calls).toEqual([]);
    });
  }

  test("the same Update with the secret arrives (the header as Telegram spells it)", async () => {
    const [msg] = await deliver(start);
    expect(msg?.text).toBe("/start Xy7_k2");
  });
});

describe("webhook registration (cloud mode)", () => {
  type CreateClient = (ctx: { config: unknown; projectConfig: { slug: string } }) => Promise<unknown>;
  const webhookUrl = "https://ferry-test.spctrm.dev/telegram";

  /** The provider's own startup step, as Spectrum() runs it with a project slug, against the fake Bot API. */
  function register(apiBase = baseUrl, botToken = TOKEN): Promise<unknown> {
    const [provider] = spectrumProviders({ imessage: false, telegram: { botToken, webhookSecret: SECRET, baseUrl: apiBase } });
    const definition = (provider as unknown as { __definition: { config: { parse: (c: unknown) => unknown }; lifecycle: { createClient: CreateClient } } }).__definition;
    const config = definition.config.parse((provider as unknown as { config: unknown }).config);
    return definition.lifecycle.createClient({ config, projectConfig: { slug: "ferry-test" } });
  }

  test("the first start registers the webhook with the secret as secret_token", async () => {
    expect(process.env.SPECTRUM_SUPER_WEBHOOK).toBeUndefined();
    await register();
    expect(calls).toEqual([
      { method: "getWebhookInfo", body: {} },
      { method: "setWebhook", body: { url: webhookUrl, secret_token: SECRET } },
    ]);
  });

  test("a start whose URL is already registered makes no setWebhook call, so a new secret needs deleteWebhook first", async () => {
    queued.set("getWebhookInfo", [Response.json({ ok: true, result: { url: webhookUrl, has_custom_certificate: false, pending_update_count: 0 } })]);
    await register();
    expect(calls.map((call) => call.method)).toEqual(["getWebhookInfo"]);
  });

  test("a failed registration's error never carries the secret, through errorText or the console mask", async () => {
    // A real token's secret part is 35 characters, the length the mask recognizes; this file's TOKEN is shorter.
    const realLength = "7000000001:AAH_fake-token_of-a-real-tokens-length";
    refuse("setWebhook", 400, "Bad Request: bad webhook: Failed to resolve host");
    const refused = await register(baseUrl, realLength).catch((e: unknown) => e);
    expect(calls.map((call) => call.method)).toEqual(["getWebhookInfo", "setWebhook"]);
    // A network failure: Bun's error keeps the tokened URL in its `path` (masked), never the request body.
    const unreachable = await register("http://127.0.0.1:1", realLength).catch((e: unknown) => e);
    for (const err of [refused, unreachable]) {
      expect(err).toBeInstanceOf(Error);
      expect((err as Error).message).toContain("Telegram webhook registration failed");
      for (const text of [errorText(err), maskConsoleArgs([err])]) {
        expect(text).not.toContain(SECRET);
        expect(text).not.toContain(realLength.split(":")[1] as string);
      }
    }
    expect(maskConsoleArgs([unreachable])).toContain("/bot•••:•••/getWebhookInfo");
  });
});

describe("outbound through Spectrum", () => {
  test("text and inline buttons go to a chat id, as sendMessage with reply_markup", async () => {
    const space = await transport.getSpace("-1001234567890");
    expect(space.isGroup).toBe(true);
    await space.sendText("Ferry to Sup Dogs is open", [
      { label: "Order", url: "https://ferry.example/r/run_1" },
      { label: "I'm in", callback_data: "run:join:run_1" },
    ]);
    await space.sendText("plain");
    expect(calls).toEqual([
      {
        method: "sendMessage",
        body: {
          chat_id: "-1001234567890",
          text: "Ferry to Sup Dogs is open",
          reply_markup: { inline_keyboard: [[{ text: "Order", url: "https://ferry.example/r/run_1" }], [{ text: "I'm in", callback_data: "run:join:run_1" }]] },
        },
      },
      { method: "sendMessage", body: { chat_id: "-1001234567890", text: "plain" } },
    ]);
  });

  test("Telegram's refusals become RecipientUnreachableError and ChatMigratedError", async () => {
    const space = await transport.getSpace("987654321");
    refuse("sendMessage", 403, "Forbidden: bot can't initiate conversation with a user");
    await expect(space.sendText("hi")).rejects.toBeInstanceOf(RecipientUnreachableError);

    refuse("sendMessage", 400, "Bad Request: chat not found");
    await expect(space.sendText("hi", [{ label: "Go", url: "https://ferry.example" }])).rejects.toBeInstanceOf(RecipientUnreachableError);

    const old = await transport.getSpace("-4001");
    refuse("sendMessage", 400, "Bad Request: group chat was upgraded to a supergroup chat", { migrate_to_chat_id: -1004001 });
    const err = await old.sendText("hi").catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ChatMigratedError);
    expect((err as ChatMigratedError).newSpaceId).toBe("-1004001");
  });

  test("invite links come from the Bot API; without admin rights it is NotAdminError", async () => {
    expect(await transport.createInviteLink?.("-1001234567890", "Hinton James")).toBe("https://t.me/+AbCdEf123");
    expect(calls).toEqual([{ method: "createChatInviteLink", body: { chat_id: "-1001234567890", name: "Hinton James" } }]);
    refuse("createChatInviteLink", 400, "Bad Request: not enough rights to manage chat invite links");
    await expect(transport.createInviteLink?.("-1001234567890")).rejects.toBeInstanceOf(NotAdminError);
  });

  test("with iMessage off, iMessage ids and phone chats are refused", async () => {
    await expect(transport.getSpace("any;-;+19195550101")).rejects.toBeInstanceOf(UnsupportedSpaceError);
    await expect(transport.createSpace(["+19195550101"])).rejects.toBeInstanceOf(UnsupportedSpaceError);
  });

  test("POST /send to a user who never pressed Start is 403 recipient_not_reachable, end to end", async () => {
    const bridge = new Bridge({
      core: new CoreClient({ coreUrl: "http://127.0.0.1:9", sharedSecret: "localtest" }),
      sidecarUrl: "http://127.0.0.1:8120",
      echo: false,
      agentPhone: undefined,
      channels: ["telegram"],
      transport: () => transport,
    });
    const handle = createHandler({ sharedSecret: "localtest", bridge, status: () => "connected", channels: ["telegram"] });
    refuse("sendMessage", 403, "Forbidden: bot can't initiate conversation with a user");
    const res = await handle(
      new Request("http://sidecar/send", {
        method: "POST",
        headers: { "x-ferry-secret": "localtest" },
        body: JSON.stringify({ to_user_ids: ["555000111"], text: "Accept Maya's invite?", buttons: [{ label: "Accept", callback_data: "inv:acc:9" }] }),
      }),
    );
    expect(res.status).toBe(403);
    const body = (await res.json()) as { error: { code: string; message: string } };
    expect(body.error.code).toBe("recipient_not_reachable");
    expect(body.error.message).toContain("press Start");
    expect(calls[0]?.body.chat_id).toBe("555000111");
  });
});
