import { installQueuedFetch, queuedImport } from "./net";
import { startPortal } from "./shell";

installQueuedFetch(window);
startPortal({ load: queuedImport() });
