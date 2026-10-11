import { describe, expect, test } from "bun:test";
import {
  botIdFromToken,
  inlineKeyboard,
  isTelegramChatId,
  isTelegramGroupId,
  telegramEvent,
  telegramInviteError,
  telegramSendError,
} from "../src/telegram";
import { ChatMigratedError, NotAdminError, RecipientUnreachableError } from "../src/types";

const BOT_ID = "7000000001";
const maya = { id: 987654321, is_bot: false, first_name: "Maya", username: "maya_l" };
const dm = { id: 987654321, type: "private", first_name: "Maya" };
const group = { id: -1001234567890, type: "supergroup", title: "Hinton James 4th floor" };

/** A TelegramApiError as @photon-ai/telegram-ts throws it. */
function apiError(code: number, description: string, parameters?: Record<string, unknown>): Error {
  return Object.assign(new Error(description), { name: "TelegramApiError", errorCode: code, telegramDescription: description, parameters });
}

describe("telegramEvent: messages", () => {
  test("a DM is a message from the user, with first name and username", () => {
    const event = telegramEvent({ update_id: 1, message: { message_id: 10, date: 1760133600, chat: dm, from: maya, text: "hi" } }, BOT_ID);
    expect(event).toEqual({
      kind: "message",
      chatId: "987654321",
      isGroup: false,
      sender: { id: "987654321", name: "Maya", username: "maya_l" },
      date: new Date("2025-10-10T22:00:00.000Z"),
      messageId: "10",
    });
  });

  test("a /start payload is an ordinary message (the text comes from Spectrum's mapping)", () => {
    const event = telegramEvent({ update_id: 2, message: { message_id: 11, date: 1760133600, chat: dm, from: maya, text: "/start Xy7_k2" } }, BOT_ID);
    expect(event?.kind).toBe("message");
    expect(event?.chatId).toBe("987654321");
  });

  test("a group message carries the chat's title and counts as a group", () => {
    const event = telegramEvent({ update_id: 3, message: { message_id: 12, date: 1760133600, chat: group, from: maya, text: "heading to Sup Dogs" } }, BOT_ID);
    expect(event?.isGroup).toBe(true);
    expect(event?.chatId).toBe("-1001234567890");
    expect(event?.chatTitle).toBe("Hinton James 4th floor");
    // A basic group (type "group") counts as one too.
    const basic = telegramEvent({ update_id: 4, message: { message_id: 1, date: 1, chat: { id: -4001, type: "group", title: "g" }, from: maya, text: "x" } }, BOT_ID);
    expect(basic?.isGroup).toBe(true);
  });

  test("a user without a first name falls back to @username; one with neither has no name", () => {
    const bare = telegramEvent({ update_id: 5, message: { message_id: 1, date: 1, chat: dm, from: { id: 5, is_bot: false, first_name: "", username: "sam" }, text: "x" } }, BOT_ID);
    expect(bare?.sender).toEqual({ id: "5", name: "@sam", username: "sam" });
    const anon = telegramEvent({ update_id: 6, message: { message_id: 1, date: 1, chat: dm, from: { id: 6, is_bot: false, first_name: "" }, text: "x" } }, BOT_ID);
    expect(anon?.sender).toEqual({ id: "6" });
  });

  test("a location pin or venue gives coordinates", () => {
    const pin = telegramEvent({ update_id: 7, message: { message_id: 1, date: 1, chat: dm, from: maya, location: { latitude: 35.913211, longitude: -79.055094 } } }, BOT_ID);
    expect(pin?.location).toEqual({ lat: 35.913211, lng: -79.055094 });
    const venue = telegramEvent(
      { update_id: 8, message: { message_id: 1, date: 1, chat: dm, from: maya, venue: { location: { latitude: 35.9, longitude: -79.0 }, title: "Sup Dogs" } } },
      BOT_ID,
    );
    expect(venue?.location).toEqual({ lat: 35.9, lng: -79.0 });
  });

  test("bots' messages (the bot's own included), channel posts, edits and reactions are dropped", () => {
    const own = { id: Number(BOT_ID), is_bot: true, first_name: "Ferry" };
    expect(telegramEvent({ update_id: 9, message: { message_id: 1, date: 1, chat: group, from: own, text: "echo" } }, BOT_ID)).toBeUndefined();
    const otherBot = { id: 42, is_bot: true, first_name: "Other" };
    expect(telegramEvent({ update_id: 10, message: { message_id: 1, date: 1, chat: group, from: otherBot, text: "x" } }, BOT_ID)).toBeUndefined();
    expect(telegramEvent({ update_id: 11, channel_post: { message_id: 1, date: 1, chat: { id: -100999, type: "channel" }, text: "x" } }, BOT_ID)).toBeUndefined();
    expect(telegramEvent({ update_id: 12, edited_message: { message_id: 1, date: 1, chat: dm, from: maya, text: "x" } }, BOT_ID)).toBeUndefined();
    expect(telegramEvent({ update_id: 13, message_reaction: { chat: dm, message_id: 1, date: 1 } }, BOT_ID)).toBeUndefined();
    expect(telegramEvent({ update_id: 14, message: { message_id: 1, date: 1, chat: group, text: "no sender" } }, BOT_ID)).toBeUndefined();
    expect(telegramEvent(null, BOT_ID)).toBeUndefined();
  });

  test("a group upgraded to a supergroup is a membership event naming the old chat id", () => {
    const event = telegramEvent(
      { update_id: 15, message: { message_id: 1, date: 1760133600, chat: group, from: maya, migrate_from_chat_id: -4001 } },
      BOT_ID,
    );
    expect(event?.kind).toBe("membership");
    expect(event?.chatId).toBe("-1001234567890");
    expect(event?.membership).toEqual({ inChat: true, isAdmin: null, migratedFrom: "-4001" });
  });

  test("a group upgrade counts even without a human sender", () => {
    const anonymous = { id: 1087968824, is_bot: true, first_name: "Group", username: "GroupAnonymousBot" };
    for (const from of [undefined, anonymous]) {
      const event = telegramEvent({ update_id: 16, message: { message_id: 1, date: 1, chat: group, ...(from ? { from } : {}), migrate_from_chat_id: -4001 } }, BOT_ID);
      expect(event?.kind).toBe("membership");
      expect(event?.membership?.migratedFrom).toBe("-4001");
      expect(event?.sender).toBeUndefined();
    }
  });
});

describe("telegramEvent: button presses", () => {
  test("a press in a DM carries its data and the query id to answer", () => {
    const event = telegramEvent(
      {
        update_id: 20,
        callback_query: { id: "cbq-1", from: maya, message: { message_id: 30, date: 1, chat: dm }, chat_instance: "x", data: "invite:acc:42" },
      },
      BOT_ID,
    );
    expect(event).toMatchObject({
      kind: "callback",
      chatId: "987654321",
      isGroup: false,
      sender: { id: "987654321", name: "Maya", username: "maya_l" },
      callbackId: "cbq-1",
      callbackData: "invite:acc:42",
    });
  });

  test("a press in a group is in that group", () => {
    const event = telegramEvent(
      { update_id: 21, callback_query: { id: "cbq-2", from: maya, message: { message_id: 31, date: 1, chat: group }, data: "run:join" } },
      BOT_ID,
    );
    expect(event?.chatId).toBe("-1001234567890");
    expect(event?.isGroup).toBe(true);
    expect(event?.chatTitle).toBe("Hinton James 4th floor");
  });

  test("a press with no message is answered in the presser's DM; one without data is dropped", () => {
    const inline = telegramEvent({ update_id: 22, callback_query: { id: "cbq-3", from: maya, inline_message_id: "im", data: "d" } }, BOT_ID);
    expect(inline?.chatId).toBe("987654321");
    expect(inline?.isGroup).toBe(false);
    expect(telegramEvent({ update_id: 23, callback_query: { id: "cbq-4", from: maya, game_short_name: "g" } }, BOT_ID)).toBeUndefined();
  });
});

describe("telegramEvent: the bot joining and leaving (my_chat_member)", () => {
  const change = (status: string, extra: Record<string, unknown> = {}, chat: object = group) => ({
    update_id: 30,
    my_chat_member: {
      chat,
      from: maya,
      date: 1760133600,
      old_chat_member: { status: "left", user: { id: Number(BOT_ID), is_bot: true, first_name: "Ferry" } },
      new_chat_member: { status, user: { id: Number(BOT_ID), is_bot: true, first_name: "Ferry" }, ...extra },
    },
  });

  test("added as a member: in the chat, not admin, with the title and who added it", () => {
    expect(telegramEvent(change("member"), BOT_ID)).toEqual({
      kind: "membership",
      chatId: "-1001234567890",
      isGroup: true,
      chatTitle: "Hinton James 4th floor",
      sender: { id: "987654321", name: "Maya", username: "maya_l" },
      date: new Date("2025-10-10T22:00:00.000Z"),
      membership: { inChat: true, isAdmin: false },
    });
  });

  test("made an administrator", () => {
    expect(telegramEvent(change("administrator", { can_invite_users: true }), BOT_ID)?.membership).toEqual({ inChat: true, isAdmin: true });
  });

  test("removed or banned: no longer in the chat", () => {
    expect(telegramEvent(change("left"), BOT_ID)?.membership).toEqual({ inChat: false, isAdmin: false });
    expect(telegramEvent(change("kicked"), BOT_ID)?.membership).toEqual({ inChat: false, isAdmin: false });
    expect(telegramEvent(change("restricted", { is_member: false }), BOT_ID)?.membership?.inChat).toBe(false);
    expect(telegramEvent(change("restricted", { is_member: true }), BOT_ID)?.membership?.inChat).toBe(true);
  });

  test("in a DM, kicked means the user blocked the bot", () => {
    const event = telegramEvent(change("kicked", {}, dm), BOT_ID);
    expect(event?.isGroup).toBe(false);
    expect(event?.chatId).toBe("987654321");
    expect(event?.membership?.inChat).toBe(false);
  });
});

describe("ids, tokens and keyboards", () => {
  test("chat ids: positive for a user's DM, negative for a group; iMessage GUIDs are neither", () => {
    expect(isTelegramChatId("987654321")).toBe(true);
    expect(isTelegramChatId("-1001234567890")).toBe(true);
    expect(isTelegramChatId("any;-;+19195550101")).toBe(false);
    expect(isTelegramChatId("0")).toBe(false);
    expect(isTelegramGroupId("-1001234567890")).toBe(true);
    expect(isTelegramGroupId("987654321")).toBe(false);
  });

  test("the bot id is the token's prefix", () => {
    expect(botIdFromToken("7000000001:AAH_secret")).toBe("7000000001");
  });

  test("buttons become one row each: url buttons and callback buttons", () => {
    expect(
      inlineKeyboard([
        { label: "Open run", url: "https://ferry.example/r/abc" },
        { label: "Accept", callback_data: "invite:acc:42" },
      ]),
    ).toEqual({
      inline_keyboard: [[{ text: "Open run", url: "https://ferry.example/r/abc" }], [{ text: "Accept", callback_data: "invite:acc:42" }]],
    });
  });
});

describe("Bot API error classification", () => {
  test("a send refused because the user never pressed Start, blocked the bot, or is unknown is unreachable", () => {
    for (const err of [
      apiError(403, "Forbidden: bot can't initiate conversation with a user"),
      apiError(403, "Forbidden: bot was blocked by the user"),
      apiError(403, "Forbidden: bot was kicked from the supergroup chat"),
      apiError(400, "Bad Request: chat not found"),
    ]) {
      const mapped = telegramSendError(err);
      expect(mapped).toBeInstanceOf(RecipientUnreachableError);
      expect((mapped as Error).message).toBe(err.message);
    }
  });

  test("a send to an upgraded group names the new chat id", () => {
    const mapped = telegramSendError(apiError(400, "Bad Request: group chat was upgraded to a supergroup chat", { migrate_to_chat_id: -1009876 }));
    expect(mapped).toBeInstanceOf(ChatMigratedError);
    expect((mapped as ChatMigratedError).newSpaceId).toBe("-1009876");
  });

  test("other errors pass through unchanged", () => {
    const tooMany = apiError(429, "Too Many Requests: retry after 3", { retry_after: 3 });
    expect(telegramSendError(tooMany)).toBe(tooMany);
    const badButton = apiError(400, "Bad Request: BUTTON_URL_INVALID");
    expect(telegramSendError(badButton)).toBe(badButton);
    const network = new Error("socket hang up");
    expect(telegramSendError(network)).toBe(network);
  });

  test("an invite link without admin rights, or outside the chat, is not_admin", () => {
    for (const err of [
      apiError(400, "Bad Request: not enough rights to manage chat invite links"),
      apiError(400, "Bad Request: CHAT_ADMIN_REQUIRED"),
      apiError(403, "Forbidden: bot is not a member of the supergroup chat"),
    ]) {
      expect(telegramInviteError(err)).toBeInstanceOf(NotAdminError);
    }
    expect(telegramInviteError(apiError(400, "Bad Request: group chat was upgraded to a supergroup chat", { migrate_to_chat_id: -100 }))).toBeInstanceOf(
      ChatMigratedError,
    );
  });
});
