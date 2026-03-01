use std::fmt::Write as _;

use super::*;
use lexer::Token;

enum ValueLayout {
    /// Everything on one line: `name: value`
    Inline,
    /// Opening delimiter on same line, contents multiline: `name: [\n    ...\n]`
    SameLineStart,
    /// Value on next line, indented: `name:\n    value`
    NextLine,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Format Boon source code into canonical style.
/// Returns `None` if the code has lex or parse errors.
pub fn format(source_code: &str) -> Option<String> {
    // 1. Lex tokens (WITH comments and newlines)
    let tokens = lexer::lexer().parse(source_code).into_output()?;

    // 2. Build CommentMap from the full token stream
    let comment_map = CommentMap::from_tokens(&tokens, source_code);

    // 3. Strip comments for parsing (keep newlines — the parser uses them)
    let mut parse_tokens = tokens;
    parse_tokens.retain(|t| !matches!(t.node, Token::Comment(_)));

    // 4. Parse AST
    reset_expression_depth();
    let input = parse_tokens.map(
        span_at(source_code.len()),
        |Spanned {
             node,
             span,
             persistence: _,
         }| (node, span),
    );
    let ast = parser().parse(input).into_output()?;

    // 5. Format
    let mut fmt = Formatter::new(source_code, &comment_map);
    fmt.format_program(&ast);
    Some(fmt.finish())
}

// ---------------------------------------------------------------------------
// CommentMap — preserves comments from the token stream
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Comment<'code> {
    text: &'code str,
    /// Byte offset in source where the `--` prefix begins
    start: usize,
    /// True when the comment is on its own line (preceded only by whitespace/newlines)
    is_standalone: bool,
}

#[derive(Debug)]
struct CommentMap<'code> {
    comments: Vec<Comment<'code>>,
}

impl<'code> CommentMap<'code> {
    fn from_tokens(tokens: &[Spanned<Token<'code>>], source: &str) -> Self {
        let mut comments = Vec::new();

        for (i, tok) in tokens.iter().enumerate() {
            if let Token::Comment(text) = tok.node {
                // Determine if this comment is standalone (on its own line).
                // Look at the previous token: if it's a Newline or this is
                // the first token, the comment is standalone.
                let is_standalone = if i == 0 {
                    true
                } else {
                    // Walk backwards past any other comments to find the
                    // previous non-comment token.
                    let mut standalone = true;
                    for j in (0..i).rev() {
                        match tokens[j].node {
                            Token::Comment(_) => continue,
                            Token::Newline => {
                                standalone = true;
                                break;
                            }
                            _ => {
                                // Check if there's a newline between the previous code
                                // token's end and this comment's start.
                                let between =
                                    &source[tokens[j].span.end..tok.span.start];
                                standalone = between.contains('\n');
                                break;
                            }
                        }
                    }
                    standalone
                };

                comments.push(Comment {
                    text,
                    start: tok.span.start,
                    is_standalone,
                });
            }
        }

        CommentMap { comments }
    }
}

// ---------------------------------------------------------------------------
// Formatter
// ---------------------------------------------------------------------------

const INDENT: &str = "    ";
/// Max total line width (including indent) — the absolute column limit.
const MAX_LINE_WIDTH: usize = 90;
/// Max prefix length for last-arg hugging. If the inline prefix (all preceding
/// args + last arg name) exceeds this, skip hugging to avoid pipe ambiguity.
const MAX_LAST_ARG_PREFIX: usize = 40;
/// Max number of pipe chain segments that can be inlined on a single line.
const MAX_INLINE_PIPE_PARTS: usize = 3;

struct Formatter<'code> {
    source: &'code str,
    buf: String,
    indent: usize,
    comments: &'code CommentMap<'code>,
    comment_cursor: usize,
    /// When true, the next `format_pipe` call will skip inline/hug and format
    /// vertically.  Set by callers that push a Pipe value to NextLine — the pipe
    /// was already judged "too long" for the original line, so inlining it at the
    /// wider indented position would create a visually jarring single long row.
    force_vertical_pipe: bool,
}

impl<'code> Formatter<'code> {
    fn new(source: &'code str, comments: &'code CommentMap<'code>) -> Self {
        Self {
            source,
            buf: String::with_capacity(source.len()),
            indent: 0,
            comments,
            comment_cursor: 0,
            force_vertical_pipe: false,
        }
    }

    fn finish(mut self) -> String {
        // Emit any remaining comments at the end of the file.
        self.emit_remaining_comments();
        // Ensure single trailing newline.
        let trimmed = self.buf.trim_end();
        let len = trimmed.len();
        self.buf.truncate(len);
        self.buf.push('\n');
        self.buf
    }

    // -- indentation helpers ------------------------------------------------

    fn write(&mut self, s: &str) {
        self.buf.push_str(s);
    }

    fn write_indent(&mut self) {
        for _ in 0..self.indent {
            self.buf.push_str(INDENT);
        }
    }

    fn newline(&mut self) {
        self.buf.push('\n');
    }

    fn current_line_width(&self) -> usize {
        self.buf.len() - self.buf.rfind('\n').map_or(0, |p| p + 1)
    }

    // -- comment emission ---------------------------------------------------

    /// Write the raw comment text as-is. The lexer's `to_slice()` captures
    /// the full match including the `--` prefix, so the text already starts
    /// with `--`.
    fn write_comment(&mut self, text: &str) {
        self.write(text);
    }

    /// Emit standalone comments whose source position falls before `before_pos`.
    fn emit_comments_before(&mut self, before_pos: usize) {
        let mut prev_comment_end: Option<usize> = None;
        while self.comment_cursor < self.comments.comments.len() {
            let c = &self.comments.comments[self.comment_cursor];
            if c.start >= before_pos {
                break;
            }
            if c.is_standalone {
                // Preserve blank lines between consecutive standalone comments
                if let Some(prev_end) = prev_comment_end {
                    let between = &self.source[prev_end..c.start];
                    let newline_count = between.chars().filter(|&ch| ch == '\n').count();
                    if newline_count >= 2 {
                        self.newline();
                    }
                }
                self.write_indent();
                self.write_comment(c.text);
                self.newline();
                prev_comment_end = Some(c.start + c.text.len());
            }
            self.comment_cursor += 1;
        }
        // Preserve blank line between last comment and the expression that follows
        if let Some(prev_end) = prev_comment_end {
            let between = &self.source[prev_end..before_pos];
            let newline_count = between.chars().filter(|&ch| ch == '\n').count();
            if newline_count >= 2 {
                self.newline();
            }
        }
    }

    /// Emit a trailing comment (if any) whose source position falls between
    /// `after_pos` and `before_next_pos`.
    fn emit_trailing_comment(&mut self, after_pos: usize, before_next_pos: usize) {
        while self.comment_cursor < self.comments.comments.len() {
            let c = &self.comments.comments[self.comment_cursor];
            if c.start >= before_next_pos {
                break;
            }
            if c.start >= after_pos && !c.is_standalone {
                self.write("  ");
                self.write_comment(c.text);
                self.comment_cursor += 1;
                return;
            }
            if c.start >= after_pos && c.is_standalone {
                // Standalone comment between two nodes — emit before the next node.
                break;
            }
            self.comment_cursor += 1;
        }
    }

    fn emit_remaining_comments(&mut self) {
        while self.comment_cursor < self.comments.comments.len() {
            let c = &self.comments.comments[self.comment_cursor];
            if c.is_standalone {
                self.write_indent();
                self.write_comment(c.text);
                self.newline();
            } else {
                self.write("  ");
                self.write_comment(c.text);
                self.newline();
            }
            self.comment_cursor += 1;
        }
    }

    // -- program formatting -------------------------------------------------

    fn format_program(&mut self, expressions: &[Spanned<Expression<'code>>]) {
        for (i, expr) in expressions.iter().enumerate() {
            // Blank line when either adjacent item is multiline,
            // or always around Functions (they're visually distinct units)
            if i > 0 {
                let prev = &expressions[i - 1];
                let is_function_boundary = matches!(prev.node, Expression::Function { .. })
                    || matches!(expr.node, Expression::Function { .. });
                if is_function_boundary
                    || self.is_item_multiline(prev)
                    || self.is_item_multiline(expr)
                {
                    self.newline();
                }
            }
            self.emit_comments_before(expr.span.start);
            self.format_top_level_expression(expr);
            // Trailing comment after this expression, before the next one
            let next_start = expressions.get(i + 1).map_or(self.source.len(), |e| e.span.start);
            self.emit_trailing_comment(expr.span.end, next_start);
            self.newline();
        }
    }

    fn format_top_level_expression(&mut self, expr: &Spanned<Expression<'code>>) {
        match &expr.node {
            Expression::Variable(var) => self.format_variable(var, &expr.span),
            Expression::Function {
                name,
                parameters,
                body,
            } => self.format_function(name, parameters, body),
            _ => {
                self.write_indent();
                self.format_expression(expr);
            }
        }
    }

    // -- expression formatting -----------------------------------------------

    fn format_expression(&mut self, expr: &Spanned<Expression<'code>>) {
        match &expr.node {
            Expression::Variable(var) => self.format_variable(var, &expr.span),
            Expression::Literal(lit) => self.format_literal(lit),
            Expression::List { items } => self.format_list(items),
            Expression::Object(obj) => self.format_object(&obj.variables),
            Expression::TaggedObject { tag, object } => {
                self.format_tagged_object(tag, &object.variables);
            }
            Expression::Map { entries } => self.format_map(entries),
            Expression::Function {
                name,
                parameters,
                body,
            } => self.format_function(name, parameters, body),
            Expression::FunctionCall { path, arguments } => {
                self.format_function_call(path, arguments);
            }
            Expression::Alias(alias) => self.format_alias(alias),
            Expression::LinkSetter { alias } => self.format_link_setter(alias),
            Expression::Link => self.write("LINK"),
            Expression::Latest { inputs } => self.format_latest(inputs),
            Expression::Hold { state_param, body } => self.format_hold(state_param, body),
            Expression::Then { body } => self.format_then(body),
            Expression::Flush { value } => self.format_flush(value),
            Expression::Spread { value } => {
                self.write("...");
                self.format_expression(value);
            }
            Expression::When { arms } => self.format_when_while("WHEN", arms),
            Expression::While { arms } => self.format_when_while("WHILE", arms),
            Expression::Pipe { from, to } => self.format_pipe(from, to),
            Expression::Skip => self.write("SKIP"),
            Expression::Block { variables, output } => self.format_block(variables, output),
            Expression::Comparator(cmp) => self.format_comparator(cmp),
            Expression::ArithmeticOperator(op) => self.format_arithmetic(op),
            Expression::TextLiteral { parts, hash_count } => {
                self.format_text_literal(parts, *hash_count);
            }
            Expression::Bits { size } => {
                self.write("BITS(");
                self.format_expression(size);
                self.write(")");
            }
            Expression::Memory { address } => {
                self.write("MEMORY(");
                self.format_expression(address);
                self.write(")");
            }
            Expression::Bytes { data } => self.format_bytes(data),
            Expression::FieldAccess { path } => {
                self.write(".");
                self.write(&path.join("."));
            }
        }
    }

    // -- Variable -----------------------------------------------------------

    fn format_variable(&mut self, var: &Variable<'code>, span: &Span) {
        self.write_indent();
        self.write(var.name);
        self.write(":");
        match self.value_layout(&var.value, " ".len()) {
            ValueLayout::Inline => {
                // name: value (all on one line)
                self.write(" ");
                self.format_expression(&var.value);
            }
            ValueLayout::SameLineStart => {
                // name: [\n    ...\n] — opening delimiter on same line
                self.write(" ");
                self.format_expression(&var.value);
            }
            ValueLayout::NextLine => {
                // name:\n    value — value on next line, indented
                self.newline();
                self.indent += 1;
                self.emit_comments_before(var.value.span.start);
                self.write_indent();
                if matches!(var.value.node, Expression::Pipe { .. }) {
                    self.force_vertical_pipe = true;
                }
                self.format_expression(&var.value);
                self.indent -= 1;
            }
        }
    }

    /// Determine how a value should be laid out relative to its variable name.
    /// `separator_width` is the width of the separator that will be written before
    /// the value (e.g. `" => ".len()` for arms, `" ".len()` for variables/arguments).
    fn value_layout(
        &self,
        expr: &Spanned<Expression<'code>>,
        separator_width: usize,
    ) -> ValueLayout {
        let available = self.current_line_width() + separator_width;
        if self.should_inline_value(expr) {
            // Verify the inline form actually fits at the current cursor position
            if let Some(inline) = self.estimate_inline(expr) {
                if available + inline.len() <= MAX_LINE_WIDTH {
                    return ValueLayout::Inline;
                }
            } else {
                return ValueLayout::Inline;
            }
            // Doesn't fit — fall through to SameLineStart/NextLine logic
        }
        // Constructs with opening delimiters that should stay on the same line
        match &expr.node {
            Expression::Object(obj) if !obj.variables.is_empty() => ValueLayout::SameLineStart,
            Expression::TaggedObject { object, .. } if !object.variables.is_empty() => {
                ValueLayout::SameLineStart
            }
            Expression::FunctionCall { arguments, .. } if !arguments.is_empty() => {
                ValueLayout::SameLineStart
            }
            Expression::List { items } if !items.is_empty() => ValueLayout::SameLineStart,
            Expression::Map { entries } if !entries.is_empty() => ValueLayout::SameLineStart,
            Expression::Latest { .. } => ValueLayout::SameLineStart,
            Expression::When { .. } | Expression::While { .. } => ValueLayout::SameLineStart,
            Expression::Hold { .. } => ValueLayout::SameLineStart,
            Expression::Then { .. } => ValueLayout::SameLineStart,
            Expression::Flush { .. } => ValueLayout::SameLineStart,
            Expression::Block { .. } => ValueLayout::SameLineStart,
            Expression::TextLiteral { .. } => ValueLayout::SameLineStart,
            Expression::Pipe { from, to } => {
                // Walk to the leftmost segment and count chain length
                let mut first: &Spanned<Expression<'code>> = from;
                let mut chain_len: usize = 2; // at least from + to
                while let Expression::Pipe { from: inner, .. } = &first.node {
                    first = inner;
                    chain_len += 1;
                }
                if chain_len == 2 {
                    if let Some(first_inline) = self.estimate_inline(first) {
                        // Check if the pipe can actually hug at the current cursor position
                        let hug_width = if let Some(to_inline) = self.estimate_inline(to) {
                            " |> ".len() + to_inline.len()
                        } else {
                            " |> ".len() + self.estimate_opening_width(&to.node)
                        };
                        if available + first_inline.len() + hug_width <= MAX_LINE_WIDTH {
                            ValueLayout::SameLineStart
                        } else {
                            // Pipe won't hug — use NextLine to keep it as one unit
                            ValueLayout::NextLine
                        }
                    } else {
                        ValueLayout::NextLine
                    }
                } else {
                    // 3+ segments: value on next line, |> aligned
                    ValueLayout::NextLine
                }
            }
            _ => ValueLayout::NextLine,
        }
    }

    // -- Literal ------------------------------------------------------------

    fn format_literal(&mut self, lit: &Literal<'code>) {
        match lit {
            Literal::Number(n) => {
                if *n == n.trunc() && n.is_finite() && n.abs() < 1e15 {
                    // Integer-like: emit without decimal point.
                    // Safe: we checked range above (abs < 1e15 fits in i64).
                    write!(&mut self.buf, "{:.0}", n).unwrap();
                } else {
                    write!(&mut self.buf, "{}", n).unwrap();
                }
            }
            Literal::Tag(t) => self.write(t),
            Literal::Text(t) => {
                self.write("TEXT { ");
                self.write(t);
                self.write(" }");
            }
        }
    }

    // -- List ---------------------------------------------------------------

    fn format_list(&mut self, items: &[Spanned<Expression<'code>>]) {
        if items.is_empty() {
            self.write("LIST {}");
            return;
        }
        self.write("LIST {");
        self.newline();
        self.indent += 1;
        for (i, item) in items.iter().enumerate() {
            // Blank line when either adjacent item is multiline
            if i > 0
                && (self.is_item_multiline(&items[i - 1])
                    || self.is_item_multiline(item))
            {
                self.newline();
            }
            self.emit_comments_before(item.span.start);
            self.write_indent();
            self.format_expression(item);
            self.newline();
        }
        self.indent -= 1;
        self.write_indent();
        self.write("}");
    }

    // -- Object -------------------------------------------------------------

    fn format_object(&mut self, variables: &[Spanned<Variable<'code>>]) {
        if variables.is_empty() {
            self.write("[]");
            return;
        }
        // Try inline if all fields fit on one line
        if let Some(inline) = self.try_inline_object(variables) {
            if inline.len() + 2 + self.current_line_width() <= MAX_LINE_WIDTH {
                self.write("[");
                self.write(&inline);
                self.write("]");
                return;
            }
        }
        self.write("[");
        self.newline();
        self.indent += 1;
        self.format_object_fields(variables);
        self.indent -= 1;
        self.write_indent();
        self.write("]");
    }

    // -- TaggedObject -------------------------------------------------------

    fn format_tagged_object(
        &mut self,
        tag: &str,
        variables: &[Spanned<Variable<'code>>],
    ) {
        if variables.is_empty() {
            self.write(tag);
            self.write("[]");
            return;
        }
        // Try inline for short tagged objects
        if let Some(inline) = self.try_inline_object(variables) {
            let total = tag.len() + 1 + inline.len() + 1;
            if total + self.current_line_width() <= MAX_LINE_WIDTH {
                self.write(tag);
                self.write("[");
                self.write(&inline);
                self.write("]");
                return;
            }
        }
        self.write(tag);
        self.write("[");
        self.newline();
        self.indent += 1;
        self.format_object_fields(variables);
        self.indent -= 1;
        self.write_indent();
        self.write("]");
    }

    /// Shared logic for object and tagged-object fields with blank line insertion.
    fn format_object_fields(&mut self, variables: &[Spanned<Variable<'code>>]) {
        for (i, var) in variables.iter().enumerate() {
            // Blank line when either adjacent field is multiline
            if i > 0
                && (self.is_variable_multiline(&variables[i - 1].node)
                    || self.is_variable_multiline(&var.node))
            {
                self.newline();
            }
            self.emit_comments_before(var.span.start);
            self.format_variable(&var.node, &var.span);
            self.newline();
        }
    }

    // -- Map ----------------------------------------------------------------

    fn format_map(&mut self, entries: &[MapEntry<'code>]) {
        if entries.is_empty() {
            self.write("MAP {}");
            return;
        }
        self.write("MAP {");
        self.newline();
        self.indent += 1;
        for entry in entries {
            self.write_indent();
            self.format_map_entry_key(&entry.key.node);
            self.write(" => ");
            self.format_expression(&entry.value);
            self.newline();
        }
        self.indent -= 1;
        self.write_indent();
        self.write("}");
    }

    fn format_map_entry_key(&mut self, key: &MapEntryKey<'code>) {
        match key {
            MapEntryKey::Literal(lit) => self.format_literal(lit),
            MapEntryKey::Alias(alias) => self.format_alias(alias),
        }
    }

    // -- Function -----------------------------------------------------------

    fn format_function(
        &mut self,
        name: &str,
        parameters: &[Spanned<&'code str>],
        body: &Spanned<Expression<'code>>,
    ) {
        self.write_indent();
        self.write("FUNCTION ");
        self.write(name);
        self.write("(");
        for (i, param) in parameters.iter().enumerate() {
            if i > 0 {
                self.write(", ");
            }
            self.write(param.node);
        }
        self.write(") {");
        self.newline();
        self.indent += 1;
        self.emit_comments_before(body.span.start);
        self.format_body_expression(body);
        self.indent -= 1;
        self.write_indent();
        self.write("}");
    }

    /// Format an expression that appears as the body of a block construct
    /// (FUNCTION, HOLD, THEN, etc.) — always at the current indent level.
    fn format_body_expression(&mut self, expr: &Spanned<Expression<'code>>) {
        match &expr.node {
            Expression::Variable(var) => {
                self.format_variable(var, &expr.span);
                self.newline();
            }
            _ => {
                self.write_indent();
                self.format_expression(expr);
                self.newline();
            }
        }
    }

    // -- FunctionCall -------------------------------------------------------

    fn format_function_call(
        &mut self,
        path: &[&'code str],
        arguments: &[Spanned<Argument<'code>>],
    ) {
        self.write(&path.join("/"));
        self.write("(");
        if arguments.is_empty() {
            self.write(")");
            return;
        }
        // Try inline
        if let Some(inline) = self.try_inline_arguments(arguments) {
            let available = MAX_LINE_WIDTH.saturating_sub(self.current_line_width() + 1); // +1 for ')'
            if inline.len() <= available {
                self.write(&inline);
                self.write(")");
                return;
            }
        }
        // Last argument has multiline value, preceding args fit inline:
        // keep everything up to the last arg's value on the opening line.
        // e.g. `Document/new(root: Element/stripe(...))` or
        //      `List/map(item, new: Element/label(...))` instead of extra indent
        {
            let last = &arguments.last().unwrap().node;
            let last_has_multiline_value = last
                .value
                .as_ref()
                .is_some_and(|v| self.estimate_inline(v).is_none());
            if last_has_multiline_value {
                // Build the inline prefix: all preceding args + last arg name + ": "
                let mut prefix_parts = Vec::new();
                let mut all_preceding_inline = true;
                for arg in &arguments[..arguments.len() - 1] {
                    let mut s = String::new();
                    s.push_str(arg.node.name);
                    if let Some(value) = &arg.node.value {
                        s.push_str(": ");
                        if let Some(inline) = self.estimate_inline(value) {
                            s.push_str(&inline);
                        } else {
                            all_preceding_inline = false;
                            break;
                        }
                    }
                    prefix_parts.push(s);
                }
                if all_preceding_inline {
                    let mut prefix = prefix_parts.join(", ");
                    if !prefix.is_empty() {
                        prefix.push_str(", ");
                    }
                    prefix.push_str(last.name);
                    prefix.push_str(": ");
                    if prefix.len() <= MAX_LAST_ARG_PREFIX
                        && self.current_line_width() + prefix.len() <= MAX_LINE_WIDTH
                    {
                        self.write(&prefix);
                        self.format_expression(last.value.as_ref().unwrap());
                        self.write(")");
                        return;
                    }
                }
            }
        }
        // Multi-line
        self.newline();
        self.indent += 1;
        for (i, arg) in arguments.iter().enumerate() {
            // Blank line when either adjacent argument is multiline
            if i > 0
                && (self.is_argument_multiline(&arguments[i - 1].node)
                    || self.is_argument_multiline(&arg.node))
            {
                self.newline();
            }
            self.emit_comments_before(arg.span.start);
            self.write_indent();
            self.format_argument(&arg.node);
            self.newline();
        }
        self.indent -= 1;
        self.write_indent();
        self.write(")");
    }

    fn format_argument(&mut self, arg: &Argument<'code>) {
        self.write(arg.name);
        if let Some(value) = &arg.value {
            self.write(":");
            match self.value_layout(value, " ".len()) {
                ValueLayout::Inline | ValueLayout::SameLineStart => {
                    self.write(" ");
                    self.format_expression(value);
                }
                ValueLayout::NextLine => {
                    self.newline();
                    self.indent += 1;
                    self.write_indent();
                    if matches!(value.node, Expression::Pipe { .. }) {
                        self.force_vertical_pipe = true;
                    }
                    self.format_expression(value);
                    self.indent -= 1;
                }
            }
        }
    }

    // -- Alias --------------------------------------------------------------

    fn format_alias(&mut self, alias: &Alias<'code>) {
        match alias {
            Alias::WithoutPassed { parts, .. } => {
                self.write(&parts.join("."));
            }
            Alias::WithPassed { extra_parts } => {
                self.write("PASSED");
                if !extra_parts.is_empty() {
                    self.write(".");
                    self.write(&extra_parts.join("."));
                }
            }
        }
    }

    // -- LinkSetter ---------------------------------------------------------

    fn format_link_setter(&mut self, alias: &Spanned<Alias<'code>>) {
        self.write("LINK { ");
        self.format_alias(&alias.node);
        self.write(" }");
    }

    // -- Latest -------------------------------------------------------------

    fn format_latest(&mut self, inputs: &[Spanned<Expression<'code>>]) {
        self.write("LATEST {");
        self.newline();
        self.indent += 1;
        for (i, input) in inputs.iter().enumerate() {
            // Blank line when either adjacent input is multiline
            if i > 0
                && (self.is_item_multiline(&inputs[i - 1])
                    || self.is_item_multiline(input))
            {
                self.newline();
            }
            self.emit_comments_before(input.span.start);
            self.format_body_expression(input);
        }
        self.indent -= 1;
        self.write_indent();
        self.write("}");
    }

    // -- Hold ---------------------------------------------------------------

    fn format_hold(
        &mut self,
        state_param: &str,
        body: &Spanned<Expression<'code>>,
    ) {
        self.write("HOLD ");
        self.write(state_param);
        self.write(" {");
        self.newline();
        self.indent += 1;
        self.emit_comments_before(body.span.start);
        self.format_body_expression(body);
        self.indent -= 1;
        self.write_indent();
        self.write("}");
    }

    // -- Then ---------------------------------------------------------------

    fn format_then(&mut self, body: &Spanned<Expression<'code>>) {
        // Try inline for short bodies
        if let Some(inline) = self.estimate_inline(body) {
            let total = "THEN { ".len() + inline.len() + " }".len();
            if total + self.current_line_width() <= MAX_LINE_WIDTH {
                self.write("THEN { ");
                self.write(&inline);
                self.write(" }");
                return;
            }
        }
        self.write("THEN {");
        self.newline();
        self.indent += 1;
        self.emit_comments_before(body.span.start);
        self.format_body_expression(body);
        self.indent -= 1;
        self.write_indent();
        self.write("}");
    }

    // -- Flush --------------------------------------------------------------

    fn format_flush(&mut self, value: &Spanned<Expression<'code>>) {
        if let Some(inline) = self.estimate_inline(value) {
            let total = "FLUSH { ".len() + inline.len() + " }".len();
            if total + self.current_line_width() <= MAX_LINE_WIDTH {
                self.write("FLUSH { ");
                self.write(&inline);
                self.write(" }");
                return;
            }
        }
        self.write("FLUSH {");
        self.newline();
        self.indent += 1;
        self.emit_comments_before(value.span.start);
        self.format_body_expression(value);
        self.indent -= 1;
        self.write_indent();
        self.write("}");
    }

    // -- When / While -------------------------------------------------------

    fn format_when_while(
        &mut self,
        keyword: &str,
        arms: &[Arm<'code>],
    ) {
        // Try inline for single short arm
        if arms.len() == 1 {
            if let Some(inline) = self.try_inline_arm(&arms[0]) {
                let total = keyword.len() + " { ".len() + inline.len() + " }".len();
                if total + self.current_line_width() <= MAX_LINE_WIDTH {
                    self.write(keyword);
                    self.write(" { ");
                    self.write(&inline);
                    self.write(" }");
                    return;
                }
            }
        }
        self.write(keyword);
        self.write(" {");
        self.newline();
        self.indent += 1;
        for (i, arm) in arms.iter().enumerate() {
            // Blank line when either adjacent arm is multiline
            if i > 0
                && (self.is_arm_multiline(&arms[i - 1])
                    || self.is_arm_multiline(arm))
            {
                self.newline();
            }
            self.emit_comments_before(arm.body.span.start);
            self.write_indent();
            self.format_arm(arm);
            self.newline();
        }
        self.indent -= 1;
        self.write_indent();
        self.write("}");
    }

    fn format_arm(&mut self, arm: &Arm<'code>) {
        self.format_pattern(&arm.pattern);
        let layout = self.value_layout(&arm.body, " => ".len());
        match layout {
            ValueLayout::Inline | ValueLayout::SameLineStart => {
                self.write(" => ");
                self.format_expression(&arm.body);
            }
            ValueLayout::NextLine => {
                self.write(" =>");
                self.newline();
                self.indent += 1;
                self.write_indent();
                if matches!(arm.body.node, Expression::Pipe { .. }) {
                    self.force_vertical_pipe = true;
                }
                self.format_expression(&arm.body);
                self.indent -= 1;
            }
        }
    }

    fn format_pattern(&mut self, pattern: &Pattern<'code>) {
        match pattern {
            Pattern::Literal(lit) => self.format_literal(lit),
            Pattern::List { items } => {
                self.write("[");
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        self.write(", ");
                    }
                    self.format_pattern(item);
                }
                self.write("]");
            }
            Pattern::Object { variables } => {
                self.write("[");
                for (i, var) in variables.iter().enumerate() {
                    if i > 0 {
                        self.write(", ");
                    }
                    self.format_pattern_variable(var);
                }
                self.write("]");
            }
            Pattern::TaggedObject { tag, variables } => {
                self.write(tag);
                self.write("[");
                for (i, var) in variables.iter().enumerate() {
                    if i > 0 {
                        self.write(", ");
                    }
                    self.format_pattern_variable(var);
                }
                self.write("]");
            }
            Pattern::Map { entries } => {
                self.write("MAP {");
                for (i, entry) in entries.iter().enumerate() {
                    if i > 0 {
                        self.write(", ");
                    }
                    self.write(" ");
                    self.format_pattern(&entry.key);
                    if let Some(value) = &entry.value {
                        self.write(": ");
                        self.format_pattern(value);
                    }
                }
                self.write(" }");
            }
            Pattern::Alias { name } => self.write(name),
            Pattern::WildCard => self.write("__"),
        }
    }

    fn format_pattern_variable(&mut self, var: &PatternVariable<'code>) {
        self.write(var.name);
        if let Some(value) = &var.value {
            self.write(": ");
            self.format_pattern(value);
        }
    }

    // -- Pipe ---------------------------------------------------------------

    fn format_pipe(
        &mut self,
        from: &Spanned<Expression<'code>>,
        to: &Spanned<Expression<'code>>,
    ) {
        // Collect the full pipe chain
        let mut chain = Vec::new();
        self.collect_pipe_chain(from, &mut chain);
        chain.push(to);

        // Consume the force-vertical flag.  When set, the pipe was pushed to
        // NextLine by its container (variable, argument or arm) — meaning it
        // was already judged "too long" for the original line.  Inlining it at
        // the wider indented position would create a jarring single long row,
        // so we skip the inline and hug attempts and go straight to vertical.
        let force_vertical = self.force_vertical_pipe;
        self.force_vertical_pipe = false;

        // Try inline (unless forced vertical)
        if !force_vertical {
            if let Some(inline) = self.try_inline_pipe_chain(&chain) {
                if inline.len() + self.current_line_width() <= MAX_LINE_WIDTH {
                    self.write(&inline);
                    return;
                }
            }
        }

        // Check if the first segment actually fits inline at the current indent
        let first_is_inline = self
            .estimate_inline(chain[0])
            .is_some_and(|inline| inline.len() + self.current_line_width() <= MAX_LINE_WIDTH);

        if !force_vertical && first_is_inline && chain.len() == 2 {
            // Single continuation after inline first: hug |> on same line
            // e.g. `selected_filter: Router/route() |> WHILE { ... }`
            self.format_expression(chain[0]);
            // Check if |> + continuation fits on this line.
            // For inline continuations: check full width.
            // For multiline continuations (WHILE, HOLD, etc.): include the opening keyword.
            let hug_width = if let Some(inline) = self.estimate_inline(chain[1]) {
                " |> ".len() + inline.len()
            } else {
                " |> ".len() + self.estimate_opening_width(&chain[1].node)
            };
            if self.current_line_width() + hug_width <= MAX_LINE_WIDTH {
                self.write(" |> ");
                self.format_expression(chain[1]);
            } else {
                // Can't hug — fall to next line, |> aligned with first segment
                self.newline();
                self.write_indent();
                self.write("|> ");
                self.format_expression(chain[1]);
            }
        } else {
            // Multiple continuations or multiline first: all |> aligned at same indent
            self.format_expression(chain[0]);
            for segment in &chain[1..] {
                self.newline();
                self.write_indent();
                self.write("|> ");
                self.format_expression(segment);
            }
        }
    }

    fn collect_pipe_chain<'a>(
        &self,
        expr: &'a Spanned<Expression<'code>>,
        chain: &mut Vec<&'a Spanned<Expression<'code>>>,
    ) {
        if let Expression::Pipe { from, to } = &expr.node {
            self.collect_pipe_chain(from, chain);
            chain.push(to);
        } else {
            chain.push(expr);
        }
    }

    // -- Block --------------------------------------------------------------

    fn format_block(
        &mut self,
        variables: &[Spanned<Variable<'code>>],
        output: &Spanned<Expression<'code>>,
    ) {
        self.write("BLOCK {");
        self.newline();
        self.indent += 1;
        for (i, var) in variables.iter().enumerate() {
            // Blank line when either adjacent variable is multiline
            if i > 0
                && (self.is_variable_multiline(&variables[i - 1].node)
                    || self.is_variable_multiline(&var.node))
            {
                self.newline();
            }
            self.emit_comments_before(var.span.start);
            self.format_variable(&var.node, &var.span);
            self.newline();
        }
        // Always blank line before output when there are bindings
        if !variables.is_empty() {
            self.newline();
        }
        self.emit_comments_before(output.span.start);
        self.format_body_expression(output);
        self.indent -= 1;
        self.write_indent();
        self.write("}");
    }

    // -- Comparator ---------------------------------------------------------

    fn format_comparator(&mut self, cmp: &Comparator<'code>) {
        let (a, op, b) = match cmp {
            Comparator::Equal { operand_a, operand_b } => (operand_a, "==", operand_b),
            Comparator::NotEqual { operand_a, operand_b } => (operand_a, "=/=", operand_b),
            Comparator::Greater { operand_a, operand_b } => (operand_a, ">", operand_b),
            Comparator::GreaterOrEqual { operand_a, operand_b } => (operand_a, ">=", operand_b),
            Comparator::Less { operand_a, operand_b } => (operand_a, "<", operand_b),
            Comparator::LessOrEqual { operand_a, operand_b } => (operand_a, "<=", operand_b),
        };
        self.format_expression(a);
        self.write(" ");
        self.write(op);
        self.write(" ");
        self.format_expression(b);
    }

    // -- ArithmeticOperator -------------------------------------------------

    fn format_arithmetic(&mut self, op: &ArithmeticOperator<'code>) {
        match op {
            ArithmeticOperator::Negate { operand } => {
                self.write("-");
                self.format_expression(operand);
            }
            ArithmeticOperator::Add { operand_a, operand_b } => {
                self.format_expression(operand_a);
                self.write(" + ");
                self.format_expression(operand_b);
            }
            ArithmeticOperator::Subtract { operand_a, operand_b } => {
                self.format_expression(operand_a);
                self.write(" - ");
                self.format_expression(operand_b);
            }
            ArithmeticOperator::Multiply { operand_a, operand_b } => {
                self.format_expression(operand_a);
                self.write(" * ");
                self.format_expression(operand_b);
            }
            ArithmeticOperator::Divide { operand_a, operand_b } => {
                self.format_expression(operand_a);
                self.write(" / ");
                self.format_expression(operand_b);
            }
        }
    }

    // -- TextLiteral --------------------------------------------------------

    fn format_text_literal(&mut self, parts: &[TextPart<'code>], hash_count: usize) {
        let hashes = "#".repeat(hash_count);
        let interp_prefix = if hash_count == 0 {
            "{".to_string()
        } else {
            format!("{}{}", hashes, "{")
        };
        // Build the content string
        let mut content = String::new();
        for part in parts {
            match part {
                TextPart::Text(text) => content.push_str(text),
                TextPart::Interpolation { var, .. } => {
                    content.push_str(&interp_prefix);
                    content.push_str(var);
                    content.push('}');
                }
            }
        }
        // Try inline: TEXT { content }
        let inline_len = "TEXT ".len() + hashes.len() + "{ ".len() + content.len() + " }".len();
        if self.current_line_width() + inline_len <= MAX_LINE_WIDTH {
            self.write("TEXT ");
            self.write(&hashes);
            self.write("{ ");
            self.write(&content);
            self.write(" }");
        } else {
            // Wrap to multiline — safe because dedent strips the formatter's indentation,
            // producing the same content as inline mode (see docs/language/TEXT_SYNTAX.md)
            self.write("TEXT ");
            self.write(&hashes);
            self.write("{");
            self.newline();
            self.indent += 1;
            self.write_indent();
            self.write(&content);
            self.newline();
            self.indent -= 1;
            self.write_indent();
            self.write("}");
        }
    }

    // -- Bytes --------------------------------------------------------------

    fn format_bytes(&mut self, data: &[Spanned<Expression<'code>>]) {
        if data.is_empty() {
            self.write("BYTES()");
            return;
        }
        self.write("BYTES(");
        self.newline();
        self.indent += 1;
        for item in data {
            self.write_indent();
            self.format_expression(item);
            self.newline();
        }
        self.indent -= 1;
        self.write_indent();
        self.write(")");
    }

    // -----------------------------------------------------------------------
    // Inline heuristics — decide single-line vs multi-line
    // -----------------------------------------------------------------------

    /// Check if a value expression should be kept on the same line as its
    /// variable name (e.g., `x: 42` vs `x:\n    LATEST { ... }`).
    fn should_inline_value(&self, expr: &Spanned<Expression<'code>>) -> bool {
        match &expr.node {
            Expression::Literal(_)
            | Expression::Alias(_)
            | Expression::Link
            | Expression::Skip
            | Expression::FieldAccess { .. } => true,
            Expression::TextLiteral { parts, .. } => {
                // Inline short TEXT literals
                self.estimate_text_literal_width(parts) <= Some(60)
            }
            Expression::FunctionCall { arguments, .. } => {
                // Inline short function calls
                self.try_inline_arguments(arguments).is_some()
            }
            Expression::TaggedObject { object, .. } => {
                self.try_inline_object(&object.variables).is_some()
            }
            Expression::Object(obj) => {
                self.try_inline_object(&obj.variables).is_some()
            }
            Expression::Pipe { .. } => {
                // Try to estimate if the entire pipe chain fits inline
                let mut chain = Vec::new();
                if let Expression::Pipe { from, to } = &expr.node {
                    self.collect_pipe_chain(from, &mut chain);
                    chain.push(to);
                }
                self.try_inline_pipe_chain(&chain).is_some()
            }
            Expression::ArithmeticOperator(_) | Expression::Comparator(_) => {
                self.estimate_inline(expr).map_or(false, |s| s.len() <= 40)
            }
            Expression::List { items } => items.is_empty(),
            _ => false,
        }
    }

    /// Returns true if this expression will definitely span multiple lines.
    fn is_multiline(&self, expr: &Spanned<Expression<'code>>) -> bool {
        match &expr.node {
            Expression::Latest { inputs } => inputs.len() > 0,
            Expression::Hold { .. } => true,
            Expression::Function { .. } => true,
            Expression::Block { .. } => true,
            Expression::When { arms } | Expression::While { arms } => {
                arms.len() > 1 || arms.iter().any(|a| self.is_multiline(&a.body))
            }
            Expression::Object(obj) => self.try_inline_object(&obj.variables).is_none(),
            Expression::TaggedObject { object, .. } => {
                self.try_inline_object(&object.variables).is_none()
            }
            Expression::List { items } => !items.is_empty(),
            Expression::Map { entries } => !entries.is_empty(),
            Expression::Pipe { .. } => {
                // Check if the chain can be inlined
                let mut chain = Vec::new();
                if let Expression::Pipe { from, to } = &expr.node {
                    self.collect_pipe_chain(from, &mut chain);
                    chain.push(to);
                }
                self.try_inline_pipe_chain(&chain).is_none()
            }
            Expression::FunctionCall { arguments, .. } => {
                self.try_inline_arguments(arguments).is_none()
            }
            _ => false,
        }
    }

    /// Try to produce a single-line representation of an expression.
    /// Returns `None` if the expression is inherently multiline.
    fn estimate_pattern_width(&self, pattern: &Pattern<'code>) -> usize {
        let mut tmp = Formatter::new(self.source, self.comments);
        tmp.comment_cursor = self.comments.comments.len();
        tmp.format_pattern(pattern);
        tmp.buf.len()
    }

    fn estimate_inline(&self, expr: &Spanned<Expression<'code>>) -> Option<String> {
        let mut tmp = Formatter::new(self.source, self.comments);
        tmp.comment_cursor = self.comments.comments.len(); // skip all comments
        tmp.format_expression(expr);
        let result = tmp.buf;
        if result.contains('\n') {
            None
        } else {
            Some(result)
        }
    }

    /// Estimate how many characters a multiline expression puts on its opening line.
    /// Used by pipe hug width to account for keywords like `WHILE {`, `HOLD state {`.
    fn estimate_opening_width(&self, expr: &Expression<'code>) -> usize {
        match expr {
            Expression::While { .. } => "WHILE {".len(),
            Expression::When { .. } => "WHEN {".len(),
            Expression::Then { .. } => "THEN {".len(),
            Expression::Latest { .. } => "LATEST {".len(),
            Expression::Hold { state_param, .. } => "HOLD ".len() + state_param.len() + " {".len(),
            Expression::FunctionCall { path, .. } => {
                path.iter().map(|s| s.len()).sum::<usize>()
                    + path.len().saturating_sub(1) // "/" separators
                    + "(".len()
            }
            Expression::Object(_) => "[".len(),
            Expression::TaggedObject { tag, .. } => tag.len() + "[".len(),
            Expression::List { .. } => "LIST {".len(),
            Expression::Block { .. } => "BLOCK {".len(),
            Expression::TextLiteral { .. } => "TEXT {".len(),
            Expression::Flush { .. } => "FLUSH(".len(),
            Expression::Map { .. } => "MAP {".len(),
            _ => 0,
        }
    }

    /// Check if a variable (name: value) would format to multiple lines at the current indent.
    fn is_variable_multiline(&self, var: &Variable<'code>) -> bool {
        let prefix = self.indent * INDENT.len() + var.name.len() + 2; // "    name: "
        match self.estimate_inline(&var.value) {
            None => true,
            Some(inline) => prefix + inline.len() > MAX_LINE_WIDTH,
        }
    }

    /// Check if a function argument (name: value) would format to multiple lines.
    fn is_argument_multiline(&self, arg: &Argument<'code>) -> bool {
        match &arg.value {
            None => false, // bare positional arg like `item` is always one line
            Some(value) => {
                let prefix = self.indent * INDENT.len() + arg.name.len() + 2; // "    name: "
                match self.estimate_inline(value) {
                    None => true,
                    Some(inline) => prefix + inline.len() > MAX_LINE_WIDTH,
                }
            }
        }
    }

    /// Check if a WHEN/WHILE arm (pattern => body) would format to multiple lines.
    fn is_arm_multiline(&self, arm: &Arm<'code>) -> bool {
        // Estimate: "    pattern => body"
        let pattern_width = self.estimate_pattern_width(&arm.pattern);
        let prefix = self.indent * INDENT.len() + pattern_width + " => ".len();
        match self.estimate_inline(&arm.body) {
            None => true,
            Some(inline) => prefix + inline.len() > MAX_LINE_WIDTH,
        }
    }

    /// Check if a body item (top-level decl, LIST item, LATEST input, etc.)
    /// would format to multiple lines at the current indent level.
    fn is_item_multiline(&self, expr: &Spanned<Expression<'code>>) -> bool {
        match &expr.node {
            Expression::Function { .. } => true,
            Expression::Variable(var) => self.is_variable_multiline(var),
            _ => match self.estimate_inline(expr) {
                None => true,
                Some(inline) => {
                    self.indent * INDENT.len() + inline.len() > MAX_LINE_WIDTH
                }
            },
        }
    }

    fn try_inline_variable(&self, var: &Spanned<Variable<'code>>) -> Option<String> {
        if !self.should_inline_value(&var.node.value) {
            return None;
        }
        let value_inline = self.estimate_inline(&var.node.value)?;
        Some(format!("{}: {}", var.node.name, value_inline))
    }

    fn try_inline_object(&self, variables: &[Spanned<Variable<'code>>]) -> Option<String> {
        let mut parts = Vec::new();
        for var in variables {
            parts.push(self.try_inline_variable(var)?);
        }
        Some(parts.join(", "))
    }

    fn try_inline_arguments(&self, arguments: &[Spanned<Argument<'code>>]) -> Option<String> {
        let mut parts = Vec::new();
        for arg in arguments {
            let mut s = String::new();
            s.push_str(arg.node.name);
            if let Some(value) = &arg.node.value {
                s.push_str(": ");
                let inline = self.estimate_inline(value)?;
                s.push_str(&inline);
            }
            parts.push(s);
        }
        Some(parts.join(", "))
    }

    fn try_inline_arm(&self, arm: &Arm<'code>) -> Option<String> {
        let body_inline = self.estimate_inline(&arm.body)?;
        let mut tmp = Formatter::new(self.source, self.comments);
        tmp.comment_cursor = self.comments.comments.len();
        tmp.format_pattern(&arm.pattern);
        let pattern_str = tmp.buf;
        Some(format!("{} => {}", pattern_str, body_inline))
    }

    fn try_inline_pipe_chain(
        &self,
        chain: &[&Spanned<Expression<'code>>],
    ) -> Option<String> {
        if chain.len() > MAX_INLINE_PIPE_PARTS {
            return None;
        }
        let mut parts = Vec::new();
        for expr in chain {
            let inline = self.estimate_inline(expr)?;
            parts.push(inline);
        }
        Some(parts.join(" |> "))
    }

    fn estimate_text_literal_width(&self, parts: &[TextPart<'code>]) -> Option<usize> {
        let mut width = "TEXT { ".len() + " }".len();
        for part in parts {
            match part {
                TextPart::Text(text) => width += text.len(),
                TextPart::Interpolation { var, .. } => width += var.len() + 2, // {var}
            }
        }
        Some(width)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_simple_variable() {
        assert_eq!(format("x: 5").unwrap(), "x: 5\n");
    }

    #[test]
    fn format_tag_literal() {
        assert_eq!(format("x: True").unwrap(), "x: True\n");
    }

    #[test]
    fn format_number_integer() {
        assert_eq!(format("x: 42").unwrap(), "x: 42\n");
    }

    #[test]
    fn format_number_decimal() {
        assert_eq!(format("x: 3.14").unwrap(), "x: 3.14\n");
    }

    #[test]
    fn format_empty_object() {
        assert_eq!(format("x: []").unwrap(), "x: []\n");
    }

    #[test]
    fn format_object_single_field() {
        assert_eq!(format("x: [a: 1]").unwrap(), "x: [a: 1]\n");
    }

    #[test]
    fn format_object_multi_field() {
        let input = "x: [a: 1, b: 2]";
        let expected = "x: [a: 1, b: 2]\n";
        assert_eq!(format(input).unwrap(), expected);
    }

    #[test]
    fn format_function() {
        let input = "FUNCTION foo(x, y) { x + y }";
        let expected = "FUNCTION foo(x, y) {\n    x + y\n}\n";
        assert_eq!(format(input).unwrap(), expected);
    }

    #[test]
    fn format_pipe_short() {
        let input = "x: a |> Math/sum()";
        let result = format(input).unwrap();
        assert!(result.contains("|>"));
    }

    #[test]
    fn format_pipe_multiline_first_at_indent() {
        // Reproduce the real todo_mvc case: at deep indent, the function call's inline estimate
        // fits at indent 0 but wraps at the actual indent, so |> should go on a new line.
        let input = include_str!("../../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let result = format(input).unwrap();
        for line in result.lines() {
            // toggle_all_checkbox(...) |> LINK should not be on the same line
            if line.contains("toggle_all_checkbox(") && line.contains("|>") {
                panic!(
                    "toggle_all_checkbox call and |> should not be on the same line:\n{}",
                    line
                );
            }
            if line.trim_start().starts_with(") |>") {
                panic!("Closing paren should not hug |> on the same line:\n{}", line);
            }
            // Function arg with NextLine pipe: "items: PASSED..." should not be on same line
            if line.contains("items: PASSED") {
                panic!(
                    "3-segment pipe arg value should use NextLine layout:\n{}",
                    line
                );
            }
        }
    }

    #[test]
    fn format_latest() {
        let input = "x: LATEST { 1, 2 }";
        let expected = "x: LATEST {\n    1\n    2\n}\n";
        assert_eq!(format(input).unwrap(), expected);
    }

    #[test]
    fn format_text_literal_simple() {
        let input = "x: TEXT { hello world }";
        assert_eq!(format(input).unwrap(), "x: TEXT { hello world }\n");
    }

    #[test]
    fn format_text_literal_with_interpolation() {
        let input = "x: TEXT { hello {name} }";
        assert_eq!(format(input).unwrap(), "x: TEXT { hello {name} }\n");
    }

    #[test]
    fn format_skip() {
        assert_eq!(format("x: SKIP").unwrap(), "x: SKIP\n");
    }

    #[test]
    fn format_link() {
        assert_eq!(format("x: LINK").unwrap(), "x: LINK\n");
    }

    #[test]
    fn format_when() {
        let input = "x: WHEN { True => 1, False => 0 }";
        let expected = "x: WHEN {\n    True => 1\n    False => 0\n}\n";
        assert_eq!(format(input).unwrap(), expected);
    }

    #[test]
    fn format_preserves_standalone_comment() {
        let input = "-- header\nx: 42";
        let result = format(input).unwrap();
        assert_eq!(result, "-- header\nx: 42\n");

        // Blank line between comment and expression is preserved
        let input = "-- header\n\nx: 42";
        let result = format(input).unwrap();
        assert_eq!(result, "-- header\n\nx: 42\n");
    }


    #[test]
    fn format_error_returns_none() {
        assert!(format("INVALID {{{{{").is_none());
    }

    #[test]
    fn format_blank_lines_between_top_level() {
        // Simple one-liners: no blank line
        let input = "x: 1\ny: 2";
        let result = format(input).unwrap();
        assert_eq!(result, "x: 1\ny: 2\n");

        // Multiline item adjacent to anything: blank line inserted
        let input = "x: 1\ny: LATEST {\n    a\n    b\n}\nz: 3";
        let result = format(input).unwrap();
        assert_eq!(result, "x: 1\n\ny: LATEST {\n    a\n    b\n}\n\nz: 3\n");

        // Both multiline: blank line before each
        let input = "x: LATEST {\n    a\n    b\n}\ny: LATEST {\n    c\n    d\n}";
        let result = format(input).unwrap();
        assert_eq!(
            result,
            "x: LATEST {\n    a\n    b\n}\n\ny: LATEST {\n    c\n    d\n}\n"
        );
    }

    #[test]
    fn format_single_arg_inline() {
        // Single named argument with multiline value keeps arg name on opening line
        let input = "x: outer(\n    name: LATEST {\n        a\n        b\n        c\n    }\n)\n";
        let expected = "x: outer(name: LATEST {\n    a\n    b\n    c\n})\n";
        assert_eq!(format(input).unwrap(), expected);
    }

    #[test]
    fn format_text_literal_wraps_when_long() {
        // Short TEXT stays inline
        let input = "x: TEXT { short }";
        assert_eq!(format(input).unwrap(), "x: TEXT { short }\n");

        // Long TEXT wraps to multiline — dedent strips formatter indentation
        let input = "description: TEXT { This is the home page. Use the navigation above to explore other pages and features. }";
        let result = format(input).unwrap();
        assert!(
            result.contains("TEXT {\n"),
            "Long TEXT should wrap to multiline, got: {result}"
        );
        assert!(
            result.contains("\n}\n"),
            "Closing brace should be on its own line, got: {result}"
        );
    }

    // -- Idempotency tests --------------------------------------------------

    macro_rules! idempotency_test {
        ($name:ident, $file:expr) => {
            #[test]
            fn $name() {
                let input = include_str!($file);
                let first = format(input).expect("First format failed");
                let second = format(&first).expect("Second format failed");
                assert_eq!(first, second, "Formatting is not idempotent for {}", stringify!($name));
            }
        };
    }

    idempotency_test!(
        idempotent_counter,
        "../../../../playground/frontend/src/examples/counter/counter.bn"
    );
    idempotency_test!(
        idempotent_hello_world,
        "../../../../playground/frontend/src/examples/hello_world/hello_world.bn"
    );
    idempotency_test!(
        idempotent_shopping_list,
        "../../../../playground/frontend/src/examples/shopping_list/shopping_list.bn"
    );
    idempotency_test!(
        idempotent_todo_mvc,
        "../../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn"
    );

    idempotency_test!(
        idempotent_fibonacci,
        "../../../../playground/frontend/src/examples/fibonacci/fibonacci.bn"
    );
    idempotency_test!(
        idempotent_interval,
        "../../../../playground/frontend/src/examples/interval/interval.bn"
    );
    idempotency_test!(
        idempotent_pages,
        "../../../../playground/frontend/src/examples/pages/pages.bn"
    );
    idempotency_test!(
        idempotent_layers,
        "../../../../playground/frontend/src/examples/layers/layers.bn"
    );
    idempotency_test!(
        idempotent_when,
        "../../../../playground/frontend/src/examples/when/when.bn"
    );
    idempotency_test!(
        idempotent_while,
        "../../../../playground/frontend/src/examples/while/while.bn"
    );
    idempotency_test!(
        idempotent_latest,
        "../../../../playground/frontend/src/examples/latest/latest.bn"
    );
    idempotency_test!(
        idempotent_counter_hold,
        "../../../../playground/frontend/src/examples/counter_hold/counter_hold.bn"
    );
    idempotency_test!(
        idempotent_then,
        "../../../../playground/frontend/src/examples/then/then.bn"
    );
    idempotency_test!(
        idempotent_minimal,
        "../../../../playground/frontend/src/examples/minimal/minimal.bn"
    );
}
