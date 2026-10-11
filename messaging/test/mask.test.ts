import { describe, expect, test } from "bun:test";
import { errorText, log } from "../src/log";
import { maskEmail, maskHandle, maskPhone, maskSpaceId, maskTelegramId, maskText, maskUsername } from "../src/mask";
import { captureLogs } from "./helpers";

describe("maskPhone", () => {
  test("keeps the country code and the last 4 digits", () => {
    expect(maskPhone("+19195550123")).toBe("+1•••0123");
    expect(maskPhone("+447700900123")).toBe("+44•••0123");
    expect(maskPhone("+353861234567")).toBe("+353•••4567");
  });

  test("keeps only the last 2 digits of a short national number", () => {
    expect(maskPhone("+3541234567")).toBe("+354•••67");
  });

  test("fully masks anything that is not E.164", () => {
    expect(maskPhone("9195550123")).toBe("•••");
    expect(maskPhone("+12")).toBe("•••");
  });
});

describe("maskEmail and maskHandle", () => {
  test("masks the local part", () => {
    expect(maskEmail("maya@example.com")).toBe("m•••@example.com");
    expect(maskHandle("maya@icloud.com")).toBe("m•••@icloud.com");
    expect(maskHandle("+19195550123")).toBe("+1•••0123");
    expect(maskHandle(undefined)).toBe("(none)");
  });
});

describe("maskText", () => {
  test("masks the phone inside a DM chat GUID", () => {
    expect(maskText("any;-;+19195550123")).toBe("any;-;+1•••0123");
  });

  test("masks emails and phones in free text", () => {
    const text = maskText("from maya@icloud.com and +1 (919) 555-0123 in any;+;chat42");
    expect(text).not.toContain("maya@");
    expect(text).not.toContain("555-0123");
    expect(text).toContain("any;+;chat42");
  });
});

describe("log", () => {
  test("masks phones in messages, fields and errors", () => {
    const { lines, restore } = captureLogs();
    try {
      log.info("inbound from +19195550123", { space: "any;-;+19195550123", error: errorText(new Error("no client serves +19195550123")) });
    } finally {
      restore();
    }
    expect(lines).toHaveLength(1);
    expect(lines[0]).not.toContain("5550123");
    expect(lines[0]).toContain("+1•••0123");
  });
});

describe("Telegram ids, usernames and bot tokens", () => {
  test("a user id keeps its last 3 digits; a username its last 3 characters", () => {
    expect(maskTelegramId("987654321")).toBe("•••321");
    expect(maskUsername("@maya_lopez")).toBe("@•••pez");
    expect(maskUsername("maya_lopez")).toBe("@•••pez");
    expect(maskHandle("987654321")).toBe("•••321");
    expect(maskHandle("@maya_lopez")).toBe("@•••pez");
  });

  test("a DM's space id is the user id and is masked; a group's negative id stays readable", () => {
    expect(maskSpaceId("987654321")).toBe("•••321");
    expect(maskSpaceId("12345")).toBe("•••345");
    expect(maskSpaceId("-1001234567890")).toBe("-1001234567890");
    expect(maskSpaceId("any;-;+19195550123")).toBe("any;-;+1•••0123");
  });

  test("in free text: user ids, @usernames and bot tokens are masked; emails, phones, groups and small numbers keep their rules", () => {
    const text = maskText(
      "chat 987654321 (@maya_lopez) in -1001234567890 failed: GET /file/bot7000000001:AAHk3x9_secretsecretsecretsecret12/photos/1.jpg after 30000 ms, status 403; maya@icloud.com +19195550123",
    );
    expect(text).not.toContain("987654321");
    expect(text).toContain("•••321");
    expect(text).not.toContain("maya_lopez");
    expect(text).toContain("@•••pez");
    expect(text).not.toContain("secretsecret");
    expect(text).not.toContain("7000000001");
    expect(text).toContain("-1001234567890");
    expect(text).toContain("30000 ms");
    expect(text).toContain("status 403");
    expect(text).toContain("m•••@icloud.com");
    expect(text).toContain("+1•••0123");
  });

  test("decimals and a masked email's domain are left alone", () => {
    expect(maskText("lat 35.9132110")).toBe("lat 35.9132110");
    expect(maskText("maya@icloud.com")).toBe("m•••@icloud.com");
  });

  test("log lines mask Telegram ids and usernames", () => {
    const { lines, restore } = captureLogs();
    try {
      log.warn("send to 987654321 failed", { space: "987654321", from: maskHandle("987654321"), user: "@maya_lopez" });
    } finally {
      restore();
    }
    expect(lines[0]).not.toContain("987654321");
    expect(lines[0]).not.toContain("maya_lopez");
  });
});
