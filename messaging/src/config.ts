// The sidecar's settings, read from the merged environment (see env.ts).

import type { Env } from "./env";
import { BOT_TOKEN_SHAPE, WEBHOOK_SECRET_SHAPE } from "./telegram";
import type { Channel } from "./types";

export const DEFAULT_SIDECAR_URL = "http://127.0.0.1:8787";
export const DEFAULT_CORE_URL = "http://127.0.0.1:8080";
export const DEFAULT_PROVIDERS: readonly Channel[] = ["telegram"];

const CHANNELS: readonly Channel[] = ["telegram", "imessage"];

export interface Config {
  /** Both set, or Spectrum stays disabled. Telegram's inbound comes through Photon's cloud too. */
  photon: { projectId: string; projectSecret: string } | undefined;
  /** MESSAGING_PROVIDERS: the providers asked for, whether or not they are configured. */
  providers: Channel[];
  /**
   * SPECTRUM_TELEGRAM_BOT_TOKEN and SPECTRUM_TELEGRAM_WEBHOOK_SECRET, when both
   * are set and well formed. The provider registers the secret with the webhook
   * and refuses every update that does not carry it.
   */
  telegram: { botToken: string; webhookSecret: string } | undefined;
  /** Why the token was not used, for the startup warning. */
  telegramTokenProblem: "unset" | "malformed" | undefined;
  /** Why the webhook secret was not used, for the startup error. */
  telegramSecretProblem: "unset" | "invalid" | undefined;
  /** TELEGRAM_BOT_USERNAME, for the startup log line only. */
  telegramBotUsername: string | undefined;
  sharedSecret: string;
  /** Base URL core uses to reach this sidecar; also where attachment URLs point. */
  sidecarUrl: string;
  port: number;
  hostname: string;
  coreUrl: string;
  echo: boolean;
  /** The agent's own number, dropped from group participant lists when known. */
  agentPhone: string | undefined;
}

const LOOPBACK = new Set(["127.0.0.1", "localhost", "::1", "[::1]"]);

export function readConfig(env: Env): Config {
  const sharedSecret = env.SIDECAR_SHARED_SECRET;
  if (!sharedSecret) throw new Error("SIDECAR_SHARED_SECRET is required: every internal route checks it");

  const sidecarUrl = trimSlash(env.SIDECAR_URL ?? DEFAULT_SIDECAR_URL);
  const parsed = parseUrl("SIDECAR_URL", sidecarUrl);
  const port = parsed.port ? Number(parsed.port) : 8787;
  // Bind loopback when core reaches us on loopback; otherwise listen on all interfaces.
  const hostname = LOOPBACK.has(parsed.hostname) ? "127.0.0.1" : "0.0.0.0";

  const coreUrl = trimSlash(env.CORE_URL ?? DEFAULT_CORE_URL);
  parseUrl("CORE_URL", coreUrl);

  const projectId = env.PHOTON_PROJECT_ID;
  const projectSecret = env.PHOTON_PROJECT_SECRET;
  const token = env.SPECTRUM_TELEGRAM_BOT_TOKEN;
  const tokenOk = token !== undefined && BOT_TOKEN_SHAPE.test(token);
  const webhookSecret = env.SPECTRUM_TELEGRAM_WEBHOOK_SECRET;
  const secretOk = webhookSecret !== undefined && WEBHOOK_SECRET_SHAPE.test(webhookSecret);

  return {
    photon: projectId && projectSecret ? { projectId, projectSecret } : undefined,
    providers: parseProviders(env.MESSAGING_PROVIDERS),
    telegram: tokenOk && secretOk ? { botToken: token, webhookSecret } : undefined,
    telegramTokenProblem: token === undefined ? "unset" : tokenOk ? undefined : "malformed",
    telegramSecretProblem: webhookSecret === undefined ? "unset" : secretOk ? undefined : "invalid",
    telegramBotUsername: env.TELEGRAM_BOT_USERNAME?.replace(/^@/, ""),
    sharedSecret,
    sidecarUrl,
    port,
    hostname,
    coreUrl,
    echo: env.SIDECAR_ECHO === "true",
    agentPhone: env.AGENT_PHONE_E164,
  };
}

/** `telegram` (the default), `imessage`, or both, comma separated. */
export function parseProviders(value: string | undefined): Channel[] {
  if (value === undefined) return [...DEFAULT_PROVIDERS];
  const names = value
    .split(",")
    .map((name) => name.trim().toLowerCase())
    .filter((name) => name !== "");
  const out: Channel[] = [];
  for (const name of names) {
    if (!(CHANNELS as readonly string[]).includes(name)) {
      throw new Error(`MESSAGING_PROVIDERS must list telegram, imessage or both, comma separated (got "${name}")`);
    }
    if (!out.includes(name as Channel)) out.push(name as Channel);
  }
  if (out.length === 0) return [...DEFAULT_PROVIDERS];
  return out;
}

/** The providers that will connect: asked for, configured, and with the Photon credentials Spectrum needs. */
export function activeChannels(config: Config): Channel[] {
  if (!config.photon) return [];
  return config.providers.filter((channel) => channel !== "telegram" || config.telegram !== undefined);
}

/** One line per reason a provider that was asked for stays off. Never prints a value. */
export function startupWarnings(config: Config): string[] {
  const warnings: string[] = [];
  if (config.providers.includes("telegram") && config.telegramTokenProblem) {
    warnings.push(
      config.telegramTokenProblem === "unset"
        ? "Telegram disabled: SPECTRUM_TELEGRAM_BOT_TOKEN is unset"
        : "Telegram disabled: SPECTRUM_TELEGRAM_BOT_TOKEN is not shaped like <bot id>:<secret>",
    );
  }
  if (!config.photon && activeChannels({ ...config, photon: { projectId: "", projectSecret: "" } }).length > 0) {
    warnings.push("Spectrum disabled: PHOTON_PROJECT_ID or PHOTON_PROJECT_SECRET is unset (Telegram's inbound also comes through Photon's cloud)");
  }
  return warnings;
}

/**
 * One line when Telegram is asked for and its token is set, but the webhook
 * secret is unset or invalid. Telegram then stays off; the internal API still
 * serves. Names the variable, never its value.
 */
export function startupErrors(config: Config): string[] {
  if (!config.providers.includes("telegram") || config.telegramTokenProblem === "unset" || !config.telegramSecretProblem) return [];
  return [
    config.telegramSecretProblem === "unset"
      ? "Telegram disabled: SPECTRUM_TELEGRAM_WEBHOOK_SECRET is unset; the bot token needs it, so the webhook refuses updates without it"
      : "Telegram disabled: SPECTRUM_TELEGRAM_WEBHOOK_SECRET must be 1 to 256 characters of A-Z, a-z, 0-9, _ and -",
  ];
}

function trimSlash(url: string): string {
  return url.replace(/\/+$/, "");
}

function parseUrl(name: string, value: string): URL {
  try {
    return new URL(value);
  } catch {
    throw new Error(`${name} is not a valid URL`);
  }
}
