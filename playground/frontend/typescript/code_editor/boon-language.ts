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
const pipeMark = Decoration.mark({class: "cm-boon-pipe"})
const chainAlternateMark = Decoration.mark({class: "cm-boon-chain-alt"})
const textLiteralContentMark = Decoration.mark({class: "cm-boon-text-literal-content"})
const textLiteralInterpolationMark = Decoration.mark({class: "cm-boon-text-literal-interpolation"})

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
    let pendingFunctionCall: {from: number, to: number} | null = null
    let chainIndex = 0

    syntaxTree(view.state).iterate({
      from,
      to,
      enter: node => {
        const text = view.state.doc.sliceString(node.from, node.to)

        if (node.name === "Keyword") {
          expectFunctionName = text === "FUNCTION"
          pendingDefinition = null
          pendingFunctionCall = null

          // Handle TEXT { content } literals
          if (text === "TEXT") {
            // Look ahead for the opening brace
            const docText = view.state.doc.toString()
            let pos = node.to
            // Skip whitespace
            while (pos < docText.length && /\s/.test(docText[pos])) {
              pos++
            }
            // Check for opening brace
            if (docText[pos] === '{') {
              const openBrace = pos
              pos++
              let braceDepth = 1
              // Find matching closing brace with balanced braces
              while (pos < docText.length && braceDepth > 0) {
                if (docText[pos] === '{') {
                  braceDepth++
                } else if (docText[pos] === '}') {
                  braceDepth--
                }
                pos++
              }
              const closeBrace = pos - 1
              // Mark content between braces (excluding the braces)
              if (openBrace + 1 < closeBrace) {
                const contentStart = openBrace + 1
                const contentEnd = closeBrace
                const content = docText.slice(contentStart, contentEnd)

                // Find and mark interpolations {var}
                let contentPos = 0
                let lastTextEnd = contentStart
                while (contentPos < content.length) {
                  const nextBrace = content.indexOf('{', contentPos)
                  if (nextBrace === -1) break

                  // Mark text before interpolation
                  if (contentStart + nextBrace > lastTextEnd) {
                    builder.add(lastTextEnd, contentStart + nextBrace, textLiteralContentMark)
                  }

                  // Find closing brace of interpolation
                  const interpStart = contentStart + nextBrace
                  let interpEnd = interpStart + 1
                  let interpDepth = 1
                  while (interpEnd < contentEnd && interpDepth > 0) {
                    if (docText[interpEnd] === '{') interpDepth++
                    else if (docText[interpEnd] === '}') interpDepth--
                    interpEnd++
                  }

                  // Mark interpolation
                  builder.add(interpStart, interpEnd, textLiteralInterpolationMark)

                  lastTextEnd = interpEnd
                  contentPos = interpEnd - contentStart
                }

                // Mark remaining text after last interpolation
                if (lastTextEnd < contentEnd) {
                  builder.add(lastTextEnd, contentEnd, textLiteralContentMark)
                }
              }
            }
          }
          return
        }

        if (node.name === "SnakeCase") {
          const before = node.from > 0 ? view.state.doc.sliceString(node.from - 1, node.from) : ""
          if (before === ".") {
            chainIndex += 1
          } else {
            chainIndex = 0
          }
          if (chainIndex % 2 === 1) {
            builder.add(node.from, node.to, chainAlternateMark)
          }
          if (expectFunctionName) {
            builder.add(node.from, node.to, functionNameMark)
            expectFunctionName = false
            pendingDefinition = null
            pendingFunctionCall = null
          } else {
            pendingDefinition = {from: node.from, to: node.to}
            pendingFunctionCall = {from: node.from, to: node.to}
          }
          return
        }

        if (node.name === "Colon") {
          if (pendingDefinition) {
            builder.add(pendingDefinition.from, pendingDefinition.to, variableDefinitionMark)
            pendingDefinition = null
          }
          expectFunctionName = false
          pendingFunctionCall = null
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
          pendingFunctionCall = null
          return false
        }

        if (node.name === "Dot") {
          builder.add(node.from, node.to, dotMark)
          pendingDefinition = null
          expectFunctionName = false
          pendingFunctionCall = null
          return
        }

        if (node.name === "Text") {
          for (let index = text.indexOf("'"); index !== -1; index = text.indexOf("'", index + 1)) {
            const position = node.from + index
            builder.add(position, position + 1, apostropheMark)
          }
          pendingDefinition = null
          expectFunctionName = false
          pendingFunctionCall = null
          return false
        }

        if (node.name === "Pipe" || node.name === "PipeBreak") {
          builder.add(node.from, node.to, pipeMark)
          pendingDefinition = null
          expectFunctionName = false
          pendingFunctionCall = null
          return
        }

        if (node.name === "BracketRoundOpen") {
          if (pendingFunctionCall) {
            builder.add(pendingFunctionCall.from, pendingFunctionCall.to, functionNameMark)
            pendingFunctionCall = null
            pendingDefinition = null
          }
          expectFunctionName = false
          return
        }

        if (node.name === "WS" || node.name === "Piece") {
          return
        }

        if (
          node.name === "Punctuation" ||
          node.name === "Program" ||
          node.name === "ProgramItems" ||
          node.name === "ObjectLiteral" ||
          node.name === "ListLiteral" ||
          node.name === "TaggedObject"
        ) {
          if (node.name === "Punctuation") {
            const punctuationText = view.state.doc.sliceString(node.from, node.to)
            if (punctuationText === ".") {
              return
            }
          }

          chainIndex = 0
          return
        }

        pendingDefinition = null
        expectFunctionName = false
        pendingFunctionCall = null
        chainIndex = 0
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
