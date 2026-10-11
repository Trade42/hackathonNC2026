// Entry point: load the root .env, start the internal API, then keep a
// Spectrum connection alive for the chosen providers (or log once why one is
// disabled).

// Must stay the first import: it masks console output before spectrum-ts loads.
import "./install-console-mask";
import { Bridge } from "./bridge";
import { activeChannels, readConfig, startupErrors, startupWarnings } from "./config";
import { CoreClient } from "./core-client";
import { loadEnv } from "./env";
import { errorText, log } from "./log";
import { startServer } from "./server";
import { spectrumFactory } from "./spectrum";
import { Supervisor } from "./supervisor";

async function main(): Promise<void> {
  const { env, file } = loadEnv();
  const config = readConfig(env);
  const channels = activeChannels(config);

  let supervisor: Supervisor | undefined;
  const bridge = new Bridge({
    core: new CoreClient({ coreUrl: config.coreUrl, sharedSecret: config.sharedSecret }),
    sidecarUrl: config.sidecarUrl,
    echo: config.echo,
    agentPhone: config.agentPhone,
    channels,
    transport: () => supervisor?.current(),
  });
  supervisor = new Supervisor({
    factory:
      config.photon && channels.length > 0
        ? spectrumFactory({
            photon: config.photon,
            imessage: channels.includes("imessage"),
            telegram: channels.includes("telegram") ? config.telegram : undefined,
          })
        : undefined,
    onMessage: (msg) => bridge.dispatch(msg),
    onDisconnect: () => bridge.reset(),
  });

  const server = startServer(
    { sharedSecret: config.sharedSecret, bridge, status: () => supervisor.status, channels },
    { port: config.port, hostname: config.hostname },
  );
  log.info("sidecar listening", {
    url: `http://${config.hostname}:${server.port}`,
    core: config.coreUrl,
    env_file: file ?? "(none)",
    providers: channels.join(",") || "(none)",
    echo: config.echo,
  });

  for (const error of startupErrors(config)) log.error(error);
  for (const warning of startupWarnings(config)) log.warn(warning);
  if (channels.length === 0) {
    log.warn("no messaging provider connects; serving the internal API only");
  } else {
    if (channels.includes("telegram") && config.telegramBotUsername) {
      log.info("telegram bot", { link: `https://t.me/${config.telegramBotUsername}` });
    }
    if (config.echo) log.info("echo mode: replying \"echo: <text>\" without calling core");
  }

  let stopping = false;
  const shutdown = async (signal: string) => {
    if (stopping) return;
    stopping = true;
    log.info("shutting down", { signal });
    await supervisor.stop();
    await server.stop();
    process.exit(0);
  };
  process.on("SIGINT", () => void shutdown("SIGINT"));
  process.on("SIGTERM", () => void shutdown("SIGTERM"));

  await supervisor.run();
}

main().catch((err) => {
  log.error("sidecar failed to start", { error: errorText(err) });
  process.exit(1);
});
