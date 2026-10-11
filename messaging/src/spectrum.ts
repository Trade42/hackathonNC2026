// The Spectrum adapter: the only file that connects to Photon. It turns one
// Spectrum app, carrying iMessage, Telegram or both, into a Transport (see
// types.ts). spectrumFactory() needs Photon; the pieces under it take an app,
// so tests build one offline (no Photon credentials) against a fake Bot API.

import { answerCallbackQuery, createChatInviteLink, createTelegramClient } from "@photon-ai/telegram-ts";
import { Spectrum, UnsupportedError, cloud, custom, type Message, type Space } from "spectrum-ts";
import { imessage } from "spectrum-ts/providers/imessage";
import { telegram } from "spectrum-ts/providers/telegram";
import { errorText, log } from "./log";
import { normalizeContent, ownHandleFromLine } from "./normalize";
import { inlineKeyboard, isTelegramChatId, isTelegramGroupId, telegramInviteError, telegramSendError, type TelegramEvent } from "./telegram";
import { FERRY_EXTRA, ferryTelegramProvider } from "./telegram-provider";
import { UnsupportedSpaceError, type ChatSpace, type InboundMessage, type Transport, type TransportFactory } from "./types";

export interface PhotonCredentials {
  projectId: string;
  projectSecret: string;
}

export interface TelegramOptions {
  botToken: string;
  /** setWebhook's secret_token: Telegram sends it with every update, and the provider refuses updates without it. */
  webhookSecret: string;
  /** The Bot API's base URL; tests point it at a fake. Defaults to https://api.telegram.org. */
  baseUrl?: string;
}

export interface SpectrumOptions {
  photon: PhotonCredentials;
  imessage: boolean;
  /** Telegram's inbound comes through Photon's cloud too, so it needs the Photon credentials as well. */
  telegram: TelegramOptions | undefined;
}

/** A Bot API client for what Spectrum does not expose: callback answers and invite links. */
export type BotClient = ReturnType<typeof createTelegramClient>;

type SpectrumApp = Awaited<ReturnType<typeof Spectrum>>;

export function spectrumFactory(opts: SpectrumOptions): TransportFactory {
  let loggedLineType = false;
  const bot = opts.telegram ? botClient(opts.telegram) : undefined;
  return async () => {
    const app = await Spectrum({
      projectId: opts.photon.projectId,
      projectSecret: opts.photon.projectSecret,
      providers: spectrumProviders(opts),
      options: { logLevel: "warn" },
    });
    if (opts.imessage && !loggedLineType) {
      loggedLineType = true;
      void logLineType(opts.photon.projectId);
    }
    return transportFromApp(app, { imessage: opts.imessage, telegram: opts.telegram !== undefined, bot });
  };
}

export function botClient(opts: Pick<TelegramOptions, "botToken" | "baseUrl">): BotClient {
  return createTelegramClient({ token: opts.botToken, ...(opts.baseUrl ? { baseUrl: opts.baseUrl } : {}) });
}

/** The providers for Spectrum(): iMessage as is, Telegram with the wrapped inbound handler. */
export function spectrumProviders(opts: { imessage: boolean; telegram: TelegramOptions | undefined }) {
  return [
    ...(opts.imessage ? [imessage.config()] : []),
    ...(opts.telegram ? [ferryTelegramProvider(opts.telegram.botToken, opts.telegram.webhookSecret, opts.telegram.baseUrl)] : []),
  ];
}

/** The Transport over a connected app. */
export function transportFromApp(app: SpectrumApp, opts: { imessage: boolean; telegram: boolean; bot: BotClient | undefined }): Transport {
  const im = opts.imessage ? imessage(app) : undefined;
  const tg = opts.telegram ? telegram(app) : undefined;
  const bot = opts.bot;

  async function* messages(): AsyncGenerator<InboundMessage> {
    for await (const [space, message] of app.messages) {
      const inbound = message.platform === "telegram" ? telegramInbound(space, message, bot) : toInbound(space, message);
      if (inbound) yield inbound;
    }
  }

  return {
    messages: messages(),
    getSpace: async (id) => {
      if (isTelegramChatId(id)) {
        if (!tg) throw new UnsupportedSpaceError("Telegram is not enabled");
        return wrapTelegramSpace(await tg.space.get(id), isTelegramGroupId(id));
      }
      if (!im) throw new UnsupportedSpaceError("iMessage is not enabled");
      return wrapSpace(await im.space.get(id));
    },
    createSpace: async (handles) => {
      if (!im) throw new UnsupportedSpaceError("iMessage is not enabled");
      try {
        const target = handles.length === 1 ? (handles[0] as string) : handles;
        return wrapSpace(await im.space.create(target));
      } catch (err) {
        if (err instanceof UnsupportedError) throw new UnsupportedSpaceError(err.message);
        throw err;
      }
    },
    ...(bot
      ? {
          createInviteLink: async (spaceId: string, name?: string) => {
            try {
              const res = await createChatInviteLink({ client: bot, body: { chat_id: spaceId, ...(name ? { name } : {}) } });
              return res.result.invite_link;
            } catch (err) {
              throw telegramInviteError(err);
            }
          },
        }
      : {}),
    stop: () => app.stop(),
  } satisfies Transport;
}

/** Logs whether the project is on a shared pool or a dedicated line (not a secret). */
async function logLineType(projectId: string): Promise<void> {
  try {
    const info = await cloud.getImessageInfo(projectId);
    log.info("photon imessage line", { type: info.type });
  } catch (err) {
    log.warn("photon imessage line type unknown", { error: errorText(err) });
  }
}

/** A Telegram Message (carrying the wrapper's TelegramEvent) as an InboundMessage. */
export function telegramInbound(space: Space, message: Message, bot: BotClient | undefined): InboundMessage | undefined {
  const event = (message as unknown as Record<string, unknown>)[FERRY_EXTRA] as TelegramEvent | undefined;
  if (!event || message.direction !== "inbound") return undefined;
  const normalized = event.kind === "message" ? normalizeContent(message.content) : undefined;
  const location = event.location ?? normalized?.location;
  if (event.kind === "message" && !normalized && !location) return undefined;
  const chat = wrapTelegramSpace(space, event.isGroup);
  const callbackId = event.callbackId;
  return {
    kind: event.kind,
    channel: "telegram",
    space: chat,
    senderId: event.sender?.id,
    ...(event.sender?.name ? { senderName: event.sender.name } : {}),
    ...(event.sender?.username ? { senderUsername: event.sender.username } : {}),
    ...(event.chatTitle ? { chatTitle: event.chatTitle } : {}),
    text: normalized?.text ?? "",
    attachments: normalized?.attachments ?? [],
    ...(location ? { location } : {}),
    ...(event.callbackData ? { callbackData: event.callbackData } : {}),
    ...(event.membership ? { membership: event.membership } : {}),
    receivedAt: event.date,
    // A message's reply is threaded to it; a button press or a join is answered in the chat.
    reply:
      event.kind === "message"
        ? async (text) => {
            try {
              await message.reply(text);
            } catch (err) {
              throw telegramSendError(err);
            }
          }
        : (text) => chat.sendText(text),
    ...(bot && callbackId
      ? {
          acknowledge: async () => {
            await answerCallbackQuery({ client: bot, body: { callback_query_id: callbackId } });
          },
        }
      : {}),
  };
}

function toInbound(space: Space, message: Message): InboundMessage | undefined {
  if (message.direction !== "inbound" || message.sender?.kind === "agent") return undefined;
  const normalized = normalizeContent(message.content);
  if (!normalized) return undefined;
  return {
    kind: "message",
    channel: "imessage",
    space: wrapSpace(space),
    senderId: message.sender?.id || undefined,
    text: normalized.text,
    attachments: normalized.attachments,
    ...(normalized.location ? { location: normalized.location } : {}),
    receivedAt: message.timestamp,
    reply: async (text) => {
      await message.reply(text);
    },
  };
}

function wrapSpace(space: Space): ChatSpace {
  const isGroup = imessage.is(space) ? space.type === "group" : space.id.includes(";+;");
  return {
    id: space.id,
    isGroup,
    ownHandle: ownHandleFromLine(imessage.is(space) ? space.phone : undefined),
    sendText: async (text, buttons) => {
      if (buttons?.length) throw new UnsupportedSpaceError("iMessage has no buttons");
      await space.send(text);
    },
    responding: (fn) => space.responding(fn),
    memberIds: async () => (await space.getMembers()).map((user) => user.id),
    rename: (name) => space.rename(name),
  };
}

/** A Telegram chat. Sends go through the stock provider; Bot API refusals become the sidecar's errors. */
function wrapTelegramSpace(space: Space, isGroup: boolean): ChatSpace {
  return {
    id: space.id,
    isGroup,
    sendText: async (text, buttons) => {
      try {
        if (buttons?.length) {
          // The provider sends `custom` content as a raw Bot API call, with chat_id filled in.
          await space.send(custom({ method: "sendMessage", params: { text, reply_markup: inlineKeyboard(buttons) } }));
        } else {
          await space.send(text);
        }
      } catch (err) {
        throw telegramSendError(err);
      }
    },
    responding: (fn) => space.responding(fn),
    // The Bot API cannot list a group's members; core gets the sender.
    memberIds: async () => [],
    rename: async () => {
      throw new UnsupportedSpaceError("the Telegram provider cannot rename chats");
    },
  };
}
