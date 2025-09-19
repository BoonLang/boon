import {EditorView} from "@codemirror/view"
import {Extension} from "@codemirror/state"
import {HighlightStyle, syntaxHighlighting} from "@codemirror/language"
import {tags as t} from "@lezer/highlight"

const chalky = "#e5c07b"
const coral = "#e06c75"
const cyan = "#56b6c2"
const invalid = "#ffffff"
const ivory = "#abb2bf"
const stone = "#7d8799"
const malibu = "#61afef"
const sage = "#98c379"
const whiskey = "#d19a66"
const violet = "#c678dd"
const darkBackground = "#21252b"
const highlightBackground = "#2c313a"
const background = "#282c34"
const tooltipBackground = "#353a42"
const selection = "#3E4451"
const cursor = "#528bff"

export const oneDarkColors = {
  chalky,
  coral,
  cyan,
  invalid,
  ivory,
  stone,
  malibu,
  sage,
  whiskey,
  violet,
  darkBackground,
  highlightBackground,
  background,
  tooltipBackground,
  selection,
  cursor,
}

export const oneDarkTheme = EditorView.theme({
  "&": {
    color: ivory,
    backgroundColor: background,
  },

  ".cm-content span.cm-boon-module-slash": {
    color: "#ff9a44 !important",
  },
  ".cm-content span.cm-boon-module-slash > span": {
    color: "#ff9a44 !important",
  },

  ".cm-content": {
    caretColor: cursor,
  },

  ".cm-cursor, .cm-dropCursor": { borderLeftColor: cursor },
  "&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground, .cm-content ::selection": { backgroundColor: selection },

  ".cm-panels": { backgroundColor: darkBackground, color: ivory },
  ".cm-panels.cm-panels-top": { borderBottom: "2px solid black" },
  ".cm-panels.cm-panels-bottom": { borderTop: "2px solid black" },

  ".cm-searchMatch": {
    backgroundColor: "#72a1ff59",
    outline: "1px solid #457dff",
  },
  ".cm-searchMatch.cm-searchMatch-selected": {
    backgroundColor: "#6199ff2f",
  },

  ".cm-activeLine": { backgroundColor: "#6699ff0b" },
  ".cm-selectionMatch": { backgroundColor: "#aafe661a" },

  "&.cm-focused .cm-matchingBracket, &.cm-focused .cm-nonmatchingBracket": {
    backgroundColor: "#bad0f847",
  },

  ".cm-gutters": {
    backgroundColor: background,
    color: stone,
    border: "none",
  },

  ".cm-activeLineGutter": {
    backgroundColor: highlightBackground,
  },

  ".cm-foldPlaceholder": {
    backgroundColor: "transparent",
    border: "none",
    color: "#ddd",
  },

  ".cm-tooltip": {
    border: "none",
    backgroundColor: tooltipBackground,
  },
  ".cm-tooltip .cm-tooltip-arrow:before": {
    borderTopColor: "transparent",
    borderBottomColor: "transparent",
  },
  ".cm-tooltip .cm-tooltip-arrow:after": {
    borderTopColor: tooltipBackground,
    borderBottomColor: tooltipBackground,
  },
  ".cm-tooltip-autocomplete": {
    "& > ul > li[aria-selected]": {
      backgroundColor: highlightBackground,
      color: ivory,
    },
  },
}, { dark: true })

export const oneDarkHighlightStyle = HighlightStyle.define([
  { tag: t.keyword, color: violet },
  { tag: t.namespace, color: malibu },
  { tag: t.tagName, color: coral },
  { tag: t.typeName, color: chalky },
  { tag: t.variableName, color: whiskey },
  { tag: [t.operator, t.operatorKeyword, t.special(t.string)], color: cyan },
  { tag: [t.separator, t.paren, t.brace, t.squareBracket], color: ivory },
  { tag: t.number, color: chalky },
  { tag: [t.string, t.processingInstruction, t.inserted], color: sage },
  { tag: [t.lineComment, t.comment, t.meta], color: stone },
  { tag: t.special(t.variableName), color: whiskey },
  { tag: t.strong, fontWeight: "bold" },
  { tag: t.emphasis, fontStyle: "italic" },
  { tag: t.strikethrough, textDecoration: "line-through" },
  { tag: t.link, color: stone, textDecoration: "underline" },
  { tag: t.heading, fontWeight: "bold", color: coral },
  { tag: t.invalid, color: invalid },
])

export const oneDark: Extension = [oneDarkTheme, syntaxHighlighting(oneDarkHighlightStyle)]
