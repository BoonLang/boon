import {LRLanguage, LanguageSupport, syntaxTree} from "@codemirror/language"
import {RangeSetBuilder} from "@codemirror/state"
import {Decoration, EditorView, ViewPlugin, ViewUpdate} from "@codemirror/view"
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
      LineComment: t.lineComment,
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
      BracketSquareOpen: t.squareBracket,
      BracketSquareClose: t.squareBracket,
      'TaggedObject/PascalCase': t.tagName
    })
  ]
})

const modulePathSlashMark = Decoration.mark({class: "cm-boon-module-slash"})
const functionNameMark = Decoration.mark({class: "cm-boon-function-name"})
const variableDefinitionMark = Decoration.mark({class: "cm-boon-variable-definition"})
const dotMark = Decoration.mark({class: "cm-boon-dot"})
const apostropheMark = Decoration.mark({class: "cm-boon-apostrophe"})

const boonSemanticHighlight = ViewPlugin.fromClass(class {
  decorations

  constructor(view: EditorView) {
    this.decorations = this.buildDecorations(view)
  }

  update(update: ViewUpdate) {
    if (update.docChanged || update.viewportChanged || update.treeChanged) {
      this.decorations = this.buildDecorations(update.view)
    }
  }

  buildDecorations(view: EditorView) {
    const builder = new RangeSetBuilder<Decoration>()
    const {from, to} = view.viewport
    let expectFunctionName = false
    let pendingDefinition: {from: number, to: number} | null = null

    syntaxTree(view.state).iterate({
      from,
      to,
      enter: node => {
        const text = view.state.doc.sliceString(node.from, node.to)

        if (node.name === "Keyword") {
          expectFunctionName = text === "FUNCTION"
          pendingDefinition = null
          return
        }

        if (node.name === "SnakeCase") {
          if (expectFunctionName) {
            builder.add(node.from, node.to, functionNameMark)
            expectFunctionName = false
            pendingDefinition = null
          } else {
            pendingDefinition = {from: node.from, to: node.to}
          }
          return
        }

        if (node.name === "Colon") {
          if (pendingDefinition) {
            builder.add(pendingDefinition.from, pendingDefinition.to, variableDefinitionMark)
            pendingDefinition = null
          }
          expectFunctionName = false
          return
        }

        if (node.name === "ModulePath") {
          for (let index = text.indexOf('/') ; index !== -1 ; index = text.indexOf('/', index + 1)) {
            const position = node.from + index
            builder.add(position, position + 1, modulePathSlashMark)
          }
          const lastSlash = text.lastIndexOf('/')
          if (lastSlash !== -1 && lastSlash + 1 < text.length) {
            const fnStart = node.from + lastSlash + 1
            builder.add(fnStart, node.to, functionNameMark)
          }
          pendingDefinition = null
          return false
        }

        if (node.name === "Dot") {
          builder.add(node.from, node.to, dotMark)
          pendingDefinition = null
          expectFunctionName = false
          return
        }

        if (node.name === "Text") {
          for (let index = text.indexOf("'"); index !== -1; index = text.indexOf("'", index + 1)) {
            const position = node.from + index
            builder.add(position, position + 1, apostropheMark)
          }
          pendingDefinition = null
          expectFunctionName = false
          return false
        }

        if (
          node.name === "WS" ||
          node.name === "Punctuation" ||
          node.name === "Piece" ||
          node.name === "Program" ||
          node.name === "ProgramItems" ||
          node.name === "ObjectLiteral" ||
          node.name === "ListLiteral" ||
          node.name === "TaggedObject"
        ) {
          return
        }

        pendingDefinition = null
        expectFunctionName = false
        return undefined
      }
    })

    return builder.finish()
  }
}, {
  decorations: plugin => plugin.decorations
})

export const boonLanguage = LRLanguage.define({
  parser,
  languageData: {
    commentTokens: {line: "--"},
    closeBrackets: {brackets: ["(", "{", "["]}
  }
})

export function boon() {
  return new LanguageSupport(boonLanguage, [boonSemanticHighlight])
}
