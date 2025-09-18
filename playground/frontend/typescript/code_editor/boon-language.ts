// boon-language.ts
import {LRLanguage, LanguageSupport} from "@codemirror/language"
import {styleTags, tags as t} from "@lezer/highlight"
// import generated parser (path must match your lezer output)
import {parser as _parser} from "./boon-parser.js"

const parser = _parser.configure({
  props: [
    styleTags({
      // identifier/name tokens
      SnakeCaseIdentifier: t.variableName,
      PascalCaseIdentifier: t.propertyName,
      // literals
      Number: t.number,
      Text: t.string,
      // keywords
      K_LIST: t.keyword,
      K_MAP: t.keyword,
      K_FUNCTION: t.keyword,
      K_LINK: t.keyword,
      K_LATEST: t.keyword,
      K_THEN: t.keyword,
      K_WHEN: t.keyword,
      K_WHILE: t.keyword,
      K_SKIP: t.keyword,
      K_BLOCK: t.keyword,
      K_PASS: t.keyword,
      K_PASSED: t.keyword,
      // punctuation & operators
      Pipe: t.operator,
      Implies: t.operator,
      NotEqual: t.operator,
      GreaterOrEqual: t.operator,
      LessOrEqual: t.operator,
      Greater: t.operator,
      Less: t.operator,
      Equal: t.operator,
      Minus: t.operator,
      Plus: t.operator,
      Asterisk: t.operator,
      Slash: t.operator,
      Wildcard: t.variableName,
      Colon: t.punctuation,
      Comma: t.punctuation,
      Dot: t.punctuation,
      BracketRoundOpen: t.punctuation,
      BracketRoundClose: t.punctuation,
      BracketCurlyOpen: t.punctuation,
      BracketCurlyClose: t.punctuation,
      BracketSquareOpen: t.punctuation,
      BracketSquareClose: t.punctuation,
      // larger constructs
      BlockBody: t.meta,
      ListCurly: t.meta,
      ListSquare: t.meta,
      Definition: t.definition(t.variableName)
    })
  ]
})

export const boonLanguage = LRLanguage.define({
  parser: parser,
  languageData: {
    commentTokens: { line: "--" },
    closeBrackets: { brackets: ["(", "{", "["] }
  }
})

export function boon() {
  return new LanguageSupport(boonLanguage)
}
