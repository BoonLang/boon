Init
- `npm install`

Build & Watch
- `./node_modules/.bin/rolldown code_editor.ts --file ../bundles/code_editor.js --watch`

Refresh parser:
- `./node_modules/.bin/lezer-generator boon.grammar -o boon-parser.ts --typeScript`

Created with commands:
- `npm install -D rolldown` (`tested with v1.0.0-beta.3`)
- `npm i -E codemirror`
- `npm i -E @codemirror/theme-one-dark`
- `npm i -E @codemirror/view`
- `npm i -E @codemirror/state`
- `npm install --save-dev @lezer/generator`

## Highlighting palette

The editor uses a custom palette on top of CodeMirror’s one-dark theme. When making adjustments, these semantic classes and tags are already in use:

| Element | Identifier | Color / Style | Hex |
| --- | --- | --- | --- |
| Keywords | `t.keyword` | chocolate, italic, bolder | `#d2691e` |
| PascalCase tags (e.g. `Column`) | `t.tagName` | bright green | `#6df59a` |
| Function names (definitions & calls) | `.cm-boon-function-name` | amber | `#fcbf49` |
| Variable definitions (`foo:`) | `.cm-boon-variable-definition` | italic pink | `#ff6ec7` |
| Wildcards (`__`) | `t.special(t.variableName)` | chocolate | `#d2691e` |
| Variable references | `t.variableName` | near-white | `#eeeeee` |
| Numbers | `t.number` | soft blue | `#7ad1ff` |
| Strings & apostrophes | `t.string`, `.cm-boon-apostrophe` | bright yellow, bold `'` | `#fff59e` |
| Module path slashes & dots | `.cm-boon-module-slash`, `.cm-boon-dot` | chocolate, bold | `#d2691e` |
| Pipes (`|>`, pipe breaks) | `.cm-boon-pipe` | chocolate, bold | `#d2691e` |
| Punctuation (`(){}[]`, commas, etc.) | `t.separator`, `t.paren`, `t.brace`, `t.squareBracket` | chocolate, bold | `#d2691e` |
| Operators (non-pipe) | `t.operator`, `t.operatorKeyword` | vivid orange | `#ff9f43` |
| Comments | `t.lineComment`, `t.comment`, `t.meta` | lightslategray, italic | `#778899` |

These are implemented in `boon-language.ts` (decorations) and `boon-theme.ts` (colors). If you introduce new semantic classes, consider updating this table.
