use super::{Parser, ParserError};
use crate::{CompletedMarker, Marker, SyntaxKind};

const MAX_NESTING_DEPTH: usize = 512;
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
const TYPE_START_KINDS: &[SyntaxKind] = &[
    SyntaxKind::LIDENT,
    SyntaxKind::UIDENT,
    SyntaxKind::SELF_TYPE_KW,
    SyntaxKind::BORROW_KW,
    SyntaxKind::OWNED_KW,
    SyntaxKind::L_PAREN,
    SyntaxKind::UNDERSCORE,
    SyntaxKind::FN_KW,
    SyntaxKind::UNSAFE_KW,
];
const BOUND_START_KINDS: &[SyntaxKind] = &[SyntaxKind::LIDENT, SyntaxKind::UIDENT];
const GENERIC_ARG_START_KINDS: &[SyntaxKind] = &[
    SyntaxKind::LIDENT,
    SyntaxKind::UIDENT,
    SyntaxKind::SELF_TYPE_KW,
    SyntaxKind::BORROW_KW,
    SyntaxKind::OWNED_KW,
    SyntaxKind::L_PAREN,
    SyntaxKind::UNDERSCORE,
    SyntaxKind::FN_KW,
    SyntaxKind::UNSAFE_KW,
    SyntaxKind::INT,
    SyntaxKind::CHAR,
    SyntaxKind::TRUE_KW,
    SyntaxKind::FALSE_KW,
    SyntaxKind::MINUS,
];
const PARAM_START_KINDS: &[SyntaxKind] = &[
    SyntaxKind::LIDENT,
    SyntaxKind::SELF_KW,
    SyntaxKind::UNDERSCORE,
    SyntaxKind::VAR_KW,
    SyntaxKind::AT,
];
const GENERIC_PARAM_START_KINDS: &[SyntaxKind] =
    &[SyntaxKind::UIDENT, SyntaxKind::CONST_KW, SyntaxKind::AT];
const EXPRESSION_START_KINDS: &[SyntaxKind] = &[
    SyntaxKind::LIDENT,
    SyntaxKind::UIDENT,
    SyntaxKind::INT,
    SyntaxKind::FLOAT,
    SyntaxKind::STRING,
    SyntaxKind::BYTE_STRING,
    SyntaxKind::CHAR,
    SyntaxKind::BYTE,
    SyntaxKind::TRUE_KW,
    SyntaxKind::FALSE_KW,
    SyntaxKind::SELF_KW,
    SyntaxKind::SELF_TYPE_KW,
    SyntaxKind::L_PAREN,
    SyntaxKind::L_BRACKET,
    SyntaxKind::L_BRACE,
    SyntaxKind::DOT,
    SyntaxKind::MINUS,
    SyntaxKind::TILDE,
    SyntaxKind::STAR,
    SyntaxKind::BORROW_KW,
    SyntaxKind::NOT_KW,
    SyntaxKind::DOT2,
    SyntaxKind::DOT2EQ,
    SyntaxKind::FSTRING_START,
    SyntaxKind::UNSAFE_KW,
    SyntaxKind::ASYNC_KW,
    SyntaxKind::IF_KW,
    SyntaxKind::MATCH_KW,
    SyntaxKind::LOOP_KW,
    SyntaxKind::WHILE_KW,
    SyntaxKind::FOR_KW,
    SyntaxKind::MOVE_KW,
    SyntaxKind::FN_KW,
    SyntaxKind::RETURN_KW,
    SyntaxKind::BREAK_KW,
    SyntaxKind::CONTINUE_KW,
];
const GENERIC_EXPR_FOLLOW_KINDS: &[SyntaxKind] = &[
    SyntaxKind::L_PAREN,
    SyntaxKind::DOT,
    SyntaxKind::R_PAREN,
    SyntaxKind::R_BRACKET,
    SyntaxKind::R_BRACE,
    SyntaxKind::COMMA,
    SyntaxKind::QUESTION,
    SyntaxKind::NEWLINE,
    SyntaxKind::FAT_ARROW,
    SyntaxKind::L_BRACE,
    SyntaxKind::EOF,
];

#[derive(Clone, Copy)]
enum NonAssociativeOperator {
    Comparison,
    Range,
}

#[derive(Clone, Copy)]
struct InfixOperator {
    kind: SyntaxKind,
    left_bp: u8,
    right_bp: u8,
    node_kind: SyntaxKind,
    non_associative: Option<NonAssociativeOperator>,
    deprecated_boolean_operator: bool,
    raw_count: u8,
}

#[derive(Clone, Copy)]
enum FnClauseKind {
    Where,
    Borrows,
    Effects,
    Requires,
    Ensures,
}

impl Parser<'_, '_> {
    /// Parses the currently implemented source-file grammar into parser events.
    ///
    /// This slice supports module headers, use declarations, and function
    /// signatures with the currently implemented block grammar. Other item forms return
    /// [`ParserError::UnsupportedItem`] until their grammar routines are
    /// implemented.
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
        self.attributes(true)?;

        if self.at_any(&[SyntaxKind::PUB_KW, SyntaxKind::EXPORT_KW]) {
            let visibility = self.start();
            let is_pub = self.at(SyntaxKind::PUB_KW);
            let pub_range = self.current_token().map(|token| token.range);
            self.bump();
            if is_pub && self.at(SyntaxKind::EXPORT_KW) {
                if let Some(range) = pub_range {
                    self.emit_parser_diagnostic(
                        sai_diagnostics::Code::RedundantPubExport,
                        range,
                        "`pub export` is redundant; use `export`",
                    )?;
                }
                self.bump();
            }
            visibility.complete(self, SyntaxKind::VISIBILITY)?;
        }

        if self.at_fn_decl_start() {
            return self.fn_decl(declaration);
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

    fn at_fn_decl_start(&self) -> bool {
        match self.current() {
            SyntaxKind::FN_KW => true,
            SyntaxKind::ASYNC_KW => self.nth(1) == SyntaxKind::FN_KW,
            SyntaxKind::UNSAFE_KW => {
                self.nth(1) == SyntaxKind::FN_KW
                    || (self.nth(1) == SyntaxKind::ASYNC_KW && self.nth(2) == SyntaxKind::FN_KW)
            }
            _ => false,
        }
    }

    fn fn_decl(&mut self, declaration: Marker) -> Result<CompletedMarker, ParserError> {
        self.eat(SyntaxKind::UNSAFE_KW);
        self.eat(SyntaxKind::ASYNC_KW);
        self.expect(SyntaxKind::FN_KW)?;
        self.name_node(&[SyntaxKind::LIDENT], SyntaxKind::NAME)?;
        if self.at(SyntaxKind::LT) {
            self.generic_param_list()?;
        }
        self.param_list()?;

        if self.at(SyntaxKind::NEWLINE) && self.nth(1) == SyntaxKind::THIN_ARROW {
            self.eat_nl();
        }
        if self.at(SyntaxKind::THIN_ARROW) {
            self.ret_type()?;
        }

        if let Err(error) = self.fn_clauses() {
            declaration.abandon(self)?;
            return Err(error);
        }

        if self.at(SyntaxKind::NEWLINE) && self.nth(1) == SyntaxKind::L_BRACE {
            self.eat_nl();
        }
        if self.at(SyntaxKind::L_BRACE) {
            if let Err(error) = self.block() {
                declaration.abandon(self)?;
                return Err(error);
            }
        } else {
            self.emit_missing_fn_body()?;
        }

        declaration.complete(self, SyntaxKind::FN_DECL)
    }

    fn fn_clauses(&mut self) -> Result<(), ParserError> {
        let mut last_rank = 0;
        let mut seen_where = false;
        let mut seen_borrows = false;
        let mut seen_effects = false;

        loop {
            if self.at(SyntaxKind::NEWLINE) {
                let mut lookahead = self.position;
                while self
                    .tokens
                    .get(lookahead)
                    .is_some_and(|token| token.kind == SyntaxKind::NEWLINE)
                {
                    lookahead += 1;
                }
                if !self.is_fn_clause_at(lookahead) {
                    break;
                }
                while self.eat_nl() {}
            }

            let Some((rank, kind)) = self.current_fn_clause() else {
                break;
            };
            let range = self.current_token().map(|token| token.range);
            if rank < last_rank {
                if let Some(range) = range {
                    self.emit_parser_diagnostic(
                        sai_diagnostics::Code::ClauseOrder,
                        range,
                        "function clauses are out of order",
                    )?;
                }
            }
            let duplicate = match kind {
                FnClauseKind::Where => std::mem::replace(&mut seen_where, true),
                FnClauseKind::Borrows => std::mem::replace(&mut seen_borrows, true),
                FnClauseKind::Effects => std::mem::replace(&mut seen_effects, true),
                FnClauseKind::Requires | FnClauseKind::Ensures => false,
            };
            if duplicate {
                if let Some(range) = range {
                    self.emit_parser_diagnostic(
                        sai_diagnostics::Code::DuplicateClause,
                        range,
                        "duplicate function clause",
                    )?;
                }
            }
            last_rank = last_rank.max(rank);

            match kind {
                FnClauseKind::Where => self.where_clause()?,
                FnClauseKind::Borrows => self.borrows_clause()?,
                FnClauseKind::Effects => self.effects_clause()?,
                FnClauseKind::Requires | FnClauseKind::Ensures => {
                    let position = self.position;
                    return Err(ParserError::UnsupportedSyntax {
                        position,
                        kind: SyntaxKind::LIDENT,
                    });
                }
            }
        }
        Ok(())
    }

    fn current_fn_clause(&self) -> Option<(u8, FnClauseKind)> {
        if self.at(SyntaxKind::WHERE_KW) {
            Some((1, FnClauseKind::Where))
        } else if self.at_contextual("borrows") {
            Some((2, FnClauseKind::Borrows))
        } else if self.at_contextual("effects") {
            Some((3, FnClauseKind::Effects))
        } else if self.at_contextual("requires") {
            Some((4, FnClauseKind::Requires))
        } else if self.at_contextual("ensures") {
            Some((5, FnClauseKind::Ensures))
        } else {
            None
        }
    }

    fn is_fn_clause_at(&self, position: usize) -> bool {
        let Some(token) = self.tokens.get(position) else {
            return false;
        };
        match token.kind {
            SyntaxKind::WHERE_KW => true,
            SyntaxKind::LIDENT => ["borrows", "effects", "requires", "ensures"]
                .iter()
                .any(|spelling| token_is_contextual(self.source, self.tokens, position, spelling)),
            _ => false,
        }
    }

    fn emit_parser_diagnostic(
        &mut self,
        code: sai_diagnostics::Code,
        range: sai_vfs::TextRange,
        message: &str,
    ) -> Result<(), ParserError> {
        let Some(context) = &self.diagnostic_source else {
            return Err(ParserError::MissingDiagnosticSource);
        };
        let location = sai_diagnostics::Location::from_decoded_source(
            context.source_ref.clone(),
            sai_vfs::Span::new(context.file.id(), range),
            context.file,
        )
        .ok_or(ParserError::InvalidDiagnosticSpan)?;
        let mut diagnostic = sai_diagnostics::Diagnostic::new(
            code,
            sai_diagnostics::Severity::Error,
            message,
            Some(location),
        );
        diagnostic.primary_label = message.to_owned();
        self.emit_error(diagnostic);
        Ok(())
    }

    fn emit_operator_continuation_diagnostic(
        &mut self,
        newline_range: sai_vfs::TextRange,
    ) -> Result<(), ParserError> {
        let Some(context) = &self.diagnostic_source else {
            return Err(ParserError::MissingDiagnosticSource);
        };
        let location = sai_diagnostics::Location::from_decoded_source(
            context.source_ref.clone(),
            sai_vfs::Span::new(context.file.id(), newline_range),
            context.file,
        )
        .ok_or(ParserError::InvalidDiagnosticSpan)?;
        let message = "unexpected newline before a binary operator";
        let mut diagnostic = sai_diagnostics::Diagnostic::new(
            sai_diagnostics::Code::UnexpectedNewline,
            sai_diagnostics::Severity::Error,
            message,
            Some(location),
        );
        diagnostic.primary_label = message.to_owned();
        diagnostic
            .push_child(sai_diagnostics::Child {
                kind: sai_diagnostics::ChildKind::Note,
                message: "move the operator to the end of the previous line or parenthesize the expression"
                    .to_owned(),
                location: None,
            })
            .map_err(|_| ParserError::InvalidDiagnosticSpan)?;
        self.emit_error(diagnostic);
        Ok(())
    }

    fn emit_missing_fn_body(&mut self) -> Result<(), ParserError> {
        let Some(context) = &self.diagnostic_source else {
            return Err(ParserError::MissingDiagnosticSource);
        };
        let range = self.current_token().map_or_else(
            || {
                sai_vfs::TextSize::from_usize(self.source.len())
                    .map(sai_vfs::TextRange::empty)
                    .ok_or(ParserError::InvalidDiagnosticSpan)
            },
            |token| Ok(token.range),
        )?;
        let location = sai_diagnostics::Location::from_decoded_source(
            context.source_ref.clone(),
            sai_vfs::Span::new(context.file.id(), range),
            context.file,
        )
        .ok_or(ParserError::InvalidDiagnosticSpan)?;
        let message = "function requires a body";
        let mut diagnostic = sai_diagnostics::Diagnostic::new(
            sai_diagnostics::Code::MissingFnBody,
            sai_diagnostics::Severity::Error,
            message,
            Some(location),
        );
        diagnostic.primary_label = message.to_owned();
        self.emit_error(diagnostic);
        Ok(())
    }

    fn generic_param_list(&mut self) -> Result<(), ParserError> {
        let params = self.start();
        self.expect(SyntaxKind::LT)?;
        if self.at(SyntaxKind::GT) {
            self.expect_any(GENERIC_PARAM_START_KINDS)?;
        }

        while !self.at_any(&[SyntaxKind::GT, SyntaxKind::EOF]) {
            let position = self.position;
            if !self.generic_param()? {
                self.recover_one()?;
                if self.position == position {
                    break;
                }
                continue;
            }
            if self.eat(SyntaxKind::COMMA) {
                continue;
            }
            if self.at_any(&[SyntaxKind::GT, SyntaxKind::EOF])
                || (self.at(SyntaxKind::NEWLINE) && self.nth(1) == SyntaxKind::GT)
            {
                break;
            }
            self.expect(SyntaxKind::COMMA)?;
            if self.position == position {
                self.recover_one()?;
                if self.position == position {
                    break;
                }
            }
        }

        self.eat_nl();
        self.expect(SyntaxKind::GT)?;
        params.complete(self, SyntaxKind::GENERIC_PARAM_LIST)?;
        Ok(())
    }

    fn generic_param(&mut self) -> Result<bool, ParserError> {
        if !self.at_any(GENERIC_PARAM_START_KINDS) {
            self.expect_any(GENERIC_PARAM_START_KINDS)?;
            return Ok(false);
        }

        let param = self.start();
        self.attributes(false)?;
        let kind = if self.eat(SyntaxKind::CONST_KW) {
            self.name_node(&[SyntaxKind::UIDENT], SyntaxKind::NAME)?;
            self.expect(SyntaxKind::COLON)?;
            self.type_(true)?;
            SyntaxKind::CONST_PARAM
        } else {
            self.name_node(&[SyntaxKind::UIDENT], SyntaxKind::NAME)?;
            if self.eat(SyntaxKind::COLON) {
                self.bound_list()?;
            }
            SyntaxKind::TYPE_PARAM
        };
        param.complete(self, kind)?;
        Ok(true)
    }

    fn bound_list(&mut self) -> Result<(), ParserError> {
        let bounds = self.start();
        self.bound_list_inner(bounds)
    }

    fn bound_list_inner(&mut self, bounds: Marker) -> Result<(), ParserError> {
        if !self.at_any(BOUND_START_KINDS) && !self.is_fn_sugar_start() {
            self.expect_any(BOUND_START_KINDS)?;
            bounds.abandon(self)?;
            return Ok(());
        }
        loop {
            if !self.at_any(BOUND_START_KINDS) && !self.is_fn_sugar_start() {
                self.expect_any(BOUND_START_KINDS)?;
                break;
            }
            self.bound_no_parens(true)?;
            if !self.eat(SyntaxKind::PLUS) {
                break;
            }
        }
        bounds.complete(self, SyntaxKind::BOUND_LIST)?;
        Ok(())
    }

    fn bound_no_parens(&mut self, allow_bound_effects: bool) -> Result<(), ParserError> {
        if self.is_fn_sugar_start() {
            return self.fn_sugar_bound(allow_bound_effects);
        }
        if !self.at_any(BOUND_START_KINDS) {
            self.expect_any(BOUND_START_KINDS)?;
            return Ok(());
        }
        let bound = self.start();
        let previous = self.allow_assoc_type_bindings;
        self.allow_assoc_type_bindings = true;
        let result = self.path_type();
        self.allow_assoc_type_bindings = previous;
        result?;
        if allow_bound_effects && self.at_contextual("effects") {
            self.effects_clause()?;
        }
        bound.complete(self, SyntaxKind::TRAIT_BOUND)?;
        Ok(())
    }

    fn fn_sugar_bound(&mut self, allow_bound_effects: bool) -> Result<(), ParserError> {
        let bound = self.start();
        self.name_node(&[SyntaxKind::UIDENT], SyntaxKind::NAME_REF)?;
        self.expect(SyntaxKind::L_PAREN)?;
        self.type_list(SyntaxKind::R_PAREN)?;
        self.expect(SyntaxKind::R_PAREN)?;
        if self.eat(SyntaxKind::THIN_ARROW) {
            self.type_no_fx()?;
        }
        if allow_bound_effects && self.at_contextual("effects") {
            self.effects_clause()?;
        }
        bound.complete(self, SyntaxKind::FN_SUGAR_BOUND)?;
        Ok(())
    }

    fn where_clause(&mut self) -> Result<(), ParserError> {
        let clause = self.start();
        self.expect(SyntaxKind::WHERE_KW)?;
        let previous = self.allow_assoc_type_bindings;
        self.allow_assoc_type_bindings = false;
        let result = (|| {
            if !self.at_any(TYPE_START_KINDS) {
                self.expect_any(TYPE_START_KINDS)?;
                clause.complete(self, SyntaxKind::WHERE_CLAUSE)?;
                return Ok(());
            }
            loop {
                if !self.at_any(TYPE_START_KINDS) {
                    self.expect_any(TYPE_START_KINDS)?;
                    break;
                }
                let predicate = self.start();
                self.type_(true)?;
                self.expect(SyntaxKind::COLON)?;
                self.where_bounds()?;
                predicate.complete(self, SyntaxKind::WHERE_PRED)?;

                if !self.eat(SyntaxKind::COMMA) {
                    break;
                }
                self.eat_nl();
            }
            clause.complete(self, SyntaxKind::WHERE_CLAUSE)?;
            Ok(())
        })();
        self.allow_assoc_type_bindings = previous;
        result
    }

    fn where_bounds(&mut self) -> Result<(), ParserError> {
        loop {
            if !self.at_any(BOUND_START_KINDS)
                && !self.is_fn_sugar_start()
                && !self.at(SyntaxKind::L_PAREN)
            {
                self.expect_any(BOUND_START_KINDS)?;
                break;
            }
            if self.eat(SyntaxKind::L_PAREN) {
                self.bound_no_parens(true)?;
                self.expect(SyntaxKind::R_PAREN)?;
            } else {
                self.bound_no_parens(false)?;
            }
            if !self.eat(SyntaxKind::PLUS) {
                break;
            }
            if !self.at_any(BOUND_START_KINDS)
                && !self.is_fn_sugar_start()
                && !self.at(SyntaxKind::L_PAREN)
            {
                self.expect_any(BOUND_START_KINDS)?;
                break;
            }
        }
        Ok(())
    }

    fn is_fn_sugar_start(&self) -> bool {
        self.at(SyntaxKind::UIDENT)
            && self.nth(1) == SyntaxKind::L_PAREN
            && ["Fn", "FnMut", "FnOnce"].iter().any(|name| {
                token_is_unraw_identifier(
                    self.source,
                    self.tokens,
                    self.position,
                    SyntaxKind::UIDENT,
                    name,
                )
            })
    }

    fn borrows_clause(&mut self) -> Result<(), ParserError> {
        let clause = self.start();
        self.bump();
        self.braced_list(|parser| parser.provenance())?;
        clause.complete(self, SyntaxKind::BORROWS_CLAUSE)?;
        Ok(())
    }

    fn effects_clause(&mut self) -> Result<(), ParserError> {
        let clause = self.start();
        self.bump();
        self.effect_set()?;
        clause.complete(self, SyntaxKind::EFFECTS_CLAUSE)?;
        Ok(())
    }

    fn effect_set(&mut self) -> Result<(), ParserError> {
        let set = self.start();
        if !self.expect(SyntaxKind::L_BRACE)? {
            set.abandon(self)?;
            return Ok(());
        }
        self.eat_nl();
        while !self.at_any(&[SyntaxKind::R_BRACE, SyntaxKind::EOF]) {
            let position = self.position;
            if !self.at(SyntaxKind::LIDENT) {
                self.expect(SyntaxKind::LIDENT)?;
                if self.eat(SyntaxKind::COMMA) {
                    self.eat_nl();
                    continue;
                }
                self.recover_one()?;
                if self.position == position {
                    break;
                }
                continue;
            }
            self.effect_ref()?;
            if self.eat(SyntaxKind::COMMA) {
                self.eat_nl();
                continue;
            }
            if self.eat_nl() {
                continue;
            }
            if self.at_any(&[SyntaxKind::R_BRACE, SyntaxKind::EOF]) {
                break;
            }
            self.expect_any(USE_ITEM_SEPARATOR_KINDS)?;
            if self.position == position {
                self.recover_one()?;
                if self.position == position {
                    break;
                }
            }
        }
        self.eat_nl();
        self.expect(SyntaxKind::R_BRACE)?;
        set.complete(self, SyntaxKind::EFFECT_SET)?;
        Ok(())
    }

    fn effect_ref(&mut self) -> Result<(), ParserError> {
        if !self.at(SyntaxKind::LIDENT) {
            self.expect(SyntaxKind::LIDENT)?;
            return Ok(());
        }
        let effect = self.start();
        self.name_node(&[SyntaxKind::LIDENT], SyntaxKind::NAME_REF)?;
        while self.eat(SyntaxKind::DOT) {
            self.name_node(&[SyntaxKind::LIDENT], SyntaxKind::NAME_REF)?;
        }
        effect.complete(self, SyntaxKind::EFFECT_REF)?;
        Ok(())
    }

    fn provenance(&mut self) -> Result<(), ParserError> {
        if !self.at_any(&[SyntaxKind::LIDENT, SyntaxKind::SELF_KW]) {
            self.expect_any(&[SyntaxKind::LIDENT, SyntaxKind::SELF_KW])?;
            return Ok(());
        }
        let provenance = self.start();
        self.name_node(
            &[SyntaxKind::LIDENT, SyntaxKind::SELF_KW],
            SyntaxKind::NAME_REF,
        )?;
        while self.eat(SyntaxKind::DOT) {
            self.name_node(&[SyntaxKind::LIDENT], SyntaxKind::NAME_REF)?;
        }
        provenance.complete(self, SyntaxKind::PROVENANCE)?;
        Ok(())
    }

    fn braced_list(
        &mut self,
        parse_item: impl Fn(&mut Self) -> Result<(), ParserError>,
    ) -> Result<(), ParserError> {
        if !self.expect(SyntaxKind::L_BRACE)? {
            return Ok(());
        }
        self.eat_nl();
        while !self.at_any(&[SyntaxKind::R_BRACE, SyntaxKind::EOF]) {
            if self.at_any(&[SyntaxKind::DOC_OUTER, SyntaxKind::DOC_INNER]) {
                self.misplaced_doc_comment()?;
                continue;
            }
            let position = self.position;
            parse_item(self)?;
            if self.eat(SyntaxKind::COMMA) {
                self.eat_nl();
                continue;
            }
            if self.eat_nl() {
                continue;
            }
            if self.at_any(&[SyntaxKind::R_BRACE, SyntaxKind::EOF]) {
                break;
            }
            self.expect_any(USE_ITEM_SEPARATOR_KINDS)?;
            if self.position == position {
                self.recover_one()?;
                if self.position == position {
                    break;
                }
            }
        }
        self.eat_nl();
        self.expect(SyntaxKind::R_BRACE)?;
        Ok(())
    }

    fn param_list(&mut self) -> Result<(), ParserError> {
        let params = self.start();
        if !self.expect(SyntaxKind::L_PAREN)? {
            params.abandon(self)?;
            return Ok(());
        }
        while !self.at_any(&[SyntaxKind::R_PAREN, SyntaxKind::EOF]) {
            if self.at_any(&[SyntaxKind::DOC_OUTER, SyntaxKind::DOC_INNER]) {
                self.misplaced_doc_comment()?;
                continue;
            }
            let position = self.position;
            if !self.param()? {
                self.recover_one()?;
                if self.position == position {
                    break;
                }
                continue;
            }
            if self.eat(SyntaxKind::COMMA) {
                continue;
            }
            if self.at_any(&[SyntaxKind::R_PAREN, SyntaxKind::EOF]) {
                break;
            }
            self.expect(SyntaxKind::COMMA)?;
            if self.position == position {
                self.recover_one()?;
                if self.position == position {
                    break;
                }
            }
        }
        self.expect(SyntaxKind::R_PAREN)?;
        params.complete(self, SyntaxKind::PARAM_LIST)?;
        Ok(())
    }

    fn param(&mut self) -> Result<bool, ParserError> {
        if !self.at_any(PARAM_START_KINDS) {
            self.expect_any(PARAM_START_KINDS)?;
            return Ok(false);
        }

        let param = self.start();
        self.attributes(false)?;
        self.eat(SyntaxKind::VAR_KW);
        let kind = if self.eat(SyntaxKind::SELF_KW) {
            self.expect(SyntaxKind::COLON)?;
            self.type_(true)?;
            SyntaxKind::SELF_PARAM
        } else {
            self.name_node(
                &[SyntaxKind::LIDENT, SyntaxKind::UNDERSCORE],
                SyntaxKind::NAME,
            )?;
            self.expect(SyntaxKind::COLON)?;
            self.type_(true)?;
            SyntaxKind::PARAM
        };
        param.complete(self, kind)?;
        Ok(true)
    }

    fn ret_type(&mut self) -> Result<(), ParserError> {
        let ret_type = self.start();
        self.expect(SyntaxKind::THIN_ARROW)?;
        self.type_no_fx()?;
        ret_type.complete(self, SyntaxKind::RET_TYPE)?;
        Ok(())
    }

    fn block(&mut self) -> Result<CompletedMarker, ParserError> {
        if self.nesting_depth >= MAX_NESTING_DEPTH {
            return self.recover_overdeep_block();
        }
        self.nesting_depth += 1;
        let result = self.block_inner();
        self.nesting_depth -= 1;
        result
    }

    fn block_inner(&mut self) -> Result<CompletedMarker, ParserError> {
        let block = self.start();
        let result = (|| {
            if !self.eat(SyntaxKind::L_BRACE) {
                self.expect(SyntaxKind::L_BRACE)?;
                return Ok(false);
            }
            while self.eat_nl() {}
            while !self.at_any(&[SyntaxKind::R_BRACE, SyntaxKind::EOF]) {
                let position = self.position;
                if self.at_any(&[SyntaxKind::DOC_OUTER, SyntaxKind::DOC_INNER]) {
                    self.misplaced_doc_comment()?;
                    continue;
                }
                let statement_ends_with_expression = self.statement()?;
                let newline = self.current_token().map(|token| token.range);
                if self.eat_nl() {
                    if statement_ends_with_expression && self.starts_binary_only_operator() {
                        let newline = newline.ok_or(ParserError::InvalidDiagnosticSpan)?;
                        self.emit_operator_continuation_diagnostic(newline)?;
                        self.recover_operator_continuation()?;
                    }
                    while self.eat_nl() {}
                    continue;
                }
                if self.at_any(&[SyntaxKind::R_BRACE, SyntaxKind::EOF]) {
                    break;
                }
                if !statement_ends_with_expression && self.position == position {
                    self.recover_to_block_separator()?;
                    while self.eat_nl() {}
                    continue;
                }
                self.expect(SyntaxKind::NEWLINE)?;
                self.recover_to_block_separator()?;
                if self.position == position
                    && !self.at_any(&[SyntaxKind::R_BRACE, SyntaxKind::EOF])
                {
                    self.recover_one()?;
                }
                while self.eat_nl() {}
            }
            while self.eat_nl() {}
            if self.eat(SyntaxKind::R_BRACE) {
                Ok(true)
            } else {
                self.expect(SyntaxKind::R_BRACE)?;
                Ok(false)
            }
        })();
        match result {
            Ok(true) => block.complete(self, SyntaxKind::BLOCK),
            Ok(false) => block.complete(self, SyntaxKind::ERROR),
            Err(error) => {
                block.abandon(self)?;
                Err(error)
            }
        }
    }

    fn recover_overdeep_block(&mut self) -> Result<CompletedMarker, ParserError> {
        let error = self.start();
        let range = self.current_token().map(|token| token.range);
        let mut brace_depth = 0usize;
        let mut consumed = false;
        while !self.at(SyntaxKind::EOF) {
            match self.current() {
                SyntaxKind::L_BRACE => brace_depth += 1,
                SyntaxKind::R_BRACE if brace_depth > 0 => brace_depth -= 1,
                SyntaxKind::R_BRACE => break,
                SyntaxKind::NEWLINE if brace_depth == 0 => break,
                _ => {}
            }
            self.bump();
            consumed = true;
            if consumed && brace_depth == 0 {
                break;
            }
        }
        let completed = if consumed {
            error.complete(self, SyntaxKind::ERROR)?
        } else {
            error.abandon(self)?;
            let empty = self.start();
            empty.complete(self, SyntaxKind::ERROR)?
        };
        if let Some(range) = range {
            self.report_nesting_limit(range)?;
        }
        Ok(completed)
    }

    fn statement(&mut self) -> Result<bool, ParserError> {
        match self.current() {
            SyntaxKind::LET_KW => self.let_stmt(),
            SyntaxKind::VAR_KW => self.var_stmt(),
            SyntaxKind::ASSERT_KW => {
                self.assert_stmt()?;
                Ok(false)
            }
            _ => self.expr_stmt(),
        }
    }

    fn let_stmt(&mut self) -> Result<bool, ParserError> {
        let statement = self.start();
        let result = (|| {
            self.bump();
            self.binding_pattern()?;
            if self.eat(SyntaxKind::COLON) {
                self.type_(true)?;
            }
            let mut ends_with_expression = if self.eat(SyntaxKind::EQ) {
                self.eat_nl();
                self.expr_bp(0)?;
                true
            } else {
                self.emit_missing_initializer()?;
                self.start().complete(self, SyntaxKind::ERROR)?;
                false
            };
            if self.at(SyntaxKind::ELSE_KW)
                || (self.at(SyntaxKind::NEWLINE) && self.nth(1) == SyntaxKind::ELSE_KW)
            {
                ends_with_expression = false;
                let else_clause = self.start();
                self.eat_nl();
                self.bump();
                self.eat_nl();
                if let Err(error) = self.block() {
                    else_clause.abandon(self)?;
                    return Err(error);
                }
                else_clause.complete(self, SyntaxKind::LET_ELSE)?;
            }
            Ok(ends_with_expression)
        })();
        match result {
            Ok(ends_with_expression) => {
                statement.complete(self, SyntaxKind::LET_STMT)?;
                Ok(ends_with_expression)
            }
            Err(error) => {
                statement.abandon(self)?;
                Err(error)
            }
        }
    }

    fn binding_pattern(&mut self) -> Result<(), ParserError> {
        if self.at(SyntaxKind::UNDERSCORE) {
            let wildcard = self.start();
            self.bump();
            wildcard.complete(self, SyntaxKind::WILDCARD_PAT)?;
            return Ok(());
        }
        if !self.at(SyntaxKind::LIDENT) {
            self.expect(SyntaxKind::LIDENT)?;
            return Ok(());
        }
        let pattern = self.start();
        self.name_node(&[SyntaxKind::LIDENT], SyntaxKind::NAME)?;
        pattern.complete(self, SyntaxKind::BINDING_PAT)?;
        Ok(())
    }

    fn var_stmt(&mut self) -> Result<bool, ParserError> {
        let statement = self.start();
        let result = (|| {
            self.bump();
            self.name_node(&[SyntaxKind::LIDENT], SyntaxKind::NAME)?;
            if self.eat(SyntaxKind::COLON) {
                self.type_(true)?;
            }
            let ends_with_expression = if self.eat(SyntaxKind::EQ) {
                self.eat_nl();
                self.expr_bp(0)?;
                true
            } else {
                self.emit_missing_initializer()?;
                self.start().complete(self, SyntaxKind::ERROR)?;
                false
            };
            Ok(ends_with_expression)
        })();
        match result {
            Ok(ends_with_expression) => {
                statement.complete(self, SyntaxKind::VAR_STMT)?;
                Ok(ends_with_expression)
            }
            Err(error) => {
                statement.abandon(self)?;
                Err(error)
            }
        }
    }

    fn assert_stmt(&mut self) -> Result<(), ParserError> {
        let statement = self.start();
        self.bump();
        let result = (|| {
            self.expr_bp(0)?;
            if self.eat(SyntaxKind::COMMA) {
                self.expr_bp(0)?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                statement.complete(self, SyntaxKind::ASSERT_STMT)?;
                Ok(())
            }
            Err(error) => {
                statement.abandon(self)?;
                Err(error)
            }
        }
    }

    fn expr_stmt(&mut self) -> Result<bool, ParserError> {
        let start_position = self.position;
        let lhs = self.expr_bp(0)?;
        if lhs.kind() == SyntaxKind::ERROR && self.position == start_position {
            return Ok(false);
        }
        if let Some((kind, raw_count)) = self.assignment_operator() {
            let statement = lhs.precede(self);
            let result = (|| {
                self.bump_as(kind, raw_count);
                self.eat_nl();
                self.expr_bp(0)?;
                Ok(())
            })();
            match result {
                Ok(()) => {
                    statement.complete(self, SyntaxKind::ASSIGN_STMT)?;
                    Ok(true)
                }
                Err(error) => {
                    statement.abandon(self)?;
                    Err(error)
                }
            }
        } else {
            lhs.precede(self).complete(self, SyntaxKind::EXPR_STMT)?;
            Ok(true)
        }
    }

    fn assignment_operator(&self) -> Option<(SyntaxKind, u8)> {
        if self.can_bump_as(SyntaxKind::SHR_EQ, 3) {
            return Some((SyntaxKind::SHR_EQ, 3));
        }
        match self.current() {
            SyntaxKind::EQ
            | SyntaxKind::PLUS_EQ
            | SyntaxKind::MINUS_EQ
            | SyntaxKind::STAR_EQ
            | SyntaxKind::SLASH_EQ
            | SyntaxKind::PERCENT_EQ
            | SyntaxKind::PLUS_PERCENT_EQ
            | SyntaxKind::MINUS_PERCENT_EQ
            | SyntaxKind::STAR_PERCENT_EQ
            | SyntaxKind::AMP_EQ
            | SyntaxKind::PIPE_EQ
            | SyntaxKind::CARET_EQ
            | SyntaxKind::SHL_EQ => Some((self.current(), 1)),
            _ => None,
        }
    }

    fn emit_missing_initializer(&mut self) -> Result<(), ParserError> {
        let range = self.current_token().map_or_else(
            || {
                sai_vfs::TextSize::from_usize(self.source.len())
                    .map(sai_vfs::TextRange::empty)
                    .ok_or(ParserError::InvalidDiagnosticSpan)
            },
            |token| Ok(token.range),
        )?;
        self.emit_parser_diagnostic(
            sai_diagnostics::Code::MissingInitializer,
            range,
            "`let` and `var` statements require an initializer",
        )?;
        Ok(())
    }

    fn expr_bp(&mut self, min_bp: u8) -> Result<CompletedMarker, ParserError> {
        if self.nesting_depth >= MAX_NESTING_DEPTH {
            return self.recover_overdeep_expression();
        }
        self.nesting_depth += 1;
        let result = self.expr_bp_inner(min_bp);
        self.nesting_depth -= 1;
        result
    }

    fn expr_bp_inner(&mut self, min_bp: u8) -> Result<CompletedMarker, ParserError> {
        let prefix_bp = match self.current() {
            SyntaxKind::NOT_KW => Some(6),
            SyntaxKind::DOT2 | SyntaxKind::DOT2EQ => Some(9),
            _ => None,
        };
        if prefix_bp.is_some_and(|prefix_bp| min_bp > prefix_bp) {
            return self.missing_expression();
        }
        let lhs = self.prefix_or_primary()?;
        self.expr_bp_loop(lhs, min_bp)
    }

    fn expr_bp_loop(
        &mut self,
        mut lhs: CompletedMarker,
        min_bp: u8,
    ) -> Result<CompletedMarker, ParserError> {
        let mut previous_non_associative =
            (lhs.kind() == SyntaxKind::RANGE_EXPR).then_some(NonAssociativeOperator::Range);
        loop {
            if self.at(SyntaxKind::AS_KW) {
                const CAST_BINDING_POWER: u8 = 23;
                if CAST_BINDING_POWER <= min_bp {
                    break;
                }
                let cast = lhs.precede(self);
                self.bump();
                if let Err(error) = self.type_no_fx() {
                    cast.abandon(self)?;
                    return Err(error);
                }
                lhs = cast.complete(self, SyntaxKind::CAST_EXPR)?;
                previous_non_associative = None;
                continue;
            }
            if self.at_any(&[
                SyntaxKind::DOT,
                SyntaxKind::L_PAREN,
                SyntaxKind::L_BRACKET,
                SyntaxKind::QUESTION,
            ]) {
                return Err(ParserError::UnsupportedSyntax {
                    position: self.position,
                    kind: self.current(),
                });
            }
            let Some(operator) = self.infix_operator() else {
                break;
            };
            if operator.left_bp <= min_bp {
                break;
            }
            let operator_range = self.operator_range(operator.raw_count)?;
            let chained = operator
                .non_associative
                .zip(previous_non_associative)
                .is_some_and(|(current, previous)| {
                    std::mem::discriminant(&current) == std::mem::discriminant(&previous)
                });
            let parent = lhs.precede(self);
            let result = (|| {
                if !self.bump_as(operator.kind, operator.raw_count) {
                    return Err(ParserError::InvalidTokenStream {
                        position: self.position,
                    });
                }
                if operator.deprecated_boolean_operator {
                    self.emit_parser_diagnostic(
                        sai_diagnostics::Code::UseAndOr,
                        operator_range,
                        "use `and` or `or` instead of `&&` or `||`",
                    )?;
                }
                if chained {
                    let (code, message) = match operator.non_associative {
                        Some(NonAssociativeOperator::Comparison) => (
                            sai_diagnostics::Code::ChainedComparison,
                            "comparison operators cannot be chained",
                        ),
                        Some(NonAssociativeOperator::Range) => (
                            sai_diagnostics::Code::ChainedRange,
                            "range operators cannot be chained",
                        ),
                        None => unreachable!("only non-associative operators can be chained"),
                    };
                    self.emit_parser_diagnostic(code, operator_range, message)?;
                }
                if operator.node_kind == SyntaxKind::RANGE_EXPR {
                    if self.at_expression_start() {
                        self.expr_bp(operator.right_bp)?;
                    } else if operator.kind == SyntaxKind::DOT2EQ
                        || self
                            .infix_operator()
                            .is_some_and(|next| next.left_bp > operator.left_bp)
                    {
                        if self.at(SyntaxKind::NEWLINE) {
                            self.missing_expression_after_operator()?;
                        } else {
                            self.missing_expression()?;
                        }
                    }
                } else if self.at(SyntaxKind::NEWLINE) {
                    self.missing_expression_after_operator()?;
                } else {
                    self.expr_bp(operator.right_bp)?;
                }
                Ok(())
            })();
            if let Err(error) = result {
                parent.abandon(self)?;
                return Err(error);
            }
            lhs = parent.complete(self, operator.node_kind)?;
            previous_non_associative = operator.non_associative;
        }
        Ok(lhs)
    }

    fn maybe_generic_args_expr(&mut self) -> Result<bool, ParserError> {
        let start_position = self.position;
        if !self.at(SyntaxKind::LT) || self.spec_failures.contains(&start_position) {
            return Ok(false);
        }
        let Some(previous) = start_position
            .checked_sub(1)
            .and_then(|position| self.tokens.get(position))
        else {
            return Ok(false);
        };
        if !matches!(previous.kind, SyntaxKind::LIDENT | SyntaxKind::UIDENT)
            || !previous.flags.contains(crate::TokenFlags::JOINT)
        {
            return Ok(false);
        }

        let event_count = self.events.len();
        let expected = self.expected.clone();
        let reported_expected_at = self.reported_expected_at;
        let reported_expected_event = self.reported_expected_event;
        let active_markers = self.active_markers.borrow().clone();
        let marker_failure_count = self.marker_failures.borrow().len();
        let nesting_depth = self.nesting_depth;
        let reported_nesting_limit = self.reported_nesting_limit;
        let allow_assoc_type_bindings = self.allow_assoc_type_bindings;

        self.allow_assoc_type_bindings = false;
        let parsed = self.generic_arg_list();
        self.allow_assoc_type_bindings = allow_assoc_type_bindings;

        let marker_failure = self
            .marker_failures
            .borrow()
            .get(marker_failure_count)
            .cloned();
        let active_markers_changed = *self.active_markers.borrow() != active_markers;
        let parser_error = marker_failure.or_else(|| {
            active_markers_changed.then(|| ParserError::InvalidMarkerNesting {
                position: active_markers
                    .last()
                    .copied()
                    .unwrap_or_else(|| u32::try_from(event_count).unwrap_or(u32::MAX)),
            })
        });
        let has_diagnostic = self.events[event_count..]
            .iter()
            .any(|event| matches!(event, crate::Event::Error { .. }));
        let succeeded = parsed.is_ok()
            && parser_error.is_none()
            && !has_diagnostic
            && GENERIC_EXPR_FOLLOW_KINDS.contains(&self.current());
        if succeeded {
            return Ok(true);
        }

        self.position = start_position;
        self.events.truncate(event_count);
        self.expected = expected;
        self.reported_expected_at = reported_expected_at;
        self.reported_expected_event = reported_expected_event;
        *self.active_markers.borrow_mut() = active_markers;
        self.marker_failures
            .borrow_mut()
            .truncate(marker_failure_count);
        self.nesting_depth = nesting_depth;
        self.reported_nesting_limit = reported_nesting_limit;
        self.spec_failures.insert(start_position);

        if let Some(error) = parser_error {
            return Err(error);
        }
        parsed?;
        Ok(false)
    }

    fn infix_operator(&self) -> Option<InfixOperator> {
        if self.can_bump_as(SyntaxKind::SHR_EQ, 3) {
            return None;
        }
        if self.can_bump_as(SyntaxKind::PIPE2, 2) {
            return Some(InfixOperator {
                kind: SyntaxKind::PIPE2,
                left_bp: 1,
                right_bp: 2,
                node_kind: SyntaxKind::BIN_EXPR,
                non_associative: None,
                deprecated_boolean_operator: true,
                raw_count: 2,
            });
        }
        if self.can_bump_as(SyntaxKind::AMP2, 2) {
            return Some(InfixOperator {
                kind: SyntaxKind::AMP2,
                left_bp: 3,
                right_bp: 4,
                node_kind: SyntaxKind::BIN_EXPR,
                non_associative: None,
                deprecated_boolean_operator: true,
                raw_count: 2,
            });
        }
        let kind = self.current();
        let (left_bp, right_bp, non_associative, deprecated_boolean_operator) = match kind {
            SyntaxKind::OR_KW => (1, 2, None, false),
            SyntaxKind::PIPE2 => (1, 2, None, true),
            SyntaxKind::AND_KW => (3, 4, None, false),
            SyntaxKind::AMP2 => (3, 4, None, true),
            SyntaxKind::GT if self.can_bump_as(SyntaxKind::SHR, 2) => (17, 18, None, false),
            SyntaxKind::GT if self.can_bump_as(SyntaxKind::GE, 2) => {
                (7, 8, Some(NonAssociativeOperator::Comparison), false)
            }
            SyntaxKind::EQ2
            | SyntaxKind::NEQ
            | SyntaxKind::LT
            | SyntaxKind::LE
            | SyntaxKind::GT => (7, 8, Some(NonAssociativeOperator::Comparison), false),
            SyntaxKind::DOT2 => (9, 10, Some(NonAssociativeOperator::Range), false),
            SyntaxKind::DOT2EQ => (9, 10, Some(NonAssociativeOperator::Range), false),
            SyntaxKind::PIPE => (11, 12, None, false),
            SyntaxKind::CARET => (13, 14, None, false),
            SyntaxKind::AMP => (15, 16, None, false),
            SyntaxKind::SHL => (17, 18, None, false),
            SyntaxKind::PLUS
            | SyntaxKind::MINUS
            | SyntaxKind::PLUS_PERCENT
            | SyntaxKind::MINUS_PERCENT => (19, 20, None, false),
            SyntaxKind::STAR
            | SyntaxKind::SLASH
            | SyntaxKind::PERCENT
            | SyntaxKind::STAR_PERCENT => (21, 22, None, false),
            _ => return None,
        };
        let (kind, raw_count) = if kind == SyntaxKind::GT {
            if self.can_bump_as(SyntaxKind::SHR, 2) {
                (SyntaxKind::SHR, 2)
            } else if self.can_bump_as(SyntaxKind::GE, 2) {
                (SyntaxKind::GE, 2)
            } else {
                (SyntaxKind::GT, 1)
            }
        } else if matches!(kind, SyntaxKind::AMP2 | SyntaxKind::PIPE2) {
            (kind, 2)
        } else {
            (kind, 1)
        };
        Some(InfixOperator {
            kind,
            left_bp,
            right_bp,
            node_kind: if matches!(kind, SyntaxKind::DOT2 | SyntaxKind::DOT2EQ) {
                SyntaxKind::RANGE_EXPR
            } else {
                SyntaxKind::BIN_EXPR
            },
            non_associative,
            deprecated_boolean_operator,
            raw_count,
        })
    }

    fn starts_binary_only_operator(&self) -> bool {
        self.infix_operator().is_some_and(|operator| {
            matches!(
                operator.kind,
                SyntaxKind::PLUS
                    | SyntaxKind::SLASH
                    | SyntaxKind::PERCENT
                    | SyntaxKind::PLUS_PERCENT
                    | SyntaxKind::MINUS_PERCENT
                    | SyntaxKind::STAR_PERCENT
                    | SyntaxKind::EQ2
                    | SyntaxKind::NEQ
                    | SyntaxKind::LT
                    | SyntaxKind::LE
                    | SyntaxKind::GT
                    | SyntaxKind::GE
                    | SyntaxKind::SHR
                    | SyntaxKind::PIPE
                    | SyntaxKind::PIPE2
                    | SyntaxKind::CARET
                    | SyntaxKind::AMP
                    | SyntaxKind::AMP2
                    | SyntaxKind::SHL
            )
        })
    }

    fn prefix_or_primary(&mut self) -> Result<CompletedMarker, ParserError> {
        if self.at(SyntaxKind::NOT_KW) {
            let prefix = self.start();
            self.bump();
            let result = self.expr_bp(6);
            return match result {
                Ok(_) => prefix.complete(self, SyntaxKind::PREFIX_EXPR),
                Err(error) => {
                    prefix.abandon(self)?;
                    Err(error)
                }
            };
        }
        if self.at_any(&[
            SyntaxKind::MINUS,
            SyntaxKind::TILDE,
            SyntaxKind::STAR,
            SyntaxKind::BORROW_KW,
        ]) {
            let prefix = self.start();
            let is_borrow = self.at(SyntaxKind::BORROW_KW);
            self.bump();
            if is_borrow {
                self.eat(SyntaxKind::MUT_KW);
            }
            let result = self.expr_bp(24);
            return match result {
                Ok(_) => prefix.complete(self, SyntaxKind::PREFIX_EXPR),
                Err(error) => {
                    prefix.abandon(self)?;
                    Err(error)
                }
            };
        }
        self.primary_expr()
    }

    fn primary_expr(&mut self) -> Result<CompletedMarker, ParserError> {
        match self.current() {
            SyntaxKind::INT
            | SyntaxKind::FLOAT
            | SyntaxKind::STRING
            | SyntaxKind::BYTE_STRING
            | SyntaxKind::CHAR
            | SyntaxKind::BYTE
            | SyntaxKind::TRUE_KW
            | SyntaxKind::FALSE_KW => {
                let literal = self.start();
                self.bump();
                literal.complete(self, SyntaxKind::LITERAL)
            }
            SyntaxKind::LIDENT | SyntaxKind::UIDENT => {
                let path = self.start();
                self.name_node(IDENT_KINDS, SyntaxKind::NAME_REF)?;
                if self.at(SyntaxKind::LT) {
                    match self.maybe_generic_args_expr() {
                        Ok(_) => {}
                        Err(error) => {
                            path.abandon(self)?;
                            return Err(error);
                        }
                    }
                }
                path.complete(self, SyntaxKind::PATH_EXPR)
            }
            SyntaxKind::SELF_KW => {
                let expression = self.start();
                self.bump();
                expression.complete(self, SyntaxKind::SELF_EXPR)
            }
            SyntaxKind::SELF_TYPE_KW => {
                let expression = self.start();
                self.bump();
                expression.complete(self, SyntaxKind::SELF_TYPE_EXPR)
            }
            SyntaxKind::L_PAREN => self.paren_or_tuple_expr(),
            SyntaxKind::L_BRACE => {
                let expression = self.start();
                if let Err(error) = self.block() {
                    expression.abandon(self)?;
                    return Err(error);
                }
                expression.complete(self, SyntaxKind::BLOCK_EXPR)
            }
            SyntaxKind::UNSAFE_KW => {
                let expression = self.start();
                self.bump();
                if !self.at(SyntaxKind::L_BRACE) {
                    let position = self.position;
                    let kind = self.current();
                    expression.abandon(self)?;
                    return Err(ParserError::UnsupportedSyntax { position, kind });
                }
                if let Err(error) = self.block() {
                    expression.abandon(self)?;
                    return Err(error);
                }
                expression.complete(self, SyntaxKind::UNSAFE_BLOCK_EXPR)
            }
            SyntaxKind::DOT2 | SyntaxKind::DOT2EQ => self.prefix_range_expr(),
            SyntaxKind::DOT if matches!(self.nth(1), SyntaxKind::UIDENT | SyntaxKind::AWAIT_KW) => {
                Err(ParserError::UnsupportedSyntax {
                    position: self.position,
                    kind: SyntaxKind::DOT,
                })
            }
            SyntaxKind::RETURN_KW => self.return_expr(),
            SyntaxKind::IF_KW
            | SyntaxKind::MATCH_KW
            | SyntaxKind::LOOP_KW
            | SyntaxKind::WHILE_KW
            | SyntaxKind::FOR_KW
            | SyntaxKind::BREAK_KW
            | SyntaxKind::CONTINUE_KW
            | SyntaxKind::MOVE_KW
            | SyntaxKind::FN_KW
            | SyntaxKind::ASYNC_KW
            | SyntaxKind::FSTRING_START
            | SyntaxKind::L_BRACKET => Err(ParserError::UnsupportedSyntax {
                position: self.position,
                kind: self.current(),
            }),
            _ => self.missing_expression(),
        }
    }

    fn paren_or_tuple_expr(&mut self) -> Result<CompletedMarker, ParserError> {
        let expression = self.start();
        self.bump();
        if self.eat(SyntaxKind::R_PAREN) {
            return expression.complete(self, SyntaxKind::UNIT_EXPR);
        }
        let result = (|| {
            self.expr_bp(0)?;
            let is_tuple = self.eat(SyntaxKind::COMMA);
            if is_tuple {
                while !self.at_any(&[SyntaxKind::R_PAREN, SyntaxKind::EOF]) {
                    self.expr_bp(0)?;
                    if !self.eat(SyntaxKind::COMMA) {
                        break;
                    }
                }
            }
            Ok(is_tuple)
        })();
        let is_tuple = match result {
            Ok(is_tuple) => is_tuple,
            Err(error) => {
                expression.abandon(self)?;
                return Err(error);
            }
        };
        if !self.eat(SyntaxKind::R_PAREN) {
            self.expect(SyntaxKind::R_PAREN)?;
            return expression.complete(self, SyntaxKind::ERROR);
        }
        expression.complete(
            self,
            if is_tuple {
                SyntaxKind::TUPLE_EXPR
            } else {
                SyntaxKind::PAREN_EXPR
            },
        )
    }

    fn prefix_range_expr(&mut self) -> Result<CompletedMarker, ParserError> {
        let range = self.start();
        let kind = self.current();
        self.bump();
        if self.at_expression_start() {
            if let Err(error) = self.expr_bp(10) {
                range.abandon(self)?;
                return Err(error);
            }
        } else if kind == SyntaxKind::DOT2EQ {
            if self.at(SyntaxKind::NEWLINE) {
                self.missing_expression_after_operator()?;
            } else {
                self.missing_expression()?;
            }
        }
        range.complete(self, SyntaxKind::RANGE_EXPR)
    }

    fn return_expr(&mut self) -> Result<CompletedMarker, ParserError> {
        let expression = self.start();
        self.bump();
        if self.current() != SyntaxKind::NEWLINE && self.at_expression_start() {
            if let Err(error) = self.expr_bp(0) {
                expression.abandon(self)?;
                return Err(error);
            }
        }
        expression.complete(self, SyntaxKind::RETURN_EXPR)
    }

    fn at_expression_start(&self) -> bool {
        self.at_any(EXPRESSION_START_KINDS)
    }

    fn missing_expression(&mut self) -> Result<CompletedMarker, ParserError> {
        for kind in EXPRESSION_START_KINDS {
            self.expected.record(self.position, *kind);
        }
        self.emit_expected_expression()?;
        self.start().complete(self, SyntaxKind::ERROR)
    }

    fn missing_expression_after_operator(&mut self) -> Result<CompletedMarker, ParserError> {
        let expression = self.missing_expression()?;
        self.add_expected_expression_note(
            "put the operand on the same line as the operator or parenthesize the expression",
        )?;
        Ok(expression)
    }

    fn operator_range(&self, raw_count: u8) -> Result<sai_vfs::TextRange, ParserError> {
        let first = self
            .tokens
            .get(self.position)
            .ok_or(ParserError::InvalidDiagnosticSpan)?;
        let last_position = self
            .position
            .checked_add(usize::from(raw_count) - 1)
            .ok_or(ParserError::InvalidDiagnosticSpan)?;
        let last = self
            .tokens
            .get(last_position)
            .ok_or(ParserError::InvalidDiagnosticSpan)?;
        sai_vfs::TextRange::new(first.range.start(), last.range.end())
            .ok_or(ParserError::InvalidDiagnosticSpan)
    }

    fn recover_overdeep_expression(&mut self) -> Result<CompletedMarker, ParserError> {
        let error = self.start();
        let range = self.current_token().map(|token| token.range);
        let mut paren_depth = 0usize;
        let mut bracket_depth = 0usize;
        let mut brace_depth = 0usize;
        let mut saw_delimiter = false;
        let mut consumed = false;
        while !self.at(SyntaxKind::EOF) {
            let kind = self.current();
            if paren_depth == 0
                && bracket_depth == 0
                && brace_depth == 0
                && matches!(
                    kind,
                    SyntaxKind::NEWLINE
                        | SyntaxKind::COMMA
                        | SyntaxKind::R_PAREN
                        | SyntaxKind::R_BRACKET
                        | SyntaxKind::R_BRACE
                )
            {
                break;
            }
            match kind {
                SyntaxKind::L_PAREN => {
                    paren_depth += 1;
                    saw_delimiter = true;
                }
                SyntaxKind::R_PAREN if paren_depth > 0 => paren_depth -= 1,
                SyntaxKind::R_PAREN => break,
                SyntaxKind::L_BRACKET => {
                    bracket_depth += 1;
                    saw_delimiter = true;
                }
                SyntaxKind::R_BRACKET if bracket_depth > 0 => bracket_depth -= 1,
                SyntaxKind::R_BRACKET => break,
                SyntaxKind::L_BRACE => {
                    brace_depth += 1;
                    saw_delimiter = true;
                }
                SyntaxKind::R_BRACE if brace_depth > 0 => brace_depth -= 1,
                SyntaxKind::R_BRACE => break,
                _ => {}
            }
            self.bump();
            consumed = true;
            if saw_delimiter && paren_depth == 0 && bracket_depth == 0 && brace_depth == 0 {
                break;
            }
        }
        let completed = if consumed {
            error.complete(self, SyntaxKind::ERROR)?
        } else {
            error.abandon(self)?;
            self.start().complete(self, SyntaxKind::ERROR)?
        };
        if let Some(range) = range {
            self.report_nesting_limit(range)?;
        }
        Ok(completed)
    }

    fn type_no_fx(&mut self) -> Result<(), ParserError> {
        self.type_(false)
    }

    fn type_(&mut self, allow_fx: bool) -> Result<(), ParserError> {
        if self.nesting_depth >= MAX_NESTING_DEPTH {
            return self.recover_overdeep_type();
        }
        self.nesting_depth += 1;
        let result = self.type_inner(allow_fx);
        self.nesting_depth -= 1;
        result
    }

    fn type_inner(&mut self, allow_fx: bool) -> Result<(), ParserError> {
        match self.current() {
            SyntaxKind::BORROW_KW => {
                let ty = self.start();
                self.bump();
                self.eat(SyntaxKind::MUT_KW);
                self.type_(true)?;
                ty.complete(self, SyntaxKind::BORROW_TYPE)?;
            }
            SyntaxKind::OWNED_KW => {
                let ty = self.start();
                self.bump();
                self.type_no_fx()?;
                ty.complete(self, SyntaxKind::OWNED_TYPE)?;
            }
            SyntaxKind::SELF_TYPE_KW => self.self_type()?,
            SyntaxKind::L_PAREN => self.paren_or_tuple_type()?,
            SyntaxKind::UNDERSCORE => {
                let ty = self.start();
                self.bump();
                ty.complete(self, SyntaxKind::INFER_TYPE)?;
            }
            SyntaxKind::FN_KW | SyntaxKind::UNSAFE_KW => self.fn_ptr_type(allow_fx)?,
            SyntaxKind::LIDENT | SyntaxKind::UIDENT => self.path_type()?,
            _ => {
                self.expect_any(TYPE_START_KINDS)?;
            }
        }
        Ok(())
    }

    fn recover_overdeep_type(&mut self) -> Result<(), ParserError> {
        let error = self.start();
        let source_position = self.current_token().map(|token| token.range);
        let mut paren_depth = 0usize;
        let mut angle_depth = 0usize;
        let mut brace_depth = 0usize;
        let mut consumed = false;

        while !self.at(SyntaxKind::EOF) {
            let kind = self.current();
            if paren_depth == 0
                && angle_depth == 0
                && brace_depth == 0
                && (is_type_recovery_boundary(kind)
                    || (kind == SyntaxKind::L_BRACE && !self.current_opens_effect_set()))
            {
                break;
            }
            match kind {
                SyntaxKind::L_PAREN => paren_depth += 1,
                SyntaxKind::R_PAREN if paren_depth > 0 => paren_depth -= 1,
                SyntaxKind::R_PAREN => break,
                SyntaxKind::LT => angle_depth += 1,
                SyntaxKind::GT if angle_depth > 0 => angle_depth -= 1,
                SyntaxKind::GT => break,
                SyntaxKind::L_BRACE => brace_depth += 1,
                SyntaxKind::R_BRACE if brace_depth > 0 => brace_depth -= 1,
                SyntaxKind::R_BRACE => break,
                _ => {}
            }
            self.bump();
            consumed = true;
        }

        if consumed {
            error.complete(self, SyntaxKind::ERROR)?;
        } else {
            error.abandon(self)?;
            self.expect_any(TYPE_START_KINDS)?;
        }
        if let Some(range) = source_position {
            self.report_nesting_limit(range)?;
        }
        Ok(())
    }

    fn current_opens_effect_set(&self) -> bool {
        self.position.checked_sub(1).is_some_and(|position| {
            token_is_contextual(self.source, self.tokens, position, "effects")
        })
    }

    fn report_nesting_limit(&mut self, range: sai_vfs::TextRange) -> Result<(), ParserError> {
        if self.reported_nesting_limit {
            return Ok(());
        }
        let Some(context) = &self.diagnostic_source else {
            return Err(ParserError::MissingDiagnosticSource);
        };
        let location = sai_diagnostics::Location::from_decoded_source(
            context.source_ref.clone(),
            sai_vfs::Span::new(context.file.id(), range),
            context.file,
        )
        .ok_or(ParserError::InvalidDiagnosticSpan)?;
        let message = "implementation nesting depth limit exceeded";
        let mut diagnostic = sai_diagnostics::Diagnostic::new(
            sai_diagnostics::Code::ImplementationLimitExceeded,
            sai_diagnostics::Severity::Error,
            message,
            Some(location),
        );
        diagnostic.primary_label = message.to_owned();
        diagnostic.data.insert(
            "limit".to_owned(),
            sai_diagnostics::Value::String("nesting depth".to_owned()),
        );
        diagnostic.data.insert(
            "value".to_owned(),
            sai_diagnostics::Value::Integer((MAX_NESTING_DEPTH + 1) as u128),
        );
        self.reported_nesting_limit = true;
        self.emit_error(diagnostic);
        Ok(())
    }

    fn path_type(&mut self) -> Result<(), ParserError> {
        let path = self.start();
        while self.at(SyntaxKind::LIDENT) {
            self.path_segment(SyntaxKind::LIDENT, false)?;
            if !self.eat(SyntaxKind::DOT) {
                self.expect(SyntaxKind::DOT)?;
                break;
            }
            if !self.at_any(&[SyntaxKind::LIDENT, SyntaxKind::UIDENT]) {
                break;
            }
        }

        if self.at(SyntaxKind::UIDENT) {
            self.path_segment(SyntaxKind::UIDENT, true)?;
        } else {
            self.expect(SyntaxKind::UIDENT)?;
        }
        while self.eat(SyntaxKind::DOT) {
            if self.at(SyntaxKind::UIDENT) {
                self.path_segment(SyntaxKind::UIDENT, true)?;
            } else {
                self.expect(SyntaxKind::UIDENT)?;
                break;
            }
        }
        path.complete(self, SyntaxKind::PATH_TYPE)?;
        Ok(())
    }

    fn path_segment(
        &mut self,
        name_kind: SyntaxKind,
        allow_generics: bool,
    ) -> Result<(), ParserError> {
        let segment = self.start();
        self.name_node(&[name_kind], SyntaxKind::NAME_REF)?;
        if allow_generics && self.at(SyntaxKind::LT) {
            self.generic_arg_list()?;
        }
        segment.complete(self, SyntaxKind::PATH_SEGMENT)?;
        Ok(())
    }

    fn self_type(&mut self) -> Result<(), ParserError> {
        let ty = self.start();
        self.bump();
        while self.eat(SyntaxKind::DOT) {
            if self.at(SyntaxKind::UIDENT) {
                self.path_segment(SyntaxKind::UIDENT, false)?;
            } else {
                self.expect(SyntaxKind::UIDENT)?;
                break;
            }
        }
        ty.complete(self, SyntaxKind::SELF_TYPE)?;
        Ok(())
    }

    fn paren_or_tuple_type(&mut self) -> Result<(), ParserError> {
        let ty = self.start();
        self.expect(SyntaxKind::L_PAREN)?;
        if self.eat(SyntaxKind::R_PAREN) {
            ty.complete(self, SyntaxKind::TUPLE_TYPE)?;
            return Ok(());
        }

        self.type_(true)?;
        if self.eat(SyntaxKind::COMMA) {
            while !self.at_any(&[SyntaxKind::R_PAREN, SyntaxKind::EOF]) {
                let position = self.position;
                self.type_(true)?;
                if self.eat(SyntaxKind::COMMA) {
                    continue;
                }
                if self.at_any(&[SyntaxKind::R_PAREN, SyntaxKind::EOF]) {
                    break;
                }
                self.expect(SyntaxKind::COMMA)?;
                if self.position == position {
                    self.recover_one()?;
                    if self.position == position {
                        break;
                    }
                }
            }
            self.expect(SyntaxKind::R_PAREN)?;
            ty.complete(self, SyntaxKind::TUPLE_TYPE)?;
        } else {
            self.expect(SyntaxKind::R_PAREN)?;
            ty.complete(self, SyntaxKind::PAREN_TYPE)?;
        }
        Ok(())
    }

    fn fn_ptr_type(&mut self, allow_fx: bool) -> Result<(), ParserError> {
        let ty = self.start();
        self.eat(SyntaxKind::UNSAFE_KW);
        if !self.expect(SyntaxKind::FN_KW)? {
            ty.abandon(self)?;
            return Ok(());
        }
        if !self.expect(SyntaxKind::L_PAREN)? {
            ty.abandon(self)?;
            return Ok(());
        }
        self.type_list(SyntaxKind::R_PAREN)?;
        self.expect(SyntaxKind::R_PAREN)?;
        if self.at(SyntaxKind::NEWLINE) && self.nth(1) == SyntaxKind::THIN_ARROW {
            self.eat_nl();
        }
        if self.eat(SyntaxKind::THIN_ARROW) {
            self.type_no_fx()?;
        }
        if allow_fx && self.at_contextual("effects") {
            self.effects_clause()?;
        }
        ty.complete(self, SyntaxKind::FN_PTR_TYPE)?;
        Ok(())
    }

    fn type_list(&mut self, close: SyntaxKind) -> Result<(), ParserError> {
        while !self.at_any(&[close, SyntaxKind::EOF]) {
            let position = self.position;
            if !self.at_any(TYPE_START_KINDS) {
                self.expect_any(TYPE_START_KINDS)?;
                self.recover_one()?;
                if self.position == position {
                    break;
                }
                continue;
            }
            self.type_(true)?;
            if self.eat(SyntaxKind::COMMA) {
                continue;
            }
            if self.at_any(&[close, SyntaxKind::EOF]) {
                break;
            }
            self.expect(SyntaxKind::COMMA)?;
            if self.position == position {
                self.recover_one()?;
                if self.position == position {
                    break;
                }
            }
        }
        Ok(())
    }

    fn generic_arg_list(&mut self) -> Result<(), ParserError> {
        let args = self.start();
        self.expect(SyntaxKind::LT)?;
        if self.at(SyntaxKind::GT) {
            self.expect_any(GENERIC_ARG_START_KINDS)?;
        }
        while !self.at_any(&[SyntaxKind::GT, SyntaxKind::EOF])
            && !(self.at(SyntaxKind::NEWLINE) && self.nth(1) == SyntaxKind::GT)
        {
            let position = self.position;
            if !self.at_any(GENERIC_ARG_START_KINDS) {
                self.expect_any(GENERIC_ARG_START_KINDS)?;
                self.recover_one()?;
                if self.position == position {
                    break;
                }
                continue;
            }
            if self.allow_assoc_type_bindings
                && self.at(SyntaxKind::UIDENT)
                && self.nth(1) == SyntaxKind::EQ
            {
                self.assoc_type_binding()?;
            } else if self.at_any(&[
                SyntaxKind::INT,
                SyntaxKind::CHAR,
                SyntaxKind::TRUE_KW,
                SyntaxKind::FALSE_KW,
                SyntaxKind::MINUS,
            ]) {
                self.const_arg()?;
            } else {
                self.type_arg()?;
            }
            if self.eat(SyntaxKind::COMMA) {
                continue;
            }
            if self.at_any(&[SyntaxKind::GT, SyntaxKind::EOF])
                || (self.at(SyntaxKind::NEWLINE) && self.nth(1) == SyntaxKind::GT)
            {
                break;
            }
            self.expect(SyntaxKind::COMMA)?;
            if self.position == position {
                self.recover_one()?;
                if self.position == position {
                    break;
                }
            }
        }
        self.eat_nl();
        self.expect(SyntaxKind::GT)?;
        args.complete(self, SyntaxKind::GENERIC_ARG_LIST)?;
        Ok(())
    }

    fn type_arg(&mut self) -> Result<(), ParserError> {
        let arg = self.start();
        let previous = self.allow_assoc_type_bindings;
        self.allow_assoc_type_bindings = false;
        let result = self.type_(true);
        self.allow_assoc_type_bindings = previous;
        result?;
        arg.complete(self, SyntaxKind::TYPE_ARG)?;
        Ok(())
    }

    fn assoc_type_binding(&mut self) -> Result<(), ParserError> {
        let binding = self.start();
        self.name_node(&[SyntaxKind::UIDENT], SyntaxKind::NAME_REF)?;
        self.expect(SyntaxKind::EQ)?;
        let previous = self.allow_assoc_type_bindings;
        self.allow_assoc_type_bindings = false;
        let result = self.type_(true);
        self.allow_assoc_type_bindings = previous;
        result?;
        binding.complete(self, SyntaxKind::ASSOC_TYPE_BINDING)?;
        Ok(())
    }

    fn const_arg(&mut self) -> Result<(), ParserError> {
        let arg = self.start();
        if self.eat(SyntaxKind::MINUS) {
            self.expect(SyntaxKind::INT)?;
        } else {
            self.bump();
        }
        arg.complete(self, SyntaxKind::CONST_ARG)?;
        Ok(())
    }

    fn attributes(&mut self, target_allowed: bool) -> Result<(), ParserError> {
        let mut reported_invalid_target = false;
        loop {
            if self.at(SyntaxKind::AT) {
                let range = self.current_token().map(|token| token.range);
                self.attribute()?;
                if !target_allowed
                    && !reported_invalid_target
                    && let Some(range) = range
                {
                    self.emit_parser_diagnostic(
                        sai_diagnostics::Code::InvalidAttributeTarget,
                        range,
                        "attribute is not allowed on this declaration",
                    )?;
                    reported_invalid_target = true;
                }
                self.eat_nl();
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
        if self.nesting_depth >= MAX_NESTING_DEPTH {
            let range = self.current_token().map(|token| token.range);
            self.skip_overdeep_attr_args()?;
            if let Some(range) = range {
                self.report_nesting_limit(range)?;
            }
            return Ok(());
        }
        self.nesting_depth += 1;
        let result = self.attr_args_inner();
        self.nesting_depth -= 1;
        result
    }

    fn attr_args_inner(&mut self) -> Result<(), ParserError> {
        let args = self.start();
        if !self.expect(SyntaxKind::L_PAREN)? {
            args.abandon(self)?;
            return Ok(());
        }

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

    fn skip_overdeep_attr_args(&mut self) -> Result<(), ParserError> {
        if !self.at(SyntaxKind::L_PAREN) {
            return self.expect(SyntaxKind::L_PAREN).map(|_| ());
        }
        let error = self.start();
        let mut depth = 0usize;
        while !self.at(SyntaxKind::EOF) {
            if depth > 0 && self.line_starts_top_level_item() {
                break;
            }
            match self.current() {
                SyntaxKind::L_PAREN => depth += 1,
                SyntaxKind::R_PAREN => {
                    depth -= 1;
                    self.bump();
                    if depth == 0 {
                        break;
                    }
                    continue;
                }
                _ => {}
            }
            self.bump();
        }
        error.complete(self, SyntaxKind::ERROR)?;
        Ok(())
    }

    fn attr_arg(&mut self) -> Result<(), ParserError> {
        if !self.at_any(ATTR_ARG_KINDS) {
            self.expect_any(ATTR_ARG_KINDS)?;
            return Ok(());
        }
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
                }
                continue;
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

    fn recover_to_block_separator(&mut self) -> Result<(), ParserError> {
        let error = self.start();
        let mut paren_depth = 0usize;
        let mut bracket_depth = 0usize;
        let mut brace_depth = 0usize;
        let mut consumed = false;
        while !self.at(SyntaxKind::EOF) {
            let kind = self.current();
            if brace_depth == 0 && kind == SyntaxKind::R_BRACE {
                break;
            }
            if paren_depth == 0
                && bracket_depth == 0
                && brace_depth == 0
                && kind == SyntaxKind::NEWLINE
            {
                break;
            }
            match kind {
                SyntaxKind::L_PAREN => paren_depth += 1,
                SyntaxKind::R_PAREN if paren_depth > 0 => paren_depth -= 1,
                SyntaxKind::L_BRACKET => bracket_depth += 1,
                SyntaxKind::R_BRACKET if bracket_depth > 0 => bracket_depth -= 1,
                SyntaxKind::L_BRACE => brace_depth += 1,
                SyntaxKind::R_BRACE if brace_depth > 0 => brace_depth -= 1,
                _ => {}
            }
            self.bump();
            consumed = true;
        }
        if consumed {
            error.complete(self, SyntaxKind::ERROR)?;
        } else {
            error.abandon(self)?;
        }
        Ok(())
    }

    fn recover_operator_continuation(&mut self) -> Result<(), ParserError> {
        let error = self.start();
        let mut paren_depth = 0usize;
        let mut bracket_depth = 0usize;
        let mut brace_depth = 0usize;
        let mut consumed = false;
        while !self.at(SyntaxKind::EOF) {
            let kind = self.current();
            if brace_depth == 0 && kind == SyntaxKind::R_BRACE {
                break;
            }
            if paren_depth == 0
                && bracket_depth == 0
                && brace_depth == 0
                && kind == SyntaxKind::NEWLINE
            {
                break;
            }
            match kind {
                SyntaxKind::L_PAREN => paren_depth += 1,
                SyntaxKind::R_PAREN if paren_depth > 0 => paren_depth -= 1,
                SyntaxKind::L_BRACKET => bracket_depth += 1,
                SyntaxKind::R_BRACKET if bracket_depth > 0 => bracket_depth -= 1,
                SyntaxKind::L_BRACE => brace_depth += 1,
                SyntaxKind::R_BRACE if brace_depth > 0 => brace_depth -= 1,
                _ => {}
            }
            self.bump();
            consumed = true;
        }
        if consumed {
            error.complete(self, SyntaxKind::ERROR)?;
        } else {
            error.abandon(self)?;
        }
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

fn token_is_unraw_identifier(
    source: &str,
    tokens: &[crate::Token],
    position: usize,
    kind: SyntaxKind,
    spelling: &str,
) -> bool {
    let Some(token) = tokens.get(position) else {
        return false;
    };
    token.kind == kind
        && !token.flags.contains(crate::TokenFlags::RAW)
        && source.get(token.range.start().to_usize()..token.range.end().to_usize())
            == Some(spelling)
}

fn is_type_recovery_boundary(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::COMMA
            | SyntaxKind::PLUS
            | SyntaxKind::NEWLINE
            | SyntaxKind::R_PAREN
            | SyntaxKind::R_BRACE
            | SyntaxKind::GT
            | SyntaxKind::EOF
    )
}
