import {LRLanguage, LanguageSupport} from "@codemirror/language"
import {styleTags, tags as t} from "@lezer/highlight"
import {parser as baseParser} from "./boon-parser"

const parser = baseParser.configure({
  props: [
    styleTags({
      Keyword: t.keyword,
      ModulePath: t.namespace,
      PascalCase: t.typeName,
      SnakeCase: t.variableName,
      Wildcard: t.special(t.variableName),
      Number: t.number,
      Text: t.string,
      Operator: t.operator,
      Pipe: t.operator,
      Arrow: t.operator,
      NotEqual: t.operator,
      GreaterOrEqual: t.operator,
      LessOrEqual: t.operator,
      Greater: t.operator,
      Less: t.operator,
      Equal: t.operator,
      Plus: t.operator,
      Minus: t.operator,
      Asterisk: t.operator,
      Slash: t.operator,
      Percent: t.operator,
      Caret: t.operator,
      Punctuation: t.separator,
      Colon: t.separator,
      Comma: t.separator,
      Dot: t.separator,
      BracketRoundOpen: t.paren,
      BracketRoundClose: t.paren,
      BracketCurlyOpen: t.brace,
      BracketCurlyClose: t.brace,
      LineComment: t.lineComment,
      'ObjectLiteral/BracketSquareOpen': t.squareBracket,
      'ObjectLiteral/BracketSquareClose': t.squareBracket,
      'TaggedObject/PascalCase': t.tagName
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
