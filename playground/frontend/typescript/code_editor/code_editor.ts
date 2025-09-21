import { EditorState, Compartment } from '@codemirror/state'
import { EditorView, keymap } from '@codemirror/view'
import { basicSetup } from 'codemirror'
import { indentWithTab, defaultKeymap } from "@codemirror/commands"
import { indentUnit } from "@codemirror/language"
import { boon } from "./boon-language"
import { oneDark } from "./boon-theme"

export class CodeEditorController {
    constructor() {}

    editor_view: EditorView | null = null
    on_change_handler = new Compartment
    editor_style = new Compartment

    init(parent_element: HTMLElement) {
        const state = EditorState.create({
            extensions: [
                basicSetup,
                oneDark,
                boon(),
                // Make the editor fill its parent so scrolling happens inside
                EditorView.theme({
                    ".cm-editor": { height: "100%" },
                    ".cm-scroller": { overflow: "auto" },
                }),
                this.editor_style.of([]),
                keymap.of(defaultKeymap),
                keymap.of([indentWithTab]),
                indentUnit.of("    "),
                this.on_change_handler.of([]),
            ],
        })
        this.editor_view = new EditorView({
            parent: parent_element,
            state,
        })
        this.editor_view.focus()
    }

    set_content(content: string) {
        if (this.editor_view!.state.doc.toString() !== content) {
            this.editor_view!.dispatch({
                changes: [
                    { from: 0, to: this.editor_view!.state.doc.length },
                    { from: 0, insert: content },
                ]
            })
        }
    }

    set_snippet_screenshot_mode(mode: boolean) {
        const basic_editor_style = EditorView.theme({
            ".cm-content, .cm-gutter": { minHeight: "200px" },
            ".cm-content": { "font-family": "'JetBrains Mono', monospace", fontFeatureSettings: "'zero' 1" },
        });
        // https://codemirror.net/examples/styling/
        const snippet_screenshot_mode_editor_style = EditorView.theme({
            ".cm-content, .cm-gutter": { minHeight: "200px" },
            ".cm-content": { 
                "font-family": "'JetBrains Mono', monospace", 
                paddingTop: "22px", 
                paddingBottom: "20px", 
            },
            ".cm-gutter": { paddingLeft: "8px" },
        });
        this.editor_view!.dispatch({
            effects: this.editor_style.reconfigure(
                mode ? snippet_screenshot_mode_editor_style : basic_editor_style
            )
        })
    }

    on_change(on_change: (content: string) => void) {
        const on_change_extension = EditorView.updateListener.of(view_update => {
            if (view_update.docChanged) {
                const document = view_update.state.doc.toString()
                on_change(document)
            }
        })
        this.editor_view!.dispatch({
            effects: this.on_change_handler.reconfigure(on_change_extension)
        })
    }
}
