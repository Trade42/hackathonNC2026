// Spectrum's Telegram provider, with an inbound handler that also forwards
// what the stock one drops. Photon's Fusor hands every raw Telegram Update to
// the provider's `messages` handler. The stock handler (spectrum-ts 12.10.1)
// maps only `message` updates, to text and lazily downloaded media, and keeps
// just the sender's id and username. The wrapper runs it, reads the same
// Update with telegramEvent(), and attaches the result to the record as the
// extra key `ferry`: Spectrum spreads a record's extra keys onto the Message.
// Callback queries, my_chat_member updates and bare location pins, which the
// stock handler drops, become records of their own with custom content.
// Outbound stays the stock provider's.

import { asCustom } from "spectrum-ts/authoring";
import { telegram } from "spectrum-ts/providers/telegram";
import { botIdFromToken, telegramEvent, type TelegramEvent } from "./telegram";

/** The Message property that carries the TelegramEvent. */
export const FERRY_EXTRA = "ferry";

/** A record as a Fusor-mode `messages` handler returns it (spectrum-ts ProviderMessageRecord). */
interface ProviderRecord {
  id: string;
  content: unknown;
  sender?: { id: string; isMe?: boolean };
  space: { id: string };
  timestamp?: Date;
  [extra: string]: unknown;
}

type MessagesHandler = (ctx: { payload: unknown }) => unknown;

/** The stock handler's records, each with the event attached, or records of our own for what it drops. */
export function wrapTelegramMessages(stock: MessagesHandler, botId: string): MessagesHandler {
  return async (ctx) => {
    const event = telegramEvent(ctx.payload, botId);
    if (!event) return undefined;
    if (event.kind === "message") {
      const records = await stock(ctx);
      if (Array.isArray(records)) return records.map((record: ProviderRecord) => ({ ...record, [FERRY_EXTRA]: event }));
      if (isRecord(records)) return { ...records, [FERRY_EXTRA]: event };
      // Text and media are the stock handler's; without them only a location pin is worth forwarding.
      if (!event.location) return undefined;
      return ownRecord(event, event.messageId ?? `location:${updateId(ctx.payload)}`);
    }
    return ownRecord(event, `${event.kind}:${updateId(ctx.payload)}`);
  };
}

function ownRecord(event: TelegramEvent, id: string): ProviderRecord {
  return {
    id,
    content: asCustom({ telegram: event.kind }),
    ...(event.sender ? { sender: { id: event.sender.id, isMe: false } } : {}),
    space: { id: event.chatId },
    timestamp: event.date,
    [FERRY_EXTRA]: event,
  };
}

function isRecord(value: unknown): value is ProviderRecord {
  return typeof value === "object" && value !== null && "id" in value && "content" in value;
}

function updateId(payload: unknown): string {
  const id = (payload as { update_id?: unknown } | null)?.update_id;
  return typeof id === "number" ? String(id) : crypto.randomUUID();
}

/**
 * `telegram.config({ botToken, webhookSecret })` with the wrapped inbound
 * handler. In cloud mode the provider registers webhookSecret as setWebhook's
 * secret_token, and its verify refuses any update whose
 * X-Telegram-Bot-Api-Secret-Token header does not match. baseUrl is for
 * tests' fake Bot API.
 */
export function ferryTelegramProvider(botToken: string, webhookSecret: string, baseUrl?: string): ReturnType<typeof telegram.config> {
  const stock = telegram.config({ botToken, webhookSecret, ...(baseUrl ? { baseUrl } : {}) });
  const definition = stock.__definition;
  return {
    ...stock,
    __definition: {
      ...definition,
      messages: wrapTelegramMessages(definition.messages as unknown as MessagesHandler, botIdFromToken(botToken)) as never,
    },
  };
}
