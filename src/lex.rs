//! The Uiua lexer

use std::{
    collections::VecDeque,
    error::Error,
    fmt,
    hash::Hash,
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

use ecow::EcoString;
use serde::*;
use serde_tuple::*;
use unicode_segmentation::UnicodeSegmentation;

use crate::{
    ast::{SubSide, Subscript},
    Ident, Inputs, Primitive, WILDCARD_CHAR,
};

/// Subscript digit characters
pub const SUBSCRIPT_DIGITS: [char; 10] = ['₀', '₁', '₂', '₃', '₄', '₅', '₆', '₇', '₈', '₉'];

/// Lex a Uiua source file
pub fn lex(
    input: &str,
    src: impl IntoInputSrc,
    inputs: &mut Inputs,
) -> (Vec<Sp<Token>>, Vec<Sp<LexError>>, InputSrc) {
    let src = inputs.add_src(src, input);

    // Guard against degenerate inputs
    let mut char_pos = 0;
    let mut byte_pos = 0;
    for (i, line) in input.lines().enumerate() {
        let span = || CodeSpan {
            start: Loc {
                line: i as u16,
                col: 1,
                char_pos,
                byte_pos,
            },
            end: Loc {
                line: i as u16,
                col: 2,
                char_pos: char_pos + 1,
                byte_pos: byte_pos + line.chars().next().unwrap().len_utf8() as u32,
            },
            src: src.clone(),
        };
        if i > u16::MAX as usize {
            let err = LexError::FileTooLong;
            let span = span();
            return (Vec::new(), vec![Sp { value: err, span }], src);
        }
        if line.chars().count() > u16::MAX as usize {
            let err = LexError::LineTooLong(i + 1);
            let span = span();
            return (Vec::new(), vec![Sp { value: err, span }], src);
        }
        for c in line.chars() {
            char_pos += 1;
            byte_pos += c.len_utf8() as u32;
        }
    }

    // Collect graphemes
    let mut input_segments: Vec<&str> = input.graphemes(true).collect();
    // Split combining characters from some base characters
    let mut i = 0;
    while i < input_segments.len() {
        for pre in [" ", "\"", "@"] {
            if let Some(rest) = input_segments[i].strip_prefix(pre) {
                input_segments[i] = pre;
                if !rest.is_empty() {
                    input_segments.insert(i + 1, rest);
                    i += 1;
                }
                break;
            }
        }
        i += 1;
    }

    let (tokens, errors) = Lexer {
        input,
        input_segments,
        loc: Loc {
            char_pos: 0,
            byte_pos: 0,
            line: 1,
            col: 1,
        },
        src: src.clone(),
        tokens: VecDeque::new(),
        errors: Vec::new(),
    }
    .run();
    (tokens, errors, src)
}

/// An error that occurred while lexing
#[allow(missing_docs)]
#[derive(Debug, Clone)]
pub enum LexError {
    UnexpectedChar(String),
    ExpectedCharacter(Vec<char>),
    InvalidEscape(String),
    InvalidUnicodeEscape(u32),
    InvalidEscapeSequence(String),
    ExpectedNumber,
    LineTooLong(usize),
    FileTooLong,
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LexError::UnexpectedChar(c) => write!(f, "Unexpected char {c:?}"),
            LexError::ExpectedCharacter(chars) if chars.is_empty() => {
                write!(f, "Expected character")
            }
            LexError::ExpectedCharacter(chars) if chars.len() == 1 => {
                write!(f, "Expected {:?}", chars[0])
            }
            LexError::ExpectedCharacter(chars) if chars.len() == 2 => {
                write!(f, "Expected {:?} or {:?}", chars[0], chars[1])
            }
            LexError::ExpectedCharacter(chars) => write!(f, "Expected one of {:?}", chars),
            LexError::InvalidEscape(c) => write!(f, "Invalid escape character {c:?}"),
            LexError::InvalidUnicodeEscape(c) => write!(f, "Invalid unicode escape \\\\{c:x}"),
            LexError::InvalidEscapeSequence(c) => write!(f, "Invalid escape \\\\{c}"),
            LexError::ExpectedNumber => write!(f, "Expected number"),
            LexError::LineTooLong(n) => write!(f, "Line {n} is too long"),
            LexError::FileTooLong => write!(f, "File is too long"),
        }
    }
}

impl Error for LexError {}

/// A location in a Uiua source file
#[allow(missing_docs)]
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize_tuple, Deserialize_tuple,
)]
pub struct Loc {
    pub line: u16,
    pub col: u16,
    pub byte_pos: u32,
    pub char_pos: u32,
}

impl fmt::Display for Loc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.col)
    }
}

impl Default for Loc {
    fn default() -> Self {
        Self {
            char_pos: 0,
            byte_pos: 0,
            line: 1,
            col: 1,
        }
    }
}

/// A runtime span in a Uiua source file
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Span {
    /// A span that has a place in actual code
    Code(CodeSpan),
    /// A span whose origin in the interpreter
    Builtin,
}

impl From<CodeSpan> for Span {
    fn from(span: CodeSpan) -> Self {
        Self::Code(span)
    }
}

impl PartialEq<CodeSpan> for Span {
    fn eq(&self, other: &CodeSpan) -> bool {
        match self {
            Self::Code(span) => span == other,
            Self::Builtin => false,
        }
    }
}

impl Span {
    /// Use this span to wrap a value
    pub fn sp<T>(self, value: T) -> Sp<T, Self> {
        Sp { value, span: self }
    }
    /// Merge two spans
    pub fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Span::Code(a), Span::Code(b)) => Span::Code(a.merge(b)),
            (Span::Code(a), Span::Builtin) => Span::Code(a),
            (Span::Builtin, Span::Code(b)) => Span::Code(b),
            (Span::Builtin, Span::Builtin) => Span::Builtin,
        }
    }
    /// Get the code span, if any
    pub fn code(self) -> Option<CodeSpan> {
        match self {
            Span::Code(span) => Some(span),
            Span::Builtin => None,
        }
    }
}

/// The source of code input into the interpreter
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(untagged, into = "InputSrcRep", from = "InputSrcRep")]
pub enum InputSrc {
    /// Code from a file with a path
    File(Arc<Path>),
    /// Code from a string
    Str(usize),
    /// Code generated by a macro
    Macro(Box<CodeSpan>),
}

impl PartialEq<Path> for InputSrc {
    fn eq(&self, other: &Path) -> bool {
        match self {
            InputSrc::File(path) => (path.canonicalize().ok())
                .zip(other.canonicalize().ok())
                .is_some_and(|(a, b)| a == b),
            _ => false,
        }
    }
}

impl PartialEq<InputSrc> for Path {
    fn eq(&self, other: &InputSrc) -> bool {
        other == self
    }
}

impl PartialEq<PathBuf> for InputSrc {
    fn eq(&self, other: &PathBuf) -> bool {
        match self {
            InputSrc::File(path) => (path.canonicalize().ok())
                .zip(other.canonicalize().ok())
                .is_some_and(|(a, b)| a == b),
            _ => false,
        }
    }
}

impl PartialEq<InputSrc> for PathBuf {
    fn eq(&self, other: &InputSrc) -> bool {
        other == self
    }
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum InputSrcRep {
    File(PathBuf),
    Str(usize),
    Macro(CodeSpan),
}

impl From<InputSrc> for InputSrcRep {
    fn from(src: InputSrc) -> Self {
        match src {
            InputSrc::File(path) => InputSrcRep::File(path.to_path_buf()),
            InputSrc::Str(index) => InputSrcRep::Str(index),
            InputSrc::Macro(span) => InputSrcRep::Macro(*span),
        }
    }
}

impl From<InputSrcRep> for InputSrc {
    fn from(src: InputSrcRep) -> Self {
        match src {
            InputSrcRep::File(path) => InputSrc::File(path.into()),
            InputSrcRep::Str(index) => InputSrc::Str(index),
            InputSrcRep::Macro(span) => InputSrc::Macro(span.into()),
        }
    }
}

impl<'a> From<&'a Path> for InputSrc {
    fn from(path: &'a Path) -> Self {
        InputSrc::File(path.into())
    }
}

/// A trait for types that can be converted into an `InputSrc`
pub trait IntoInputSrc {
    /// Convert into an `InputSrc`
    fn into_input_src(self, str_index: usize) -> InputSrc;
}

impl IntoInputSrc for InputSrc {
    fn into_input_src(self, _: usize) -> InputSrc {
        self
    }
}

impl IntoInputSrc for &Path {
    fn into_input_src(self, _: usize) -> InputSrc {
        self.into()
    }
}

impl IntoInputSrc for &PathBuf {
    fn into_input_src(self, _: usize) -> InputSrc {
        self.as_path().into()
    }
}

impl IntoInputSrc for () {
    fn into_input_src(self, str_index: usize) -> InputSrc {
        InputSrc::Str(str_index)
    }
}

/// A span in a Uiua source file
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize_tuple, Deserialize_tuple)]
pub struct CodeSpan {
    /// The path of the file
    pub src: InputSrc,
    /// The starting location
    pub start: Loc,
    /// The ending location
    pub end: Loc,
}

impl fmt::Debug for CodeSpan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} - {}", self.start, self.end)
    }
}

impl fmt::Display for CodeSpan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.src {
            InputSrc::File(path) => {
                let mut file: String = path.to_string_lossy().into_owned();
                if let Some(s) = file.strip_prefix("C:\\Users\\") {
                    if let Some((_, sub)) = s.split_once('\\') {
                        file = format!("~\\{}", sub);
                    } else {
                        file = s.to_string();
                    }
                }
                let file = file.replace("\\.\\", "\\");
                write!(f, "{}:{}", file, self.start)
            }
            InputSrc::Str(_) => self.start.fmt(f),
            InputSrc::Macro(span) => span.fmt(f),
        }
    }
}

impl fmt::Debug for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Span::Code(span) => span.fmt(f),
            Span::Builtin => write!(f, "<builtin>"),
        }
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Span::Code(span) => span.fmt(f),
            Span::Builtin => write!(f, "<builtin>"),
        }
    }
}

impl CodeSpan {
    pub(crate) const fn sp<T>(self, value: T) -> Sp<T> {
        Sp { value, span: self }
    }
    pub(crate) fn dummy() -> Self {
        Self {
            src: InputSrc::Str(0),
            start: Loc::default(),
            end: Loc::default(),
        }
    }
    /// Get the number of characters in the span
    pub fn char_count(&self) -> u32 {
        self.end.char_pos.saturating_sub(self.start.char_pos)
    }
    /// Merge two spans
    pub fn merge(mut self, end: Self) -> Self {
        self.merge_with(end);
        self
    }
    /// Merge two spans
    pub fn merge_with(&mut self, end: Self) {
        self.start = self.start.min(end.start);
        self.end = self.end.max(end.end);
    }
    pub(crate) fn end_to(self, other: &Self) -> Self {
        CodeSpan {
            start: self.end,
            end: other.start,
            ..self
        }
    }
    /// Get the text of the span
    pub fn byte_range(&self) -> Range<usize> {
        self.start.byte_pos as usize..self.end.byte_pos as usize
    }
    /// Check if the span contains a line and column
    ///
    /// Excludes the end column
    pub fn contains_line_col(&self, line: usize, col: usize) -> bool {
        self.contains_line_col_impl(line, col, false)
    }
    /// Check if the span contains a line and column
    ///
    /// Includes the end column
    pub fn contains_line_col_end(&self, line: usize, col: usize) -> bool {
        self.contains_line_col_impl(line, col, true)
    }
    fn contains_line_col_impl(&self, line: usize, col: usize, include_end: bool) -> bool {
        let line = line as u16;
        let col = col as u16;
        if self.start.line == self.end.line {
            self.start.line == line
                && (!include_end && (self.start.col..self.end.col).contains(&col)
                    || include_end && (self.start.col..=self.end.col).contains(&col))
        } else {
            (self.start.line..=self.end.line).contains(&line)
                && (self.start.line < line || col > self.start.col)
                && (self.end.line > line
                    || !include_end && col < self.end.col
                    || include_end && col <= self.end.col)
        }
    }
    /// Get the text of the span from the inputs
    #[track_caller]
    pub fn as_str<T>(&self, inputs: &Inputs, f: impl FnOnce(&str) -> T) -> T {
        inputs.get_with(&self.src, |input| f(&input[self.byte_range()]))
    }
    /// Get the text before the span from the inputs
    pub fn before_str<T>(&self, inputs: &Inputs, f: impl FnOnce(&str) -> T) -> T {
        inputs.get_with(&self.src, |input| f(&input[..self.start.byte_pos as usize]))
    }
    /// Get the text of the span from the inputs
    pub fn try_as_str<T>(&self, inputs: &Inputs, f: impl FnOnce(&str) -> T) -> Option<T> {
        inputs.try_get_with(&self.src, |input| f(&input[self.byte_range()]))
    }
    /// Get just the span of the first character
    pub fn just_start(&self, inputs: &Inputs) -> Self {
        let start = self.start;
        let mut end = self.start;
        end.char_pos += 1;
        end.byte_pos += self.as_str(inputs, |s| {
            s.chars().next().map_or(0, char::len_utf8) as u32
        });
        end.col += 1;
        CodeSpan {
            start,
            end,
            ..self.clone()
        }
    }
    /// Get just the span of the last character
    pub fn just_end(&self, inputs: &Inputs) -> Self {
        let end = self.end;
        let mut start = self.end;
        start.char_pos = start.char_pos.saturating_sub(1);
        start.byte_pos = start.byte_pos.saturating_sub(self.as_str(inputs, |s| {
            s.chars().next_back().map_or(0, char::len_utf8) as u32
        }));
        start.col = start.col.saturating_sub(1);
        CodeSpan {
            start,
            end,
            ..self.clone()
        }
    }
}

/// A span wrapping a value
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Sp<T, S = CodeSpan> {
    /// The value
    pub value: T,
    /// The span
    pub span: S,
}

impl<T> Sp<T> {
    /// Map the value
    pub fn map<U, F: FnOnce(T) -> U>(self, f: F) -> Sp<U> {
        Sp {
            value: f(self.value),
            span: self.span,
        }
    }
    /// Map the value into a new one
    pub fn map_into<U>(self) -> Sp<U>
    where
        T: Into<U>,
    {
        self.map(Into::into)
    }
    /// Get a spanned reference to the value
    pub fn as_ref(&self) -> Sp<&T> {
        Sp {
            value: &self.value,
            span: self.span.clone(),
        }
    }
    /// Maybe map the value
    pub fn filter_map<U>(self, f: impl FnOnce(T) -> Option<U>) -> Option<Sp<U>> {
        f(self.value).map(|value| Sp {
            value,
            span: self.span,
        })
    }
}

impl<T: Clone> Sp<&T> {
    /// Clone a span-wrapped reference
    pub fn cloned(self) -> Sp<T> {
        Sp {
            value: self.value.clone(),
            span: self.span,
        }
    }
}

impl<T: fmt::Debug, S: fmt::Debug> fmt::Debug for Sp<T, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: ", self.span)?;
        self.value.fmt(f)
    }
}

impl<T: fmt::Display, S: fmt::Display> fmt::Display for Sp<T, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.span, self.value)
    }
}

impl<T: Error> Error for Sp<T> {}

impl<T> From<Sp<T>> for Sp<T, Span> {
    fn from(value: Sp<T>) -> Self {
        Self {
            value: value.value,
            span: Span::Code(value.span),
        }
    }
}

/// A Uiua lexical token
#[allow(missing_docs)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    Comment,
    SemanticComment(SemanticComment),
    OutputComment(usize),
    Ident(Ident),
    Number,
    Char(String),
    Str(String),
    Label(Ident),
    FormatStr(Vec<String>),
    MultilineString(String),
    MultilineFormatStr(Vec<String>),
    Simple(AsciiToken),
    Glyph(Primitive),
    Placeholder(usize),
    Subscr(Subscript),
    LeftArrow,
    LeftStrokeArrow,
    LeftArrowTilde,
    OpenAngle,
    CloseAngle,
    OpenModule,
    CloseModule,
    Newline,
    Spaces,
}

impl Token {
    pub(crate) fn as_ident(&self) -> Option<Ident> {
        match self {
            Token::Ident(ident) => Some(ident.clone()),
            _ => None,
        }
    }
    pub(crate) fn as_char(&self) -> Option<String> {
        match self {
            Token::Char(char) => Some(char.clone()),
            _ => None,
        }
    }
    pub(crate) fn as_string(&self) -> Option<&str> {
        match self {
            Token::Str(string) => Some(string),
            _ => None,
        }
    }
    pub(crate) fn as_format_string(&self) -> Option<Vec<String>> {
        match self {
            Token::FormatStr(frags) => Some(frags.clone()),
            _ => None,
        }
    }
    pub(crate) fn as_multiline_string(&self) -> Option<String> {
        match self {
            Token::MultilineString(s) => Some(s.clone()),
            _ => None,
        }
    }
    pub(crate) fn as_multiline_format_string(&self) -> Option<Vec<String>> {
        match self {
            Token::MultilineFormatStr(parts) => Some(parts.clone()),
            _ => None,
        }
    }
    pub(crate) fn as_output_comment(&self) -> Option<usize> {
        match self {
            Token::OutputComment(n) => Some(*n),
            _ => None,
        }
    }
    pub(crate) fn as_label(&self) -> Option<&str> {
        match self {
            Token::Label(label) => Some(label),
            _ => None,
        }
    }
    pub(crate) fn as_placeholder(&self) -> Option<usize> {
        match self {
            Token::Placeholder(i) => Some(*i),
            _ => None,
        }
    }
    pub(crate) fn as_semantic_comment(&self) -> Option<SemanticComment> {
        match self {
            Token::SemanticComment(sc) => Some(sc.clone()),
            _ => None,
        }
    }
    pub(crate) fn as_subscript(&self) -> Option<Subscript> {
        match self {
            Token::Subscr(n) => Some(*n),
            _ => None,
        }
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Token::Comment => write!(f, "comment"),
            Token::SemanticComment(sc) => sc.fmt(f),
            Token::OutputComment(_) => write!(f, "output comment"),
            Token::Ident(_) => write!(f, "identifier"),
            Token::Number => write!(f, "number"),
            Token::Char(c) => {
                for c in c.chars() {
                    write!(f, "{c:?}")?;
                }
                Ok(())
            }
            Token::Str(s) => write!(f, "{s:?}"),
            Token::MultilineString(s) => {
                for line in s.lines() {
                    writeln!(f, "$ {line}")?;
                }
                Ok(())
            }
            Token::Label(s) => write!(f, "${s}"),
            Token::FormatStr(parts) | Token::MultilineFormatStr(parts) => {
                write!(f, "format string")?;
                for (i, part) in parts.iter().enumerate() {
                    if i > 0 {
                        write!(f, "_")?;
                    }
                    write!(f, "{part}")?;
                }
                Ok(())
            }
            Token::Simple(t) => t.fmt(f),
            Token::Glyph(p) => p.fmt(f),
            Token::LeftArrow => write!(f, "←"),
            Token::LeftStrokeArrow => write!(f, "↚"),
            Token::LeftArrowTilde => write!(f, "←~"),
            Token::OpenAngle => write!(f, "⟨"),
            Token::CloseAngle => write!(f, "⟩"),
            Token::Newline => write!(f, "newline"),
            Token::Spaces => write!(f, "space(s)"),
            Token::Subscr(sub) => match sub {
                Subscript::Empty => write!(f, "__"),
                sub => sub.fmt(f),
            },
            Token::OpenModule => write!(f, "┌─╴"),
            Token::CloseModule => write!(f, "└─╴"),
            Token::Placeholder(i) => write!(f, "^{i}"),
        }
    }
}

/// An ASCII lexical token
#[allow(missing_docs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AsciiToken {
    OpenParen,
    CloseParen,
    OpenCurly,
    CloseCurly,
    OpenBracket,
    CloseBracket,
    Underscore,
    Bar,
    Colon,
    Semicolon,
    DoubleSemicolon,
    Star,
    Percent,
    Caret,
    Equal,
    EqualTilde,
    BangEqual,
    LessEqual,
    GreaterEqual,
    Backtick,
    Tilde,
    Quote,
}

impl fmt::Display for AsciiToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AsciiToken::OpenParen => write!(f, "("),
            AsciiToken::CloseParen => write!(f, ")"),
            AsciiToken::OpenCurly => write!(f, "{{"),
            AsciiToken::CloseCurly => write!(f, "}}"),
            AsciiToken::OpenBracket => write!(f, "["),
            AsciiToken::CloseBracket => write!(f, "]"),
            AsciiToken::Underscore => write!(f, "_"),
            AsciiToken::Bar => write!(f, "|"),
            AsciiToken::Colon => write!(f, ":"),
            AsciiToken::Semicolon => write!(f, ";"),
            AsciiToken::DoubleSemicolon => write!(f, ";;"),
            AsciiToken::Star => write!(f, "*"),
            AsciiToken::Percent => write!(f, "%"),
            AsciiToken::Caret => write!(f, "^"),
            AsciiToken::Equal => write!(f, "="),
            AsciiToken::BangEqual => write!(f, "!="),
            AsciiToken::EqualTilde => write!(f, "=~"),
            AsciiToken::LessEqual => write!(f, "<="),
            AsciiToken::GreaterEqual => write!(f, ">="),
            AsciiToken::Backtick => write!(f, "`"),
            AsciiToken::Tilde => write!(f, "~"),
            AsciiToken::Quote => write!(f, "'"),
        }
    }
}

impl From<AsciiToken> for Token {
    fn from(s: AsciiToken) -> Self {
        Self::Simple(s)
    }
}

impl From<Primitive> for Token {
    fn from(p: Primitive) -> Self {
        Self::Glyph(p)
    }
}

impl From<SemanticComment> for Token {
    fn from(sc: SemanticComment) -> Self {
        Self::SemanticComment(sc)
    }
}

/// The kinds of semantic comments
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[allow(clippy::manual_non_exhaustive)]
pub enum SemanticComment {
    /// Allow experimental features
    Experimental,
    /// Prevent the containing function from being inlined
    NoInline,
    /// Prevent stack traces from going deeper
    TrackCaller,
    /// Mark that a function should be bound externally
    External,
    /// Mark a function as deprecated
    Deprecated(EcoString),
    #[doc(hidden)]
    Boo,
}

use SemanticComment::*;

impl fmt::Display for SemanticComment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SemanticComment::Experimental => write!(f, "# Experimental!"),
            SemanticComment::NoInline => write!(f, "# No inline!"),
            SemanticComment::TrackCaller => write!(f, "# Track caller!"),
            SemanticComment::External => write!(f, "# External!"),
            SemanticComment::Deprecated(s) if s.is_empty() => write!(f, "# Deprecated!"),
            SemanticComment::Deprecated(s) => write!(f, "# Deprecated! {s}"),
            SemanticComment::Boo => write!(f, "# Boo!"),
        }
    }
}

struct Lexer<'a> {
    input: &'a str,
    input_segments: Vec<&'a str>,
    loc: Loc,
    src: InputSrc,
    tokens: VecDeque<Sp<Token>>,
    errors: Vec<Sp<LexError>>,
}

impl<'a> Lexer<'a> {
    fn peek_char(&self) -> Option<&'a str> {
        self.input_segments.get(self.loc.char_pos as usize).copied()
    }
    fn update_loc(&mut self, c: &'a str) {
        for c in c.chars() {
            match c {
                '\n' => {
                    self.loc.line += 1;
                    self.loc.col = 1;
                }
                '\r' => {}
                _ => self.loc.col += 1,
            }
        }
        self.loc.char_pos += 1;
        self.loc.byte_pos += c.len() as u32;
    }
    fn next_char_if(&mut self, f: impl Fn(&str) -> bool) -> Option<&'a str> {
        let c = *self.input_segments.get(self.loc.char_pos as usize)?;
        if !f(c) {
            return None;
        }
        self.update_loc(c);
        Some(c)
    }
    fn next_char_if_all(&mut self, f: impl Fn(char) -> bool + Copy) -> Option<&'a str> {
        self.next_char_if(|c| c.chars().all(f))
    }
    fn next_char_exact(&mut self, c: &str) -> bool {
        self.next_char_if(|c2| c2 == c).is_some()
    }
    fn next_char(&mut self) -> Option<&'a str> {
        self.next_char_if(|_| true)
    }
    fn next_chars_exact<'b>(&mut self, s: impl IntoIterator<Item = &'b str>) -> bool {
        let start = self.loc;
        for c in s {
            if !self.next_char_exact(c) {
                self.loc = start;
                return false;
            }
        }
        true
    }
    #[track_caller]
    fn make_span(&self, start: Loc, end: Loc) -> CodeSpan {
        assert!(end.char_pos >= start.char_pos, "empty span");
        assert!(end.byte_pos >= start.byte_pos, "empty span");
        assert!(
            end.col >= start.col || end.line > start.line,
            "invalid start/end span"
        );
        CodeSpan {
            start,
            end,
            src: self.src.clone(),
        }
    }
    #[track_caller]
    fn end_span(&self, start: Loc) -> CodeSpan {
        self.make_span(start, self.loc)
    }
    #[track_caller]
    fn end(&mut self, token: impl Into<Token>, start: Loc) {
        self.tokens.push_back(Sp {
            value: token.into(),
            span: self.end_span(start),
        })
    }
    fn run(mut self) -> (Vec<Sp<Token>>, Vec<Sp<LexError>>) {
        use {self::AsciiToken::*, Token::*};
        // Main loop
        'main: loop {
            let start = self.loc;
            let replacement: String;
            let Some(mut c) = self.next_char() else {
                break;
            };

            // Handle escapes
            if c == "\\" && self.next_char_exact("\\") {
                let mut text = String::new();
                while let Some(c) = self.next_char_if_all(|c| c.is_ascii_alphanumeric()) {
                    text.push_str(c);
                }
                if text.len() >= 2 {
                    if let Some(special) = find_special(&text) {
                        c = special;
                    } else {
                        let mut code = 0;
                        let mut allow_hex = true;
                        for c in text.chars() {
                            let Some(digit) = c.to_digit(16) else {
                                self.errors.push(
                                    self.end_span(start)
                                        .sp(LexError::InvalidEscapeSequence(text.clone())),
                                );
                                allow_hex = false;
                                break;
                            };
                            code = code << 4 | digit;
                        }
                        if allow_hex {
                            replacement =
                                if let Some(c) = std::char::from_u32(code).filter(|_| code > 127) {
                                    c.to_string()
                                } else {
                                    self.errors.push(
                                        self.end_span(start)
                                            .sp(LexError::InvalidUnicodeEscape(code)),
                                    );
                                    continue;
                                };
                            c = &replacement;
                        }
                    }
                } else {
                    self.loc = start;
                    self.next_char();
                }
            }

            match c {
                // Backwards compatibility
                "∶" => self.end(Primitive::Flip, start),
                "⮌" => self.end(Primitive::Orient, start),
                "¨" | "𝄈" => self.end(Primitive::Backward, start),
                "⍛" => self.end(Primitive::Obverse, start),
                "◫" => {
                    self.end(Primitive::Stencil, start);
                    self.end(Primitive::Identity, self.loc);
                }
                "∈" => self.end(Primitive::MemberOf, start),

                "(" => self.end(OpenParen, start),
                ")" => self.end(CloseParen, start),
                "{" => self.end(OpenCurly, start),
                "}" => self.end(CloseCurly, start),
                "[" => self.end(OpenBracket, start),
                "]" => self.end(CloseBracket, start),
                "⟨" => self.end(OpenAngle, start),
                "⟩" => self.end(CloseAngle, start),
                "_" => {
                    if self.next_char_exact("_") {
                        if self.next_char_exact("<") {
                            self.end(Subscr(Subscript::Side(SubSide::Left)), start);
                            continue;
                        } else if self.next_char_exact(">") {
                            self.end(Subscr(Subscript::Side(SubSide::Right)), start);
                            continue;
                        }
                        let mut n: Option<i32> = None;
                        let neg = self.next_char_exact("₋")
                            || self.next_char_exact("`")
                            || self.next_char_exact("¯");
                        let mut overflow = false;
                        loop {
                            if let Some(c) = self.next_char_if_all(|c| c.is_ascii_digit()) {
                                let n = n.get_or_insert(0);
                                *n = *n * 10 + c.parse::<i32>().unwrap();
                            } else if let Some(c) =
                                self.next_char_if_all(|c| SUBSCRIPT_DIGITS.contains(&c))
                            {
                                let c = c.chars().next().unwrap();
                                let n = n.get_or_insert(0);
                                let (m, over_m) = n.overflowing_mul(10);
                                let (a, over_a) = m.overflowing_add(
                                    SUBSCRIPT_DIGITS.iter().position(|&d| d == c).unwrap() as i32,
                                );
                                overflow |= over_m | over_a;
                                *n = a;
                            } else {
                                break;
                            }
                        }
                        let sub = pick_subscript(neg, n, overflow);
                        self.end(Subscr(sub), start)
                    } else {
                        self.end(Underscore, start)
                    }
                }
                "|" => self.end(Bar, start),
                ":" => self.end(Colon, start),
                ";" if self.next_char_exact(";") => self.end(DoubleSemicolon, start),
                ";" => self.end(Semicolon, start),
                "~" => self.end(Tilde, start),
                "'" => self.end(Quote, start),
                "`" => {
                    if self.number("-") {
                        self.end(Number, start)
                    } else {
                        self.end(Backtick, start)
                    }
                }
                "¯" if self
                    .peek_char()
                    .filter(|c| c.chars().all(|c| c.is_ascii_digit()))
                    .is_some() =>
                {
                    self.number("-");
                    self.end(Number, start)
                }
                "*" => self.end(Star, start),
                "%" => self.end(Percent, start),
                "^" => {
                    if let Some(x) = self.next_char_if(|c| c.chars().all(|c| c.is_ascii_digit())) {
                        self.end(Placeholder(x.parse().unwrap()), start)
                    } else if self.next_char_exact("!") {
                        self.end(Placeholder(0), start)
                    } else {
                        self.end(Caret, start)
                    }
                }
                "=" if self.next_char_exact("~") => self.end(EqualTilde, start),
                "=" => self.end(Equal, start),
                "<" if self.next_char_exact("=") => self.end(LessEqual, start),
                ">" if self.next_char_exact("=") => self.end(GreaterEqual, start),
                "!" if self.next_char_exact("=") => self.end(BangEqual, start),
                "←" if self.next_char_exact("~") => self.end(LeftArrowTilde, start),
                "←" => self.end(LeftArrow, start),
                "↚" => self.end(LeftStrokeArrow, start),
                "┌" if self.next_char_exact("─") && self.next_char_exact("╴") => {
                    self.end(OpenModule, start)
                }
                "└" if self.next_char_exact("─") && self.next_char_exact("╴") => {
                    self.end(CloseModule, start)
                }
                // Stack
                "?" => {
                    self.end(Primitive::Stack, start);
                    let mut n = 0;
                    let start = self.loc;
                    while self.next_char_exact("?") {
                        n += 1;
                    }
                    if n > 0 {
                        self.end(Subscr(Subscript::N(n)), start);
                    }
                }
                // Comments
                "#" => {
                    let mut n = 0;
                    while self.next_char_exact("#") {
                        n += 1;
                    }
                    if n == 0 {
                        let mut comment = String::new();
                        while let Some(c) = self.next_char_if(|c| !c.ends_with('\n')) {
                            comment.push_str(c);
                        }
                        if comment.starts_with(' ') {
                            comment.remove(0);
                        }
                        match comment.trim() {
                            "Experimental!" => self.end(Experimental, start),
                            "No inline!" => self.end(NoInline, start),
                            "Track caller!" => self.end(TrackCaller, start),
                            "External!" => self.end(External, start),
                            "Boo!" => self.end(Boo, start),
                            s => {
                                if let Some(suf) = s.strip_prefix("Deprecated!") {
                                    self.end(Deprecated(suf.trim().into()), start);
                                } else {
                                    self.end(Comment, start);
                                }
                            }
                        }
                    } else {
                        loop {
                            while self.next_char_if(|c| !c.ends_with('\n')).is_some() {}
                            let restore = self.loc;
                            self.next_char_exact("\r");
                            self.next_char_exact("\n");
                            while self
                                .next_char_if(|c| c.chars().all(char::is_whitespace))
                                .is_some()
                            {}
                            if !self.next_chars_exact(["#", "#"]) {
                                self.loc = restore;
                                self.end(OutputComment(n), start);
                                continue 'main;
                            }
                            while self.next_char_exact("#") {}
                        }
                    }
                }
                // Characters
                "@" => {
                    let mut escaped = false;
                    let char = match self.character(&mut escaped, None, EscapeMode::All) {
                        Ok(Some(c)) => c,
                        Ok(None) => {
                            self.errors
                                .push(self.end_span(start).sp(LexError::ExpectedCharacter(vec![])));
                            continue;
                        }
                        Err(e) => {
                            self.errors
                                .push(self.end_span(start).sp(LexError::InvalidEscape(e.into())));
                            continue;
                        }
                    };
                    self.end(Char(char), start)
                }
                // Strings
                "\"" | "$" => {
                    let first_dollar = c == "$";
                    let reset = self.loc;
                    let format_raw = first_dollar && self.next_char_exact("$");
                    if first_dollar
                        && (self.next_char_exact(" ")
                            || self.peek_char().map_or(true, |c| "\r\n".contains(c)))
                    {
                        // Raw strings
                        let mut start = start;
                        let escape_mode = if format_raw {
                            EscapeMode::UnderscoreOnly
                        } else {
                            EscapeMode::None
                        };
                        loop {
                            let inner = self.parse_string_contents(start, None, escape_mode);
                            if format_raw {
                                let string = parse_format_fragments(&inner);
                                self.end(MultilineFormatStr(string), start);
                            } else {
                                self.end(MultilineString(inner), start);
                            }
                            let checkpoint = self.loc;
                            while self.next_char_exact("\r") {}
                            if self.next_char_if(|c| c.ends_with('\n')).is_some() {
                                // Eat leading whitespace on next line
                                while self
                                    .next_char_if(|c| {
                                        c.chars().all(char::is_whitespace) && !c.ends_with('\n')
                                    })
                                    .is_some()
                                {}
                                start = self.loc;
                                // Check for matching $ on next line
                                if !format_raw && (self.next_chars_exact(["$", " "]))
                                    || format_raw
                                        && (self.next_chars_exact(["$", "$", " "])
                                            || self.next_chars_exact(["$", "$"]))
                                {
                                    continue;
                                }
                            }
                            self.loc = checkpoint;
                            break;
                        }
                        continue;
                    }
                    self.loc = reset;
                    if first_dollar && !self.next_char_exact("\"") {
                        let label = canonicalize_ident(&self.ident(self.loc, ""));
                        self.end(Label(label), start);
                        continue;
                    }
                    // Single-line strings
                    let inner = self.parse_string_contents(start, Some('"'), EscapeMode::All);
                    if !self.next_char_exact("\"") {
                        self.errors.push(
                            self.end_span(start)
                                .sp(LexError::ExpectedCharacter(vec!['"'])),
                        );
                    }
                    if first_dollar {
                        let frags = parse_format_fragments(&inner);
                        self.end(FormatStr(frags), start)
                    } else {
                        self.end(Str(inner), start)
                    }
                }
                // Formatted subscripts
                c if "₋⌞⌟".contains(c) || c.chars().all(|c| SUBSCRIPT_DIGITS.contains(&c)) => {
                    match c {
                        "⌞" => {
                            self.end(Subscr(Subscript::Side(SubSide::Left)), start);
                            continue;
                        }
                        "⌟" => {
                            self.end(Subscr(Subscript::Side(SubSide::Right)), start);
                            continue;
                        }
                        _ => {}
                    }
                    let (mut s, neg) = if c == "₋" {
                        (String::new(), true)
                    } else {
                        (c.to_string(), false)
                    };
                    loop {
                        if let Some(c) =
                            self.next_char_if(|c| c.chars().all(|c| SUBSCRIPT_DIGITS.contains(&c)))
                        {
                            s.push_str(c);
                        } else if self.next_chars_exact(["_"; 2]) {
                            while let Some(c) = self.next_char_if_all(|c| c.is_ascii_digit()) {
                                let i: usize = c.parse().unwrap();
                                s.push(SUBSCRIPT_DIGITS[i]);
                            }
                        } else {
                            break;
                        }
                    }
                    let mut n: Option<i32> = None;
                    let mut overflow = false;
                    for c in s.chars() {
                        let i = SUBSCRIPT_DIGITS.iter().position(|&d| d == c).unwrap() as i32;
                        let n = n.get_or_insert(0);
                        let (m, over_m) = n.overflowing_mul(10);
                        let (a, over_a) = m.overflowing_add(i);
                        overflow |= over_m | over_a;
                        *n = a;
                    }
                    let sub = pick_subscript(neg, n, overflow);
                    self.end(Subscr(sub), start)
                }
                // Identifiers and unformatted glyphs
                c if is_custom_glyph(c) || c.chars().all(is_ident_start) || "&!‼".contains(c) => {
                    // Get ident start
                    let mut ident = self.ident(start, c).to_string();
                    let mut exclam_count = match c {
                        "!" => 1,
                        "‼" => 2,
                        _ => 0,
                    };
                    while let Some((ch, count)) = if self.next_char_exact("!") {
                        Some(('!', 1))
                    } else if self.next_char_exact("‼") {
                        Some(('‼', 2))
                    } else {
                        None
                    } {
                        ident.push(ch);
                        exclam_count += count;
                    }
                    let ambiguous_ne = exclam_count == 1
                        && self.input_segments.get(self.loc.char_pos as usize) == Some(&"=");
                    if ambiguous_ne {
                        ident.pop();
                        self.loc.char_pos -= 1;
                        self.loc.byte_pos -= 1;
                        self.loc.col -= 1;
                    }
                    // Try to parse as primitives
                    let lowercase_end = ident
                        .char_indices()
                        .find(|(_, c)| !c.is_ascii_lowercase() && *c != '&')
                        .map_or(ident.len(), |(i, _)| i);
                    let lowercase = &ident[..lowercase_end];
                    if let Some(prims) = Primitive::from_format_name_multi(lowercase) {
                        let first_start = start;
                        let mut start = start;
                        let prim_count = prims.len();
                        for (i, (prim, frag)) in prims.into_iter().enumerate() {
                            let end = if i < prim_count - 1 {
                                Loc {
                                    col: start.col + frag.chars().count() as u16,
                                    char_pos: start.char_pos + frag.chars().count() as u32,
                                    byte_pos: start.byte_pos + frag.len() as u32,
                                    ..start
                                }
                            } else {
                                Loc {
                                    col: first_start.col + lowercase.chars().count() as u16,
                                    char_pos: first_start.char_pos
                                        + lowercase.chars().count() as u32,
                                    byte_pos: first_start.byte_pos + lowercase.len() as u32,
                                    ..first_start
                                }
                            };
                            self.tokens.push_back(Sp {
                                value: Glyph(prim),
                                span: self.make_span(start, end),
                            });
                            start = end;
                        }
                        let rest = &ident[lowercase_end..];
                        if !rest.is_empty() {
                            let ident = canonicalize_ident(rest);
                            if let Some(sub) = subscript(&ident) {
                                self.end(Subscr(sub), start);
                            } else {
                                self.end(Ident(ident), start);
                            }
                        }
                    } else {
                        // Lone ident
                        let ident = canonicalize_ident(&ident);
                        if let Some(sub) = subscript(&ident) {
                            self.end(Subscr(sub), start);
                        } else {
                            self.end(Ident(ident), start);
                        }
                    }
                }
                // Numbers
                c if c.chars().all(|c| c.is_ascii_digit()) => {
                    self.number(c);
                    self.end(Number, start)
                }
                // Newlines
                "\n" | "\r\n" => self.end(Newline, start),
                " " | "\t" => {
                    while self.next_char_exact(" ") || self.next_char_exact("\t") {}
                    self.end(Spaces, start)
                }
                c if c.chars().all(|c| c.is_whitespace()) => continue,
                c => {
                    if c.chars().count() == 1 {
                        let c = c.chars().next().unwrap();
                        if let Some(prim) = Primitive::from_glyph(c) {
                            // Formatted glyphs
                            self.end(Glyph(prim), start);
                            continue;
                        }
                    }
                    self.errors
                        .push(self.end_span(start).sp(LexError::UnexpectedChar(c.into())));
                }
            };
        }

        // Combine some tokens

        struct PostLexer<'a> {
            tokens: VecDeque<Sp<Token>>,
            input: &'a str,
        }

        impl PostLexer<'_> {
            fn nth_is(&self, n: usize, f: impl Fn(&str) -> bool) -> bool {
                self.tokens
                    .get(n)
                    .is_some_and(|t| f(&self.input[t.span.byte_range()]))
            }
            fn next_if(&mut self, f: impl Fn(&str) -> bool) -> Option<Sp<Token>> {
                if self.nth_is(0, f) {
                    self.next()
                } else {
                    None
                }
            }
            fn next(&mut self) -> Option<Sp<Token>> {
                self.tokens.pop_front()
            }
        }

        let mut post = PostLexer {
            tokens: self.tokens,
            input: self.input,
        };

        let mut processed = Vec::new();
        while let Some(token) = post.next() {
            let s = &self.input[token.span.byte_range()];
            processed.push(
                if is_signed_numbery(s) || (["`", "¯"].contains(&s) && post.nth_is(0, is_numbery))
                {
                    let mut span = token.span;
                    if ["`", "¯"].contains(&s) {
                        let n_tok = post.next().unwrap();
                        span = span.merge(n_tok.span);
                    }
                    if post.nth_is(0, |s| s == "/")
                        && post.nth_is(1, |s| {
                            is_signed_numbery(s)
                                || (["`", "¯"].contains(&s) && post.nth_is(2, is_numbery))
                        })
                    {
                        let _slash = post.next().unwrap();
                        let _neg = post.next_if(|s| ["`", "¯"].contains(&s));
                        span = span.merge(post.next().unwrap().span);
                    }
                    span.sp(Number)
                } else {
                    token
                },
            );
        }

        (processed, self.errors)
    }
    fn ident(&mut self, start: Loc, c: &str) -> String {
        let mut s = c.to_string();
        let end = self.loc.byte_pos as usize;
        let raw = &self.input[start.byte_pos as usize..end];
        if raw.contains('\\') || is_custom_glyph(c) || !c.is_empty() && "!‼".contains(c) {
            return s;
        }
        let mut started_subscript = false;
        let mut got_neg = false;
        // Handle identifiers beginning with __
        loop {
            if self.next_chars_exact(["_"; 2]) {
                s.push_str("__");
                if let Some(left) = self.next_char_if_all(|c| "⌞<".contains(c)) {
                    s.push_str(left);
                    break s;
                }
                if let Some(right) = self.next_char_if_all(|c| "⌟>".contains(c)) {
                    s.push_str(right);
                    break s;
                }
                if !got_neg {
                    if let Some(neg) = self.next_char_if_all(|c| "₋`¯".contains(c)) {
                        s.push_str(neg);
                        got_neg = true;
                    }
                }
                while let Some(c) = self.next_char_if_all(|c| c.is_ascii_digit()) {
                    s.push_str(c);
                }
                started_subscript = true;
            } else if let Some(c) =
                self.next_char_if_all(|c| !started_subscript && is_ident_start(c))
            {
                s.push_str(c);
            } else if !got_neg && self.next_char_exact("₋") {
                s.push('₋');
                got_neg = true;
            } else if let Some(c) = self.next_char_if_all(|c| SUBSCRIPT_DIGITS.contains(&c)) {
                s.push_str(c);
                started_subscript = true;
            } else if let Some(c) = self.next_char_if(|c| "⌞⌟".contains(c)) {
                s.push_str(c);
                break s;
            } else {
                break s;
            }
        }
    }
    fn number(&mut self, init: &str) -> bool {
        // Whole part
        let mut got_digit = false;
        while self
            .next_char_if(|c| c.chars().all(|c| c.is_ascii_digit()))
            .is_some()
        {
            got_digit = true;
        }
        if !init.chars().all(|c| c.is_ascii_digit()) && !got_digit {
            return false;
        }
        // Fractional part
        let before_dot = self.loc;
        if self.next_char_exact(".") {
            let mut has_decimal = false;
            while self
                .next_char_if(|c| c.chars().all(|c| c.is_ascii_digit()))
                .is_some()
            {
                has_decimal = true;
            }
            if !has_decimal {
                self.loc = before_dot;
            }
        }
        // Exponent
        let loc_before_e = self.loc;
        if self.next_char_if(|c| c == "e" || c == "E").is_some() {
            self.next_char_if(|c| c == "-" || c == "`" || c == "¯");
            let mut got_digit = false;
            while self
                .next_char_if(|c| c.chars().all(|c| c.is_ascii_digit()))
                .is_some()
            {
                got_digit = true;
            }
            if !got_digit {
                self.loc = loc_before_e;
            }
        }
        true
    }
    fn character(
        &mut self,
        escaped: &mut bool,
        escape_char: Option<char>,
        escape_mode: EscapeMode,
    ) -> Result<Option<String>, &'a str> {
        let Some(c) =
            self.next_char_if_all(|c| !"\r\n".contains(c) && (Some(c) != escape_char || *escaped))
        else {
            return Ok(None);
        };
        Ok(Some(if *escaped {
            *escaped = false;
            match c {
                "n" => '\n'.to_string(),
                "r" => '\r'.to_string(),
                "t" => '\t'.to_string(),
                "0" => '\0'.to_string(),
                "s" => ' '.to_string(),
                "b" => '\x07'.to_string(),
                "\\" => '\\'.to_string(),
                "\"" => '"'.to_string(),
                "'" => '\''.to_string(),
                "_" => char::MAX.to_string(),
                "W" => WILDCARD_CHAR.to_string(),
                "Z" => '\u{200d}'.to_string(),
                "x" => {
                    let mut code = 0;
                    for _ in 0..2 {
                        let c = self
                            .next_char_if_all(|c| c.is_ascii_hexdigit())
                            .ok_or("x")?;
                        code = code << 4 | c.chars().next().unwrap().to_digit(16).unwrap();
                    }
                    std::char::from_u32(code).ok_or("x")?.into()
                }
                "u" => {
                    let mut code = 0;
                    match self.peek_char().ok_or("u")? {
                        "{" => {
                            self.next_char_if(|c| c == "{").ok_or("u")?;
                            for _ in 0..7 {
                                match self
                                    .next_char_if_all(|c| c.is_ascii_hexdigit() || c == '}')
                                    .ok_or("u")?
                                {
                                    "}" => break,
                                    c => {
                                        code = code << 4
                                            | c.chars().next().unwrap().to_digit(16).unwrap()
                                    }
                                }
                            }
                        }
                        _ => {
                            for _ in 0..4 {
                                let c = self
                                    .next_char_if_all(|c| c.is_ascii_hexdigit())
                                    .ok_or("u")?;
                                code = code << 4 | c.chars().next().unwrap().to_digit(16).unwrap();
                            }
                        }
                    }
                    std::char::from_u32(code).ok_or("u")?.into()
                }
                c => return Err(c),
            }
        } else if c == "\\"
            && (escape_mode == EscapeMode::All
                || escape_mode == EscapeMode::UnderscoreOnly && self.peek_char() == Some("_"))
        {
            *escaped = true;
            return self.character(escaped, escape_char, escape_mode);
        } else {
            c.into()
        }))
    }
    fn parse_string_contents(
        &mut self,
        start: Loc,
        escape_char: Option<char>,
        escape_mode: EscapeMode,
    ) -> String {
        let mut string = String::new();
        let mut escaped = false;
        loop {
            match self.character(&mut escaped, escape_char, escape_mode) {
                Ok(Some(c)) => string.push_str(&c),
                Ok(None) => break,
                Err(e) => {
                    self.errors
                        .push(self.end_span(start).sp(LexError::InvalidEscape(e.into())));
                }
            }
        }
        string
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum EscapeMode {
    All,
    None,
    UnderscoreOnly,
}

fn is_signed_numbery(mut s: &str) -> bool {
    if s.starts_with(['`', '¯']) {
        let c_len = s.chars().next().unwrap().len_utf8();
        s = &s[c_len..];
    }
    if s.is_empty() {
        return false;
    }
    is_numbery(s)
}

fn is_numbery(s: &str) -> bool {
    s.chars().all(|c| c.is_ascii_digit())
        || s == "∞"
        || (3..="infinity".len()).rev().any(|n| s == &"infinity"[..n])
        || [Primitive::Eta, Primitive::Pi, Primitive::Tau]
            .iter()
            .any(|p| {
                p.name() == s
                    || s.chars().count() == 1 && p.glyph().unwrap() == s.chars().next().unwrap()
            })
}

fn parse_format_fragments(s: &str) -> Vec<String> {
    let mut frags: Vec<String> = Vec::new();
    let mut curr = String::new();
    for c in s.chars() {
        match c {
            '_' => {
                frags.push(curr);
                curr = String::new();
            }
            char::MAX => curr.push('_'),
            c => curr.push(c),
        }
    }
    frags.push(curr);
    frags
}

/// Whether a character can be part of a Uiua identifier
pub fn is_ident_char(c: char) -> bool {
    is_ident_start(c) || SUBSCRIPT_DIGITS.contains(&c)
}

/// Whether a character can be among the first characters of a Uiua identifier
pub fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() && !"ⁿₙπτηℂ".contains(c)
}

pub(crate) fn subscript(s: &str) -> Option<Subscript> {
    if s.is_empty() {
        return None;
    }
    match s {
        "⌞" | "<" => return Some(Subscript::Side(SubSide::Left)),
        "⌟" | ">" => return Some(Subscript::Side(SubSide::Right)),
        _ => {}
    }
    let mut chars = s.chars().peekable();
    let first = *chars.peek().unwrap();
    let neg = "₋`¯".contains(first);
    if neg {
        chars.next();
    }
    let mut n: Option<i32> = None;
    let mut overflow = false;
    for c in chars {
        let i = (SUBSCRIPT_DIGITS.iter().position(|&d| c == d))
            .or_else(|| "0123456789".chars().position(|d| c == d))? as i32;
        let n = n.get_or_insert(0);
        let (m, over_m) = n.overflowing_mul(10);
        let (a, over_a) = m.overflowing_add(i);
        overflow |= over_m | over_a;
        *n = a;
    }
    Some(pick_subscript(neg, n, overflow))
}

/// Whether a string is a custom glyph
pub fn is_custom_glyph(c: &str) -> bool {
    match c.chars().count() {
        0 => false,
        1 => {
            let c = c.chars().next().unwrap();
            !c.is_ascii() && !is_ident_char(c) && Primitive::from_glyph(c).is_none()
        }
        _ => c
            .chars()
            .all(|c| !c.is_ascii() && !is_ident_char(c) && Primitive::from_glyph(c).is_none()),
    }
}

pub(crate) fn canonicalize_ident(ident: &str) -> Ident {
    canonicalize_subscripts(canonicalize_exclams(ident))
}

/// Rewrite the identifier with the same number of exclamation points
/// using double and single exclamation point characters as needed
fn canonicalize_exclams(ident: &str) -> Ident {
    let num_margs = crate::parse::ident_modifier_args(ident);
    place_exclams(ident, num_margs)
}

/// Rewrite an identifier with the given amount of double and single exclamation points
fn place_exclams(ident: &str, count: usize) -> Ident {
    let mut new: Ident = ident.trim_end_matches(['!', '‼']).into();
    let num_double = count / 2;
    let trailing_single = count % 2 == 1;
    for _ in 0..num_double {
        new.push('‼');
    }
    if trailing_single {
        new.push('!');
    }
    new
}

/// Rewrite the identifier with numerals preceded by `__` replaced with subscript characters
fn canonicalize_subscripts(ident: Ident) -> Ident {
    if !ident.contains('_') {
        return ident;
    }
    // This hasty canonicalization is okay because the stricter
    // rules about the syntax are handled in the lexer
    (ident.chars().filter(|c| *c != '_'))
        .map(|c| {
            if let Some(d) = c.to_digit(10) {
                crate::lex::SUBSCRIPT_DIGITS[d as usize]
            } else {
                match c {
                    '<' => '⌞',
                    '>' => '⌟',
                    '`' | '¯' => '₋',
                    c => c,
                }
            }
        })
        .collect()
}

thread_local! {
    static SPECIAL: Vec<(&'static str, &'static str, &'static str)> = [
        ("Alpha", "α", "Α"),
        ("Beta", "β", "Β"),
        ("Gamma", "γ", "Γ"),
        ("Delta", "δ", "Δ"),
        ("Epsilon", "ε", "Ε"),
        ("Zeta", "ζ", "Ζ"),
        ("Eta", "η", "Η"),
        ("Theta", "θ", "Θ"),
        ("Iota", "ι", "Ι"),
        ("Kappa", "κ", "Κ"),
        ("Lambda", "λ", "Λ"),
        ("Mu", "μ", "Μ"),
        ("Nu", "ν", "Ν"),
        ("Xi", "ξ", "Ξ"),
        ("Omicron", "ο", "Ο"),
        ("Pi", "π", "Π"),
        ("Rho", "ρ", "Ρ"),
        ("Sigma", "σ", "Σ"),
        ("Tau", "τ", "Τ"),
        ("Upsilon", "υ", "Υ"),
        ("Phi", "φ", "Φ"),
        ("Chi", "χ", "Χ"),
        ("Psi", "ψ", "Ψ"),
        ("Omega", "ω", "Ω"),
    ].into()
}

fn find_special(s: &str) -> Option<&'static str> {
    SPECIAL.with(|map| {
        for &(name, lower, upper) in map {
            if s == name {
                return Some(upper);
            }
            if !lower.is_empty() && s.eq_ignore_ascii_case(name) {
                return Some(lower);
            }
        }
        None
    })
}

fn pick_subscript(neg: bool, n: Option<i32>, overflow: bool) -> Subscript {
    if overflow {
        return Subscript::TooLarge;
    }
    match (neg, n) {
        (false, None) => Subscript::Empty,
        (true, None) => Subscript::NegOnly,
        (false, Some(n)) => Subscript::N(n),
        (true, Some(n)) => Subscript::N(-n),
    }
}
