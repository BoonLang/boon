import {LRLanguage, LanguageSupport} from "@codemirror/language"
import {styleTags, tags as t} from "@lezer/highlight"
import {parser as baseParser} from "./boon-parser"

const parser = baseParser.configure({
  props: [
    styleTags({
      Identifier: t.variableName,
      PascalCaseIdentifier: t.typeName,
      Wildcard: t.special(t.variableName),
      Number: t.number,
      Text: t.string,
      KeywordToken: t.keyword,
      OperatorToken: t.operator,
      PunctuationToken: t.punctuation,
      LineComment: t.lineComment
    })
  ]
})

export const boonLanguage = LRLanguage.define({
  parser,
  languageData: {
    commentTokens: {line: "--"},
    closeBrackets: {brackets: ["(", "{", "["]}
  }
})

export function boon() {
  return new LanguageSupport(boonLanguage)
}
