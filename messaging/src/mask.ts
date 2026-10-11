// Masks phone numbers, email handles, Telegram user ids and usernames, and
// bot tokens before anything reaches a log. Phones keep the country code and
// the last 2 to 4 digits: +1•••0123. Telegram ids and usernames keep their
// last 3 characters: •••789, @•••pez.

const MASK = "•••";

// ITU-T E.164 country codes of one and two digits; every other code has three.
const ONE_DIGIT_CODES = new Set(["1", "7"]);
const TWO_DIGIT_CODES = new Set([
  "20", "27", "30", "31", "32", "33", "34", "36", "39", "40", "41", "43", "44", "45", "46", "47", "48", "49",
  "51", "52", "53", "54", "55", "56", "57", "58", "60", "61", "62", "63", "64", "65", "66", "81", "82", "84",
  "86", "90", "91", "92", "93", "94", "95", "98",
]);

function countryCodeLength(digits: string): number {
  if (ONE_DIGIT_CODES.has(digits.slice(0, 1))) return 1;
  if (TWO_DIGIT_CODES.has(digits.slice(0, 2))) return 2;
  return 3;
}

/** Masks one phone number. Anything that is not E.164-like is fully masked. */
export function maskPhone(phone: string): string {
  const digits = phone.replace(/[^\d]/g, "");
  if (!phone.trim().startsWith("+") || digits.length < 7 || digits.length > 15) return MASK;
  const cc = digits.slice(0, countryCodeLength(digits));
  const national = digits.slice(cc.length);
  const keep = national.length >= 8 ? 4 : 2;
  return `+${cc}${MASK}${national.slice(-keep)}`;
}

/** Masks an email handle down to its first character and domain. */
export function maskEmail(email: string): string {
  const at = email.lastIndexOf("@");
  if (at <= 0) return MASK;
  return `${email[0]}${MASK}${email.slice(at)}`;
}

/** Masks a Telegram user id (a DM's chat id is the same number): •••789. */
export function maskTelegramId(id: string): string {
  return `${MASK}${id.slice(-3)}`;
}

/** Masks a Telegram @username: @•••pez. */
export function maskUsername(username: string): string {
  const name = username.replace(/^@/, "");
  return `@${MASK}${name.slice(-3)}`;
}

/** Masks a sender handle: an iMessage phone or email, or a Telegram user id or @username. */
export function maskHandle(handle: string | undefined): string {
  if (!handle) return "(none)";
  if (/^\d+$/.test(handle)) return maskTelegramId(handle);
  if (handle.startsWith("@")) return maskUsername(handle);
  return handle.includes("@") ? maskEmail(handle) : maskPhone(handle);
}

/**
 * Masks a space id for a log: a Telegram DM's id is the user's id, so it is
 * masked; a group's (negative) id and an iMessage GUID go through maskText.
 */
export function maskSpaceId(id: string): string {
  return /^\d+$/.test(id) ? maskTelegramId(id) : maskText(id);
}

// A bot token, also inside a Bot API file URL (`/file/bot<token>/...`).
const BOT_TOKEN_IN_TEXT = /(?<!\d)\d{5,12}:[A-Za-z0-9_-]{30,}/g;
const PHONE_IN_TEXT = /\+\d[\d\s().-]{5,18}\d/g;
const EMAIL_IN_TEXT = /[^\s;:/<>"'@]+@[^\s;:/<>"'@]+\.[A-Za-z]{2,}/g;
// A Telegram @username (5 to 32 characters), not the domain of a masked email.
const USERNAME_IN_TEXT = /(?<![\w.@•])@([A-Za-z][A-Za-z0-9_]{3,31})(?![\w@])/g;
// A Telegram user id: a run of 7 or more digits, not part of a word, a decimal, a phone, or a negative group id.
const TELEGRAM_ID_IN_TEXT = /(?<![\w+\-•.])\d{7,}(?![\w•])/g;

/**
 * Masks every bot token, phone, email, Telegram user id and @username inside
 * free text, such as an iMessage chat GUID (`any;-;+19195550123`) or an error
 * message. Telegram group ids (negative) stay readable.
 */
export function maskText(text: string): string {
  return text
    .replace(BOT_TOKEN_IN_TEXT, `${MASK}:${MASK}`)
    .replace(EMAIL_IN_TEXT, (m) => maskEmail(m))
    .replace(PHONE_IN_TEXT, (m) => maskPhone(m))
    .replace(USERNAME_IN_TEXT, (m) => maskUsername(m))
    .replace(TELEGRAM_ID_IN_TEXT, (m) => maskTelegramId(m));
}
