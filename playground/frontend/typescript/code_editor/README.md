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
