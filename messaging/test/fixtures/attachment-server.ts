// Child process for the attachment stream test in server.test.ts. It serves
// one attachment whose stream sends 3 bytes and then fails with a full phone
// number in its error, and prints its port as JSON on stdout. Bun prints a
// body stream's error itself, past every JS handler, so only a separate
// process's own stderr can show whether that error leaks.

import { startServer } from "../../src/server";

const phone = process.argv[2] ?? "+19195550123";

const server = startServer(
  {
    sharedSecret: "localtest",
    status: () => "connected",
    channels: ["imessage"],
    bridge: {
      send: async () => ({ space_id: "s" }),
      createGroup: async () => ({ space_id: "g" }),
      createInviteLink: async () => ({ url: "u" }),
      attachment: (id) =>
        id === "broken"
          ? {
              mimeType: "image/jpeg",
              name: "photo.jpg",
              stream: async () =>
                new ReadableStream<Uint8Array>({
                  start(controller) {
                    controller.enqueue(new TextEncoder().encode("JPE"));
                  },
                  async pull() {
                    await Bun.sleep(50);
                    throw new Error(`attachment download failed in chat any;-;${phone}`);
                  },
                }),
            }
          : undefined,
    },
  },
  { port: 0, hostname: "127.0.0.1" },
);

console.log(JSON.stringify({ port: server.port }));
