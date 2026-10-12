import { testContributorBundles } from "../../../../../tools/web/bundle-test";

// Web chat carries a conversation, a WebGL mark, a session rail, its own streaming Markdown
// package (healing, rendering and syntax highlighting, about 29 KB), and files: composer uploads,
// media in the log and the picture Viewer (about 12 KB), so it gets more room than a settings page.
testContributorBundles(new URL("../../../", import.meta.url), 96);
