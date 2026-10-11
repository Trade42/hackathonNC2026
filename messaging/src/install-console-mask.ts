// index.ts imports this first. ES modules run every import before their own
// body, so a call inside index.ts would only mask the console after
// spectrum-ts had loaded.

import { installConsoleMask } from "./console-mask";

installConsoleMask();
