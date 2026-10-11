// Shapes shared by the bridge, the internal API and the Spectrum adapter.
// The adapter is the only file that imports spectrum-ts; tests inject fakes
// that implement Transport.

export interface LatLng {
  lat: number;
  lng: number;
}

/** A messaging provider the sidecar can carry (MESSAGING_PROVIDERS). */
export type Channel = "telegram" | "imessage";

/** What an inbound event is: a user's message, a button press, or the bot joining or leaving a chat. */
export type InboundKind = "message" | "callback" | "membership";

/**
 * POST {CORE_URL}/internal/messages/inbound. Spec section 3's fields keep
 * their names; the rest were added for Telegram (see messaging/README.md).
 */
export interface InboundPayload {
  kind: InboundKind;
  channel: Channel;
  space_id: string;
  is_group: boolean;
  participants: string[];
  /** The iMessage handle; null on Telegram, which has no phone numbers. */
  sender_phone: string | null;
  /** The provider's id for the sender: a Telegram user id, or the iMessage handle. */
  sender_id: string | null;
  /** Telegram first name, else "@username"; null when unknown and on iMessage. */
  sender_name: string | null;
  /** Telegram username without "@"; null when unset and on iMessage. */
  sender_username: string | null;
  /** Telegram group title; null for DMs and on iMessage. */
  chat_title: string | null;
  text: string;
  attachments: { type: string; url: string }[];
  location?: LatLng;
  received_at: string;
  /** kind "callback": the pressed button's callback_data. */
  callback_data?: string;
  /** kind "membership": whether the bot is in the chat now. */
  bot_in_chat?: boolean;
  /** kind "membership": whether the bot is an administrator now; null when Telegram did not say. */
  bot_is_admin?: boolean | null;
  /** kind "membership": the old chat id when Telegram upgraded a group to a supergroup. */
  migrated_from_space_id?: string;
}

export interface CoreReply {
  space_id: string;
  text: string;
}

/** An inline keyboard button: a link, or a button whose press reaches core as kind "callback". */
export type Button = { label: string; url: string } | { label: string; callback_data: string };

/** A DM or group chat the agent can write into. */
export interface ChatSpace {
  readonly id: string;
  readonly isGroup: boolean;
  /** The agent's own number in this chat, when the platform says (a dedicated line); undefined on the shared pool. */
  readonly ownHandle?: string;
  /** Sends text, with inline buttons under it when given (Telegram only). */
  sendText(text: string, buttons?: Button[]): Promise<void>;
  /** Runs fn with the typing indicator on. */
  responding<T>(fn: () => Promise<T>): Promise<T>;
  /** Participant handles of a group, without the agent where the platform knows it. Throws for a DM. */
  memberIds(): Promise<string[]>;
  rename(name: string): Promise<void>;
}

export interface InboundAttachment {
  mimeType: string;
  name: string;
  stream(): Promise<ReadableStream<Uint8Array>>;
}

/** What a membership event says about the bot. */
export interface Membership {
  inChat: boolean;
  /** null when the event does not say (a group upgrade). */
  isAdmin: boolean | null;
  /** The old chat id, when Telegram upgraded a group to a supergroup. */
  migratedFrom?: string;
}

/** One inbound event, already reduced to what core needs. */
export interface InboundMessage {
  kind: InboundKind;
  channel: Channel;
  space: ChatSpace;
  /** iMessage: E.164 phone or email, as iMessage reports the sender. Telegram: the user id. */
  senderId: string | undefined;
  /** Telegram first name, else "@username". Never logged. */
  senderName?: string;
  /** Telegram username without "@". */
  senderUsername?: string;
  /** Telegram group title. */
  chatTitle?: string;
  text: string;
  attachments: InboundAttachment[];
  location?: LatLng;
  /** kind "callback". */
  callbackData?: string;
  /** kind "membership". */
  membership?: Membership;
  receivedAt: Date;
  /** The README pattern's message.reply (Telegram: threads the reply to the message). */
  reply(text: string): Promise<void>;
  /** Telegram callbacks: stops the button's loading spinner (answerCallbackQuery). */
  acknowledge?(): Promise<void>;
}

/** Raised when the platform cannot do what was asked (for example, create a group on a shared line). */
export class UnsupportedSpaceError extends Error {
  override name = "UnsupportedSpaceError";
}

/** Telegram refused a send: the user never pressed Start, blocked the bot, or the bot left the group. */
export class RecipientUnreachableError extends Error {
  override name = "RecipientUnreachableError";
}

/** Telegram upgraded the group to a supergroup, which has a new chat id. */
export class ChatMigratedError extends Error {
  override name = "ChatMigratedError";
  constructor(
    readonly newSpaceId: string,
    message: string,
  ) {
    super(message);
  }
}

/** The bot is not an administrator of the chat (or not in it), so it cannot create invite links there. */
export class NotAdminError extends Error {
  override name = "NotAdminError";
}

/** One live connection to the messaging platform. */
export interface Transport {
  readonly messages: AsyncIterable<InboundMessage>;
  /** A handle for a known space id: a Telegram chat id or an iMessage chat GUID. */
  getSpace(id: string): Promise<ChatSpace>;
  /** iMessage: a DM for one handle, a group for several. Throws UnsupportedSpaceError when the line cannot. */
  createSpace(handles: string[]): Promise<ChatSpace>;
  /** Telegram: a new invite link to a group the bot administers. Absent without Telegram. */
  createInviteLink?(spaceId: string, name?: string): Promise<string>;
  stop(): Promise<void>;
}

export type TransportFactory = () => Promise<Transport>;
