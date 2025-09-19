import {LRLanguage, LanguageSupport} from "@codemirror/language"
import {styleTags, tags as t} from "@lezer/highlight"
import {parser as baseParser} from "./boon-parser"

const parser = baseParser.configure({
  props: [
    styleTags({
      SnakeCaseIdentifier: t.variableName,
      PascalCaseIdentifier: t.typeName,
      ModulePath: t.namespace,
      'ModulePath/PascalCaseIdentifier': t.namespace,
      'ModulePathTail/PascalCaseIdentifier': t.namespace,
      'ModulePathTail/SnakeCaseIdentifier': t.namespace,
      'ModulePath/Slash': t.separator,
      'ModulePathTail/Slash': t.separator,
      Wildcard: t.special(t.variableName),
      Number: t.number,
      Text: t.string,
      LineComment: t.lineComment,
      Pipe: t.operator,
      PipeBreak: t.operator,
      AddOperator: t.operator,
      MulOperator: t.operator,
      Caret: t.operator,
      Percent: t.operator,
      CompareOperator: t.operator,
      Colon: t.separator,
      Comma: t.separator,
      Dot: t.separator,
      BracketRoundOpen: t.paren,
      BracketRoundClose: t.paren,
      BracketCurlyOpen: t.brace,
      BracketCurlyClose: t.brace,
      BracketSquareOpen: t.squareBracket,
      BracketSquareClose: t.squareBracket,
      'FUNCTION THEN WHEN WHILE LATEST BLOCK LINK SKIP PASS LIST MAP': t.keyword,
      'FunctionDef/SnakeCaseIdentifier': t.definition(t.function(t.variableName)),
      'ParamEntry/SnakeCaseIdentifier': t.local(t.variableName),
      'NamedArgument/SnakeCaseIdentifier': t.propertyName,
      'Definition/Name/SnakeCaseIdentifier': t.definition(t.variableName)
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
