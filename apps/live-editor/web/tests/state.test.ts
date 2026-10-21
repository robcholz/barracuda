// The project model, diffs and the manifest check.
import { expect, test } from "bun:test";
import { hunks } from "../src/state/diff";
import { manifestProblem } from "../src/manifest";
import {
  appId,
  applyProposal,
  createProject,
  currentVersion,
  lineCounts,
  proposed,
  restore,
  summarize,
  withFiles,
} from "../src/state/project";
import { BLANK } from "../src/app/templates";

test("an app ID comes from the description, and never repeats one", () => {
  expect(appId("A kitchen timer with big digits", [])).toBe(
    "kitchen-timer-big",
  );
  expect(appId("A kitchen timer with big digits", ["kitchen-timer-big"])).toBe(
    "kitchen-timer-big-2",
  );
  expect(appId("计数器", [])).toBe("app");
  expect(appId("2048 game", [])).toBe("app-2048-game");
});

test("applying a proposal keeps a version, and restoring keeps the newer ones", () => {
  const project = createProject("hello", BLANK, "round", 1);
  expect(JSON.parse(project.files["app.json"]).id).toBe("hello");
  expect(currentVersion(project)?.n).toBe(1);

  const lua = `${project.files["main.lua"]}-- more\n`;
  const proposing = {
    ...project,
    proposal: {
      turn: "t",
      title: "More",
      files: { "main.lua": lua, "app.tsx": null },
    },
  };
  expect(proposed(proposing)["main.lua"]).toBe(lua);
  expect("app.tsx" in proposed(proposing)).toBe(false);
  expect(summarize(project.files, proposing.proposal.files)).toEqual([
    { name: "app.tsx", add: 0, del: 11 },
    { name: "main.lua", add: 1, del: 0 },
  ]);

  const applied = applyProposal(proposing, 2);
  expect(applied.proposal).toBeNull();
  expect(applied.versions.map((v) => [v.n, v.title])).toEqual([
    [1, "First version"],
    [2, "More"],
  ]);

  // Edits not in any version are kept as one before a restore.
  const edited = withFiles(
    applied,
    { ...applied.files, "main.lua": "print(1)\n" },
    3,
  );
  expect(currentVersion(edited)).toBeNull();
  const restored = restore(edited, 1, 4);
  expect(restored.files).toEqual(project.files);
  expect(restored.versions.map((v) => v.title)).toEqual([
    "First version",
    "More",
    "Your edits",
  ]);
  expect(currentVersion(restored)?.n).toBe(1);
});

test("the manifest follows its name into the project", () => {
  const project = createProject("hello", BLANK, "round", 1);
  const manifest = { ...JSON.parse(project.files["app.json"]), name: "Hello" };
  expect(
    withFiles(project, {
      ...project.files,
      "app.json": JSON.stringify(manifest),
    }).name,
  ).toBe("Hello");
});

test("line counts and hunks follow the longest common lines", () => {
  expect(lineCounts("a\nb\nc", "a\nB\nc\nd")).toEqual({ add: 2, del: 1 });
  const rows = hunks(
    Array.from({ length: 12 }, (_, i) => `line ${i}`).join("\n"),
    Array.from({ length: 12 }, (_, i) =>
      i === 6 ? "changed" : `line ${i}`,
    ).join("\n"),
  );
  expect(rows[0]).toEqual({ hunk: "@@ 4 @@" });
  expect(rows.filter((row) => "kind" in row && row.kind !== " ")).toEqual([
    { kind: "-", old: 7, text: "line 6" },
    { kind: "+", new: 7, text: "changed" },
  ]);
  expect(rows.length).toBe(1 + 3 + 2 + 3);
});

test("a manifest the device would reject fails the build first", () => {
  const good = {
    format: 1,
    id: "counter",
    name: "Counter",
    version: "0.1.0",
    view: { program: "view.bvir", styles: "styles.bin" },
  };
  expect(manifestProblem(good)).toBeNull();
  expect(manifestProblem({ ...good, format: 2 })).toBe("$.format must be 1");
  expect(manifestProblem({ ...good, colour: "red" })).toBe(
    "$.colour is not a manifest field",
  );
  expect(manifestProblem({ ...good, id: "Counter" })).toBe(
    "$.id may hold only a-z, 0-9 and -",
  );
  expect(manifestProblem({ ...good, name: "" })).toBe(
    "$.name must not be empty",
  );
  expect(manifestProblem({ ...good, view: { program: "view.bvir" } })).toBe(
    "$.view.styles is missing",
  );
  expect(
    manifestProblem({ ...good, view: { ...good.view, shape: "round" } }),
  ).toBe("$.view.shape is not a manifest field");
  expect(
    manifestProblem({ ...good, requires: ["notify@2"], uses: ["notify"] }),
  ).toBe(
    "notify is named more than once across requires, uses and permissions",
  );
});
