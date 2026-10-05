use super::{Parser, ParserError};
use crate::{CompletedMarker, SyntaxKind};

const IDENT_KINDS: &[SyntaxKind] = &[SyntaxKind::LIDENT, SyntaxKind::UIDENT];
const ITEM_SEPARATOR_KINDS: &[SyntaxKind] = &[SyntaxKind::NEWLINE];
const USE_ITEM_SEPARATOR_KINDS: &[SyntaxKind] = &[SyntaxKind::COMMA, SyntaxKind::NEWLINE];
const ATTR_ARG_KINDS: &[SyntaxKind] = &[
    SyntaxKind::LIDENT,
    SyntaxKind::UIDENT,
    SyntaxKind::STRING,
    SyntaxKind::INT,
];
const ATTR_ARG_SEPARATOR_KINDS: &[SyntaxKind] = &[SyntaxKind::COMMA];

impl Parser<'_, '_> {
    /// Parses the currently implemented source-file grammar into parser events.
    ///
    /// This slice supports module headers and use declarations. Other item
    /// forms return [`ParserError::UnsupportedItem`] until their grammar
    /// routines are implemented.
    pub fn parse_source_file(&mut self) -> Result<(), ParserError> {
        if self.diagnostic_source.is_none() {
            return Err(ParserError::MissingDiagnosticSource);
        }

        let root = self.start();
        match self.parse_source_file_contents() {
            Ok(()) => root.complete(self, SyntaxKind::SOURCE_FILE).map(|_| ()),
            Err(error) => {
                root.abandon(self)?;
                Err(error)
            }
        }
    }

    fn parse_source_file_contents(&mut self) -> Result<(), ParserError> {
        self.module_header()?;

        while !self.at(SyntaxKind::EOF) {
            if self.eat_nl() {
                continue;
            }
            if self.at(SyntaxKind::DOC_INNER) {
                self.misplaced_doc_comment()?;
                continue;
            }
            if self.outer_docs_end_at_eof() {
                while self.at(SyntaxKind::DOC_OUTER) {
                    self.misplaced_doc_comment()?;
                }
                continue;
            }
            self.item()?;
            if !self.at(SyntaxKind::EOF) && !self.eat_nl() && !self.line_starts_top_level_item() {
                if self.at_any(&[SyntaxKind::DOC_OUTER, SyntaxKind::DOC_INNER]) {
                    while self.at_any(&[SyntaxKind::DOC_OUTER, SyntaxKind::DOC_INNER]) {
                        self.misplaced_doc_comment()?;
                    }
                    if self.line_starts_top_level_item() || self.at(SyntaxKind::EOF) {
                        continue;
                    }
                }
                self.expect_any(ITEM_SEPARATOR_KINDS)?;
                self.recover_to_newline()?;
            }
        }

        self.bump();
        Ok(())
    }

    fn module_header(&mut self) -> Result<(), ParserError> {
        let mut lookahead = 0;
        while self.nth(lookahead) == SyntaxKind::DOC_INNER {
            lookahead += 1;
        }
        let module_keyword_at = self.position.checked_add(lookahead);
        let has_unsafe_module = module_keyword_at.is_some_and(|position| {
            self.tokens
                .get(position)
                .is_some_and(|token| token.kind == SyntaxKind::UNSAFE_KW)
                && position.checked_add(1).is_some_and(|next| {
                    token_is_contextual(self.source, self.tokens, next, "module")
                })
        });
        if lookahead == 0 && !has_unsafe_module {
            return Ok(());
        }

        let header = self.start();
        if lookahead > 0 {
            let docs = self.start();
            while self.at(SyntaxKind::DOC_INNER) {
                self.bump();
            }
            docs.complete(self, SyntaxKind::DOCS)?;
        }
        if has_unsafe_module {
            self.bump();
            self.bump();
            self.expect(SyntaxKind::NEWLINE)?;
        }
        header.complete(self, SyntaxKind::MODULE_HEADER)?;
        Ok(())
    }

    fn item(&mut self) -> Result<CompletedMarker, ParserError> {
        let declaration = self.start();
        self.outer_docs()?;
        self.attributes()?;

        if self.at(SyntaxKind::PUB_KW) {
            let visibility = self.start();
            self.bump();
            visibility.complete(self, SyntaxKind::VISIBILITY)?;
        }

        if !self.at(SyntaxKind::USE_KW) {
            let position = self.position;
            let kind = self.current();
            declaration.abandon(self)?;
            return Err(ParserError::UnsupportedItem { position, kind });
        }

        self.expect(SyntaxKind::USE_KW)?;
        self.expect(SyntaxKind::PKG_PATH)?;
        if self.at(SyntaxKind::AS_KW) {
            self.use_alias()?;
        }
        if self.at(SyntaxKind::DOT) {
            self.use_items()?;
        }

        declaration.complete(self, SyntaxKind::USE_DECL)
    }

    fn attributes(&mut self) -> Result<(), ParserError> {
        loop {
            if self.at(SyntaxKind::AT) {
                self.attribute()?;
                self.expect(SyntaxKind::NEWLINE)?;
            } else if self.at_any(&[SyntaxKind::DOC_OUTER, SyntaxKind::DOC_INNER]) {
                self.misplaced_doc_comment()?;
            } else {
                break;
            }
        }
        Ok(())
    }

    fn attribute(&mut self) -> Result<(), ParserError> {
        let attribute = self.start();
        self.expect(SyntaxKind::AT)?;
        self.name_node(&[SyntaxKind::LIDENT], SyntaxKind::NAME_REF)?;
        if self.at(SyntaxKind::L_PAREN) {
            self.attr_args()?;
        }
        attribute.complete(self, SyntaxKind::ATTRIBUTE)?;
        Ok(())
    }

    fn attr_args(&mut self) -> Result<(), ParserError> {
        let args = self.start();
        self.expect(SyntaxKind::L_PAREN)?;

        while !self.at_any(&[SyntaxKind::R_PAREN, SyntaxKind::EOF]) {
            if self.line_starts_top_level_item() {
                break;
            }
            if self.at_any(&[SyntaxKind::DOC_OUTER, SyntaxKind::DOC_INNER]) {
                self.misplaced_doc_comment()?;
                continue;
            }
            let position = self.position;
            self.attr_arg()?;
            if self.eat(SyntaxKind::COMMA) {
                continue;
            }
            if self.at_any(&[SyntaxKind::R_PAREN, SyntaxKind::EOF]) {
                break;
            }

            self.expect_any(ATTR_ARG_SEPARATOR_KINDS)?;
            if self.position == position {
                let before_recovery = self.position;
                self.recover_one()?;
                if self.position == before_recovery {
                    break;
                }
            }
        }

        self.expect(SyntaxKind::R_PAREN)?;
        args.complete(self, SyntaxKind::ATTR_ARGS)?;
        Ok(())
    }

    fn attr_arg(&mut self) -> Result<(), ParserError> {
        let arg = self.start();
        if self.at_any(IDENT_KINDS) {
            self.name_node(IDENT_KINDS, SyntaxKind::NAME_REF)?;
            if self.eat(SyntaxKind::EQ) {
                self.attr_value()?;
            } else if self.at(SyntaxKind::L_PAREN) {
                self.attr_args()?;
            }
        } else if self.at_any(&[SyntaxKind::STRING, SyntaxKind::INT]) {
            self.bump();
        } else {
            self.expect_any(ATTR_ARG_KINDS)?;
        }
        arg.complete(self, SyntaxKind::ATTR_ARG)?;
        Ok(())
    }

    fn attr_value(&mut self) -> Result<(), ParserError> {
        if self.at_any(IDENT_KINDS) {
            self.name_node(IDENT_KINDS, SyntaxKind::NAME_REF)?;
            return Ok(());
        }
        if self.at_any(&[SyntaxKind::STRING, SyntaxKind::INT]) {
            self.bump();
            return Ok(());
        }
        self.expect_any(ATTR_ARG_KINDS)?;
        Ok(())
    }

    fn outer_docs(&mut self) -> Result<(), ParserError> {
        if !self.at(SyntaxKind::DOC_OUTER) {
            return Ok(());
        }
        let docs = self.start();
        while self.at(SyntaxKind::DOC_OUTER) {
            self.bump();
        }
        docs.complete(self, SyntaxKind::DOCS)?;
        Ok(())
    }

    fn outer_docs_end_at_eof(&self) -> bool {
        if !self.at(SyntaxKind::DOC_OUTER) {
            return false;
        }

        let mut position = self.position;
        while self
            .tokens
            .get(position)
            .is_some_and(|token| matches!(token.kind, SyntaxKind::DOC_OUTER | SyntaxKind::NEWLINE))
        {
            position += 1;
        }
        self.tokens
            .get(position)
            .is_some_and(|token| token.kind == SyntaxKind::EOF)
    }

    fn misplaced_doc_comment(&mut self) -> Result<(), ParserError> {
        let token = self
            .current_token()
            .ok_or(ParserError::InvalidDiagnosticSpan)?;
        if !matches!(token.kind, SyntaxKind::DOC_OUTER | SyntaxKind::DOC_INNER) {
            return Err(ParserError::InvalidTokenStream {
                position: self.position,
            });
        }
        let context = self
            .diagnostic_source
            .as_ref()
            .ok_or(ParserError::MissingDiagnosticSource)?;
        let location = sai_diagnostics::Location::from_decoded_source(
            context.source_ref.clone(),
            sai_vfs::Span::new(context.file.id(), token.range),
            context.file,
        )
        .ok_or(ParserError::InvalidDiagnosticSpan)?;
        let message = "misplaced doc comment";
        let mut diagnostic = sai_diagnostics::Diagnostic::new(
            sai_diagnostics::Code::MisplacedDocComment,
            sai_diagnostics::Severity::Warning,
            message,
            Some(location),
        );
        diagnostic.primary_label = message.to_owned();
        self.emit_error(diagnostic);
        self.demote_to_trivia();
        self.bump();
        Ok(())
    }

    fn use_alias(&mut self) -> Result<(), ParserError> {
        let alias = self.start();
        self.bump();
        self.name_node(&[SyntaxKind::LIDENT], SyntaxKind::NAME)?;
        alias.complete(self, SyntaxKind::USE_ALIAS)?;
        Ok(())
    }

    fn use_items(&mut self) -> Result<(), ParserError> {
        if self.nth(1) != SyntaxKind::L_BRACE {
            let recovery = self.start();
            self.bump();
            self.expect(SyntaxKind::L_BRACE)?;
            recovery.complete(self, SyntaxKind::ERROR)?;
            return Ok(());
        }

        let items = self.start();
        self.bump();
        self.bump();

        while !self.at(SyntaxKind::R_BRACE) && !self.at(SyntaxKind::EOF) {
            if self.line_starts_top_level_item() {
                break;
            }
            if self.at_any(&[SyntaxKind::DOC_OUTER, SyntaxKind::DOC_INNER]) {
                self.misplaced_doc_comment()?;
                continue;
            }
            if self.at(SyntaxKind::NEWLINE) {
                if self.line_starts_top_level_item() {
                    break;
                }
                self.eat_nl();
                continue;
            }
            let position = self.position;
            let parsed_item = self.use_item()?;
            if !parsed_item {
                self.recover_one()?;
            }
            if self.line_starts_top_level_item() {
                break;
            }

            if self.eat(SyntaxKind::COMMA) {
                if self.line_starts_top_level_item() {
                    break;
                }
                self.eat_nl();
                continue;
            }
            if self.at(SyntaxKind::NEWLINE) {
                if self.line_starts_top_level_item() {
                    break;
                }
                self.eat_nl();
                continue;
            }
            if self.at(SyntaxKind::R_BRACE) || self.at(SyntaxKind::EOF) {
                continue;
            }

            self.expect_any(USE_ITEM_SEPARATOR_KINDS)?;
            if self.position == position {
                self.recover_one()?;
            }
        }

        self.expect(SyntaxKind::R_BRACE)?;
        items.complete(self, SyntaxKind::USE_ITEMS)?;
        Ok(())
    }

    fn line_starts_top_level_item(&self) -> bool {
        let after_newline = self.at(SyntaxKind::NEWLINE);
        let mut position = self.position + usize::from(after_newline);
        let Some(first) = self.tokens.get(position) else {
            return false;
        };
        if !after_newline && !first.flags.contains(crate::TokenFlags::AFTER_LINE_BREAK) {
            return false;
        }

        loop {
            if self.tokens.get(position).is_some_and(|token| {
                matches!(token.kind, SyntaxKind::DOC_OUTER | SyntaxKind::DOC_INNER)
            }) {
                position += 1;
                continue;
            }
            if self
                .tokens
                .get(position)
                .is_some_and(|token| token.kind == SyntaxKind::AT)
            {
                let Some(after_attribute) = self.skip_attribute_at(position) else {
                    return false;
                };
                position = after_attribute;
                if self
                    .tokens
                    .get(position)
                    .is_some_and(|token| token.kind == SyntaxKind::NEWLINE)
                {
                    position += 1;
                    continue;
                }
                if self
                    .tokens
                    .get(position)
                    .is_some_and(|token| token.flags.contains(crate::TokenFlags::AFTER_LINE_BREAK))
                {
                    continue;
                }
                return false;
            }
            break;
        }

        if matches!(
            self.tokens.get(position).map(|token| token.kind),
            Some(
                SyntaxKind::USE_KW
                    | SyntaxKind::PUB_KW
                    | SyntaxKind::EXPORT_KW
                    | SyntaxKind::FN_KW
                    | SyntaxKind::TYPE_KW
                    | SyntaxKind::ENUM_KW
                    | SyntaxKind::TRAIT_KW
                    | SyntaxKind::IMPL_KW
                    | SyntaxKind::CONST_KW
                    | SyntaxKind::EXTERN_KW
                    | SyntaxKind::UNSAFE_KW
                    | SyntaxKind::ASYNC_KW
            )
        ) {
            return true;
        }

        let next = position
            .checked_add(1)
            .and_then(|next| self.tokens.get(next));
        match self.tokens.get(position).map(|token| token.kind) {
            Some(SyntaxKind::LIDENT)
                if token_is_contextual(self.source, self.tokens, position, "distinct")
                    || token_is_contextual(self.source, self.tokens, position, "resource")
                    || token_is_contextual(self.source, self.tokens, position, "invariant") =>
            {
                next.is_some_and(|token| token.kind == SyntaxKind::UIDENT)
            }
            Some(SyntaxKind::LIDENT)
                if token_is_contextual(self.source, self.tokens, position, "effect")
                    || token_is_contextual(self.source, self.tokens, position, "interface")
                    || token_is_contextual(self.source, self.tokens, position, "world") =>
            {
                next.is_some_and(|token| token.kind == SyntaxKind::LIDENT)
            }
            Some(SyntaxKind::LIDENT)
                if token_is_contextual(self.source, self.tokens, position, "test")
                    || token_is_contextual(self.source, self.tokens, position, "property") =>
            {
                next.is_some_and(|token| token.kind == SyntaxKind::STRING)
            }
            _ => false,
        }
    }

    fn skip_attribute_at(&self, mut position: usize) -> Option<usize> {
        if self.tokens.get(position)?.kind != SyntaxKind::AT
            || self.tokens.get(position + 1)?.kind != SyntaxKind::LIDENT
        {
            return None;
        }
        position += 2;
        if self.tokens.get(position)?.kind != SyntaxKind::L_PAREN {
            return Some(position);
        }

        let mut depth = 0usize;
        while let Some(token) = self.tokens.get(position) {
            match token.kind {
                SyntaxKind::L_PAREN => depth += 1,
                SyntaxKind::R_PAREN => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return Some(position + 1);
                    }
                }
                SyntaxKind::EOF => return None,
                _ => {}
            }
            position += 1;
        }
        None
    }

    fn use_item(&mut self) -> Result<bool, ParserError> {
        if !self.at_any(IDENT_KINDS) {
            self.expect_any(IDENT_KINDS)?;
            return Ok(false);
        }

        let item = self.start();
        self.name_node(IDENT_KINDS, SyntaxKind::NAME_REF)?;
        if self.eat(SyntaxKind::AS_KW) {
            self.name_node(IDENT_KINDS, SyntaxKind::NAME)?;
        }
        item.complete(self, SyntaxKind::USE_ITEM)?;
        Ok(true)
    }

    fn name_node(
        &mut self,
        expected: &[SyntaxKind],
        node_kind: SyntaxKind,
    ) -> Result<bool, ParserError> {
        if !self.at_any(expected) {
            self.expect_any(expected)?;
            return Ok(false);
        }
        let name = self.start();
        self.bump();
        name.complete(self, node_kind)?;
        Ok(true)
    }

    fn recover_one(&mut self) -> Result<(), ParserError> {
        if self.at_any(&[
            SyntaxKind::NEWLINE,
            SyntaxKind::COMMA,
            SyntaxKind::R_BRACE,
            SyntaxKind::EOF,
        ]) {
            return Ok(());
        }
        let error = self.start();
        self.bump();
        error.complete(self, SyntaxKind::ERROR)?;
        Ok(())
    }

    fn recover_to_newline(&mut self) -> Result<(), ParserError> {
        if self.at_any(&[SyntaxKind::NEWLINE, SyntaxKind::EOF]) {
            return Ok(());
        }
        let error = self.start();
        while !self.at_any(&[SyntaxKind::NEWLINE, SyntaxKind::EOF]) {
            self.bump();
        }
        error.complete(self, SyntaxKind::ERROR)?;
        Ok(())
    }
}

fn token_is_contextual(
    source: &str,
    tokens: &[crate::Token],
    position: usize,
    spelling: &str,
) -> bool {
    let Some(token) = tokens.get(position) else {
        return false;
    };
    token.kind == SyntaxKind::LIDENT
        && !token.flags.contains(crate::TokenFlags::RAW)
        && source.get(token.range.start().to_usize()..token.range.end().to_usize())
            == Some(spelling)
}
