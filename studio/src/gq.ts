// GQ support for the script editor: syntax highlighting, completion and
// splitting a script into statements.

import { type CompletionContext, type CompletionResult } from "@codemirror/autocomplete";
import { StreamLanguage, type StringStream } from "@codemirror/language";

export const KEYWORDS = [
  "MATCH", "OPTIONAL", "WHERE", "RETURN", "WITH", "UNWIND", "AS", "DISTINCT", "ORDER", "BY", "ASC", "DESC",
  "SKIP", "LIMIT", "CREATE", "SET", "REMOVE", "DELETE", "DETACH", "RETRACT", "VALID", "KNOWN", "AT", "AND",
  "OR", "XOR", "NOT", "IN", "IS", "NULL", "TRUE", "FALSE", "STARTS", "ENDS", "CONTAINS",
  "MERGE", "ON", "UNWIND", "UNION", "ALL", "CALL", "YIELD", "FOREACH", "CASE", "WHEN", "THEN",
  "ELSE", "END", "EXISTS", "EXPLAIN", "PROFILE",
];

export const FUNCTIONS = [
  "all", "any", "none", "single", "reduce", "exists", "head", "size",
  "abs", "avg", "ceil", "coalesce", "collect", "count", "date", "endNode", "floor", "id", "keys", "labels",
  "last", "length", "max", "min", "now", "properties", "range", "replace", "round", "split", "sqrt",
  "startNode", "substring", "sum", "time", "toFloat", "toInteger", "toLower", "toString", "toUpper", "trim",
  "type",
];

export const PSEUDO_PROPERTIES = [
  "@id", "@labels", "@text", "@embedding", "@sources", "@created_at", "@updated_at", "@importance",
  "@accesses", "@last_access", "@label", "@from", "@to", "@valid_from", "@valid_to", "@recorded_at",
  "@expired_at",
];

const KEYWORD_SET = new Set(KEYWORDS);
const FUNCTION_SET = new Set(FUNCTIONS.map((name) => name.toLowerCase()));

interface State {
  /** The quote of a string that continues on the next line. */
  quote: string | null;
}

function readString(stream: StringStream, state: State): string {
  const quote = state.quote!;
  while (!stream.eol()) {
    const char = stream.next();
    if (char === "\\") stream.next();
    else if (char === quote) {
      state.quote = null;
      break;
    }
  }
  return "string";
}

/** Highlighting for GQ, a Cypher-style language. */
export const gqLanguage = StreamLanguage.define<State>({
  name: "gq",
  startState: () => ({ quote: null }),
  token(stream, state) {
    if (state.quote) return readString(stream, state);
    if (stream.eatSpace()) return null;
    if (stream.match("//")) {
      stream.skipToEnd();
      return "comment";
    }
    const char = stream.peek()!;
    if (char === "'" || char === '"') {
      stream.next();
      state.quote = char;
      return readString(stream, state);
    }
    if (char === "`") {
      stream.next();
      while (!stream.eol() && stream.next() !== "`");
      return "variableName";
    }
    if (stream.match(/^\$[\w]+/)) return "variableName.special";
    if (stream.match(/^@[a-z_]+/i)) return "propertyName.special";
    if (stream.match(/^-?\d+(\.\d+)?([eE][+-]?\d+)?/)) return "number";
    if (stream.match(/^:[A-Za-z_][\w]*/)) return "typeName";
    if (stream.match(/^[A-Za-z_][\w]*/)) {
      const word = stream.current();
      if (KEYWORD_SET.has(word.toUpperCase())) return "keyword";
      if (FUNCTION_SET.has(word.toLowerCase()) && stream.peek() === "(") return "function(variableName)";
      return "variableName";
    }
    if (stream.match(/^(->|<-|<>|<=|>=|=~|\.\.)/)) return "operator";
    stream.next();
    return /[()[\]{}]/.test(char) ? "bracket" : /[,;.]/.test(char) ? "punctuation" : "operator";
  },
  languageData: { commentTokens: { line: "//" } },
});

/** Completion for keywords, functions, pseudo-properties and the namespace's labels. */
export function gqCompletion(schema: () => { labels: string[]; edgeLabels: string[] }) {
  return (context: CompletionContext): CompletionResult | null => {
    const label = context.matchBefore(/:[A-Za-z_]\w*|:/);
    if (label) {
      const { labels, edgeLabels } = schema();
      return {
        from: label.from + 1,
        options: [
          ...labels.map((name) => ({ label: name, type: "type", detail: "node label" })),
          ...edgeLabels.map((name) => ({ label: name, type: "type", detail: "edge label" })),
        ],
        validFor: /^\w*$/,
      };
    }
    const pseudo = context.matchBefore(/@\w*/);
    if (pseudo) {
      return {
        from: pseudo.from,
        options: PSEUDO_PROPERTIES.map((name) => ({ label: name, type: "property" })),
        validFor: /^@\w*$/,
      };
    }
    const word = context.matchBefore(/\w+/);
    if (!word || (word.from === word.to && !context.explicit)) return null;
    return {
      from: word.from,
      options: [
        ...KEYWORDS.map((name) => ({ label: name, type: "keyword" })),
        ...FUNCTIONS.map((name) => ({ label: name, type: "function", apply: `${name}(` })),
      ],
      validFor: /^\w*$/,
    };
  };
}

export interface Statement {
  text: string;
  /** Offset of the statement's first character in the script. */
  from: number;
  to: number;
  /** 1-based line of `from`. */
  line: number;
}

/**
 * Splits a script at the semicolons that are outside strings, backtick
 * names and comments. Empty statements (only blanks and comments) are left
 * out.
 */
export function splitStatements(script: string): Statement[] {
  const statements: Statement[] = [];
  let start = 0;
  let i = 0;
  const push = (end: number) => {
    const raw = script.slice(start, end);
    const lead = raw.length - raw.trimStart().length;
    const text = raw.trim();
    const meaningful = text.replace(/\/\/[^\n]*/g, "").trim();
    if (meaningful) {
      const from = start + lead;
      statements.push({ text, from, to: from + text.length, line: script.slice(0, from).split("\n").length });
    }
  };
  while (i < script.length) {
    const char = script[i];
    if (char === "/" && script[i + 1] === "/") {
      while (i < script.length && script[i] !== "\n") i += 1;
      continue;
    }
    if (char === "'" || char === '"' || char === "`") {
      i += 1;
      while (i < script.length && script[i] !== char) {
        if (script[i] === "\\" && char !== "`") i += 1;
        i += 1;
      }
      i += 1;
      continue;
    }
    if (char === ";") {
      push(i);
      start = i + 1;
    }
    i += 1;
  }
  push(script.length);
  return statements;
}

/** The statement under the cursor (or the last one before it). */
export function statementAt(script: string, offset: number): Statement | undefined {
  const statements = splitStatements(script);
  return (
    statements.find((statement) => offset >= statement.from && offset <= statement.to + 1) ??
    [...statements].reverse().find((statement) => statement.to <= offset) ??
    statements[0]
  );
}

/** The line and column a GQ error message names, if any. */
export function errorPosition(message: string): { line: number; column: number } | null {
  const match = message.match(/\(line (\d+), column (\d+)\)\s*$/);
  return match ? { line: Number(match[1]), column: Number(match[2]) } : null;
}
