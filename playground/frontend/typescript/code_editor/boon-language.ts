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

const modulePathSlashHighlight = ViewPlugin.fromClass(class {
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

    syntaxTree(view.state).iterate({
      from,
      to,
      enter: node => {
        if (node.name === "ModulePath") {
          const text = view.state.doc.sliceString(node.from, node.to)
          for (let index = text.indexOf('/') ; index !== -1 ; index = text.indexOf('/', index + 1)) {
            const position = node.from + index
            builder.add(position, position + 1, modulePathSlashMark)
          }
          return false
        }
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
  return new LanguageSupport(boonLanguage, [modulePathSlashHighlight])
}
