import { testContributorBundles } from "../../../../../tools/web/bundle-test";

// Web chat carries a conversation, a WebGL mark, a session rail and its own streaming Markdown
// package (healing, rendering and syntax highlighting, about 29 KB), so it gets more room than a
// settings page.
testContributorBundles(new URL("../../../", import.meta.url), 80);
