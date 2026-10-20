/**
 * The Code view: CodeMirror over the open app's sources, with the design's file menu, find bar,
 * file actions and the build's problem, drawn from the design system's tokens.
 */

import {
  autocompletion,
  closeBrackets,
  closeBracketsKeymap,
  completionKeymap,
} from "@codemirror/autocomplete";
import {
  defaultKeymap,
  history,
  historyKeymap,
  indentWithTab,
} from "@codemirror/commands";
import { javascript } from "@codemirror/lang-javascript";
import { json } from "@codemirror/lang-json";
import {
  bracketMatching,
  HighlightStyle,
  indentOnInput,
  StreamLanguage,
  syntaxHighlighting,
} from "@codemirror/language";
import { lua } from "@codemirror/legacy-modes/mode/lua";
import {
  linter,
  lintGutter,
  setDiagnostics,
  type Diagnostic,
} from "@codemirror/lint";
import {
  findNext,
  findPrevious,
  getSearchQuery,
  highlightSelectionMatches,
  search,
  SearchQuery,
  setSearchQuery,
} from "@codemirror/search";
import {
  EditorState,
  StateEffect,
  StateField,
  type Extension,
} from "@codemirror/state";
import {
  Decoration,
  drawSelection,
  EditorView,
  highlightActiveLine,
  highlightActiveLineGutter,
  keymap,
  lineNumbers,
  type DecorationSet,
} from "@codemirror/view";
import { tags } from "@lezer/highlight";
import type { App } from "../app/app";
import { t } from "../i18n";
import { required } from "../state/project";
import { button, fill, h, icon, iconButton, status } from "./dom";
import type { Pane } from "./panes";
import type { Menus } from "./shell";

/** The web chat's token colours (`.bc-tok-*`), for CodeMirror's highlighter. */
const highlight = HighlightStyle.define([
  {
    tag: [
      tags.keyword,
      tags.controlKeyword,
      tags.operatorKeyword,
      tags.definitionKeyword,
      tags.modifier,
    ],
    color: "var(--red-900)",
  },
  {
    tag: [
      tags.function(tags.variableName),
      tags.function(tags.propertyName),
      tags.typeName,
      tags.tagName,
    ],
    color: "var(--blue-900)",
  },
  {
    tag: [tags.string, tags.special(tags.string), tags.regexp],
    color: "var(--lime-900)",
  },
  {
    tag: [tags.number, tags.bool, tags.null, tags.atom],
    color: "var(--amber-900)",
  },
  {
    tag: [tags.comment, tags.lineComment, tags.blockComment],
    color: "var(--muted-foreground)",
    fontStyle: "italic",
  },
]);

function language(name: string): Extension {
  if (name.endsWith(".lua")) return StreamLanguage.define(lua);
  if (name.endsWith(".json")) return json();
  if (name.endsWith(".tsx")) return javascript({ jsx: true, typescript: true });
  if (name.endsWith(".ts")) return javascript({ typescript: true });
  return [];
}

/** A line to flash: brought into view by "Show problem" and the problem's link. */
const flashLine = StateEffect.define<number | null>();
const flashField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(value, transaction) {
    for (const effect of transaction.effects)
      if (effect.is(flashLine)) {
        if (effect.value === null) return Decoration.none;
        const line = transaction.state.doc.line(
          Math.min(effect.value, transaction.state.doc.lines),
        );
        return Decoration.set([
          Decoration.line({ class: "le-line--flash" }).range(line.from),
        ]);
      }
    return value.map(transaction.changes);
  },
  provide: (field) => EditorView.decorations.from(field),
});

export class CodePane implements Pane {
  readonly el: HTMLElement;
  private readonly bar = h("div", { class: "bc-pane__bar" });
  private readonly rename = h("div", { style: { display: "contents" } });
  private readonly find = h("div", { style: { display: "contents" } });
  private readonly problem = h("div", { style: { display: "contents" } });
  private readonly host = h("div", { class: "le-editor" });
  private readonly view: EditorView;
  private readonly states = new Map<string, EditorState>();
  private file = "";
  private finding = false;
  private query = "";
  private renaming: { value: string; error: string } | null = null;
  private key = "";
  private applying = false;
  private lastJump = 0;

  constructor(
    private readonly app: App,
    private readonly menus: Menus,
    private readonly onAsk: (text: string) => void,
  ) {
    this.view = new EditorView({ parent: this.host });
    this.el = h(
      "section",
      { class: "bc-pane le-view", "aria-label": t.views.code },
      this.bar,
      this.rename,
      this.find,
      h("div", { class: "bc-pane__body" }, this.host),
      this.problem,
    );
  }

  private state(name: string, text: string): EditorState {
    return EditorState.create({
      doc: text,
      extensions: [
        lineNumbers(),
        highlightActiveLineGutter(),
        history(),
        drawSelection(),
        indentOnInput(),
        bracketMatching(),
        closeBrackets(),
        autocompletion(),
        highlightActiveLine(),
        highlightSelectionMatches(),
        search(),
        lintGutter(),
        linter(null),
        flashField,
        syntaxHighlighting(highlight),
        language(name),
        EditorState.tabSize.of(2),
        EditorView.lineWrapping,
        keymap.of([
          { key: "Mod-f", run: () => (this.openFind(), true) },
          {
            key: "Escape",
            run: () => (this.finding ? (this.closeFind(), true) : false),
          },
          ...closeBracketsKeymap,
          ...defaultKeymap,
          ...historyKeymap,
          ...completionKeymap,
          indentWithTab,
        ]),
        EditorView.updateListener.of((update) => {
          if (!update.docChanged || this.applying) return;
          this.app.edit(this.file, update.state.doc.toString());
        }),
        EditorView.contentAttributes.of({ "aria-label": t.codeOf(name) }),
      ],
    });
  }

  /** Shows `name`, keeping each file's own undo history and selection. */
  private show(name: string, text: string) {
    if (this.file && this.file !== name)
      this.states.set(this.file, this.view.state);
    if (this.file !== name) {
      this.file = name;
      this.view.setState(this.states.get(name) ?? this.state(name, text));
    }
    const current = this.view.state.doc.toString();
    if (current !== text) {
      // The sources changed elsewhere: applied changes, a restored version.
      this.applying = true;
      this.view.dispatch({
        changes: { from: 0, to: current.length, insert: text },
      });
      this.applying = false;
    }
  }

  private openFind() {
    this.finding = true;
    this.key = "";
    this.update();
    requestAnimationFrame(() =>
      (this.find.querySelector("input") as HTMLInputElement | null)?.focus(),
    );
  }

  private closeFind() {
    this.finding = false;
    this.view.dispatch({
      effects: setSearchQuery.of(new SearchQuery({ search: "" })),
    });
    this.key = "";
    this.update();
    this.view.focus();
  }

  private matches(): { count: number; index: number } {
    const query = getSearchQuery(this.view.state);
    if (!query.search || !query.valid) return { count: 0, index: 0 };
    const cursor = query.getCursor(this.view.state);
    const head = this.view.state.selection.main.from;
    let count = 0;
    let index = 0;
    for (let next = cursor.next(); !next.done; next = cursor.next()) {
      count += 1;
      if (next.value.from <= head) index = count;
    }
    return { count, index };
  }

  update() {
    const app = this.app;
    const project = app.project();
    if (!project) return;
    const names = Object.keys(project.files).sort();
    const file =
      project.files[app.file] !== undefined
        ? app.file
        : names.includes("app.tsx")
          ? "app.tsx"
          : names[0];
    this.show(file, project.files[file] ?? "");
    const problem = app.build.status === "failed" ? app.build.problem : null;
    const locked = required(project.files).includes(file);

    if (app.jump && app.jump.at !== this.lastJump && app.jump.file === file) {
      this.lastJump = app.jump.at;
      const line = this.view.state.doc.line(
        Math.min(Math.max(1, app.jump.line), this.view.state.doc.lines),
      );
      this.view.dispatch({
        selection: { anchor: line.from },
        effects: [
          EditorView.scrollIntoView(line.from, { y: "center" }),
          flashLine.of(app.jump.line),
        ],
      });
      setTimeout(
        () => this.view.dispatch({ effects: flashLine.of(null) }),
        1200,
      );
      this.view.focus();
    }

    // The problem underlines its place in the file.
    const diagnostics: Diagnostic[] = [];
    if (problem?.file === file && problem.line) {
      const doc = this.view.state.doc;
      const line = doc.line(Math.min(problem.line, doc.lines));
      const from = Math.min(
        line.from + Math.max(0, (problem.column ?? 1) - 1),
        line.to,
      );
      diagnostics.push({
        from,
        to: Math.max(from + 1, line.to),
        severity: "error",
        message: problem.message,
      });
    }
    this.view.dispatch(setDiagnostics(this.view.state, diagnostics));

    // Typing in the find and rename fields must not rebuild them, so their text is not in the key.
    const key = JSON.stringify([
      file,
      names,
      problem,
      this.finding,
      this.renaming && [this.newFile, this.renaming.error],
      app.build.status,
      this.menus.open?.startsWith("code-") ? this.menus.open : null,
      project.name,
    ]);
    if (key === this.key) return;
    this.key = key;

    const problems = (name: string) => (problem?.file === name ? 1 : 0);
    fill(
      this.bar,
      h(
        "span",
        { class: "bc-menu-anchor" },
        h(
          "button",
          {
            class: "bc-button bc-button--outline bc-button--sm",
            type: "button",
            "aria-label": t.fileLabel(file),
            "aria-haspopup": "menu",
            "aria-expanded": String(this.menus.open === "code-files"),
            onClick: (event: Event) =>
              this.menus.toggle("code-files", event, 220),
          },
          icon("file-code"),
          h("span", { class: "bc-mono" }, file),
          icon("chevron-down"),
        ),
        this.menus.open === "code-files" &&
          h(
            "div",
            {
              class: "bc-menu le-menu",
              role: "menu",
              "aria-label": t.filesLabel,
              style: { ...this.menus.side(), minWidth: "220px" },
            },
            names.map((name) =>
              h(
                "button",
                {
                  class: "bc-menu-item",
                  type: "button",
                  role: "menuitemradio",
                  "aria-checked": String(name === file),
                  onClick: () => {
                    this.menus.close();
                    app.openFile(name);
                  },
                },
                h("span", { class: "bc-mono le-grow" }, name),
                problems(name) > 0 &&
                  h(
                    "span",
                    { class: "bc-caption bc-destructive" },
                    t.oneProblem,
                  ),
              ),
            ),
          ),
      ),
      h(
        "span",
        { class: "le-bar-text" },
        problem
          ? status(
              "destructive",
              "circle-alert",
              h("span", { class: "le-wide" }, t.oneProblem),
            )
          : app.build.status === "building"
            ? status(
                "muted",
                "loader-circle",
                h("span", { class: "le-wide" }, t.building),
              )
            : status(
                "success",
                "check",
                h("span", { class: "le-wide" }, t.noProblems),
              ),
      ),
      iconButton(
        "search",
        t.findInFile,
        () => (this.finding ? this.closeFind() : this.openFind()),
        { pressed: this.finding },
      ),
      h(
        "span",
        { class: "bc-menu-anchor" },
        iconButton(
          "ellipsis",
          t.moreActions,
          (event) => this.menus.toggle("code-more", event, 200),
          { expanded: this.menus.open === "code-more" },
        ),
        this.menus.open === "code-more" &&
          h(
            "div",
            {
              class: "bc-menu le-menu",
              role: "menu",
              "aria-label": t.fileActions,
              style: { ...this.menus.side(), minWidth: "200px" },
            },
            h(
              "button",
              {
                class: "bc-menu-item",
                type: "button",
                role: "menuitem",
                onClick: () => {
                  this.menus.close();
                  this.renaming = { value: "", error: "" };
                  this.newFile = true;
                  this.key = "";
                  this.update();
                },
              },
              icon("file-plus"),
              t.newFile,
            ),
            h(
              "button",
              {
                class: "bc-menu-item",
                type: "button",
                role: "menuitem",
                disabled: locked,
                onClick: () => {
                  this.menus.close();
                  this.newFile = false;
                  this.renaming = { value: file, error: "" };
                  this.key = "";
                  this.update();
                },
              },
              icon("pencil"),
              t.rename,
            ),
            h(
              "button",
              {
                class: "bc-menu-item bc-menu-item--destructive",
                type: "button",
                role: "menuitem",
                disabled: locked,
                onClick: (event: Event) => {
                  this.menus.toggle("code-delete", event, 280);
                },
              },
              icon("trash-2"),
              t.deleteFile,
            ),
            locked &&
              h(
                "p",
                { class: "le-menu-note" },
                t.needs(project.name, h("span", { class: "bc-mono" }, file)),
              ),
          ),
        this.menus.open === "code-delete" &&
          h(
            "div",
            {
              class: "bc-menu bc-menu--confirm",
              role: "alertdialog",
              "aria-labelledby": "le-delete-title",
              style: this.menus.side(),
            },
            h(
              "span",
              {
                style: { display: "flex", flexDirection: "column", gap: "4px" },
              },
              h(
                "span",
                { id: "le-delete-title", class: "bc-option-title" },
                t.deleteTitle,
              ),
              h(
                "span",
                { class: "bc-small bc-muted" },
                t.deleteText(h("span", { class: "bc-mono" }, file)),
              ),
            ),
            h(
              "span",
              {
                style: {
                  display: "flex",
                  justifyContent: "flex-end",
                  gap: "8px",
                },
              },
              button(t.cancel, () => this.menus.close(), {
                kind: "outline",
                sm: true,
              }),
              button(
                t.deleteFile,
                () => {
                  this.menus.close();
                  this.states.delete(file);
                  app.keepVersion(`Before deleting ${file}`);
                  app.deleteFile(file);
                },
                { kind: "danger", sm: true, icon: "trash-2" },
              ),
            ),
          ),
      ),
      iconButton("x", t.closeView(t.views.code), () => app.closeView("code")),
    );

    fill(
      this.rename,
      this.renaming &&
        h(
          "div",
          { class: "bc-pane__section bc-expanded" },
          h(
            "form",
            {
              class: "bc-expanded__panel",
              onSubmit: (event: Event) => {
                event.preventDefault();
                const renaming = this.renaming;
                if (!renaming) return;
                const name = renaming.value.trim();
                const error = this.newFile
                  ? app.addFile(name)
                  : app.renameFile(file, name);
                if (error) {
                  this.renaming = { value: renaming.value, error };
                } else {
                  if (!this.newFile) this.states.delete(file);
                  this.renaming = null;
                }
                this.key = "";
                this.update();
              },
            },
            h(
              "label",
              { class: "bc-field", style: { flex: "1 1 200px" } },
              h(
                "span",
                { class: "bc-label" },
                this.newFile ? t.fileName : t.newName,
              ),
              h("input", {
                class: "bc-input bc-input--mono",
                value: this.renaming.value,
                placeholder: this.newFile ? "util.lua" : "",
                spellcheck: "false",
                "aria-invalid": String(!!this.renaming.error),
                onInput: (event: Event) => {
                  if (this.renaming)
                    this.renaming.value = (
                      event.target as HTMLInputElement
                    ).value;
                },
              }),
              this.renaming.error &&
                h(
                  "span",
                  { class: "bc-hint bc-hint--error" },
                  this.renaming.error,
                ),
            ),
            h(
              "span",
              { class: "le-row" },
              button(
                t.cancel,
                () => {
                  this.renaming = null;
                  this.key = "";
                  this.update();
                },
                { kind: "outline" },
              ),
              button(this.newFile ? t.addFile : t.rename, () => {}, {
                type: "submit",
              }),
            ),
          ),
        ),
    );

    fill(
      this.find,
      this.finding &&
        h(
          "div",
          { class: "bc-pane__section le-row le-pad" },
          h(
            "label",
            { class: "bc-picker__search" },
            icon("search"),
            h("input", {
              class: "bc-input bc-input--mono",
              type: "text",
              "aria-label": t.findInFile,
              value: this.query,
              onInput: (event: Event) => {
                this.query = (event.target as HTMLInputElement).value;
                this.view.dispatch({
                  effects: setSearchQuery.of(
                    new SearchQuery({ search: this.query }),
                  ),
                });
                findNext(this.view);
                this.refreshFind();
              },
              onKeyDown: (event: Event) => {
                const key = event as KeyboardEvent;
                if (key.key === "Enter") {
                  key.preventDefault();
                  (key.shiftKey ? findPrevious : findNext)(this.view);
                  this.refreshFind();
                }
                if (key.key === "Escape") this.closeFind();
              },
            }),
          ),
          h(
            "span",
            { class: "bc-mono bc-caption bc-muted le-find-count" },
            this.findLabel(this.matches()),
          ),
          iconButton("chevron-up", t.previousMatch, () => {
            findPrevious(this.view);
            this.refreshFind();
          }),
          iconButton("chevron-down", t.nextMatch, () => {
            findNext(this.view);
            this.refreshFind();
          }),
        ),
    );

    fill(
      this.problem,
      problem &&
        h(
          "div",
          { class: "bc-pane__section le-pad" },
          h(
            "section",
            { class: "bc-card", "aria-label": t.problem },
            h(
              "div",
              { class: "bc-card__head" },
              icon("circle-alert", "bc-destructive"),
              h("h3", { class: "bc-title le-grow" }, t.buildFailed),
              problem.file &&
                h(
                  "button",
                  {
                    class: "bc-link bc-mono bc-caption",
                    type: "button",
                    onClick: () =>
                      app.openFile(problem.file ?? file, problem.line),
                  },
                  [problem.file, problem.line, problem.column]
                    .filter((part) => part !== undefined)
                    .join(":"),
                ),
            ),
            h("div", { class: "bc-card__body bc-small" }, problem.message),
            h(
              "div",
              { class: "bc-card__foot" },
              button(
                t.askToFix,
                () => this.onAsk(t.fixTheBuild(problem.message)),
                { kind: "outline", sm: true },
              ),
            ),
          ),
        ),
    );
  }

  private newFile = false;

  private findLabel(found: { count: number; index: number }) {
    if (!this.query) return "";
    return found.count ? t.matchOf(found.index || 1, found.count) : t.noMatches;
  }

  private refreshFind() {
    const label = this.find.querySelector(".le-find-count");
    if (label) label.textContent = this.findLabel(this.matches());
  }
}
