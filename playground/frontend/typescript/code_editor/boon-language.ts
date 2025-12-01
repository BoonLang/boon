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
const pipeMark = Decoration.mark({class: "cm-boon-pipe"})
const chainAlternateMark = Decoration.mark({class: "cm-boon-chain-alt"})
const textLiteralContentMark = Decoration.mark({class: "cm-boon-text-literal-content"})
const textLiteralInterpolationMark = Decoration.mark({class: "cm-boon-text-literal-interpolation"})
const textLiteralInterpolationDelimiterMark = Decoration.mark({class: "cm-boon-text-literal-interpolation-delimiter"})
const negativeSignMark = Decoration.mark({class: "cm-boon-negative-sign"})

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
    let pendingMinus: {from: number, to: number} | null = null
    let chainIndex = 0
    let textLiteralEnd = 0  // Track end of TEXT literal to skip nodes inside it

    syntaxTree(view.state).iterate({
      from,
      to,
      enter: node => {
        // Skip nodes that fall within a TEXT literal we already processed
        if (node.from < textLiteralEnd) {
          return
        }

        const text = view.state.doc.sliceString(node.from, node.to)

        if (node.name === "Keyword") {
          expectFunctionName = text === "FUNCTION"
          pendingDefinition = null
          pendingFunctionCall = null

          // Handle TEXT { content } or TEXT #{ content } literals with hash escaping
          if (text === "TEXT") {
            // Look ahead for optional hashes and opening brace
            const docText = view.state.doc.toString()
            let pos = node.to
            // Skip whitespace
            while (pos < docText.length && /\s/.test(docText[pos])) {
              pos++
            }
            // Count hashes (hash escaping: TEXT #{ uses #{var}, TEXT ##{ uses ##{var}, etc.)
            let hashCount = 0
            while (pos < docText.length && docText[pos] === '#') {
              hashCount++
              pos++
            }
            // Build interpolation marker based on hash count
            const interpMarker = hashCount === 0 ? '{' : '#'.repeat(hashCount) + '{'
            // Check for opening brace
            if (docText[pos] === '{') {
              const openBrace = pos
              // Mark outer opening delimiter: #{ or ##{ or { (hashes + brace)
              const outerOpenStart = openBrace - hashCount
              builder.add(outerOpenStart, openBrace + 1, textLiteralInterpolationDelimiterMark)

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

                // Find and mark interpolations using the correct marker
                let contentPos = 0
                let lastTextEnd = contentStart
                while (contentPos < content.length) {
                  const nextInterp = content.indexOf(interpMarker, contentPos)
                  if (nextInterp === -1) break

                  // Mark text before interpolation
                  if (contentStart + nextInterp > lastTextEnd) {
                    builder.add(lastTextEnd, contentStart + nextInterp, textLiteralContentMark)
                  }

                  // Find closing brace of interpolation
                  const interpStart = contentStart + nextInterp
                  let interpEnd = interpStart + interpMarker.length
                  let interpDepth = 1
                  while (interpEnd < contentEnd && interpDepth > 0) {
                    if (docText[interpEnd] === '{') interpDepth++
                    else if (docText[interpEnd] === '}') interpDepth--
                    interpEnd++
                  }

                  // Mark interpolation delimiters and content separately
                  // Opening delimiter: #{ or ##{ or { etc.
                  const openDelimEnd = interpStart + interpMarker.length
                  builder.add(interpStart, openDelimEnd, textLiteralInterpolationDelimiterMark)
                  // Inner content (variable name)
                  const closeDelimStart = interpEnd - 1
                  if (openDelimEnd < closeDelimStart) {
                    builder.add(openDelimEnd, closeDelimStart, textLiteralInterpolationMark)
                  }
                  // Closing delimiter: }
                  builder.add(closeDelimStart, interpEnd, textLiteralInterpolationDelimiterMark)

                  lastTextEnd = interpEnd
                  contentPos = interpEnd - contentStart
                }

                // Mark remaining text after last interpolation
                if (lastTextEnd < contentEnd) {
                  builder.add(lastTextEnd, contentEnd, textLiteralContentMark)
                }
              }
              // Mark outer closing delimiter: }
              builder.add(closeBrace, closeBrace + 1, textLiteralInterpolationDelimiterMark)
              // Track end so we skip nodes inside this TEXT literal
              textLiteralEnd = closeBrace + 1
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

        // Handle negative numbers: - immediately followed by Number
        if (node.name === "Minus") {
          pendingMinus = {from: node.from, to: node.to}
          pendingDefinition = null
          expectFunctionName = false
          pendingFunctionCall = null
          return
        }

        if (node.name === "Number") {
          // If Minus immediately precedes this Number, color it as part of the number
          if (pendingMinus && pendingMinus.to === node.from) {
            builder.add(pendingMinus.from, pendingMinus.to, negativeSignMark)
          }
          pendingMinus = null
          pendingDefinition = null
          expectFunctionName = false
          pendingFunctionCall = null
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
          pendingMinus = null
          return
        }

        pendingDefinition = null
        expectFunctionName = false
        pendingFunctionCall = null
        pendingMinus = null
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
