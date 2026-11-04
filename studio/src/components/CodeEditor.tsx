import { autocompletion, closeBrackets, closeBracketsKeymap, completionKeymap } from "@codemirror/autocomplete";
import { defaultKeymap, history, historyKeymap, indentWithTab, toggleComment } from "@codemirror/commands";
import { HighlightStyle, bracketMatching, indentOnInput, syntaxHighlighting } from "@codemirror/language";
import { EditorSelection, EditorState, StateEffect, StateField } from "@codemirror/state";
import {
  Decoration,
  type DecorationSet,
  EditorView,
  drawSelection,
  highlightActiveLine,
  highlightActiveLineGutter,
  keymap,
  lineNumbers,
  placeholder,
} from "@codemirror/view";
import { tags } from "@lezer/highlight";
import { forwardRef, useEffect, useImperativeHandle, useRef } from "react";

import { gqCompletion, gqLanguage } from "../gq.ts";

const highlight = HighlightStyle.define([
  { tag: tags.keyword, color: "var(--syntax-keyword)", fontWeight: "600" },
  { tag: tags.string, color: "var(--syntax-string)" },
  { tag: tags.number, color: "var(--syntax-number)" },
  { tag: tags.comment, color: "var(--syntax-comment)", fontStyle: "italic" },
  { tag: tags.typeName, color: "var(--syntax-label)" },
  { tag: tags.function(tags.variableName), color: "var(--syntax-function)" },
  { tag: tags.special(tags.propertyName), color: "var(--syntax-pseudo)" },
  { tag: tags.special(tags.variableName), color: "var(--syntax-param)" },
  { tag: [tags.operator, tags.punctuation, tags.bracket], color: "var(--muted)" },
]);

const theme = EditorView.theme({
  "&": { height: "100%", fontSize: "13.5px", backgroundColor: "transparent", color: "var(--text)" },
  ".cm-scroller": { fontFamily: "var(--font-mono)", lineHeight: "1.6" },
  ".cm-content": { caretColor: "var(--accent)", padding: "10px 0" },
  ".cm-gutters": { backgroundColor: "transparent", color: "var(--faint)", border: "none", paddingLeft: "6px" },
  ".cm-activeLineGutter": { backgroundColor: "transparent", color: "var(--text)" },
  ".cm-activeLine": { backgroundColor: "var(--hover)" },
  "&.cm-focused": { outline: "none" },
  "&.cm-focused .cm-selectionBackground, .cm-selectionBackground, ::selection": { backgroundColor: "var(--selection) !important" },
  ".cm-cursor": { borderLeftColor: "var(--accent)", borderLeftWidth: "2px" },
  ".cm-matchingBracket": { backgroundColor: "var(--accent-soft)", outline: "none" },
  ".cm-tooltip": { backgroundColor: "var(--panel-raised)", border: "1px solid var(--border)", borderRadius: "8px", overflow: "hidden" },
  ".cm-tooltip-autocomplete ul li[aria-selected]": { backgroundColor: "var(--accent-soft)", color: "var(--text)" },
  ".cm-placeholder": { color: "var(--faint)" },
  ".cm-error-line": { backgroundColor: "var(--danger-soft)" },
  ".cm-running-line": { backgroundColor: "var(--accent-soft)" },
});

const setMarks = StateEffect.define<{ error?: number; running?: [number, number] } | null>();

/** Highlights the line of an error and the statement being run. */
const marks = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(value, transaction) {
    value = value.map(transaction.changes);
    for (const effect of transaction.effects) {
      if (!effect.is(setMarks)) continue;
      const ranges = [];
      const doc = transaction.state.doc;
      if (effect.value?.running) {
        const [from, to] = effect.value.running;
        const first = doc.lineAt(Math.min(from, doc.length)).number;
        const last = doc.lineAt(Math.min(to, doc.length)).number;
        for (let line = first; line <= last; line += 1) {
          ranges.push(Decoration.line({ class: "cm-running-line" }).range(doc.line(line).from));
        }
      }
      if (effect.value?.error && effect.value.error <= doc.lines) {
        ranges.push(Decoration.line({ class: "cm-error-line" }).range(doc.line(effect.value.error).from));
      }
      value = Decoration.set(ranges, true);
    }
    return value;
  },
  provide: (field) => EditorView.decorations.from(field),
});

export interface CodeEditorHandle {
  /** The selected text, if any, with its offset. */
  selection(): { text: string; from: number } | null;
  cursor(): number;
  focus(): void;
  insert(text: string): void;
  /** Moves the cursor to a 1-based line and column. */
  goto(line: number, column: number): void;
  mark(marks: { error?: number; running?: [number, number] } | null): void;
}

interface Props {
  value: string;
  onChange: (value: string) => void;
  onRun: (all: boolean) => void;
  schema: () => { labels: string[]; edgeLabels: string[] };
}

export const CodeEditor = forwardRef<CodeEditorHandle, Props>(function CodeEditor({ value, onChange, onRun, schema }, ref) {
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  // Callbacks change on every render; the editor reads the latest ones.
  const latest = useRef({ onChange, onRun, schema });
  latest.current = { onChange, onRun, schema };

  useEffect(() => {
    if (!host.current) return;
    const runKeys = keymap.of([
      { key: "Mod-Enter", run: () => (latest.current.onRun(false), true) },
      { key: "Shift-Mod-Enter", run: () => (latest.current.onRun(true), true) },
      { key: "F5", run: () => (latest.current.onRun(true), true) },
      { key: "Mod-/", run: toggleComment },
    ]);
    view.current = new EditorView({
      parent: host.current,
      state: EditorState.create({
        doc: value,
        extensions: [
          runKeys,
          lineNumbers(),
          highlightActiveLineGutter(),
          highlightActiveLine(),
          history(),
          drawSelection(),
          indentOnInput(),
          bracketMatching(),
          closeBrackets(),
          autocompletion({ override: [gqCompletion(() => latest.current.schema())] }),
          gqLanguage,
          syntaxHighlighting(highlight),
          theme,
          marks,
          placeholder("// Write GQ statements, each ending with ;\nMATCH (n) RETURN n LIMIT 25;"),
          keymap.of([...closeBracketsKeymap, ...defaultKeymap, ...historyKeymap, ...completionKeymap, indentWithTab]),
          EditorView.updateListener.of((update) => {
            if (update.docChanged) latest.current.onChange(update.state.doc.toString());
          }),
        ],
      }),
    });
    return () => view.current?.destroy();
    // The editor is created once; `value` changes are pushed below.
  }, []);

  // Replace the document when another script is opened.
  useEffect(() => {
    const editor = view.current;
    if (editor && editor.state.doc.toString() !== value) {
      editor.dispatch({ changes: { from: 0, to: editor.state.doc.length, insert: value }, effects: setMarks.of(null) });
    }
  }, [value]);

  useImperativeHandle(ref, () => ({
    selection() {
      const editor = view.current;
      if (!editor) return null;
      const range = editor.state.selection.main;
      return range.empty ? null : { text: editor.state.sliceDoc(range.from, range.to), from: range.from };
    },
    cursor: () => view.current?.state.selection.main.head ?? 0,
    focus: () => view.current?.focus(),
    insert(text: string) {
      const editor = view.current;
      if (!editor) return;
      editor.dispatch(editor.state.replaceSelection(text));
      editor.focus();
    },
    goto(line: number, column: number) {
      const editor = view.current;
      if (!editor) return;
      const doc = editor.state.doc;
      const target = doc.line(Math.min(Math.max(line, 1), doc.lines));
      const pos = Math.min(target.from + Math.max(column - 1, 0), target.to);
      editor.dispatch({ selection: EditorSelection.cursor(pos), scrollIntoView: true });
      editor.focus();
    },
    mark(value) {
      view.current?.dispatch({ effects: setMarks.of(value) });
    },
  }));

  return <div className="code-editor" ref={host} />;
});
