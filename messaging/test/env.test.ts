import { afterAll, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { activeChannels, parseProviders, readConfig, startupErrors, startupWarnings } from "../src/config";
import { findEnvFile, loadEnv, mergeEnv, parseDotenv } from "../src/env";

// Every test points at its own temp tree; none reads the repo's real .env.
const root = mkdtempSync(join(tmpdir(), "ferry-env-"));
afterAll(() => rmSync(root, { recursive: true, force: true }));

function tree(name: string, envText: string, withContract = true): string {
  const dir = join(root, name);
  mkdirSync(join(dir, "messaging", "src"), { recursive: true });
  if (withContract) mkdirSync(join(dir, "contract"));
  writeFileSync(join(dir, ".env"), envText);
  return dir;
}

describe("parseDotenv", () => {
  test("reads keys, quotes, export prefixes and comments", () => {
    const vars = parseDotenv(
      ["# comment", "A=1", "export B=two", 'C="quoted # not a comment"', "D='single'", "E=plain # trailing", "F=", "bad line"].join("\n"),
    );
    expect(vars).toEqual({ A: "1", B: "two", C: "quoted # not a comment", D: "single", E: "plain", F: "" });
  });
});

describe("mergeEnv", () => {
  test("process env wins over the file", () => {
    expect(mergeEnv({ CORE_URL: "http://proc" }, { CORE_URL: "http://file" }).CORE_URL).toBe("http://proc");
  });

  test("a process value set to empty wins and means unset", () => {
    const env = mergeEnv({ PHOTON_PROJECT_ID: "" }, { PHOTON_PROJECT_ID: "from-file" });
    expect(env.PHOTON_PROJECT_ID).toBeUndefined();
    expect("PHOTON_PROJECT_ID" in env).toBe(false);
  });

  test("an empty file value means unset", () => {
    expect(mergeEnv({}, { SIDECAR_URL: "" }).SIDECAR_URL).toBeUndefined();
  });

  test("file values fill keys the process lacks", () => {
    expect(mergeEnv({}, { SIDECAR_SHARED_SECRET: "s" }).SIDECAR_SHARED_SECRET).toBe("s");
  });
});

describe("findEnvFile and loadEnv", () => {
  test("walks up to the first directory with both .env and contract/", () => {
    const dir = tree("walk", "SIDECAR_SHARED_SECRET=walked\n");
    expect(findEnvFile(join(dir, "messaging", "src"), undefined)).toBe(join(dir, ".env"));
    const { env, file } = loadEnv({}, join(dir, "messaging"));
    expect(file).toBe(join(dir, ".env"));
    expect(env.SIDECAR_SHARED_SECRET).toBe("walked");
  });

  test("skips a .env that has no contract/ beside it", () => {
    const outer = tree("outer", "X=outer\n");
    const inner = join(outer, "messaging");
    writeFileSync(join(inner, ".env"), "X=inner\n");
    expect(loadEnv({}, join(inner, "src")).env.X).toBe("outer");
  });

  test("FERRY_ENV_FILE overrides the walk", () => {
    const dir = tree("explicit", "X=walk\n");
    const other = join(root, "other.env");
    writeFileSync(other, "X=explicit\n");
    expect(loadEnv({ FERRY_ENV_FILE: other }, dir).env.X).toBe("explicit");
  });

  test("a missing FERRY_ENV_FILE is an error", () => {
    expect(() => loadEnv({ FERRY_ENV_FILE: join(root, "nope.env") }, root)).toThrow("FERRY_ENV_FILE");
  });

  test("the process env beats the file, even when empty", () => {
    const dir = tree("precedence", "PHOTON_PROJECT_ID=file-id\nPHOTON_PROJECT_SECRET=file-secret\nSIDECAR_SHARED_SECRET=file\n");
    const { env } = loadEnv({ PHOTON_PROJECT_ID: "", SIDECAR_SHARED_SECRET: "proc" }, dir);
    expect(env.PHOTON_PROJECT_ID).toBeUndefined();
    expect(env.PHOTON_PROJECT_SECRET).toBe("file-secret");
    expect(env.SIDECAR_SHARED_SECRET).toBe("proc");
    // With one Photon credential unset, Spectrum stays disabled.
    expect(readConfig(env).photon).toBeUndefined();
  });

  test("no .env anywhere gives just the process env", () => {
    const bare = join(root, "bare");
    mkdirSync(bare);
    const { env, file } = loadEnv({ SIDECAR_SHARED_SECRET: "x" }, bare);
    expect(env.SIDECAR_SHARED_SECRET).toBe("x");
    // The temp dir may sit under a directory with a .env, but never one with contract/ too.
    expect(file === undefined || !file.startsWith(root)).toBe(true);
  });
});

describe("readConfig", () => {
  test("defaults: sidecar on 8787, core on 8080, echo off", () => {
    const config = readConfig({ SIDECAR_SHARED_SECRET: "s" });
    expect(config.port).toBe(8787);
    expect(config.hostname).toBe("127.0.0.1");
    expect(config.coreUrl).toBe("http://127.0.0.1:8080");
    expect(config.echo).toBe(false);
    expect(config.photon).toBeUndefined();
  });

  test("reads the port from SIDECAR_URL and SIDECAR_ECHO=true", () => {
    const config = readConfig({
      SIDECAR_SHARED_SECRET: "s",
      SIDECAR_URL: "http://127.0.0.1:8120/",
      CORE_URL: "http://127.0.0.1:8121",
      SIDECAR_ECHO: "true",
      PHOTON_PROJECT_ID: "id",
      PHOTON_PROJECT_SECRET: "secret",
    });
    expect(config.port).toBe(8120);
    expect(config.sidecarUrl).toBe("http://127.0.0.1:8120");
    expect(config.coreUrl).toBe("http://127.0.0.1:8121");
    expect(config.echo).toBe(true);
    expect(config.photon).toEqual({ projectId: "id", projectSecret: "secret" });
  });

  test("listens on all interfaces when core reaches it by a non-loopback name", () => {
    expect(readConfig({ SIDECAR_SHARED_SECRET: "s", SIDECAR_URL: "http://sidecar:8787" }).hostname).toBe("0.0.0.0");
  });

  test("refuses to start without SIDECAR_SHARED_SECRET", () => {
    expect(() => readConfig({})).toThrow("SIDECAR_SHARED_SECRET");
  });
});

const TOKEN = "7000000001:TEST_fake_token";
const SECRET = "TEST_fake-webhook_secret_0123456789";
const photon = { PHOTON_PROJECT_ID: "id", PHOTON_PROJECT_SECRET: "secret" };

describe("MESSAGING_PROVIDERS and the Telegram token", () => {

  test("defaults to telegram; accepts imessage, both, any case and spacing; drops repeats", () => {
    expect(parseProviders(undefined)).toEqual(["telegram"]);
    expect(parseProviders("imessage")).toEqual(["imessage"]);
    expect(parseProviders(" Telegram , imessage ")).toEqual(["telegram", "imessage"]);
    expect(parseProviders("telegram,telegram")).toEqual(["telegram"]);
    expect(parseProviders(" , ")).toEqual(["telegram"]);
  });

  test("an unknown provider stops startup", () => {
    expect(() => parseProviders("telegram,whatsapp")).toThrow("MESSAGING_PROVIDERS");
    expect(() => readConfig({ SIDECAR_SHARED_SECRET: "s", MESSAGING_PROVIDERS: "sms" })).toThrow("MESSAGING_PROVIDERS");
  });

  test("Telegram connects with the token, the webhook secret and the Photon credentials", () => {
    const config = readConfig({
      SIDECAR_SHARED_SECRET: "s",
      SPECTRUM_TELEGRAM_BOT_TOKEN: TOKEN,
      SPECTRUM_TELEGRAM_WEBHOOK_SECRET: SECRET,
      TELEGRAM_BOT_USERNAME: "@Ferrygo_bot",
      ...photon,
    });
    expect(config.telegram).toEqual({ botToken: TOKEN, webhookSecret: SECRET });
    expect(config.telegramBotUsername).toBe("Ferrygo_bot");
    expect(activeChannels(config)).toEqual(["telegram"]);
    expect(startupWarnings(config)).toEqual([]);
    expect(startupErrors(config)).toEqual([]);
  });

  test("an unset token disables Telegram with one warning; the internal API still serves", () => {
    const config = readConfig({ SIDECAR_SHARED_SECRET: "s", ...photon });
    expect(config.telegram).toBeUndefined();
    expect(activeChannels(config)).toEqual([]);
    expect(startupWarnings(config)).toEqual(["Telegram disabled: SPECTRUM_TELEGRAM_BOT_TOKEN is unset"]);
    // Without a token the secret does not matter: no error line.
    expect(startupErrors(config)).toEqual([]);
  });

  test("a malformed token is never used and never printed", () => {
    const config = readConfig({ SIDECAR_SHARED_SECRET: "s", SPECTRUM_TELEGRAM_BOT_TOKEN: "not a token", SPECTRUM_TELEGRAM_WEBHOOK_SECRET: SECRET, ...photon });
    expect(config.telegram).toBeUndefined();
    const warnings = startupWarnings(config);
    expect(warnings).toHaveLength(1);
    expect(warnings[0]).toContain("not shaped like");
    expect(warnings[0]).not.toContain("not a token");
  });

  test("a token without the Photon credentials: Telegram's inbound needs Photon's cloud, so nothing connects", () => {
    const config = readConfig({ SIDECAR_SHARED_SECRET: "s", SPECTRUM_TELEGRAM_BOT_TOKEN: TOKEN, SPECTRUM_TELEGRAM_WEBHOOK_SECRET: SECRET });
    expect(activeChannels(config)).toEqual([]);
    expect(startupWarnings(config)).toEqual([
      "Spectrum disabled: PHOTON_PROJECT_ID or PHOTON_PROJECT_SECRET is unset (Telegram's inbound also comes through Photon's cloud)",
    ]);
  });

  test("the original run: token and Photon variables set empty in the process env, so nothing connects", () => {
    const env = mergeEnv(
      { SPECTRUM_TELEGRAM_BOT_TOKEN: "", PHOTON_PROJECT_ID: "", PHOTON_PROJECT_SECRET: "", SIDECAR_SHARED_SECRET: "s" },
      { SPECTRUM_TELEGRAM_BOT_TOKEN: TOKEN, SPECTRUM_TELEGRAM_WEBHOOK_SECRET: SECRET, ...photon },
    );
    const config = readConfig(env);
    expect(config.photon).toBeUndefined();
    expect(config.telegram).toBeUndefined();
    expect(activeChannels(config)).toEqual([]);
    expect(startupErrors(config)).toEqual([]);
  });

  test("both providers: iMessage connects without a Telegram token; Telegram joins when the token is set", () => {
    const noToken = readConfig({ SIDECAR_SHARED_SECRET: "s", MESSAGING_PROVIDERS: "telegram,imessage", ...photon });
    expect(activeChannels(noToken)).toEqual(["imessage"]);
    expect(startupWarnings(noToken)).toEqual(["Telegram disabled: SPECTRUM_TELEGRAM_BOT_TOKEN is unset"]);
    const both = readConfig({
      SIDECAR_SHARED_SECRET: "s",
      MESSAGING_PROVIDERS: "telegram,imessage",
      SPECTRUM_TELEGRAM_BOT_TOKEN: TOKEN,
      SPECTRUM_TELEGRAM_WEBHOOK_SECRET: SECRET,
      ...photon,
    });
    expect(activeChannels(both)).toEqual(["telegram", "imessage"]);
  });

  test("iMessage only: no Telegram warning even without a token", () => {
    const config = readConfig({ SIDECAR_SHARED_SECRET: "s", MESSAGING_PROVIDERS: "imessage", ...photon });
    expect(activeChannels(config)).toEqual(["imessage"]);
    expect(startupWarnings(config)).toEqual([]);
  });
});

describe("SPECTRUM_TELEGRAM_WEBHOOK_SECRET", () => {
  const SECRET_ERROR_UNSET =
    "Telegram disabled: SPECTRUM_TELEGRAM_WEBHOOK_SECRET is unset; the bot token needs it, so the webhook refuses updates without it";

  test("a token without a secret disables Telegram with one error line; the internal API still serves", () => {
    for (const env of [{ ...photon }, {}]) {
      const config = readConfig({ SIDECAR_SHARED_SECRET: "s", SPECTRUM_TELEGRAM_BOT_TOKEN: TOKEN, ...env });
      expect(config.telegram).toBeUndefined();
      expect(config.telegramSecretProblem).toBe("unset");
      expect(activeChannels(config)).toEqual([]);
      expect(startupErrors(config)).toEqual([SECRET_ERROR_UNSET]);
      // No second line: the Photon warning is about providers that would otherwise connect.
      expect(startupWarnings(config)).toEqual([]);
    }
  });

  test("a secret set to empty in the process env means unset, even when the file has one", () => {
    const env = mergeEnv(
      { SPECTRUM_TELEGRAM_WEBHOOK_SECRET: "", SIDECAR_SHARED_SECRET: "s" },
      { SPECTRUM_TELEGRAM_BOT_TOKEN: TOKEN, SPECTRUM_TELEGRAM_WEBHOOK_SECRET: SECRET, ...photon },
    );
    const config = readConfig(env);
    expect(config.telegram).toBeUndefined();
    expect(startupErrors(config)).toEqual([SECRET_ERROR_UNSET]);
  });

  test("an invalid secret is refused and never printed", () => {
    const invalid = ["has space", "slash/char", "dot.ted", "colon:x", "plus+sign", "naïve", "x".repeat(257)];
    for (const value of invalid) {
      const config = readConfig({ SIDECAR_SHARED_SECRET: "s", SPECTRUM_TELEGRAM_BOT_TOKEN: TOKEN, SPECTRUM_TELEGRAM_WEBHOOK_SECRET: value, ...photon });
      expect(config.telegram).toBeUndefined();
      expect(config.telegramSecretProblem).toBe("invalid");
      expect(activeChannels(config)).toEqual([]);
      const errors = startupErrors(config);
      expect(errors).toEqual(["Telegram disabled: SPECTRUM_TELEGRAM_WEBHOOK_SECRET must be 1 to 256 characters of A-Z, a-z, 0-9, _ and -"]);
      expect(errors[0]).not.toContain(value);
      expect(startupWarnings(config)).toEqual([]);
    }
  });

  test("Telegram's rule at its edges: 1 and 256 characters of A-Z, a-z, 0-9, _ and - are accepted", () => {
    const valid = ["a", "-", "_", "AZaz09_-", "x".repeat(256), "0123456789abcdef".repeat(4)];
    for (const value of valid) {
      const config = readConfig({ SIDECAR_SHARED_SECRET: "s", SPECTRUM_TELEGRAM_BOT_TOKEN: TOKEN, SPECTRUM_TELEGRAM_WEBHOOK_SECRET: value, ...photon });
      expect(config.telegram).toEqual({ botToken: TOKEN, webhookSecret: value });
      expect(startupErrors(config)).toEqual([]);
    }
  });

  test("read like the token: from the env file, with the process env winning", () => {
    const file = join(root, "secret.env");
    writeFileSync(file, `SIDECAR_SHARED_SECRET=s\nSPECTRUM_TELEGRAM_BOT_TOKEN=${TOKEN}\nSPECTRUM_TELEGRAM_WEBHOOK_SECRET=${SECRET}\n`);
    expect(readConfig(loadEnv({ FERRY_ENV_FILE: file }, root).env).telegram).toEqual({ botToken: TOKEN, webhookSecret: SECRET });
    const overridden = readConfig(loadEnv({ FERRY_ENV_FILE: file, SPECTRUM_TELEGRAM_WEBHOOK_SECRET: "from_process" }, root).env);
    expect(overridden.telegram).toEqual({ botToken: TOKEN, webhookSecret: "from_process" });
  });

  test("both providers: iMessage still connects when the secret is missing", () => {
    const config = readConfig({ SIDECAR_SHARED_SECRET: "s", MESSAGING_PROVIDERS: "telegram,imessage", SPECTRUM_TELEGRAM_BOT_TOKEN: TOKEN, ...photon });
    expect(activeChannels(config)).toEqual(["imessage"]);
    expect(startupErrors(config)).toEqual([SECRET_ERROR_UNSET]);
  });

  test("iMessage only: a token without a secret is no error, since Telegram was not asked for", () => {
    const config = readConfig({ SIDECAR_SHARED_SECRET: "s", MESSAGING_PROVIDERS: "imessage", SPECTRUM_TELEGRAM_BOT_TOKEN: TOKEN, ...photon });
    expect(activeChannels(config)).toEqual(["imessage"]);
    expect(startupErrors(config)).toEqual([]);
  });
});
