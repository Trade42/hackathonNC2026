import { describe, expect, test } from "bun:test";
import { Bridge } from "../src/bridge";
import { CoreClient } from "../src/core-client";
import { Supervisor } from "../src/supervisor";
import { FakeTransport, message, until } from "./helpers";

describe("echo mode", () => {
  test("replies echo: <text> inside space.responding and never calls core", async () => {
    let coreCalls = 0;
    const core = {
      forward: async () => {
        coreCalls++;
        return [];
      },
    } as unknown as CoreClient;
    const transport = new FakeTransport();
    let supervisor: Supervisor | undefined;
    const bridge = new Bridge({
      core,
      sidecarUrl: "http://127.0.0.1:8120",
      echo: true,
      agentPhone: undefined,
      channels: ["imessage"],
      transport: () => supervisor?.current(),
    });
    supervisor = new Supervisor({ factory: async () => transport, onMessage: (m) => bridge.dispatch(m) });
    const running = supervisor.run();

    const dm = transport.space("any;-;+19195550101");
    const group = transport.space("any;+;chat1", true, ["+19195550101", "+19195550102"]);
    transport.queue.push(message(dm, { text: "hi ferry" }));
    transport.queue.push(message(group, { text: "heading to Sup Dogs" }));
    transport.queue.push(message(dm, { text: "" }));

    await until(() => dm.sent.length === 2 && group.sent.length === 1);
    await bridge.idle();
    expect(dm.events).toEqual(["typing:start", "reply:echo: hi ferry", "typing:stop", "typing:start", "reply:echo: (no text)", "typing:stop"]);
    expect(group.events).toEqual(["typing:start", "reply:echo: heading to Sup Dogs", "typing:stop"]);
    expect(coreCalls).toBe(0);

    await supervisor.stop();
    await running;
    expect(transport.stopped).toBe(true);
  });
});
