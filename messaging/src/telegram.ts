// Telegram specifics that need no SDK: reading a raw Bot API Update, telling
// chat ids apart, building inline keyboards and classifying Bot API errors.
// Structural types only, so tests feed raw Update JSON.
//
// Spectrum's Telegram provider (12.10.1) maps only `message` updates and
// keeps the sender's id and username. telegram-provider.ts hands every raw
// Update to telegramEvent() as well, which is where callback queries,
// my_chat_member, the chat type and title, and first names come from. See
// docs/DECISIONS.md, Photon Spectrum, Telegram.

import {
  ChatMigratedError,
  NotAdminError,
  RecipientUnreachableError,
  type Button,
  type InboundKind,
  type LatLng,
  type Membership,
} from "./types";

/** Telegram's own limit on a message's text. */
export const MAX_TELEGRAM_TEXT = 4096;
/** Telegram's limit on a button's callback_data, in bytes. */
export const MAX_CALLBACK_DATA_BYTES = 64;
/** The provider's own check on the token's shape (`<bot id>:<secret>`). */
export const BOT_TOKEN_SHAPE = /^\d+:[A-Za-z0-9_-]+$/;
/** setWebhook's rule for secret_token, which the provider also checks: 1 to 256 of A-Z, a-z, 0-9, _ and -. */
export const WEBHOOK_SECRET_SHAPE = /^[A-Za-z0-9_-]{1,256}$/;

const CHAT_ID = /^-?[1-9]\d{0,19}$/;
const GROUP_ID = /^-[1-9]\d{0,19}$/;

/** A Telegram chat id: a user's id for a DM (positive), negative for a group or supergroup. */
export function isTelegramChatId(id: string): boolean {
  return CHAT_ID.test(id);
}

export function isTelegramGroupId(id: string): boolean {
  return GROUP_ID.test(id);
}

/** The bot's own user id is the token's prefix. */
export function botIdFromToken(token: string): string {
  return token.split(":")[0] ?? "";
}

interface TgUser {
  id: number;
  is_bot?: boolean;
  first_name?: string;
  username?: string;
}

interface TgChat {
  id: number;
  type: string;
  title?: string;
}

interface TgPoint {
  latitude: number;
  longitude: number;
}

interface TgMessage {
  message_id: number;
  date: number;
  chat: TgChat;
  from?: TgUser;
  location?: TgPoint;
  venue?: { location?: TgPoint };
  migrate_from_chat_id?: number;
}

interface TgCallbackQuery {
  id: string;
  from: TgUser;
  message?: { message_id: number; date: number; chat: TgChat };
  data?: string;
}

interface TgChatMemberUpdated {
  chat: TgChat;
  from: TgUser;
  date: number;
  new_chat_member: { status: string; is_member?: boolean };
}

/** The parts of a Bot API Update the sidecar reads. */
export interface TgUpdate {
  update_id: number;
  message?: TgMessage;
  callback_query?: TgCallbackQuery;
  my_chat_member?: TgChatMemberUpdated;
}

export interface TelegramSender {
  id: string;
  /** First name, else "@username". */
  name?: string;
  username?: string;
}

/** One Update reduced to what the sidecar forwards, beside what Spectrum maps. */
export interface TelegramEvent {
  kind: InboundKind;
  chatId: string;
  isGroup: boolean;
  chatTitle?: string;
  /** The message's id (kind "message"), for threaded replies. */
  messageId?: string;
  sender?: TelegramSender;
  date: Date;
  callbackId?: string;
  callbackData?: string;
  membership?: Membership;
  /** A native location or venue pin (Spectrum drops these). */
  location?: LatLng;
}

const IN_CHAT = new Set(["creator", "administrator", "member"]);

/**
 * Reads one raw Update. Undefined for everything core does not get: channel
 * posts, edits, reactions, bots' messages (the bot's own included), a
 * callback without data, and the old chat's half of a group upgrade.
 */
export function telegramEvent(update: unknown, botId: string): TelegramEvent | undefined {
  if (!isObject(update)) return undefined;
  const u = update as Partial<TgUpdate>;
  if (u.message) return fromMessage(u.message, botId);
  if (u.callback_query) return fromCallback(u.callback_query);
  if (u.my_chat_member) return fromMembership(u.my_chat_member);
  return undefined;
}

function fromMessage(msg: TgMessage, botId: string): TelegramEvent | undefined {
  const from = msg.from;
  const human = from && !from.is_bot && String(from.id) !== botId ? from : undefined;
  const base = {
    chatId: String(msg.chat.id),
    isGroup: msg.chat.type !== "private",
    ...(msg.chat.title ? { chatTitle: msg.chat.title } : {}),
    ...(human ? { sender: sender(human) } : {}),
    date: new Date(msg.date * 1000),
  };
  // A group upgrade counts whoever (or whatever) sent the service message.
  if (msg.migrate_from_chat_id !== undefined) {
    return {
      ...base,
      kind: "membership",
      membership: { inChat: true, isAdmin: null, migratedFrom: String(msg.migrate_from_chat_id) },
    };
  }
  // Bots (the bot itself, and GroupAnonymousBot for anonymous admins) and senderless messages are dropped.
  if (!human) return undefined;
  const point = msg.location ?? msg.venue?.location;
  const location = point ? toLatLng(point) : undefined;
  return { ...base, kind: "message", messageId: String(msg.message_id), ...(location ? { location } : {}) };
}

function fromCallback(query: TgCallbackQuery): TelegramEvent | undefined {
  if (typeof query.data !== "string" || query.data === "") return undefined;
  const chat = query.message?.chat;
  return {
    kind: "callback",
    // A button on an inline-mode message has no chat; answer in the presser's DM.
    chatId: String(chat?.id ?? query.from.id),
    isGroup: chat !== undefined && chat.type !== "private",
    ...(chat?.title ? { chatTitle: chat.title } : {}),
    sender: sender(query.from),
    // The press has no time of its own.
    date: new Date(),
    callbackId: query.id,
    callbackData: query.data,
  };
}

function fromMembership(change: TgChatMemberUpdated): TelegramEvent {
  const { status, is_member } = change.new_chat_member;
  return {
    kind: "membership",
    chatId: String(change.chat.id),
    isGroup: change.chat.type !== "private",
    ...(change.chat.title ? { chatTitle: change.chat.title } : {}),
    sender: sender(change.from),
    date: new Date(change.date * 1000),
    membership: {
      inChat: IN_CHAT.has(status) || (status === "restricted" && is_member === true),
      isAdmin: status === "administrator" || status === "creator",
    },
  };
}

function sender(user: TgUser): TelegramSender {
  const name = user.first_name || (user.username ? `@${user.username}` : undefined);
  return {
    id: String(user.id),
    ...(name ? { name } : {}),
    ...(user.username ? { username: user.username } : {}),
  };
}

function toLatLng(point: TgPoint): LatLng | undefined {
  const { latitude: lat, longitude: lng } = point;
  if (!Number.isFinite(lat) || !Number.isFinite(lng) || Math.abs(lat) > 90 || Math.abs(lng) > 180) return undefined;
  return { lat, lng };
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

/** Telegram's reply_markup for buttons: one button per row. */
export function inlineKeyboard(buttons: Button[]): { inline_keyboard: ({ text: string; url: string } | { text: string; callback_data: string })[][] } {
  return {
    inline_keyboard: buttons.map((button) =>
      "url" in button ? [{ text: button.label, url: button.url }] : [{ text: button.label, callback_data: button.callback_data }],
    ),
  };
}

/** The fields of @photon-ai/telegram-ts's TelegramApiError (token-free). */
export interface TelegramApiErrorLike {
  errorCode: number;
  description: string;
  migrateTo?: string;
}

export function asTelegramApiError(err: unknown): TelegramApiErrorLike | undefined {
  if (!(err instanceof Error) || err.name !== "TelegramApiError") return undefined;
  const e = err as Error & { errorCode?: unknown; telegramDescription?: unknown; parameters?: { migrate_to_chat_id?: unknown } };
  if (typeof e.errorCode !== "number") return undefined;
  const migrateTo = e.parameters?.migrate_to_chat_id;
  return {
    errorCode: e.errorCode,
    description: typeof e.telegramDescription === "string" ? e.telegramDescription : err.message,
    ...(typeof migrateTo === "number" ? { migrateTo: String(migrateTo) } : {}),
  };
}

const SEND_UNREACHABLE =
  /chat not found|user not found|PEER_ID_INVALID|not enough rights|have no rights|CHAT_WRITE_FORBIDDEN|bot was kicked|bot is not a member/i;
const INVITE_NOT_ADMIN =
  /not enough rights|CHAT_ADMIN_REQUIRED|administrator|have no rights|chat not found|bot was kicked|bot is not a member/i;

/** Turns a failed send's Bot API error into the sidecar's own; anything else comes back unchanged. */
export function telegramSendError(err: unknown): unknown {
  const api = asTelegramApiError(err);
  if (!api) return err;
  if (api.migrateTo) return new ChatMigratedError(api.migrateTo, api.description);
  if (api.errorCode === 403 || (api.errorCode === 400 && SEND_UNREACHABLE.test(api.description))) {
    return new RecipientUnreachableError(api.description);
  }
  return err;
}

/** The same for createChatInviteLink. */
export function telegramInviteError(err: unknown): unknown {
  const api = asTelegramApiError(err);
  if (!api) return err;
  if (api.migrateTo) return new ChatMigratedError(api.migrateTo, api.description);
  if (api.errorCode === 403 || (api.errorCode === 400 && INVITE_NOT_ADMIN.test(api.description))) {
    return new NotAdminError(api.description);
  }
  return err;
}
