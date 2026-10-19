/** Live Editor's page: loads what this browser kept, then follows the address bar. */

import { App } from "./app/app";
import { Shell } from "./ui/shell";
import { InstallPage } from "./ui/install";
import { NewApp } from "./ui/new-app";
import { SettingsPage } from "./ui/settings";
import { Workspace } from "./ui/workspace";
import { floatTooltips } from "./ui/tooltips";
import { attachAgent } from "./agent/agent";

const base = new URL("./", import.meta.url);
const app = new App(base);
attachAgent(app);
const root = document.getElementById("app") as HTMLElement;
floatTooltips(root);
new Shell(app, root, (shell) => {
  switch (app.route.page) {
    case "app":
      return new Workspace(app, shell.menus);
    case "install":
      return new InstallPage(app);
    case "settings":
      return new SettingsPage(app);
    default:
      return new NewApp(app, shell.menus);
  }
});
await app.load();
app.navigate(location.hash);
addEventListener("hashchange", () => app.navigate(location.hash));
