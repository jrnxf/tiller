//! A shell parser for the backstop: enough of the POSIX and bash grammar to
//! find every command position in a Bash tool call, never to run one.
//!
//! The parser removes quotes, decodes `$'...'`, splits lists, pipelines,
//! subshells, groups, compound commands, function definitions, command and
//! process substitutions and heredocs, and keeps the leading assignments and
//! the redirections of each simple command. An expansion it cannot know
//! statically (`$VAR`, `$(...)`, a glob) marks its word `dynamic` and keeps
//! the source text, so the classifier can still see the tokens in it.
//! Anything it does not understand is an error, which the classifier turns
//! into a deny when a protected token is present (`authority.md` section 9,
//! "Fail mode").

use std::fmt;

/// A word after quote removal.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Word {
    /// The literal text, with each expansion kept as its source text.
    pub text: String,
    /// The word holds an unquoted glob or an expansion whose value the parser
    /// cannot know.
    pub dynamic: bool,
    /// Every part of the word was quoted (so it can never be a reserved word
    /// or an assignment).
    pub quoted: bool,
    /// The word starts with an unquoted `~`.
    pub tilde: bool,
    /// The word holds an unquoted brace list `{a,b}` or sequence `{a..c}`,
    /// which makes several words.
    pub braces: bool,
}

impl Word {
    pub fn literal(text: &str) -> Self {
        Word {
            text: text.to_owned(),
            ..Word::default()
        }
    }

    /// The text, when the word is fully static.
    pub fn fixed(&self) -> Option<&str> {
        (!self.dynamic).then_some(self.text.as_str())
    }
}

impl fmt::Display for Word {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// One redirection of a simple command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Redirect {
    pub op: String,
    pub target: Word,
    /// The redirection writes to its target (`>`, `>>`, `>|`, `&>`, `<>`).
    pub writes: bool,
    /// A heredoc or herestring: `target` is the text fed on standard input.
    pub stdin_text: bool,
}

/// A simple command with where it ran.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Simple {
    pub assigns: Vec<(String, Word)>,
    pub words: Vec<Word>,
    pub redirects: Vec<Redirect>,
    /// The command runs in the session's own shell, so a `cd` in it persists
    /// into the next Bash call: top level, not in a subshell, a
    /// substitution, a pipeline of two or more, or the background.
    pub persists: bool,
    /// The command reads standard input from a pipe.
    pub piped_in: bool,
}

/// What a script contains, flattened: every simple command at any depth, and
/// every function it defines.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Script {
    pub commands: Vec<Simple>,
    pub functions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError(pub String);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

type Result<T> = std::result::Result<T, ParseError>;

/// Parses a whole script.
pub fn parse(source: &str) -> Result<Script> {
    let mut parser = Parser::new(source);
    let mut script = Script::default();
    parser.list(&mut script, true, &[])?;
    parser.skip_blank_and_newlines();
    if parser.pos < parser.chars.len() {
        return Err(parser.error("unexpected text"));
    }
    if !parser.pending_heredocs.is_empty() {
        return Err(ParseError("a heredoc has no body".into()));
    }
    Ok(script)
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    Word(Word),
    /// `;`, `&`, `&&`, `||`, `|`, `|&`, `;;`, `;&`, `;;&`, `(`, `)`, newline.
    Op(&'static str),
    Redirect(String),
    Eof,
}

struct PendingHeredoc {
    delimiter: String,
    strip_tabs: bool,
    expand: bool,
    /// Index into the command whose redirect gets the body.
    slot: (usize, usize),
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
    pending_heredocs: Vec<PendingHeredoc>,
    /// Heredoc bodies read so far, by slot; filled in when the line ends.
    bodies: Vec<((usize, usize), Word, Script)>,
    depth: usize,
}

const RESERVED_SEPARATORS: &[&str] = &[
    "if", "then", "elif", "else", "fi", "do", "done", "while", "until", "!", "{", "}", "time",
];

fn is_name(text: &str) -> bool {
    let mut chars = text.chars();
    matches!(chars.next(), Some(c) if c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// `NAME=value`, `NAME+=value` or `NAME[i]=value`, unquoted up to the `=`.
fn assignment(word: &Word, raw_prefix_quoted: bool) -> Option<(String, Word)> {
    if raw_prefix_quoted {
        return None;
    }
    let eq = word.text.find('=')?;
    let mut name = &word.text[..eq];
    if let Some(stripped) = name.strip_suffix('+') {
        name = stripped;
    }
    if let Some(open) = name.find('[') {
        name = &name[..open];
    }
    is_name(name).then(|| {
        (
            name.to_owned(),
            Word {
                text: word.text[eq + 1..].to_owned(),
                dynamic: word.dynamic,
                quoted: false,
                tilde: word.text[eq + 1..].starts_with('~'),
                // Bash does not expand braces in an assignment value.
                braces: false,
            },
        )
    })
}

impl Parser {
    fn new(source: &str) -> Self {
        Parser {
            chars: source.chars().collect(),
            pos: 0,
            pending_heredocs: Vec::new(),
            bodies: Vec::new(),
            depth: 0,
        }
    }

    fn error(&self, what: &str) -> ParseError {
        ParseError(format!("{what} at offset {}", self.pos))
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    fn starts_with(&self, text: &str) -> bool {
        text.chars()
            .enumerate()
            .all(|(i, c)| self.peek_at(i) == Some(c))
    }

    fn skip_blank(&mut self) {
        loop {
            match self.peek() {
                Some(' ' | '\t') => self.pos += 1,
                Some('\\') if self.peek_at(1) == Some('\n') => self.pos += 2,
                Some('#') => {
                    while !matches!(self.peek(), None | Some('\n')) {
                        self.pos += 1;
                    }
                }
                _ => return,
            }
        }
    }

    fn skip_blank_and_newlines(&mut self) {
        loop {
            self.skip_blank();
            if self.peek() == Some('\n') {
                self.pos += 1;
                // A newline ends the line: heredoc bodies follow it.
                if self.read_heredoc_bodies().is_err() {
                    return;
                }
            } else {
                return;
            }
        }
    }

    /// The next token, without consuming it.
    fn peek_tok(&mut self) -> Result<Tok> {
        // Reading a word queues its substitutions; a lookahead must not.
        let saved = (self.pos, self.bodies.len(), self.pending_heredocs.len());
        let tok = self.next_tok();
        self.pos = saved.0;
        self.bodies.truncate(saved.1);
        self.pending_heredocs.truncate(saved.2);
        tok
    }

    fn next_tok(&mut self) -> Result<Tok> {
        self.skip_blank();
        let Some(c) = self.peek() else {
            return Ok(Tok::Eof);
        };
        let op = |len: usize, name: &'static str, parser: &mut Parser| {
            parser.pos += len;
            Ok(Tok::Op(name))
        };
        match c {
            '\n' => op(1, "\n", self),
            ';' if self.starts_with(";;&") => op(3, ";;&", self),
            ';' if self.starts_with(";;") => op(2, ";;", self),
            ';' if self.starts_with(";&") => op(2, ";&", self),
            ';' => op(1, ";", self),
            '&' if self.starts_with("&&") => op(2, "&&", self),
            '&' if self.starts_with("&>>") => self.redirect_op(3),
            '&' if self.starts_with("&>") => self.redirect_op(2),
            '&' => op(1, "&", self),
            '|' if self.starts_with("||") => op(2, "||", self),
            '|' if self.starts_with("|&") => op(2, "|&", self),
            '|' => op(1, "|", self),
            '(' if self.starts_with("((") => {
                // Arithmetic command: only its substitutions run.
                self.pos += 2;
                let mut subs = Vec::new();
                self.balanced("((", "))", &mut subs)?;
                self.bodies_subs(subs);
                Ok(Tok::Word(Word {
                    text: "((arith))".into(),
                    dynamic: true,
                    quoted: true,
                    tilde: false,
                    braces: false,
                }))
            }
            '(' => op(1, "(", self),
            ')' => op(1, ")", self),
            '<' | '>' => {
                if (c == '<' || c == '>') && self.peek_at(1) == Some('(') {
                    // Process substitution starts a word.
                    return self.word().map(Tok::Word);
                }
                self.redirect_from_here()
            }
            d if d.is_ascii_digit() => {
                // `2>file`, `2>&1`: a digit run followed by a redirect.
                let mut end = self.pos;
                while self.chars.get(end).is_some_and(char::is_ascii_digit) {
                    end += 1;
                }
                if matches!(self.chars.get(end), Some('<' | '>'))
                    && self.chars.get(end + 1) != Some(&'(')
                {
                    self.pos = end;
                    self.redirect_from_here()
                } else {
                    self.word().map(Tok::Word)
                }
            }
            _ => self.word().map(Tok::Word),
        }
    }

    fn redirect_op(&mut self, len: usize) -> Result<Tok> {
        let op: String = self.chars[self.pos..self.pos + len].iter().collect();
        self.pos += len;
        Ok(Tok::Redirect(op))
    }

    fn redirect_from_here(&mut self) -> Result<Tok> {
        for op in ["<<<", "<<-", "<<", "<>", "<&", ">>", ">|", ">&", "<", ">"] {
            if self.starts_with(op) {
                return self.redirect_op(op.chars().count());
            }
        }
        Err(self.error("bad redirection"))
    }

    /// Reads to the end of a balanced construct opened just before `pos`
    /// (`${...}`, `$((...))`, `((...))`). Nothing in it runs a program except
    /// the command substitutions it holds, which are parsed into `subs`.
    fn balanced(&mut self, open: &str, close: &str, subs: &mut Vec<Script>) -> Result<()> {
        let mut depth = 1;
        while depth > 0 {
            if self.pos >= self.chars.len() {
                return Err(self.error(&format!("unclosed {open}")));
            }
            if self.starts_with(close) {
                depth -= 1;
                self.pos += close.chars().count();
            } else if self.starts_with(open) {
                depth += 1;
                self.pos += open.chars().count();
            } else if self.peek() == Some('\\') {
                self.pos += 2;
            } else if self.peek() == Some('\'') {
                self.pos += 1;
                while self.peek().is_some_and(|c| c != '\'') {
                    self.pos += 1;
                }
                self.pos += 1;
            } else if self.starts_with("$(") && self.peek_at(2) != Some('(') {
                self.pos += 2;
                let (_, inner) = self.command_substitution()?;
                subs.push(inner);
            } else if self.peek() == Some('`') {
                self.pos += 1;
                let (_, inner) = self.backquote()?;
                subs.push(inner);
            } else {
                self.pos += 1;
            }
        }
        Ok(())
    }

    /// Parses `$( ... )` from just after `$(`; returns the source text and the
    /// inner script.
    fn command_substitution(&mut self) -> Result<(String, Script)> {
        let start = self.pos;
        let mut inner = Script::default();
        self.depth += 1;
        if self.depth > 32 {
            return Err(self.error("nesting too deep"));
        }
        self.list(&mut inner, false, &[")"])?;
        self.depth -= 1;
        self.skip_blank_and_newlines();
        if self.peek() != Some(')') {
            return Err(self.error("unclosed $("));
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        self.pos += 1;
        Ok((text, inner))
    }

    /// Parses a backquoted substitution from just after the opening backquote.
    fn backquote(&mut self) -> Result<(String, Script)> {
        let mut text = String::new();
        loop {
            match self.peek() {
                None => return Err(self.error("unclosed backquote")),
                Some('`') => {
                    self.pos += 1;
                    break;
                }
                Some('\\') => {
                    let next = self.peek_at(1);
                    if matches!(next, Some('`' | '\\' | '$')) {
                        text.push(next.unwrap_or_default());
                        self.pos += 2;
                    } else {
                        text.push('\\');
                        self.pos += 1;
                    }
                }
                Some(c) => {
                    text.push(c);
                    self.pos += 1;
                }
            }
        }
        let inner = self.nested(&text)?;
        Ok((text, inner))
    }

    fn nested(&mut self, text: &str) -> Result<Script> {
        if self.depth > 32 {
            return Err(self.error("nesting too deep"));
        }
        let mut parser = Parser::new(text);
        parser.depth = self.depth + 1;
        let mut script = Script::default();
        parser.list(&mut script, false, &[])?;
        parser.skip_blank_and_newlines();
        if parser.pos < parser.chars.len() {
            return Err(ParseError(format!("unexpected text in {text:?}")));
        }
        Ok(script)
    }

    /// Reads `$...` from the `$`, appending the source to `word`. Command
    /// substitutions are parsed into `subs`.
    fn dollar(&mut self, word: &mut Word, subs: &mut Vec<Script>, in_double: bool) -> Result<()> {
        let start = self.pos;
        self.pos += 1;
        match self.peek() {
            Some('(') if self.peek_at(1) == Some('(') => {
                self.pos += 2;
                self.balanced("((", "))", subs)?;
            }
            Some('(') => {
                self.pos += 1;
                let (_, inner) = self.command_substitution()?;
                subs.push(inner);
            }
            Some('{') => {
                self.pos += 1;
                self.balanced("{", "}", subs)?;
            }
            Some('\'') if !in_double => {
                // ANSI-C quoting decodes to static text.
                self.pos += 1;
                let text = self.ansi_c()?;
                word.text.push_str(&text);
                return Ok(());
            }
            Some('"') if !in_double => {
                // Locale quoting: the same as double quotes.
                return Ok(());
            }
            Some(c) if c == '_' || c.is_ascii_alphabetic() => {
                while self
                    .peek()
                    .is_some_and(|c| c == '_' || c.is_ascii_alphanumeric())
                {
                    self.pos += 1;
                }
            }
            Some(c) if c.is_ascii_digit() || "@*#?$!-".contains(c) => self.pos += 1,
            _ => {
                // A lone `$` is literal.
                word.text.push('$');
                return Ok(());
            }
        }
        let source: String = self.chars[start..self.pos].iter().collect();
        word.text.push_str(&source);
        word.dynamic = true;
        Ok(())
    }

    fn ansi_c(&mut self) -> Result<String> {
        let mut out = String::new();
        loop {
            match self.peek() {
                None => return Err(self.error("unclosed $'")),
                Some('\'') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some('\\') => {
                    self.pos += 1;
                    let Some(c) = self.peek() else {
                        return Err(self.error("unclosed $'"));
                    };
                    self.pos += 1;
                    match c {
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        'r' => out.push('\r'),
                        'a' => out.push('\u{7}'),
                        'b' => out.push('\u{8}'),
                        'e' | 'E' => out.push('\u{1b}'),
                        'f' => out.push('\u{c}'),
                        'v' => out.push('\u{b}'),
                        'x' => out.push(self.radix_char(16, 2)?),
                        'u' => out.push(self.radix_char(16, 4)?),
                        'U' => out.push(self.radix_char(16, 8)?),
                        '0'..='7' => {
                            self.pos -= 1;
                            out.push(self.radix_char(8, 3)?);
                        }
                        'c' => {
                            let Some(ctl) = self.peek() else {
                                return Err(self.error("bad \\c"));
                            };
                            self.pos += 1;
                            out.push(char::from((ctl as u8) & 0x1f));
                        }
                        other => out.push(other),
                    }
                }
                Some(c) => {
                    out.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    fn radix_char(&mut self, radix: u32, max: usize) -> Result<char> {
        let mut value = 0u32;
        let mut read = 0;
        while read < max {
            match self.peek().and_then(|c| c.to_digit(radix)) {
                Some(digit) => {
                    value = value * radix + digit;
                    self.pos += 1;
                    read += 1;
                }
                None => break,
            }
        }
        if read == 0 {
            return Err(self.error("bad escape"));
        }
        char::from_u32(value).ok_or_else(|| self.error("bad escape"))
    }

    /// Reads one word from `pos`.
    fn word(&mut self) -> Result<Word> {
        let mut subs = Vec::new();
        let word = self.word_with_subs(&mut subs)?;
        // Substitutions inside a word run before the command: they are
        // queued and flattened by the caller.
        self.bodies_subs(subs);
        Ok(word)
    }

    fn bodies_subs(&mut self, subs: Vec<Script>) {
        for sub in subs {
            self.bodies
                .push(((usize::MAX, usize::MAX), Word::default(), sub));
        }
    }

    fn word_with_subs(&mut self, subs: &mut Vec<Script>) -> Result<Word> {
        let mut word = Word {
            quoted: true,
            ..Word::default()
        };
        let mut any = false;
        let mut first = true;
        while let Some(c) = self.peek() {
            match c {
                ' ' | '\t' | '\n' | ';' | '&' | '|' | ')' | '(' => break,
                '<' | '>' => {
                    if self.peek_at(1) == Some('(') {
                        // Process substitution: runs a command.
                        self.pos += 2;
                        let (text, inner) = self.command_substitution()?;
                        subs.push(inner);
                        word.text.push_str(&format!("{c}({text})"));
                        word.dynamic = true;
                        word.quoted = false;
                        any = true;
                    } else {
                        break;
                    }
                }
                '\\' => {
                    self.pos += 1;
                    match self.peek() {
                        Some('\n') => self.pos += 1,
                        Some(next) => {
                            word.text.push(next);
                            self.pos += 1;
                        }
                        None => word.text.push('\\'),
                    }
                    any = true;
                }
                '\'' => {
                    self.pos += 1;
                    loop {
                        match self.peek() {
                            None => return Err(self.error("unclosed '")),
                            Some('\'') => {
                                self.pos += 1;
                                break;
                            }
                            Some(ch) => {
                                word.text.push(ch);
                                self.pos += 1;
                            }
                        }
                    }
                    any = true;
                }
                '"' => {
                    self.pos += 1;
                    self.double_quoted(&mut word, subs)?;
                    any = true;
                }
                '$' => {
                    if self.peek_at(1) == Some('"') {
                        self.pos += 2;
                        self.double_quoted(&mut word, subs)?;
                    } else {
                        let before = word.dynamic;
                        self.dollar(&mut word, subs, false)?;
                        if word.dynamic && !before {
                            word.quoted = false;
                        }
                    }
                    any = true;
                }
                '`' => {
                    self.pos += 1;
                    let (text, inner) = self.backquote()?;
                    subs.push(inner);
                    word.text.push('`');
                    word.text.push_str(&text);
                    word.text.push('`');
                    word.dynamic = true;
                    word.quoted = false;
                    any = true;
                }
                '*' | '?' | '[' => {
                    word.text.push(c);
                    word.dynamic = true;
                    word.quoted = false;
                    self.pos += 1;
                    any = true;
                }
                '~' if first => {
                    word.text.push('~');
                    word.tilde = true;
                    word.quoted = false;
                    self.pos += 1;
                    any = true;
                }
                '{' => {
                    // Brace expansion `a{b,c}` or `a{1..3}` makes several
                    // words.
                    let rest: String = self.chars[self.pos..].iter().take(256).collect();
                    if let Some(close) = rest.find('}')
                        && (rest[..close].contains(',') || rest[..close].contains(".."))
                    {
                        word.dynamic = true;
                        word.braces = true;
                    }
                    word.text.push('{');
                    word.quoted = false;
                    self.pos += 1;
                    any = true;
                }
                _ => {
                    word.text.push(c);
                    word.quoted = false;
                    self.pos += 1;
                    any = true;
                }
            }
            first = false;
        }
        if !any {
            return Err(self.error("expected a word"));
        }
        Ok(word)
    }

    fn double_quoted(&mut self, word: &mut Word, subs: &mut Vec<Script>) -> Result<()> {
        loop {
            match self.peek() {
                None => return Err(self.error("unclosed \"")),
                Some('"') => {
                    self.pos += 1;
                    return Ok(());
                }
                Some('\\') => {
                    let next = self.peek_at(1);
                    match next {
                        Some('$' | '`' | '"' | '\\') => {
                            word.text.push(next.unwrap_or_default());
                            self.pos += 2;
                        }
                        Some('\n') => self.pos += 2,
                        _ => {
                            word.text.push('\\');
                            self.pos += 1;
                        }
                    }
                }
                Some('$') => self.dollar(word, subs, true)?,
                Some('`') => {
                    self.pos += 1;
                    let (text, inner) = self.backquote()?;
                    subs.push(inner);
                    word.text.push('`');
                    word.text.push_str(&text);
                    word.text.push('`');
                    word.dynamic = true;
                }
                Some(c) => {
                    word.text.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    /// Moves the substitutions queued while reading words into `script`.
    fn flush_subs(&mut self, script: &mut Script) {
        let mut kept = Vec::new();
        for (slot, word, sub) in self.bodies.drain(..) {
            if slot.0 == usize::MAX {
                merge(script, sub, false);
            } else {
                kept.push((slot, word, sub));
            }
        }
        self.bodies = kept;
    }

    /// After a newline: read each pending heredoc body in order.
    fn read_heredoc_bodies(&mut self) -> Result<()> {
        let pending = std::mem::take(&mut self.pending_heredocs);
        for doc in pending {
            let mut body = String::new();
            loop {
                if self.pos >= self.chars.len() {
                    // An unterminated heredoc runs to the end of the script.
                    break;
                }
                let mut line = String::new();
                while let Some(c) = self.peek() {
                    self.pos += 1;
                    if c == '\n' {
                        break;
                    }
                    line.push(c);
                }
                let compare = if doc.strip_tabs {
                    line.trim_start_matches('\t')
                } else {
                    line.as_str()
                };
                if compare == doc.delimiter {
                    break;
                }
                body.push_str(compare);
                body.push('\n');
            }
            let mut word = Word {
                text: body.clone(),
                quoted: true,
                ..Word::default()
            };
            let mut subs = Script::default();
            if doc.expand && (body.contains('$') || body.contains('`')) {
                // Unquoted delimiter: substitutions in the body run.
                let mut parser = Parser::new(&body);
                parser.depth = self.depth + 1;
                parser.heredoc_expansions(&mut word, &mut subs)?;
            }
            self.bodies.push((doc.slot, word, subs));
        }
        Ok(())
    }

    /// Scans an expanding heredoc body for substitutions.
    fn heredoc_expansions(&mut self, word: &mut Word, script: &mut Script) -> Result<()> {
        let mut subs = Vec::new();
        while let Some(c) = self.peek() {
            match c {
                '\\' => self.pos += 2,
                '$' => {
                    let mut scratch = Word::default();
                    self.dollar(&mut scratch, &mut subs, true)?;
                    if scratch.dynamic {
                        word.dynamic = true;
                    }
                }
                '`' => {
                    self.pos += 1;
                    let (_, inner) = self.backquote()?;
                    subs.push(inner);
                    word.dynamic = true;
                }
                _ => self.pos += 1,
            }
        }
        for sub in subs {
            merge(script, sub, false);
        }
        Ok(())
    }

    /// Parses a list until EOF or one of `stops` (a token or a reserved word
    /// at command position). `top` is true when the list runs in the
    /// session's own shell.
    fn list(&mut self, script: &mut Script, top: bool, stops: &[&str]) -> Result<()> {
        loop {
            self.skip_blank_and_newlines();
            let tok = self.peek_tok()?;
            match &tok {
                Tok::Eof => return Ok(()),
                Tok::Op(op) if stops.contains(op) => return Ok(()),
                Tok::Op(";;" | ";&" | ";;&") if stops.contains(&";;") => return Ok(()),
                Tok::Word(word) if !word.quoted && stops.contains(&word.text.as_str()) => {
                    return Ok(());
                }
                _ => {}
            }
            let start = script.commands.len();
            let count = self.pipeline(script, top)?;
            // A list element followed by `&` runs in the background.
            self.skip_blank();
            match self.peek_tok()? {
                Tok::Op("&") => {
                    self.next_tok()?;
                    for command in &mut script.commands[start..] {
                        command.persists = false;
                    }
                }
                Tok::Op(";" | "&&" | "||" | "\n") => {
                    let tok = self.next_tok()?;
                    if tok == Tok::Op("\n") {
                        self.read_heredoc_bodies()?;
                        self.attach_bodies(script);
                    }
                }
                Tok::Eof => {}
                Tok::Op(op) if stops.contains(&op) => {}
                Tok::Op(";;" | ";&" | ";;&") if stops.contains(&";;") => {}
                Tok::Word(word) if !word.quoted && stops.contains(&word.text.as_str()) => {}
                Tok::Op(")") if !stops.contains(&")") => {
                    return Err(self.error("unexpected )"));
                }
                other => {
                    if count == 0 {
                        return Err(self.error(&format!("unexpected {other:?}")));
                    }
                }
            }
            self.attach_bodies(script);
        }
    }

    /// Gives each heredoc body to its redirect, and its substitutions to the
    /// script.
    fn attach_bodies(&mut self, script: &mut Script) {
        let mut kept = Vec::new();
        for (slot, word, sub) in self.bodies.drain(..) {
            if slot.0 == usize::MAX {
                merge(script, sub, false);
            } else if let Some(command) = script.commands.get_mut(slot.0)
                && let Some(redirect) = command.redirects.get_mut(slot.1)
            {
                redirect.target = word;
                merge(script, sub, false);
            } else {
                kept.push((slot, word, sub));
            }
        }
        self.bodies = kept;
    }

    /// Parses a pipeline; returns how many commands it had.
    fn pipeline(&mut self, script: &mut Script, top: bool) -> Result<usize> {
        let start = script.commands.len();
        let mut count = 0;
        loop {
            self.skip_blank();
            let piped_in = count > 0;
            let before = script.commands.len();
            self.command(script, top)?;
            if piped_in && let Some(first) = script.commands.get_mut(before) {
                first.piped_in = true;
            }
            count += 1;
            self.skip_blank();
            match self.peek_tok()? {
                Tok::Op("|" | "|&") => {
                    self.next_tok()?;
                    self.skip_blank_and_newlines();
                }
                _ => break,
            }
        }
        if count > 1 {
            for command in &mut script.commands[start..] {
                command.persists = false;
            }
        }
        Ok(count)
    }

    /// Parses one command: a subshell, a group, a compound command, a
    /// function definition or a simple command.
    fn command(&mut self, script: &mut Script, top: bool) -> Result<()> {
        self.skip_blank();
        match self.peek_tok()? {
            Tok::Op("(") => {
                self.next_tok()?;
                let mut inner = Script::default();
                self.list(&mut inner, false, &[")"])?;
                self.skip_blank_and_newlines();
                if self.next_tok()? != Tok::Op(")") {
                    return Err(self.error("unclosed ("));
                }
                merge(script, inner, false);
                self.trailing_redirects(script)?;
                Ok(())
            }
            Tok::Word(word) if !word.quoted && word.text == "case" => {
                self.next_tok()?;
                self.case(script, top)
            }
            Tok::Word(word) if !word.quoted && (word.text == "for" || word.text == "select") => {
                self.next_tok()?;
                // `for name [in words]` up to the `;` or newline, or `for ((...))`.
                loop {
                    match self.next_tok()? {
                        Tok::Op(";" | "\n") | Tok::Eof => break,
                        Tok::Word(w) if !w.quoted && w.text == "do" => {
                            self.pos -= 2;
                            break;
                        }
                        Tok::Word(_) => {}
                        other => return Err(self.error(&format!("unexpected {other:?} in for"))),
                    }
                }
                self.flush_subs(script);
                Ok(())
            }
            Tok::Word(word) if !word.quoted && word.text == "function" => {
                self.next_tok()?;
                let Tok::Word(name) = self.next_tok()? else {
                    return Err(self.error("function needs a name"));
                };
                script.functions.push(name.text.clone());
                self.skip_blank();
                if self.starts_with("()") {
                    self.pos += 2;
                } else if self.peek() == Some('(') {
                    self.pos += 1;
                    self.skip_blank();
                    if self.peek() != Some(')') {
                        return Err(self.error("bad function definition"));
                    }
                    self.pos += 1;
                }
                self.skip_blank_and_newlines();
                self.command(script, false)
            }
            Tok::Word(word)
                if !word.quoted && RESERVED_SEPARATORS.contains(&word.text.as_str()) =>
            {
                // Reserved words only shape control flow; the commands around
                // them are what runs.
                self.next_tok()?;
                if word.text == "time" {
                    // `time -p cmd`
                    self.skip_blank();
                    if self.starts_with("-p ") {
                        self.pos += 2;
                    }
                }
                Ok(())
            }
            Tok::Eof => Ok(()),
            Tok::Op(op) if op == "\n" || op == ";" || op == "&&" || op == "||" => Ok(()),
            _ => self.simple(script, top),
        }
    }

    fn case(&mut self, script: &mut Script, top: bool) -> Result<()> {
        let Tok::Word(_) = self.next_tok()? else {
            return Err(self.error("case needs a word"));
        };
        self.flush_subs(script);
        self.skip_blank_and_newlines();
        match self.next_tok()? {
            Tok::Word(w) if w.text == "in" => {}
            _ => return Err(self.error("case needs in")),
        }
        loop {
            self.skip_blank_and_newlines();
            match self.peek_tok()? {
                Tok::Word(w) if !w.quoted && w.text == "esac" => {
                    self.next_tok()?;
                    return Ok(());
                }
                Tok::Eof => return Err(self.error("unclosed case")),
                _ => {}
            }
            if self.peek_tok()? == Tok::Op("(") {
                self.next_tok()?;
            }
            // Patterns up to `)`.
            loop {
                match self.next_tok()? {
                    Tok::Word(_) | Tok::Op("|") => {}
                    Tok::Op(")") => break,
                    other => return Err(self.error(&format!("unexpected {other:?} in case"))),
                }
            }
            self.flush_subs(script);
            self.list(script, top, &[";;", "esac"])?;
            self.skip_blank_and_newlines();
            if let Tok::Op(";;" | ";&" | ";;&") = self.peek_tok()? {
                self.next_tok()?;
            }
        }
    }

    fn trailing_redirects(&mut self, script: &mut Script) -> Result<()> {
        loop {
            self.skip_blank();
            match self.peek_tok()? {
                Tok::Redirect(op) => {
                    self.next_tok()?;
                    let mut holder = Simple::default();
                    self.redirect_target(&op, &mut holder, script.commands.len())?;
                    holder.persists = false;
                    script.commands.push(holder);
                }
                _ => return Ok(()),
            }
        }
    }

    fn redirect_target(&mut self, op: &str, simple: &mut Simple, index: usize) -> Result<()> {
        self.skip_blank();
        let target = match self.next_tok()? {
            Tok::Word(word) => word,
            other => return Err(self.error(&format!("redirection needs a target, got {other:?}"))),
        };
        let heredoc = op == "<<" || op == "<<-";
        let herestring = op == "<<<";
        let dup = (op == ">&" || op == "<&")
            && target
                .fixed()
                .is_some_and(|t| t == "-" || t.chars().all(|c| c.is_ascii_digit()));
        let writes = !heredoc && !herestring && !dup && op != "<" && op != "<&";
        simple.redirects.push(Redirect {
            op: op.to_owned(),
            target: target.clone(),
            writes,
            stdin_text: heredoc || herestring,
        });
        if heredoc {
            let raw_quoted = target.quoted || target.text.contains('\'');
            self.pending_heredocs.push(PendingHeredoc {
                delimiter: target.text.clone(),
                strip_tabs: op == "<<-",
                expand: !raw_quoted,
                slot: (index, simple.redirects.len() - 1),
            });
        }
        Ok(())
    }

    fn simple(&mut self, script: &mut Script, top: bool) -> Result<()> {
        let index = script.commands.len();
        let mut simple = Simple {
            persists: top,
            ..Simple::default()
        };
        loop {
            self.skip_blank();
            match self.peek_tok()? {
                Tok::Word(word) => {
                    self.next_tok()?;
                    if simple.words.is_empty()
                        && let Some(assign) = assignment(&word, word.quoted)
                    {
                        simple.assigns.push(assign);
                        continue;
                    }
                    // `name () { ... }`: a function definition.
                    if simple.words.is_empty() && simple.assigns.is_empty() {
                        self.skip_blank();
                        if self.starts_with("()") || self.starts_with("( )") {
                            self.pos += if self.starts_with("()") { 2 } else { 3 };
                            script.functions.push(word.text.clone());
                            self.skip_blank_and_newlines();
                            self.flush_subs(script);
                            return self.command(script, false);
                        }
                    }
                    simple.words.push(word);
                }
                Tok::Redirect(op) => {
                    self.next_tok()?;
                    self.redirect_target(&op, &mut simple, index)?;
                }
                _ => break,
            }
        }
        if simple.words.is_empty() && simple.assigns.is_empty() && simple.redirects.is_empty() {
            return Err(self.error("expected a command"));
        }
        // `[[ ... ]]` is a test, not a program.
        script.commands.push(simple);
        self.flush_subs(script);
        Ok(())
    }
}

/// Appends `inner`'s commands to `script`; when the inner script ran in a
/// subshell or a substitution nothing in it persists.
fn merge(script: &mut Script, inner: Script, persists: bool) {
    for mut command in inner.commands {
        if !persists {
            command.persists = false;
        }
        script.commands.push(command);
    }
    script.functions.extend(inner.functions);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(script: &Script) -> Vec<Vec<String>> {
        script
            .commands
            .iter()
            .map(|c| c.words.iter().map(|w| w.text.clone()).collect())
            .collect()
    }

    #[test]
    fn splits_lists_pipelines_and_quotes() {
        let script = parse("git 'push' origin \"main\" && echo ok | tee x; ls").unwrap();
        assert_eq!(
            words(&script),
            vec![
                vec!["git", "push", "origin", "main"],
                vec!["echo", "ok"],
                vec!["tee", "x"],
                vec!["ls"],
            ]
        );
        assert!(script.commands[0].persists);
        assert!(!script.commands[1].persists);
        assert!(script.commands[2].piped_in);
    }

    #[test]
    fn substitutions_are_commands_too() {
        let script = parse("echo $(gh pr merge 1) `curl x` <(wget y)").unwrap();
        let all = words(&script);
        assert!(all.contains(&vec!["gh".into(), "pr".into(), "merge".into(), "1".into()]));
        assert!(all.contains(&vec!["curl".into(), "x".into()]));
        assert!(all.contains(&vec!["wget".into(), "y".into()]));
        assert!(script.commands.iter().any(|c| c.words[0].text == "echo"));
    }

    #[test]
    fn substitutions_inside_expansions_are_commands_too() {
        for text in [
            ": ${x:=$(git push origin HEAD:main)}",
            "echo \"${x:-`git push origin HEAD:main`}\"",
            "echo $(( $(git push origin HEAD:main) + 1 ))",
            "(( $(git push origin HEAD:main) ))",
            "echo ${#a[$(git push origin HEAD:main)]}",
        ] {
            let script = parse(text).unwrap();
            assert!(
                words(&script).contains(&vec![
                    "git".into(),
                    "push".into(),
                    "origin".into(),
                    "HEAD:main".into()
                ]),
                "{text}: {:?}",
                words(&script)
            );
        }
    }

    #[test]
    fn ansi_c_quotes_decode() {
        let script = parse(r"$'\x67it' $'\160ush'").unwrap();
        assert_eq!(words(&script), vec![vec!["git", "push"]]);
        assert!(!script.commands[0].words[0].dynamic);
    }

    #[test]
    fn assignments_redirects_and_subshells() {
        let script = parse("A=1 PATH=./bin:$PATH tiller x > out 2>&1; (cd /x && make)").unwrap();
        let first = &script.commands[0];
        assert_eq!(first.assigns[0].0, "A");
        assert_eq!(first.assigns[1].0, "PATH");
        assert_eq!(first.redirects.len(), 2);
        assert!(first.redirects[0].writes);
        assert!(!first.redirects[1].writes);
        let cd = script
            .commands
            .iter()
            .find(|c| c.words[0].text == "cd")
            .unwrap();
        assert!(!cd.persists);
    }

    #[test]
    fn compound_commands_and_functions() {
        let script = parse(
            "if true; then git push origin main; fi\nfor x in a b; do curl $x; done\n\
             tiller() { echo hi; }\ncase $1 in a|b) gh pr merge 1;; *) ls;; esac\nwhile false; do :; done",
        )
        .unwrap();
        let all = words(&script);
        assert!(all.contains(&vec![
            "git".into(),
            "push".into(),
            "origin".into(),
            "main".into()
        ]));
        assert!(
            all.iter()
                .any(|w| w.first().map(String::as_str) == Some("curl"))
        );
        assert!(all.contains(&vec!["gh".into(), "pr".into(), "merge".into(), "1".into()]));
        assert_eq!(script.functions, vec!["tiller".to_string()]);
    }

    #[test]
    fn heredocs_keep_their_body() {
        let script = parse("bash <<'EOF'\ngit push origin main\nEOF\necho done").unwrap();
        let bash = &script.commands[0];
        assert_eq!(bash.redirects[0].target.text, "git push origin main\n");
        assert!(bash.redirects[0].stdin_text);
        assert_eq!(script.commands[1].words[0].text, "echo");
    }

    #[test]
    fn expanding_heredoc_runs_substitutions() {
        let script = parse("cat <<EOF\n$(curl x)\nEOF").unwrap();
        assert!(script.commands.iter().any(|c| c.words[0].text == "curl"));
    }

    #[test]
    fn malformed_input_is_an_error() {
        for bad in [
            "echo 'open",
            "echo \"open",
            "echo $(open",
            "(echo",
            "echo `x",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn comments_and_continuations() {
        let script = parse("git \\\n push origin x # git push origin main").unwrap();
        assert_eq!(words(&script), vec![vec!["git", "push", "origin", "x"]]);
    }

    #[test]
    fn dynamic_words_keep_source() {
        let script = parse("$(which gh) pr merge 12").unwrap();
        let cmd = script.commands.iter().find(|c| c.words.len() == 4).unwrap();
        assert!(cmd.words[0].dynamic);
        assert_eq!(cmd.words[0].text, "$(which gh)");
    }
}
