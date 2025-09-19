import {EditorView} from "@codemirror/view"
import {Extension} from "@codemirror/state"
import {HighlightStyle, syntaxHighlighting} from "@codemirror/language"
import {tags as t} from "@lezer/highlight"

const invalid = "#ffffff"
const ivory = "#d9e1f2"
const stone = "#5c6773"
const darkBackground = "#21252b"
const highlightBackground = "#2c313a"
const background = "#282c34"
const tooltipBackground = "#353a42"
const selection = "#3E4451"
const cursor = "#528bff"

const keywordChocolate = "chocolate"
const namespaceBlue = "#6cb6ff"
const tagGreen = "#6df59a"
const typeLavender = "#6f9cff"
const variableWhite = "#eeeeee"
const functionAmber = "#fcbf49"
const definitionPink = "#ff6ec7"
const operatorOrange = "#ff9f43"
const stringGold = "#ffea7a"
const numberAqua = "#ff66ff"
const wildcardViolet = "#bd93f9"
const punctuationSnow = "#f5f7ff"
const slashRed = "#ff5c57"
const dotRed = "#ff0000"
const commentGray = "lightslategray"

export const oneDarkColors = {
  invalid,
  ivory,
  stone,
  darkBackground,
  highlightBackground,
  background,
  tooltipBackground,
  selection,
  cursor,
  keywordChocolate,
  namespaceBlue,
  tagGreen,
  typeLavender,
  variableWhite,
  functionAmber,
  definitionPink,
  operatorOrange,
  stringGold,
  numberAqua,
  wildcardViolet,
  punctuationSnow,
  slashRed,
  dotRed,
}

export const oneDarkTheme = EditorView.theme({
  "&": {
    color: ivory,
    backgroundColor: background,
  },

  ".cm-content span.cm-boon-module-slash": {
    color: `${slashRed} !important`,
    fontWeight: "700",
  },
  ".cm-content span.cm-boon-module-slash > span": {
    color: `${slashRed} !important`,
    fontWeight: "700",
  },
  ".cm-content span.cm-boon-function-name": {
    color: `${functionAmber} !important`,
    fontWeight: "600",
  },
  ".cm-content span.cm-boon-function-name > span": {
    color: `${functionAmber} !important`,
    fontWeight: "600",
  },
  ".cm-content span.cm-boon-variable-definition": {
    color: `${definitionPink} !important`,
    fontStyle: "italic",
    fontWeight: "600",
  },
  ".cm-content span.cm-boon-variable-definition > span": {
    color: `${definitionPink} !important`,
    fontStyle: "italic",
    fontWeight: "600",
  },
  ".cm-content span.cm-boon-dot": {
    color: `${dotRed} !important`,
    fontWeight: "700",
  },
  ".cm-content span.cm-boon-dot > span": {
    color: `${dotRed} !important`,
    fontWeight: "700",
  },
  ".cm-content span.cm-boon-apostrophe": {
    color: `${punctuationSnow} !important`,
    fontWeight: "700",
  },
  ".cm-content span.cm-boon-apostrophe > span": {
    color: `${punctuationSnow} !important`,
    fontWeight: "700",
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
  { tag: t.keyword, color: keywordChocolate, fontStyle: "italic", fontWeight: "bolder" },
  { tag: t.namespace, color: namespaceBlue },
  { tag: t.tagName, color: tagGreen },
  { tag: t.typeName, color: typeLavender },
  { tag: t.variableName, color: variableWhite },
  { tag: [t.operator, t.operatorKeyword], color: operatorOrange, fontWeight: "600" },
  { tag: [t.separator, t.paren, t.brace, t.squareBracket], color: punctuationSnow, fontWeight: "700" },
  { tag: t.number, color: numberAqua },
  { tag: [t.string, t.processingInstruction, t.inserted], color: stringGold },
  { tag: [t.lineComment, t.comment, t.meta], color: commentGray, fontStyle: "italic" },
  { tag: t.special(t.variableName), color: wildcardViolet },
  { tag: t.strong, fontWeight: "bold" },
  { tag: t.emphasis, fontStyle: "italic" },
  { tag: t.strikethrough, textDecoration: "line-through" },
  { tag: t.link, color: namespaceBlue, textDecoration: "underline" },
  { tag: t.heading, fontWeight: "bold", color: keywordChocolate },
  { tag: t.invalid, color: invalid },
])

export const oneDark: Extension = [oneDarkTheme, syntaxHighlighting(oneDarkHighlightStyle)]
