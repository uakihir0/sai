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
const PATTERN_START_KINDS: &[SyntaxKind] = &[
    SyntaxKind::UNDERSCORE,
    SyntaxKind::BORROW_KW,
    SyntaxKind::VAR_KW,
    SyntaxKind::LIDENT,
    SyntaxKind::UIDENT,
    SyntaxKind::MINUS,
    SyntaxKind::INT,
    SyntaxKind::FLOAT,
    SyntaxKind::STRING,
    SyntaxKind::BYTE_STRING,
    SyntaxKind::CHAR,
    SyntaxKind::BYTE,
    SyntaxKind::TRUE_KW,
    SyntaxKind::FALSE_KW,
    SyntaxKind::DOT2EQ,
    SyntaxKind::DOT,
    SyntaxKind::L_PAREN,
    SyntaxKind::L_BRACKET,
];
const RANGE_END_START_KINDS: &[SyntaxKind] = &[
    SyntaxKind::MINUS,
    SyntaxKind::INT,
    SyntaxKind::CHAR,
    SyntaxKind::BYTE,
    SyntaxKind::LIDENT,
    SyntaxKind::UIDENT,
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
    SyntaxKind::LABEL,
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

#[derive(Clone, Copy, Eq, PartialEq)]
enum FnBodyPolicy {
    Required,
    Optional,
    Forbidden,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ImplTargetKind {
    Trait,
    Inherent,
    Interface,
}

impl Parser<'_, '_> {
    /// Parses the currently implemented source-file grammar into parser events.
    ///
    /// Item and expression forms are supported as their grammar routines are
    /// implemented. Unsupported forms return [`ParserError::UnsupportedItem`]
    /// or [`ParserError::UnsupportedSyntax`] for the enclosing recovery path.
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
            self.recovered_match_tail = false;
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
            if self.at_any(&[
                SyntaxKind::R_PAREN,
                SyntaxKind::R_BRACKET,
                SyntaxKind::R_BRACE,
            ]) {
                self.recover_unexpected_top_level_closer()?;
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

    fn recover_unexpected_top_level_closer(&mut self) -> Result<(), ParserError> {
        let range = self
            .current_token()
            .map(|token| token.range)
            .ok_or(ParserError::InvalidDiagnosticSpan)?;
        self.emit_parser_diagnostic(
            sai_diagnostics::Code::ExpectedToken,
            range,
            "unexpected closing delimiter at file scope",
        )?;
        let error = self.start();
        self.bump();
        error.complete(self, SyntaxKind::ERROR)?;
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

        let mut exported_range = None;
        let mut public_range = None;
        if self.at_any(&[SyntaxKind::PUB_KW, SyntaxKind::EXPORT_KW]) {
            let visibility = self.start();
            let is_pub = self.at(SyntaxKind::PUB_KW);
            let visibility_range = self.current_token().map(|token| token.range);
            self.bump();
            if is_pub && self.at(SyntaxKind::EXPORT_KW) {
                if let Some(range) = visibility_range {
                    self.emit_parser_diagnostic(
                        sai_diagnostics::Code::RedundantPubExport,
                        range,
                        "`pub export` is redundant; use `export`",
                    )?;
                }
                exported_range = self.current_token().map(|token| token.range);
                self.bump();
            } else if !is_pub {
                exported_range = visibility_range;
            } else {
                public_range = visibility_range;
            }
            visibility.complete(self, SyntaxKind::VISIBILITY)?;
        }

        if self.at_fn_decl_start() {
            return self.fn_decl(declaration, FnBodyPolicy::Required);
        }

        if let Some(range) = exported_range {
            self.emit_parser_diagnostic(
                sai_diagnostics::Code::InvalidExportTarget,
                range,
                "only functions may be exported",
            )?;
        }

        let public_impl = self.at(SyntaxKind::IMPL_KW)
            || (self.at(SyntaxKind::UNSAFE_KW) && self.nth(1) == SyntaxKind::IMPL_KW);
        let public_invariant = self.at_contextual("invariant") && self.nth(1) == SyntaxKind::UIDENT;
        if let Some(range) = public_range
            && (public_impl || public_invariant)
        {
            self.emit_parser_diagnostic(
                sai_diagnostics::Code::ExpectedToken,
                range,
                if public_impl {
                    "impl blocks do not have a visibility marker"
                } else {
                    "invariant declarations do not have a visibility marker"
                },
            )?;
        }

        if self.at(SyntaxKind::TYPE_KW) {
            return self.type_decl(declaration);
        }

        if self.at(SyntaxKind::ENUM_KW) {
            return self.enum_decl(declaration);
        }

        if self.at_contextual("distinct") && self.nth(1) == SyntaxKind::UIDENT {
            return self.distinct_decl(declaration);
        }

        if self.at_contextual("resource") && self.nth(1) == SyntaxKind::UIDENT {
            return self.resource_decl(declaration);
        }

        if self.at(SyntaxKind::TRAIT_KW)
            || (self.at(SyntaxKind::UNSAFE_KW) && self.nth(1) == SyntaxKind::TRAIT_KW)
        {
            return self.trait_decl(declaration);
        }

        if self.at(SyntaxKind::IMPL_KW)
            || (self.at(SyntaxKind::UNSAFE_KW) && self.nth(1) == SyntaxKind::IMPL_KW)
        {
            return self.impl_decl(declaration);
        }

        if self.at(SyntaxKind::CONST_KW) {
            return self.const_decl(declaration);
        }

        if self.at_contextual("invariant") && self.nth(1) == SyntaxKind::UIDENT {
            return self.invariant_decl(declaration);
        }

        if self.at_contextual("effect") && self.nth(1) == SyntaxKind::LIDENT {
            return self.effect_decl(declaration);
        }

        if self.at_contextual("interface") && self.nth(1) == SyntaxKind::LIDENT {
            return self.interface_decl(declaration);
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
        self.fn_decl_starts_at(self.position)
    }

    fn fn_decl_starts_at(&self, position: usize) -> bool {
        let kind_at = |offset: usize| {
            position
                .checked_add(offset)
                .and_then(|index| self.tokens.get(index))
                .map(|token| token.kind)
        };
        match kind_at(0) {
            Some(SyntaxKind::FN_KW) => true,
            Some(SyntaxKind::ASYNC_KW) => kind_at(1) == Some(SyntaxKind::FN_KW),
            Some(SyntaxKind::UNSAFE_KW) => {
                kind_at(1) == Some(SyntaxKind::FN_KW)
                    || (kind_at(1) == Some(SyntaxKind::ASYNC_KW)
                        && kind_at(2) == Some(SyntaxKind::FN_KW))
            }
            _ => false,
        }
    }

    fn type_decl(&mut self, declaration: Marker) -> Result<CompletedMarker, ParserError> {
        self.expect(SyntaxKind::TYPE_KW)?;
        self.name_node(&[SyntaxKind::UIDENT], SyntaxKind::NAME)?;
        if self.at(SyntaxKind::LT) {
            self.generic_param_list()?;
        }
        if self.at(SyntaxKind::EQ) {
            self.alias_def()?;
        } else {
            self.struct_def()?;
        }
        declaration.complete(self, SyntaxKind::TYPE_DECL)
    }

    fn const_decl(&mut self, declaration: Marker) -> Result<CompletedMarker, ParserError> {
        self.expect(SyntaxKind::CONST_KW)?;
        self.name_node(&[SyntaxKind::UIDENT], SyntaxKind::NAME)?;
        self.expect(SyntaxKind::COLON)?;
        if self.item_starts_at_after_attributes(self.position) {
            let range = self
                .current_token()
                .map(|token| token.range)
                .ok_or(ParserError::InvalidDiagnosticSpan)?;
            self.emit_parser_diagnostic(
                sai_diagnostics::Code::ExpectedToken,
                range,
                "expected a type before the next item",
            )?;
            self.start().complete(self, SyntaxKind::ERROR)?;
        } else {
            self.type_(true)?;
        }
        self.expect(SyntaxKind::EQ)?;
        self.eat_nl();
        if self.at_physical_line_start() && self.item_starts_at_after_attributes(self.position) {
            self.missing_expression()?;
        } else {
            self.expr_bp(0)?;
        }
        declaration.complete(self, SyntaxKind::CONST_DECL)
    }

    fn invariant_decl(&mut self, declaration: Marker) -> Result<CompletedMarker, ParserError> {
        self.bump();
        self.name_node(&[SyntaxKind::UIDENT], SyntaxKind::NAME)?;
        if self.at(SyntaxKind::NEWLINE) && self.nth(1) == SyntaxKind::L_BRACE {
            self.eat_nl();
        }
        self.invariant_expr_list()?;
        declaration.complete(self, SyntaxKind::INVARIANT_DECL)
    }

    fn invariant_expr_list(&mut self) -> Result<(), ParserError> {
        if !self.at(SyntaxKind::L_BRACE) {
            self.expect(SyntaxKind::L_BRACE)?;
            return Ok(());
        }
        let expressions = self.start();
        self.bump();
        self.eat_nl();
        let mut has_expression = false;
        while !self.at_any(&[SyntaxKind::R_BRACE, SyntaxKind::EOF]) {
            if self.at(SyntaxKind::NEWLINE) {
                if self.invariant_expr_list_boundary_after_newline() {
                    break;
                }
                self.eat_nl();
                continue;
            }
            if self.at_physical_line_start() && self.item_starts_at_after_attributes(self.position)
            {
                break;
            }

            let position = self.position;
            self.expr_bp(0)?;
            has_expression = true;
            if self.eat(SyntaxKind::COMMA) {
                self.eat_nl();
                continue;
            }
            if self.at(SyntaxKind::NEWLINE) {
                if self.invariant_expr_list_boundary_after_newline() {
                    break;
                }
                self.eat_nl();
                continue;
            }
            if self.at_any(&[SyntaxKind::R_BRACE, SyntaxKind::EOF]) {
                break;
            }
            if self.at_expression_start() {
                self.expect_any(&[SyntaxKind::COMMA, SyntaxKind::NEWLINE])?;
                continue;
            }
            self.expect_any(&[SyntaxKind::COMMA, SyntaxKind::NEWLINE])?;
            if self.position == position {
                self.recover_one()?;
                if self.position == position {
                    break;
                }
            }
        }
        if !has_expression {
            self.missing_expression()?;
        }
        if !self.invariant_expr_list_boundary_after_newline() {
            self.eat_nl();
        }
        self.expect(SyntaxKind::R_BRACE)?;
        expressions.complete(self, SyntaxKind::INVARIANT_EXPR_LIST)?;
        Ok(())
    }

    fn invariant_expr_list_boundary_after_newline(&self) -> bool {
        if !self.at(SyntaxKind::NEWLINE) {
            return false;
        }
        let mut position = self.position;
        while self
            .tokens
            .get(position)
            .is_some_and(|token| token.kind == SyntaxKind::NEWLINE)
        {
            position += 1;
        }
        self.tokens
            .get(position)
            .is_some_and(|token| token.flags.contains(crate::TokenFlags::AFTER_LINE_BREAK))
            && self.item_starts_at_after_attributes(position)
    }

    fn effect_decl(&mut self, declaration: Marker) -> Result<CompletedMarker, ParserError> {
        self.bump();
        self.name_node(&[SyntaxKind::LIDENT], SyntaxKind::NAME)?;
        if self.at(SyntaxKind::NEWLINE)
            && self.position.checked_add(1).is_some_and(|position| {
                token_is_contextual(self.source, self.tokens, position, "covers")
            })
        {
            self.eat_nl();
        }
        if self.at_contextual("covers") {
            let covers = self.start();
            self.bump();
            self.effect_set()?;
            covers.complete(self, SyntaxKind::COVERS_CLAUSE)?;
        }
        declaration.complete(self, SyntaxKind::EFFECT_DECL)
    }

    fn enum_decl(&mut self, declaration: Marker) -> Result<CompletedMarker, ParserError> {
        self.expect(SyntaxKind::ENUM_KW)?;
        self.name_node(&[SyntaxKind::UIDENT], SyntaxKind::NAME)?;
        if self.at(SyntaxKind::LT) {
            self.generic_param_list()?;
        }
        if self.at(SyntaxKind::NEWLINE) && self.nth(1) == SyntaxKind::WHERE_KW {
            self.eat_nl();
        }
        if self.at(SyntaxKind::WHERE_KW) {
            self.where_clause()?;
        }
        if self.at(SyntaxKind::NEWLINE) && self.nth(1) == SyntaxKind::L_BRACE {
            self.eat_nl();
        }

        let mut has_payload_variant = false;
        let mut first_discriminant = None;
        self.variant_list(&mut has_payload_variant, &mut first_discriminant)?;
        if has_payload_variant {
            if let Some(range) = first_discriminant {
                self.emit_parser_diagnostic(
                    sai_diagnostics::Code::ExplicitDiscriminantOnDataEnum,
                    range,
                    "explicit discriminants are not allowed on enums with payload variants",
                )?;
            }
        }
        declaration.complete(self, SyntaxKind::ENUM_DECL)
    }

    fn distinct_decl(&mut self, declaration: Marker) -> Result<CompletedMarker, ParserError> {
        self.bump();
        self.name_node(&[SyntaxKind::UIDENT], SyntaxKind::NAME)?;
        if self.at(SyntaxKind::LT) {
            self.generic_param_list()?;
        }
        self.expect(SyntaxKind::EQ)?;
        self.eat(SyntaxKind::PUB_KW);
        if self.item_starts_at_after_attributes(self.position) {
            let range = self
                .current_token()
                .map(|token| token.range)
                .ok_or(ParserError::InvalidDiagnosticSpan)?;
            self.emit_parser_diagnostic(
                sai_diagnostics::Code::ExpectedToken,
                range,
                "expected a base type before the next item",
            )?;
            self.start().complete(self, SyntaxKind::ERROR)?;
        } else {
            self.type_(true)?;
        }
        if self.at(SyntaxKind::NEWLINE) && self.nth(1) == SyntaxKind::WHERE_KW {
            self.eat_nl();
        }
        if self.at(SyntaxKind::WHERE_KW) {
            let refinement = self.start();
            self.bump();
            self.expr_bp(0)?;
            refinement.complete(self, SyntaxKind::REFINEMENT_CLAUSE)?;
        }
        declaration.complete(self, SyntaxKind::DISTINCT_DECL)
    }

    fn resource_decl(&mut self, declaration: Marker) -> Result<CompletedMarker, ParserError> {
        self.bump();
        self.name_node(&[SyntaxKind::UIDENT], SyntaxKind::NAME)?;
        if self.at(SyntaxKind::NEWLINE) && self.nth(1) == SyntaxKind::L_BRACE {
            self.eat_nl();
        }
        if self.at(SyntaxKind::L_BRACE) {
            self.field_list()?;
        }
        declaration.complete(self, SyntaxKind::RESOURCE_DECL)
    }

    fn interface_decl(&mut self, declaration: Marker) -> Result<CompletedMarker, ParserError> {
        self.bump();
        self.name_node(&[SyntaxKind::LIDENT], SyntaxKind::NAME)?;
        if self.at(SyntaxKind::NEWLINE) && self.nth(1) == SyntaxKind::L_BRACE {
            self.eat_nl();
        }
        self.interface_item_list()?;
        declaration.complete(self, SyntaxKind::INTERFACE_DECL)
    }

    fn interface_item_list(&mut self) -> Result<(), ParserError> {
        if !self.at(SyntaxKind::L_BRACE) {
            self.expect(SyntaxKind::L_BRACE)?;
            return Ok(());
        }
        let items = self.start();
        self.bump();
        while self.at(SyntaxKind::NEWLINE) && !self.interface_item_list_boundary_after_newline() {
            self.eat_nl();
        }
        while !self.at_any(&[SyntaxKind::R_BRACE, SyntaxKind::EOF]) {
            if self.at(SyntaxKind::NEWLINE) && self.interface_item_list_boundary_after_newline() {
                break;
            }
            if self.at(SyntaxKind::DOC_INNER)
                || (self.at(SyntaxKind::DOC_OUTER)
                    && !self.interface_item_starts_at_after_attributes(self.position))
            {
                self.misplaced_doc_comment()?;
                continue;
            }
            let position = self.position;
            self.interface_item()?;
            if self.at(SyntaxKind::NEWLINE) && self.interface_item_list_boundary_after_newline() {
                break;
            }
            if self.eat(SyntaxKind::NEWLINE) {
                while self.at(SyntaxKind::NEWLINE)
                    && !self.interface_item_list_boundary_after_newline()
                {
                    self.eat_nl();
                }
                continue;
            }
            if self.at_any(&[SyntaxKind::R_BRACE, SyntaxKind::EOF]) {
                break;
            }
            self.expect(SyntaxKind::NEWLINE)?;
            if self.position == position {
                self.recover_one()?;
                if self.position == position {
                    break;
                }
            }
        }
        if !self.interface_item_list_boundary_after_newline() {
            self.eat_nl();
        }
        self.expect(SyntaxKind::R_BRACE)?;
        items.complete(self, SyntaxKind::ITEM_LIST)?;
        Ok(())
    }

    fn interface_item_list_boundary_after_newline(&self) -> bool {
        if !self.at(SyntaxKind::NEWLINE) {
            return false;
        }
        let mut position = self.position;
        while self
            .tokens
            .get(position)
            .is_some_and(|token| token.kind == SyntaxKind::NEWLINE)
        {
            position += 1;
        }
        let Some(token) = self.tokens.get(position) else {
            return false;
        };
        token.flags.contains(crate::TokenFlags::AFTER_LINE_BREAK)
            && !self.interface_item_starts_at_after_attributes(position)
            && self.item_starts_at_after_attributes(position)
    }

    fn interface_item_starts_at_after_attributes(&self, position: usize) -> bool {
        let Some(mut position) = self.position_after_attributes(position) else {
            return false;
        };
        if self
            .tokens
            .get(position)
            .is_some_and(|token| matches!(token.kind, SyntaxKind::PUB_KW | SyntaxKind::EXPORT_KW))
        {
            position += 1;
        }
        match self.tokens.get(position).map(|token| token.kind) {
            Some(SyntaxKind::TYPE_KW | SyntaxKind::ENUM_KW | SyntaxKind::FN_KW) => true,
            Some(SyntaxKind::ASYNC_KW) => self
                .tokens
                .get(position + 1)
                .is_some_and(|token| token.kind == SyntaxKind::FN_KW),
            Some(SyntaxKind::UNSAFE_KW) => self.fn_decl_starts_at(position),
            Some(SyntaxKind::LIDENT)
                if token_is_contextual(self.source, self.tokens, position, "resource") =>
            {
                self.tokens
                    .get(position + 1)
                    .is_some_and(|token| token.kind == SyntaxKind::UIDENT)
            }
            _ => false,
        }
    }

    fn interface_item(&mut self) -> Result<(), ParserError> {
        let declaration = self.start();
        self.outer_docs()?;
        self.attributes(true)?;
        if self.at_any(&[SyntaxKind::PUB_KW, SyntaxKind::EXPORT_KW]) {
            let range = self
                .current_token()
                .map(|token| token.range)
                .ok_or(ParserError::InvalidDiagnosticSpan)?;
            self.emit_parser_diagnostic(
                sai_diagnostics::Code::ExpectedToken,
                range,
                "interface items do not have a visibility marker",
            )?;
            let visibility = self.start();
            self.bump();
            visibility.complete(self, SyntaxKind::ERROR)?;
        }
        if self.at(SyntaxKind::TYPE_KW) {
            self.type_decl(declaration)?;
            return Ok(());
        }
        if self.at(SyntaxKind::ENUM_KW) {
            self.enum_decl(declaration)?;
            return Ok(());
        }
        if self.at_contextual("resource") && self.nth(1) == SyntaxKind::UIDENT {
            self.iface_resource(declaration)?;
            return Ok(());
        }
        if self.at_fn_decl_start() {
            self.fn_decl(declaration, FnBodyPolicy::Forbidden)?;
            return Ok(());
        }

        let range = self
            .current_token()
            .map(|token| token.range)
            .ok_or(ParserError::InvalidDiagnosticSpan)?;
        self.emit_parser_diagnostic(
            sai_diagnostics::Code::ExpectedToken,
            range,
            "expected an interface type, enum, resource, or function",
        )?;
        self.recover_one()?;
        declaration.complete(self, SyntaxKind::ERROR)?;
        Ok(())
    }

    fn iface_resource(&mut self, declaration: Marker) -> Result<CompletedMarker, ParserError> {
        self.bump();
        self.name_node(&[SyntaxKind::UIDENT], SyntaxKind::NAME)?;
        if self.at(SyntaxKind::NEWLINE) && self.nth(1) == SyntaxKind::L_BRACE {
            self.eat_nl();
        }
        if self.at(SyntaxKind::L_BRACE) {
            self.iface_resource_item_list()?;
        }
        declaration.complete(self, SyntaxKind::IFACE_RESOURCE)
    }

    fn iface_resource_item_list(&mut self) -> Result<(), ParserError> {
        let items = self.start();
        self.bump();
        while self.at(SyntaxKind::NEWLINE)
            && !self.iface_resource_item_list_boundary_after_newline()
        {
            self.eat_nl();
        }
        while !self.at_any(&[SyntaxKind::R_BRACE, SyntaxKind::EOF]) {
            if self.at(SyntaxKind::NEWLINE)
                && self.iface_resource_item_list_boundary_after_newline()
            {
                break;
            }
            if self.at(SyntaxKind::DOC_INNER)
                || (self.at(SyntaxKind::DOC_OUTER)
                    && !self.iface_resource_item_starts_at_after_attributes(self.position))
            {
                self.misplaced_doc_comment()?;
                continue;
            }
            let position = self.position;
            self.iface_resource_item()?;
            if self.at(SyntaxKind::NEWLINE)
                && self.iface_resource_item_list_boundary_after_newline()
            {
                break;
            }
            if self.eat(SyntaxKind::NEWLINE) {
                while self.at(SyntaxKind::NEWLINE)
                    && !self.iface_resource_item_list_boundary_after_newline()
                {
                    self.eat_nl();
                }
                continue;
            }
            if self.at_any(&[SyntaxKind::R_BRACE, SyntaxKind::EOF]) {
                break;
            }
            self.expect(SyntaxKind::NEWLINE)?;
            if self.position == position {
                self.recover_one()?;
                if self.position == position {
                    break;
                }
            }
        }
        if !self.iface_resource_item_list_boundary_after_newline() {
            self.eat_nl();
        }
        self.expect(SyntaxKind::R_BRACE)?;
        items.complete(self, SyntaxKind::ITEM_LIST)?;
        Ok(())
    }

    fn iface_resource_item_list_boundary_after_newline(&self) -> bool {
        if !self.at(SyntaxKind::NEWLINE) {
            return false;
        }
        let mut position = self.position;
        while self
            .tokens
            .get(position)
            .is_some_and(|token| token.kind == SyntaxKind::NEWLINE)
        {
            position += 1;
        }
        let Some(token) = self.tokens.get(position) else {
            return false;
        };
        token.flags.contains(crate::TokenFlags::AFTER_LINE_BREAK)
            && !self.iface_resource_item_starts_at_after_attributes(position)
            && self.item_starts_at_after_attributes(position)
    }

    fn iface_resource_item_starts_at_after_attributes(&self, position: usize) -> bool {
        let Some(mut position) = self.position_after_attributes(position) else {
            return false;
        };
        if self
            .tokens
            .get(position)
            .is_some_and(|token| matches!(token.kind, SyntaxKind::PUB_KW | SyntaxKind::EXPORT_KW))
        {
            position += 1;
        }
        self.fn_decl_starts_at(position)
    }

    fn iface_resource_item(&mut self) -> Result<(), ParserError> {
        let declaration = self.start();
        self.outer_docs()?;
        self.attributes(true)?;
        if self.at_any(&[SyntaxKind::PUB_KW, SyntaxKind::EXPORT_KW]) {
            let range = self
                .current_token()
                .map(|token| token.range)
                .ok_or(ParserError::InvalidDiagnosticSpan)?;
            self.emit_parser_diagnostic(
                sai_diagnostics::Code::ExpectedToken,
                range,
                "interface resource items do not have a visibility marker",
            )?;
            let visibility = self.start();
            self.bump();
            visibility.complete(self, SyntaxKind::ERROR)?;
        }
        if self.at_fn_decl_start() {
            self.fn_decl(declaration, FnBodyPolicy::Forbidden)?;
            return Ok(());
        }

        let range = self
            .current_token()
            .map(|token| token.range)
            .ok_or(ParserError::InvalidDiagnosticSpan)?;
        self.emit_parser_diagnostic(
            sai_diagnostics::Code::ExpectedToken,
            range,
            "expected a function in the interface resource",
        )?;
        self.recover_one()?;
        declaration.complete(self, SyntaxKind::ERROR)?;
        Ok(())
    }

    fn trait_decl(&mut self, declaration: Marker) -> Result<CompletedMarker, ParserError> {
        self.eat(SyntaxKind::UNSAFE_KW);
        self.expect(SyntaxKind::TRAIT_KW)?;
        self.name_node(&[SyntaxKind::UIDENT], SyntaxKind::NAME)?;
        if self.at(SyntaxKind::LT) {
            self.generic_param_list()?;
        }
        if self.eat(SyntaxKind::COLON) {
            self.bound_list()?;
        }
        if self.at(SyntaxKind::NEWLINE) && self.nth(1) == SyntaxKind::WHERE_KW {
            self.eat_nl();
        }
        if self.at(SyntaxKind::WHERE_KW) {
            self.where_clause()?;
        }
        if self.at(SyntaxKind::NEWLINE) && self.nth(1) == SyntaxKind::L_BRACE {
            self.eat_nl();
        }
        self.trait_item_list()?;
        declaration.complete(self, SyntaxKind::TRAIT_DECL)
    }

    fn trait_item_list(&mut self) -> Result<(), ParserError> {
        if !self.at(SyntaxKind::L_BRACE) {
            self.expect(SyntaxKind::L_BRACE)?;
            return Ok(());
        }
        let items = self.start();
        self.bump();
        while self.at(SyntaxKind::NEWLINE) && !self.trait_item_list_boundary_after_newline() {
            self.eat_nl();
        }
        while !self.at_any(&[SyntaxKind::R_BRACE, SyntaxKind::EOF]) {
            if self.at(SyntaxKind::NEWLINE) && self.trait_item_list_boundary_after_newline() {
                break;
            }
            if self.at(SyntaxKind::DOC_INNER)
                || (self.at(SyntaxKind::DOC_OUTER)
                    && !self.trait_item_starts_at_after_attributes(self.position))
            {
                self.misplaced_doc_comment()?;
                continue;
            }
            let position = self.position;
            self.trait_item()?;
            if self.at(SyntaxKind::NEWLINE) && self.trait_item_list_boundary_after_newline() {
                break;
            }
            if self.eat(SyntaxKind::NEWLINE) {
                while self.at(SyntaxKind::NEWLINE) && !self.trait_item_list_boundary_after_newline()
                {
                    self.eat_nl();
                }
                continue;
            }
            if self.at_any(&[SyntaxKind::R_BRACE, SyntaxKind::EOF]) {
                break;
            }
            self.expect(SyntaxKind::NEWLINE)?;
            if self.position == position {
                self.recover_one()?;
                if self.position == position {
                    break;
                }
            }
        }
        if !self.trait_item_list_boundary_after_newline() {
            self.eat_nl();
        }
        self.expect(SyntaxKind::R_BRACE)?;
        items.complete(self, SyntaxKind::ITEM_LIST)?;
        Ok(())
    }

    fn trait_item_list_boundary_after_newline(&self) -> bool {
        if !self.at(SyntaxKind::NEWLINE) {
            return false;
        }
        let mut position = self.position;
        while self
            .tokens
            .get(position)
            .is_some_and(|token| token.kind == SyntaxKind::NEWLINE)
        {
            position += 1;
        }
        let Some(token) = self.tokens.get(position) else {
            return false;
        };
        if !token.flags.contains(crate::TokenFlags::AFTER_LINE_BREAK)
            || self.trait_item_starts_at_after_attributes(position)
        {
            return false;
        }
        self.statement_starts_at_after_attributes(position)
    }

    fn trait_item_starts_at_after_attributes(&self, position: usize) -> bool {
        let Some(mut position) = self.position_after_attributes(position) else {
            return false;
        };
        if self
            .tokens
            .get(position)
            .is_some_and(|token| matches!(token.kind, SyntaxKind::PUB_KW | SyntaxKind::EXPORT_KW))
        {
            position += 1;
        }
        match self.tokens.get(position).map(|token| token.kind) {
            Some(SyntaxKind::TYPE_KW) => {
                self.tokens
                    .get(position + 1)
                    .is_some_and(|token| token.kind == SyntaxKind::UIDENT)
                    && !self
                        .tokens
                        .get(position + 2)
                        .is_some_and(|token| token.kind == SyntaxKind::EQ)
            }
            Some(SyntaxKind::FN_KW) => true,
            Some(SyntaxKind::ASYNC_KW) => self
                .tokens
                .get(position + 1)
                .is_some_and(|token| token.kind == SyntaxKind::FN_KW),
            Some(SyntaxKind::UNSAFE_KW) => {
                self.tokens
                    .get(position + 1)
                    .is_some_and(|token| token.kind == SyntaxKind::FN_KW)
                    || (self
                        .tokens
                        .get(position + 1)
                        .is_some_and(|token| token.kind == SyntaxKind::ASYNC_KW)
                        && self
                            .tokens
                            .get(position + 2)
                            .is_some_and(|token| token.kind == SyntaxKind::FN_KW))
            }
            _ => false,
        }
    }

    fn trait_item(&mut self) -> Result<(), ParserError> {
        self.outer_docs()?;
        self.attributes(true)?;
        if self.at_any(&[SyntaxKind::PUB_KW, SyntaxKind::EXPORT_KW]) {
            let range = self
                .current_token()
                .map(|token| token.range)
                .ok_or(ParserError::InvalidDiagnosticSpan)?;
            self.emit_parser_diagnostic(
                sai_diagnostics::Code::ExpectedToken,
                range,
                "trait items do not have a visibility marker",
            )?;
            let visibility = self.start();
            self.bump();
            visibility.complete(self, SyntaxKind::ERROR)?;
        }
        if self.at(SyntaxKind::TYPE_KW) {
            return self.assoc_type_decl();
        }
        if self.at_fn_decl_start() {
            let function = self.start();
            self.fn_decl(function, FnBodyPolicy::Optional)?;
            return Ok(());
        }

        let range = self
            .current_token()
            .map(|token| token.range)
            .ok_or(ParserError::InvalidDiagnosticSpan)?;
        self.emit_parser_diagnostic(
            sai_diagnostics::Code::ExpectedToken,
            range,
            "expected an associated type or function in the trait",
        )?;
        let error = self.start();
        self.recover_one()?;
        error.complete(self, SyntaxKind::ERROR)?;
        Ok(())
    }

    fn assoc_type_decl(&mut self) -> Result<(), ParserError> {
        let declaration = self.start();
        self.expect(SyntaxKind::TYPE_KW)?;
        self.name_node(&[SyntaxKind::UIDENT], SyntaxKind::NAME)?;
        if self.eat(SyntaxKind::COLON) {
            self.bound_list()?;
        }
        declaration.complete(self, SyntaxKind::ASSOC_TYPE_DECL)?;
        Ok(())
    }

    fn impl_decl(&mut self, declaration: Marker) -> Result<CompletedMarker, ParserError> {
        self.eat(SyntaxKind::UNSAFE_KW);
        self.expect(SyntaxKind::IMPL_KW)?;
        if self.at(SyntaxKind::LT) {
            self.generic_param_list()?;
        }

        let target_kind;
        if !self.impl_header_has_for_at(self.position)
            && self.mod_path_end_at(self.position).is_some()
        {
            self.mod_path()?;
            target_kind = ImplTargetKind::Interface;
        } else {
            let is_trait_impl = self.impl_header_has_for_at(self.position);
            let previous = self.allow_assoc_type_bindings;
            self.allow_assoc_type_bindings = is_trait_impl;
            let target_result = if self.at_any(TYPE_START_KINDS) {
                self.type_(true)
            } else {
                self.expect_any(TYPE_START_KINDS).map(|_| ())
            };
            self.allow_assoc_type_bindings = previous;
            target_result?;
            if self.eat(SyntaxKind::FOR_KW) {
                target_kind = ImplTargetKind::Trait;
                if self.at_any(TYPE_START_KINDS) {
                    self.type_(true)?;
                } else {
                    self.expect_any(TYPE_START_KINDS)?;
                }
            } else {
                target_kind = ImplTargetKind::Inherent;
            }
        }

        if self.at(SyntaxKind::NEWLINE) && self.nth(1) == SyntaxKind::WHERE_KW {
            self.eat_nl();
        }
        if self.at(SyntaxKind::WHERE_KW) {
            self.where_clause()?;
        }
        if self.at(SyntaxKind::NEWLINE) && self.nth(1) == SyntaxKind::L_BRACE {
            self.eat_nl();
        }
        self.impl_item_list(target_kind)?;
        declaration.complete(self, SyntaxKind::IMPL_DECL)
    }

    fn impl_header_has_for_at(&self, mut position: usize) -> bool {
        let mut paren_depth = 0usize;
        let mut bracket_depth = 0usize;
        let mut brace_depth = 0usize;
        let mut angle_depth = 0usize;
        while let Some(token) = self.tokens.get(position) {
            if paren_depth == 0 && bracket_depth == 0 && brace_depth == 0 && angle_depth == 0 {
                match token.kind {
                    SyntaxKind::FOR_KW => return true,
                    SyntaxKind::WHERE_KW
                    | SyntaxKind::NEWLINE
                    | SyntaxKind::L_BRACE
                    | SyntaxKind::EOF => return false,
                    _ => {}
                }
            }
            match token.kind {
                SyntaxKind::L_PAREN => paren_depth += 1,
                SyntaxKind::R_PAREN if paren_depth > 0 => paren_depth -= 1,
                SyntaxKind::R_PAREN => return false,
                SyntaxKind::L_BRACKET => bracket_depth += 1,
                SyntaxKind::R_BRACKET if bracket_depth > 0 => bracket_depth -= 1,
                SyntaxKind::R_BRACKET => return false,
                SyntaxKind::L_BRACE => brace_depth += 1,
                SyntaxKind::R_BRACE if brace_depth > 0 => brace_depth -= 1,
                SyntaxKind::R_BRACE => return false,
                SyntaxKind::LT => angle_depth += 1,
                SyntaxKind::GT if angle_depth > 0 => angle_depth -= 1,
                SyntaxKind::GT => return false,
                _ => {}
            }
            position += 1;
        }
        false
    }

    fn mod_path_end_at(&self, mut position: usize) -> Option<usize> {
        if self
            .tokens
            .get(position)
            .is_none_or(|token| token.kind != SyntaxKind::LIDENT)
        {
            return None;
        }
        position += 1;
        while self.tokens.get(position).is_some_and(|token| {
            token.kind == SyntaxKind::DOT
                && self
                    .tokens
                    .get(position + 1)
                    .is_some_and(|next| next.kind == SyntaxKind::LIDENT)
        }) {
            position += 2;
        }
        self.tokens
            .get(position)
            .is_some_and(|token| {
                matches!(
                    token.kind,
                    SyntaxKind::WHERE_KW
                        | SyntaxKind::NEWLINE
                        | SyntaxKind::L_BRACE
                        | SyntaxKind::EOF
                )
            })
            .then_some(position)
    }

    fn mod_path(&mut self) -> Result<(), ParserError> {
        let path = self.start();
        self.name_node(&[SyntaxKind::LIDENT], SyntaxKind::NAME_REF)?;
        while self.eat(SyntaxKind::DOT) {
            self.name_node(&[SyntaxKind::LIDENT], SyntaxKind::NAME_REF)?;
        }
        path.complete(self, SyntaxKind::MOD_PATH)?;
        Ok(())
    }

    fn assoc_type_def(&mut self) -> Result<bool, ParserError> {
        let definition = self.start();
        self.expect(SyntaxKind::TYPE_KW)?;
        self.name_node(&[SyntaxKind::UIDENT], SyntaxKind::NAME)?;
        self.expect(SyntaxKind::EQ)?;
        let missing_type =
            self.at_physical_line_start() && self.item_starts_at_after_attributes(self.position);
        if missing_type {
            let range = self
                .current_token()
                .map(|token| token.range)
                .ok_or(ParserError::InvalidDiagnosticSpan)?;
            self.emit_parser_diagnostic(
                sai_diagnostics::Code::ExpectedToken,
                range,
                "expected a type before the next item",
            )?;
            self.start().complete(self, SyntaxKind::ERROR)?;
        } else if self.at_any(TYPE_START_KINDS) {
            self.type_(true)?;
        } else {
            self.expect_any(TYPE_START_KINDS)?;
        }
        definition.complete(self, SyntaxKind::ASSOC_TYPE_DEF)?;
        Ok(missing_type)
    }

    fn impl_item_list(&mut self, target_kind: ImplTargetKind) -> Result<(), ParserError> {
        if !self.at(SyntaxKind::L_BRACE) {
            self.expect(SyntaxKind::L_BRACE)?;
            return Ok(());
        }
        let items = self.start();
        self.bump();
        while self.at(SyntaxKind::NEWLINE) && !self.impl_item_list_boundary_after_newline() {
            self.eat_nl();
        }
        while !self.at_any(&[SyntaxKind::R_BRACE, SyntaxKind::EOF]) {
            if self.at(SyntaxKind::NEWLINE) && self.impl_item_list_boundary_after_newline() {
                break;
            }
            if self.at(SyntaxKind::DOC_INNER)
                || (self.at(SyntaxKind::DOC_OUTER)
                    && !self.impl_item_starts_at_after_attributes(self.position))
            {
                self.misplaced_doc_comment()?;
                continue;
            }
            let position = self.position;
            let missing_assoc_type = self.impl_item(target_kind)?;
            if missing_assoc_type
                && self.at_physical_line_start()
                && self.impl_item_starts_at_after_attributes(self.position)
            {
                continue;
            }
            if self.at(SyntaxKind::NEWLINE) && self.impl_item_list_boundary_after_newline() {
                break;
            }
            if self.eat(SyntaxKind::NEWLINE) {
                while self.at(SyntaxKind::NEWLINE) && !self.impl_item_list_boundary_after_newline()
                {
                    self.eat_nl();
                }
                continue;
            }
            if self.at_any(&[SyntaxKind::R_BRACE, SyntaxKind::EOF]) {
                break;
            }
            self.expect(SyntaxKind::NEWLINE)?;
            if self.position == position {
                self.recover_one()?;
                if self.position == position {
                    break;
                }
            }
        }
        if !self.impl_item_list_boundary_after_newline() {
            self.eat_nl();
        }
        self.expect(SyntaxKind::R_BRACE)?;
        items.complete(self, SyntaxKind::ITEM_LIST)?;
        Ok(())
    }

    fn impl_item_list_boundary_after_newline(&self) -> bool {
        if !self.at(SyntaxKind::NEWLINE) {
            return false;
        }
        let mut position = self.position;
        while self
            .tokens
            .get(position)
            .is_some_and(|token| token.kind == SyntaxKind::NEWLINE)
        {
            position += 1;
        }
        let Some(token) = self.tokens.get(position) else {
            return false;
        };
        if !token.flags.contains(crate::TokenFlags::AFTER_LINE_BREAK)
            || self.impl_item_starts_at_after_attributes(position)
        {
            return false;
        }
        self.statement_starts_at_after_attributes(position)
    }

    fn impl_item_starts_at_after_attributes(&self, position: usize) -> bool {
        let Some(mut position) = self.position_after_attributes(position) else {
            return false;
        };
        if self
            .tokens
            .get(position)
            .is_some_and(|token| matches!(token.kind, SyntaxKind::PUB_KW | SyntaxKind::EXPORT_KW))
        {
            position += 1;
        }
        match self.tokens.get(position).map(|token| token.kind) {
            Some(SyntaxKind::TYPE_KW) => {
                self.tokens
                    .get(position + 1)
                    .is_some_and(|token| token.kind == SyntaxKind::UIDENT)
                    && self
                        .tokens
                        .get(position + 2)
                        .is_some_and(|token| token.kind == SyntaxKind::EQ)
            }
            Some(SyntaxKind::FN_KW) => true,
            Some(SyntaxKind::ASYNC_KW) => self
                .tokens
                .get(position + 1)
                .is_some_and(|token| token.kind == SyntaxKind::FN_KW),
            Some(SyntaxKind::UNSAFE_KW) => {
                self.tokens
                    .get(position + 1)
                    .is_some_and(|token| token.kind == SyntaxKind::FN_KW)
                    || (self
                        .tokens
                        .get(position + 1)
                        .is_some_and(|token| token.kind == SyntaxKind::ASYNC_KW)
                        && self
                            .tokens
                            .get(position + 2)
                            .is_some_and(|token| token.kind == SyntaxKind::FN_KW))
            }
            _ => false,
        }
    }

    fn impl_item(&mut self, target_kind: ImplTargetKind) -> Result<bool, ParserError> {
        self.outer_docs()?;
        self.attributes(true)?;
        if self.at(SyntaxKind::PUB_KW) {
            let range = self
                .current_token()
                .map(|token| token.range)
                .ok_or(ParserError::InvalidDiagnosticSpan)?;
            let is_function = self.fn_decl_starts_at(self.position + 1);
            if is_function
                && matches!(
                    target_kind,
                    ImplTargetKind::Trait | ImplTargetKind::Interface
                )
            {
                self.emit_parser_diagnostic(
                    sai_diagnostics::Code::VisibilityOnTraitImplItem,
                    range,
                    "`pub` is not allowed on an item of a trait or interface impl",
                )?;
                let visibility = self.start();
                self.bump();
                visibility.complete(self, SyntaxKind::ERROR)?;
            } else if is_function && target_kind == ImplTargetKind::Inherent {
                let visibility = self.start();
                self.bump();
                visibility.complete(self, SyntaxKind::VISIBILITY)?;
            } else {
                self.emit_parser_diagnostic(
                    sai_diagnostics::Code::ExpectedToken,
                    range,
                    if matches!(
                        target_kind,
                        ImplTargetKind::Trait | ImplTargetKind::Interface
                    ) {
                        "`pub` cannot be applied to an associated type in a trait or interface impl"
                    } else {
                        "`pub` is allowed only on a function in an inherent impl"
                    },
                )?;
                let visibility = self.start();
                self.bump();
                visibility.complete(self, SyntaxKind::ERROR)?;
            }
        } else if self.at(SyntaxKind::EXPORT_KW) {
            let range = self
                .current_token()
                .map(|token| token.range)
                .ok_or(ParserError::InvalidDiagnosticSpan)?;
            self.emit_parser_diagnostic(
                sai_diagnostics::Code::ExpectedToken,
                range,
                "impl items cannot be exported",
            )?;
            let visibility = self.start();
            self.bump();
            visibility.complete(self, SyntaxKind::ERROR)?;
        }

        if self.at(SyntaxKind::TYPE_KW) {
            return self.assoc_type_def();
        }
        if self.at_fn_decl_start() {
            let function = self.start();
            self.fn_decl(function, FnBodyPolicy::Required)?;
            return Ok(false);
        }

        let range = self
            .current_token()
            .map(|token| token.range)
            .ok_or(ParserError::InvalidDiagnosticSpan)?;
        self.emit_parser_diagnostic(
            sai_diagnostics::Code::ExpectedToken,
            range,
            "expected an associated type definition or function in the impl",
        )?;
        let error = self.start();
        self.recover_one()?;
        error.complete(self, SyntaxKind::ERROR)?;
        Ok(false)
    }

    fn variant_list(
        &mut self,
        has_payload_variant: &mut bool,
        first_discriminant: &mut Option<sai_vfs::TextRange>,
    ) -> Result<bool, ParserError> {
        if !self.at(SyntaxKind::L_BRACE) {
            self.expect(SyntaxKind::L_BRACE)?;
            return Ok(false);
        }

        let variants = self.start();
        self.bump();
        while self.at(SyntaxKind::NEWLINE) && !self.variant_list_boundary_after_newline() {
            self.eat_nl();
        }
        while !self.at_any(&[SyntaxKind::R_BRACE, SyntaxKind::EOF]) {
            if self.at(SyntaxKind::NEWLINE) {
                if self.variant_list_boundary_after_newline() {
                    break;
                }
                self.eat_nl();
                continue;
            }
            if self.variant_list_boundary_at(self.position) {
                break;
            }
            if self.at_any(&[SyntaxKind::DOC_OUTER, SyntaxKind::DOC_INNER])
                && !self.variant_decl_starts_at(self.position)
            {
                self.misplaced_doc_comment()?;
                continue;
            }

            if !self.variant_decl_starts_at(self.position) {
                let error = self.start();
                if !self.eat(SyntaxKind::COMMA) {
                    self.recover_one()?;
                }
                error.complete(self, SyntaxKind::ERROR)?;
                continue;
            }

            let position = self.position;
            let (has_payload, discriminant) = self.variant_decl()?;
            *has_payload_variant |= has_payload;
            if first_discriminant.is_none() {
                *first_discriminant = discriminant;
            }
            if self.eat(SyntaxKind::COMMA) {
                if self.variant_list_boundary_after_newline() {
                    break;
                }
                while self.eat_nl() {}
                continue;
            }
            if self.at(SyntaxKind::NEWLINE) {
                if self.variant_list_boundary_after_newline() {
                    break;
                }
                while self.eat_nl() {}
                continue;
            }
            if self.at_any(&[SyntaxKind::R_BRACE, SyntaxKind::EOF]) {
                break;
            }
            self.expect_any(&[SyntaxKind::COMMA, SyntaxKind::NEWLINE])?;
            if self.position == position {
                self.recover_one()?;
                if self.position == position {
                    break;
                }
            }
        }
        self.expect(SyntaxKind::R_BRACE)?;
        variants.complete(self, SyntaxKind::VARIANT_LIST)?;
        Ok(true)
    }

    fn variant_decl(&mut self) -> Result<(bool, Option<sai_vfs::TextRange>), ParserError> {
        let variant = self.start();
        self.outer_docs()?;
        self.attributes(true)?;
        self.name_node(&[SyntaxKind::UIDENT], SyntaxKind::NAME)?;
        let has_payload = if self.at(SyntaxKind::L_PAREN) {
            self.variant_shape()?;
            true
        } else {
            false
        };

        let discriminant = if self.at(SyntaxKind::EQ) {
            let discriminant = self.start();
            let equals_range = self.current_token().map(|token| token.range);
            self.bump();
            self.eat(SyntaxKind::MINUS);
            let has_integer = self.at(SyntaxKind::INT);
            self.expect(SyntaxKind::INT)?;
            discriminant.complete(self, SyntaxKind::DISCRIMINANT)?;
            equals_range.filter(|_| has_integer)
        } else {
            None
        };
        variant.complete(self, SyntaxKind::VARIANT_DECL)?;
        Ok((has_payload, discriminant))
    }

    fn variant_shape(&mut self) -> Result<(), ParserError> {
        let is_record = self.variant_field_starts_at(self.position + 1);
        let shape = self.start();
        self.bump();
        if is_record {
            while !self.variant_field_list_boundary() {
                if self.at_any(&[SyntaxKind::DOC_OUTER, SyntaxKind::DOC_INNER])
                    && !self.variant_field_starts_at(self.position)
                {
                    self.misplaced_doc_comment()?;
                    continue;
                }
                if !self.variant_field_starts_at(self.position) {
                    let position = self.position;
                    let error = self.start();
                    if !self.eat(SyntaxKind::COMMA) {
                        self.recover_one()?;
                    }
                    error.complete(self, SyntaxKind::ERROR)?;
                    if self.position == position {
                        break;
                    }
                    continue;
                }
                let position = self.position;
                self.variant_field()?;
                if self.eat(SyntaxKind::COMMA) {
                    while self.eat_nl() {}
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
            shape.complete(self, SyntaxKind::RECORD_SHAPE)?;
        } else {
            if self.at(SyntaxKind::R_PAREN) {
                self.expect_any(TYPE_START_KINDS)?;
            } else {
                self.variant_type_list(SyntaxKind::R_PAREN)?;
            }
            self.expect(SyntaxKind::R_PAREN)?;
            shape.complete(self, SyntaxKind::TUPLE_SHAPE)?;
        }
        Ok(())
    }

    fn variant_field(&mut self) -> Result<(), ParserError> {
        let field = self.start();
        self.attributes(false)?;
        self.name_node(&[SyntaxKind::LIDENT], SyntaxKind::NAME)?;
        self.expect(SyntaxKind::COLON)?;
        self.type_(true)?;
        field.complete(self, SyntaxKind::VARIANT_FIELD)?;
        Ok(())
    }

    fn variant_field_starts_at(&self, position: usize) -> bool {
        self.variant_field_type_position(position).is_some()
    }

    fn variant_field_type_position(&self, position: usize) -> Option<usize> {
        let position = self.position_after_attributes(position)?;
        if !self
            .tokens
            .get(position)
            .is_some_and(|token| token.kind == SyntaxKind::LIDENT)
            || !self
                .tokens
                .get(position + 1)
                .is_some_and(|token| token.kind == SyntaxKind::COLON)
        {
            return None;
        }

        Some(position + 2)
    }

    fn variant_payload_type_starts_at(&self, mut position: usize) -> bool {
        let mut parenthesized_closes = Vec::new();
        let mut outer_parentheses = None;
        let mut after_type = None;
        loop {
            match self.tokens.get(position).map(|token| token.kind) {
                Some(SyntaxKind::BORROW_KW) => {
                    position += 1;
                    if self
                        .tokens
                        .get(position)
                        .is_some_and(|token| token.kind == SyntaxKind::MUT_KW)
                    {
                        position += 1;
                    }
                }
                Some(SyntaxKind::OWNED_KW) => position += 1,
                Some(SyntaxKind::L_PAREN) => {
                    let Some(close) = self
                        .matching_r_parens
                        .get(position)
                        .copied()
                        .filter(|close| *close != usize::MAX)
                    else {
                        return false;
                    };
                    outer_parentheses.get_or_insert((position, close));
                    if close == position + 1 {
                        after_type = Some(close + 1);
                        break;
                    }
                    parenthesized_closes.push(close);
                    position += 1;
                }
                _ => break,
            }
        }

        if after_type.is_none() {
            after_type = match self.tokens.get(position).map(|token| token.kind) {
                Some(SyntaxKind::LIDENT | SyntaxKind::UIDENT) => {
                    self.generic_argument_path_type_end_at(position)
                }
                Some(SyntaxKind::SELF_TYPE_KW) => self.variant_self_type_end_at(position),
                Some(SyntaxKind::FN_KW) => self.variant_function_pointer_type_end_at(position),
                Some(SyntaxKind::UNSAFE_KW)
                    if self
                        .tokens
                        .get(position + 1)
                        .is_some_and(|token| token.kind == SyntaxKind::FN_KW) =>
                {
                    self.variant_function_pointer_type_end_at(position)
                }
                Some(SyntaxKind::UNDERSCORE) => Some(position + 1),
                _ => None,
            };
        }
        let Some(mut after_type) = after_type else {
            return false;
        };

        if let Some((open, close)) = outer_parentheses {
            if !self.variant_parenthesized_contents_are_type_syntax(open, close) {
                return false;
            }
        }
        while let Some(close) = parenthesized_closes.pop() {
            if after_type == close
                || (after_type < close
                    && self
                        .tokens
                        .get(after_type)
                        .is_some_and(|token| token.kind == SyntaxKind::COMMA))
            {
                after_type = close + 1;
            } else {
                return false;
            }
        }
        self.tokens
            .get(after_type)
            .is_some_and(|next| matches!(next.kind, SyntaxKind::COMMA | SyntaxKind::R_PAREN))
    }

    fn variant_parenthesized_contents_are_type_syntax(&self, open: usize, close: usize) -> bool {
        let mut angle_depth = 0usize;
        let mut brace_depth = 0usize;
        let mut bracket_depth = 0usize;
        for position in open + 1..close {
            let Some(token) = self.tokens.get(position) else {
                return false;
            };
            let kind = token.kind;
            match kind {
                SyntaxKind::LT if brace_depth > 0 || bracket_depth > 0 => continue,
                SyntaxKind::LT
                    if self
                        .tokens
                        .get(position.saturating_sub(1))
                        .is_some_and(|previous| {
                            matches!(previous.kind, SyntaxKind::UIDENT | SyntaxKind::GT)
                        }) =>
                {
                    angle_depth += 1;
                    continue;
                }
                SyntaxKind::GT if angle_depth > 0 => {
                    angle_depth -= 1;
                    continue;
                }
                SyntaxKind::GT if brace_depth > 0 || bracket_depth > 0 => continue,
                SyntaxKind::LT | SyntaxKind::GT => return false,
                SyntaxKind::L_BRACE if angle_depth == 0 => {
                    brace_depth += 1;
                    continue;
                }
                SyntaxKind::R_BRACE if brace_depth > 0 => {
                    brace_depth -= 1;
                    continue;
                }
                SyntaxKind::L_BRACKET if angle_depth == 0 && brace_depth == 0 => {
                    bracket_depth += 1;
                    continue;
                }
                SyntaxKind::R_BRACKET if bracket_depth > 0 => {
                    bracket_depth -= 1;
                    continue;
                }
                _ => {}
            }
            if angle_depth == 0
                && brace_depth == 0
                && bracket_depth == 0
                && matches!(
                    kind,
                    SyntaxKind::EQ
                        | SyntaxKind::DOT2
                        | SyntaxKind::DOT2EQ
                        | SyntaxKind::QUESTION
                        | SyntaxKind::PLUS
                        | SyntaxKind::MINUS
                        | SyntaxKind::STAR
                        | SyntaxKind::SLASH
                        | SyntaxKind::PERCENT
                        | SyntaxKind::PLUS_PERCENT
                        | SyntaxKind::MINUS_PERCENT
                        | SyntaxKind::STAR_PERCENT
                        | SyntaxKind::AMP
                        | SyntaxKind::PIPE
                        | SyntaxKind::CARET
                        | SyntaxKind::SHL
                        | SyntaxKind::SHR
                        | SyntaxKind::LE
                        | SyntaxKind::EQ2
                        | SyntaxKind::NEQ
                        | SyntaxKind::GT
                        | SyntaxKind::GE
                        | SyntaxKind::AMP2
                        | SyntaxKind::PIPE2
                )
            {
                return false;
            }
        }
        angle_depth == 0 && brace_depth == 0 && bracket_depth == 0
    }

    fn variant_self_type_end_at(&self, mut position: usize) -> Option<usize> {
        position += 1;
        while self
            .tokens
            .get(position)
            .is_some_and(|token| token.kind == SyntaxKind::DOT)
        {
            if !self
                .tokens
                .get(position + 1)
                .is_some_and(|token| token.kind == SyntaxKind::UIDENT)
            {
                return None;
            }
            position += 2;
        }
        Some(position)
    }

    fn variant_function_pointer_type_end_at(&self, mut position: usize) -> Option<usize> {
        if self
            .tokens
            .get(position)
            .is_some_and(|token| token.kind == SyntaxKind::UNSAFE_KW)
        {
            position += 1;
        }
        if self
            .tokens
            .get(position)
            .is_none_or(|token| token.kind != SyntaxKind::FN_KW)
            || self
                .tokens
                .get(position + 1)
                .is_none_or(|token| token.kind != SyntaxKind::L_PAREN)
            || self.closure_body_follows_fn_type(position)
        {
            return None;
        }
        let parameters_open = position + 1;
        let parameters_close = self
            .matching_r_parens
            .get(parameters_open)
            .copied()
            .filter(|close| *close != usize::MAX)?;
        let mut end = parameters_close + 1;
        if self
            .tokens
            .get(end)
            .is_some_and(|token| token.kind == SyntaxKind::NEWLINE)
            && self
                .tokens
                .get(end + 1)
                .is_some_and(|token| token.kind == SyntaxKind::THIN_ARROW)
        {
            end += 1;
        }
        if self
            .tokens
            .get(end)
            .is_some_and(|token| token.kind == SyntaxKind::THIN_ARROW)
        {
            end = self.variant_function_return_type_end_at(end + 1)?;
        }
        if self
            .tokens
            .get(end)
            .is_some_and(|token| token.kind == SyntaxKind::LIDENT)
            && token_is_contextual(self.source, self.tokens, end, "effects")
            && self
                .tokens
                .get(end + 1)
                .is_some_and(|token| token.kind == SyntaxKind::L_BRACE)
        {
            end = self.matching_brace_after(end + 1)?;
        }
        Some(end)
    }

    fn variant_function_return_type_end_at(&self, position: usize) -> Option<usize> {
        if !self
            .tokens
            .get(position)
            .is_some_and(|token| TYPE_START_KINDS.contains(&token.kind))
        {
            return None;
        }
        let mut paren_depth = 0usize;
        let mut bracket_depth = 0usize;
        let mut brace_depth = 0usize;
        let mut angle_depth = 0usize;
        for (position, token) in self.tokens.iter().enumerate().skip(position) {
            if paren_depth == 0 && bracket_depth == 0 && brace_depth == 0 && angle_depth == 0 {
                if matches!(token.kind, SyntaxKind::COMMA | SyntaxKind::R_PAREN) {
                    return Some(position);
                }
                if token.kind == SyntaxKind::LIDENT
                    && token_is_contextual(self.source, self.tokens, position, "effects")
                    && self
                        .tokens
                        .get(position + 1)
                        .is_some_and(|next| next.kind == SyntaxKind::L_BRACE)
                {
                    return Some(position);
                }
                if matches!(
                    token.kind,
                    SyntaxKind::EQ
                        | SyntaxKind::DOT2
                        | SyntaxKind::DOT2EQ
                        | SyntaxKind::QUESTION
                        | SyntaxKind::PLUS
                        | SyntaxKind::MINUS
                        | SyntaxKind::STAR
                        | SyntaxKind::SLASH
                        | SyntaxKind::PERCENT
                        | SyntaxKind::PLUS_PERCENT
                        | SyntaxKind::MINUS_PERCENT
                        | SyntaxKind::STAR_PERCENT
                        | SyntaxKind::AMP
                        | SyntaxKind::PIPE
                        | SyntaxKind::CARET
                        | SyntaxKind::SHL
                        | SyntaxKind::SHR
                        | SyntaxKind::LE
                        | SyntaxKind::EQ2
                        | SyntaxKind::NEQ
                        | SyntaxKind::GE
                        | SyntaxKind::AMP2
                        | SyntaxKind::PIPE2
                ) {
                    return None;
                }
            }
            match token.kind {
                SyntaxKind::L_PAREN => paren_depth += 1,
                SyntaxKind::R_PAREN if paren_depth > 0 => paren_depth -= 1,
                SyntaxKind::L_BRACKET => bracket_depth += 1,
                SyntaxKind::R_BRACKET if bracket_depth > 0 => bracket_depth -= 1,
                SyntaxKind::L_BRACE => brace_depth += 1,
                SyntaxKind::R_BRACE if brace_depth > 0 => brace_depth -= 1,
                SyntaxKind::LT if brace_depth > 0 || bracket_depth > 0 => {}
                SyntaxKind::LT
                    if self
                        .tokens
                        .get(position.saturating_sub(1))
                        .is_some_and(|previous| {
                            matches!(previous.kind, SyntaxKind::UIDENT | SyntaxKind::GT)
                        }) =>
                {
                    angle_depth += 1;
                }
                SyntaxKind::LT => return None,
                SyntaxKind::GT if angle_depth > 0 => angle_depth -= 1,
                SyntaxKind::GT if brace_depth > 0 || bracket_depth > 0 => {}
                SyntaxKind::GT => return None,
                SyntaxKind::EOF => return None,
                _ => {}
            }
        }
        None
    }

    fn matching_brace_after(&self, open: usize) -> Option<usize> {
        if self.tokens.get(open)?.kind != SyntaxKind::L_BRACE {
            return None;
        }
        self.matching_r_braces
            .get(open)
            .copied()
            .filter(|close| *close != usize::MAX)
            .map(|close| close + 1)
    }

    fn variant_field_list_boundary(&self) -> bool {
        if self.at_any(&[SyntaxKind::R_PAREN, SyntaxKind::R_BRACE, SyntaxKind::EOF]) {
            return true;
        }
        if !self.at_physical_line_start() || self.variant_field_starts_at(self.position) {
            return false;
        }

        let next_kind = self
            .position_after_attributes(self.position)
            .and_then(|position| self.tokens.get(position))
            .map(|token| token.kind);
        matches!(
            next_kind,
            Some(SyntaxKind::LET_KW | SyntaxKind::VAR_KW | SyntaxKind::ASSERT_KW)
        ) || self.item_starts_at_after_attributes(self.position)
            || next_kind.is_some_and(|kind| EXPRESSION_START_KINDS.contains(&kind))
    }

    fn variant_decl_starts_at(&self, position: usize) -> bool {
        let Some(position) = self.position_after_attributes(position) else {
            return false;
        };
        if !self
            .tokens
            .get(position)
            .is_some_and(|token| token.kind == SyntaxKind::UIDENT)
        {
            return false;
        }
        if !self
            .tokens
            .get(position + 1)
            .is_some_and(|token| token.kind == SyntaxKind::L_PAREN)
        {
            return true;
        }

        true
    }

    fn variant_decl_starts_like_expression_at(&self, position: usize) -> bool {
        let Some(position) = self.position_after_attributes(position) else {
            return false;
        };
        if !self
            .tokens
            .get(position)
            .is_some_and(|token| token.kind == SyntaxKind::UIDENT)
            || !self
                .tokens
                .get(position + 1)
                .is_some_and(|token| token.kind == SyntaxKind::L_PAREN)
        {
            return false;
        }

        let shape_start = position + 2;
        if let Some(value_start) = self.variant_field_type_position(shape_start) {
            return self
                .tokens
                .get(value_start)
                .is_some_and(|token| EXPRESSION_START_KINDS.contains(&token.kind))
                && !self.variant_payload_type_starts_at(value_start);
        }

        self.tokens.get(shape_start).is_some_and(|token| {
            token.kind == SyntaxKind::R_PAREN
                || (EXPRESSION_START_KINDS.contains(&token.kind)
                    && !self.variant_payload_type_starts_at(shape_start))
        })
    }

    fn variant_call_followed_by_statement(&self, position: usize) -> bool {
        let mut call_position = position;
        let mut scanned_positions = Vec::new();
        let result = loop {
            if let Some(cached) = self
                .variant_call_boundary_cache
                .borrow()
                .get(call_position)
                .copied()
                .flatten()
            {
                break cached;
            }
            scanned_positions.push(call_position);
            let Some(position) = self.position_after_attributes(call_position) else {
                break false;
            };
            if !self
                .tokens
                .get(position)
                .is_some_and(|token| token.kind == SyntaxKind::UIDENT)
                || !self
                    .tokens
                    .get(position + 1)
                    .is_some_and(|token| token.kind == SyntaxKind::L_PAREN)
            {
                break false;
            }

            let Some(mut next_position) = self
                .matching_r_parens
                .get(position + 1)
                .copied()
                .filter(|close| *close != usize::MAX)
                .map(|close| close + 1)
            else {
                break false;
            };
            while self.tokens.get(next_position).is_some_and(|token| {
                matches!(
                    token.kind,
                    SyntaxKind::NEWLINE | SyntaxKind::DOC_OUTER | SyntaxKind::DOC_INNER
                )
            }) {
                next_position += 1;
            }
            if !self
                .tokens
                .get(next_position)
                .is_some_and(|token| token.flags.contains(crate::TokenFlags::AFTER_LINE_BREAK))
            {
                break false;
            }
            if self.variant_decl_starts_at(next_position) {
                if self.variant_decl_starts_like_expression_at(next_position) {
                    call_position = next_position;
                    continue;
                }
                break false;
            }

            let Some(next_position) = self.position_after_attributes(next_position) else {
                break false;
            };
            let next_kind = self.tokens.get(next_position).map(|token| token.kind);
            break matches!(
                next_kind,
                Some(SyntaxKind::LET_KW | SyntaxKind::VAR_KW | SyntaxKind::ASSERT_KW)
            ) || self.item_starts_at(next_position)
                || next_kind.is_some_and(|kind| EXPRESSION_START_KINDS.contains(&kind));
        };
        let mut cache = self.variant_call_boundary_cache.borrow_mut();
        for position in scanned_positions {
            if let Some(value) = cache.get_mut(position) {
                value.get_or_insert(result);
            }
        }
        result
    }

    fn variant_list_boundary_after_newline(&self) -> bool {
        if !self.at(SyntaxKind::NEWLINE) {
            return false;
        }
        let mut position = self.position;
        while self
            .tokens
            .get(position)
            .is_some_and(|token| token.kind == SyntaxKind::NEWLINE)
        {
            position += 1;
        }
        self.variant_list_boundary_at(position)
    }

    fn variant_list_boundary_at(&self, position: usize) -> bool {
        if !self
            .tokens
            .get(position)
            .is_some_and(|token| token.flags.contains(crate::TokenFlags::AFTER_LINE_BREAK))
        {
            return false;
        }
        if self.variant_decl_starts_at(position)
            && !(self.variant_decl_starts_like_expression_at(position)
                && self.variant_call_followed_by_statement(position))
        {
            return false;
        }

        let next_kind = self
            .position_after_attributes(position)
            .and_then(|position| self.tokens.get(position))
            .map(|token| token.kind);
        matches!(
            next_kind,
            Some(SyntaxKind::LET_KW | SyntaxKind::VAR_KW | SyntaxKind::ASSERT_KW)
        ) || self.item_starts_at_after_attributes(position)
            || next_kind.is_some_and(|kind| EXPRESSION_START_KINDS.contains(&kind))
    }

    fn variant_type_list_recovery_boundary(&self) -> bool {
        if self.at_any(&[SyntaxKind::R_BRACE, SyntaxKind::EOF]) {
            return true;
        }
        if !self.at_physical_line_start()
            || !self.statement_starts_at_after_attributes(self.position)
        {
            return false;
        }

        let Some(position) = self.position_after_attributes(self.position) else {
            return false;
        };
        let Some(token) = self.tokens.get(position) else {
            return false;
        };
        if !TYPE_START_KINDS.contains(&token.kind) {
            return true;
        }
        !self.variant_payload_type_starts_at(position)
    }

    fn struct_def(&mut self) -> Result<(), ParserError> {
        let definition = self.start();
        if self.at(SyntaxKind::NEWLINE) && self.nth(1) == SyntaxKind::WHERE_KW {
            self.eat_nl();
        }
        if self.at(SyntaxKind::WHERE_KW) {
            self.where_clause()?;
        }
        if self.at(SyntaxKind::NEWLINE) && self.nth(1) == SyntaxKind::L_BRACE {
            self.eat_nl();
        }
        if !self.field_list()? {
            definition.abandon(self)?;
            return Ok(());
        }
        definition.complete(self, SyntaxKind::STRUCT_DEF)?;
        Ok(())
    }

    fn field_list(&mut self) -> Result<bool, ParserError> {
        if !self.at(SyntaxKind::L_BRACE) {
            self.expect(SyntaxKind::L_BRACE)?;
            return Ok(false);
        }

        let fields = self.start();
        self.bump();
        while self.at(SyntaxKind::NEWLINE) && !self.field_list_boundary_after_newline() {
            self.eat_nl();
        }
        while !self.at_any(&[SyntaxKind::R_BRACE, SyntaxKind::EOF]) {
            if self.at(SyntaxKind::NEWLINE) {
                if self.field_list_boundary_after_newline() {
                    break;
                }
                self.eat_nl();
                continue;
            }
            if self.field_list_boundary_at(self.position) {
                break;
            }
            if self.at_any(&[SyntaxKind::DOC_OUTER, SyntaxKind::DOC_INNER])
                && !self.field_decl_starts_at(self.position)
            {
                self.misplaced_doc_comment()?;
                continue;
            }

            if !self.field_decl_starts_at(self.position) {
                let error = self.start();
                if !self.eat(SyntaxKind::COMMA) {
                    self.recover_one()?;
                }
                error.complete(self, SyntaxKind::ERROR)?;
                continue;
            }

            let position = self.position;
            self.field_decl()?;
            if self.eat(SyntaxKind::COMMA) {
                if self.field_list_boundary_after_newline() {
                    break;
                }
                while self.eat_nl() {}
                continue;
            }
            if self.at(SyntaxKind::NEWLINE) {
                if self.field_list_boundary_after_newline() {
                    break;
                }
                while self.eat_nl() {}
                continue;
            }
            if self.at_any(&[SyntaxKind::R_BRACE, SyntaxKind::EOF]) {
                break;
            }
            self.expect_any(&[SyntaxKind::COMMA, SyntaxKind::NEWLINE])?;
            if self.position == position {
                self.recover_one()?;
                if self.position == position {
                    break;
                }
            }
        }
        self.expect(SyntaxKind::R_BRACE)?;
        fields.complete(self, SyntaxKind::FIELD_LIST)?;
        Ok(true)
    }

    fn field_decl(&mut self) -> Result<(), ParserError> {
        let field = self.start();
        self.outer_docs()?;
        self.attributes(true)?;
        self.eat(SyntaxKind::PUB_KW);
        self.name_node(&[SyntaxKind::LIDENT], SyntaxKind::NAME)?;
        self.expect(SyntaxKind::COLON)?;
        self.type_(true)?;
        field.complete(self, SyntaxKind::FIELD_DECL)?;
        Ok(())
    }

    fn field_decl_starts_at(&self, position: usize) -> bool {
        let Some(mut position) = self.position_after_attributes(position) else {
            return false;
        };
        if self
            .tokens
            .get(position)
            .is_some_and(|token| token.kind == SyntaxKind::PUB_KW)
        {
            position += 1;
        }
        self.tokens
            .get(position)
            .is_some_and(|token| token.kind == SyntaxKind::LIDENT)
            && self
                .tokens
                .get(position + 1)
                .is_some_and(|token| token.kind == SyntaxKind::COLON)
    }

    fn field_list_boundary_after_newline(&self) -> bool {
        if !self.at(SyntaxKind::NEWLINE) {
            return false;
        }
        let mut position = self.position;
        while self
            .tokens
            .get(position)
            .is_some_and(|token| token.kind == SyntaxKind::NEWLINE)
        {
            position += 1;
        }
        self.field_list_boundary_at(position)
    }

    fn field_list_boundary_at(&self, position: usize) -> bool {
        if !self
            .tokens
            .get(position)
            .is_some_and(|token| token.flags.contains(crate::TokenFlags::AFTER_LINE_BREAK))
            || self.field_decl_starts_at(position)
        {
            return false;
        }

        let next_kind = self
            .position_after_attributes(position)
            .and_then(|position| self.tokens.get(position))
            .map(|token| token.kind);
        matches!(
            next_kind,
            Some(SyntaxKind::LET_KW | SyntaxKind::VAR_KW | SyntaxKind::ASSERT_KW)
        ) || self.item_starts_at_after_attributes(position)
            || next_kind.is_some_and(|kind| EXPRESSION_START_KINDS.contains(&kind))
    }

    fn alias_def(&mut self) -> Result<(), ParserError> {
        let alias = self.start();
        self.expect(SyntaxKind::EQ)?;
        self.eat_nl();
        if self.item_starts_at_after_attributes(self.position) {
            let range = self
                .current_token()
                .map(|token| token.range)
                .ok_or(ParserError::InvalidDiagnosticSpan)?;
            self.emit_parser_diagnostic(
                sai_diagnostics::Code::ExpectedToken,
                range,
                "expected a type before the next item",
            )?;
            self.start().complete(self, SyntaxKind::ERROR)?;
        } else {
            self.type_(true)?;
        }
        if self.at(SyntaxKind::NEWLINE) && self.nth(1) == SyntaxKind::WHERE_KW {
            self.eat_nl();
        }
        if self.at(SyntaxKind::WHERE_KW) {
            let refinement = self.start();
            self.bump();
            self.expr_bp(0)?;
            refinement.complete(self, SyntaxKind::REFINEMENT_CLAUSE)?;
        }
        alias.complete(self, SyntaxKind::ALIAS_DEF)?;
        Ok(())
    }

    fn fn_decl(
        &mut self,
        declaration: Marker,
        body_policy: FnBodyPolicy,
    ) -> Result<CompletedMarker, ParserError> {
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
            if body_policy == FnBodyPolicy::Forbidden {
                let range = self
                    .current_token()
                    .map(|token| token.range)
                    .ok_or(ParserError::InvalidDiagnosticSpan)?;
                self.emit_parser_diagnostic(
                    sai_diagnostics::Code::InterfaceFnBody,
                    range,
                    "interface and world functions cannot have a body",
                )?;
            }
            let body = match self.block() {
                Ok(body) => body,
                Err(error) => {
                    declaration.abandon(self)?;
                    return Err(error);
                }
            };
            if body_policy == FnBodyPolicy::Forbidden {
                body.precede(self).complete(self, SyntaxKind::ERROR)?;
            }
        } else if body_policy == FnBodyPolicy::Required {
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
                FnClauseKind::Requires => self.contract_clause(SyntaxKind::REQUIRES_CLAUSE)?,
                FnClauseKind::Ensures => self.contract_clause(SyntaxKind::ENSURES_CLAUSE)?,
            }
        }
        Ok(())
    }

    fn contract_clause(&mut self, kind: SyntaxKind) -> Result<(), ParserError> {
        let clause = self.start();
        self.bump();
        self.expr_bp(0)?;
        clause.complete(self, kind)?;
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
        let diagnostic = self.parser_diagnostic(code, range, message)?;
        self.emit_error(diagnostic);
        Ok(())
    }

    fn parser_diagnostic(
        &self,
        code: sai_diagnostics::Code,
        range: sai_vfs::TextRange,
        message: &str,
    ) -> Result<sai_diagnostics::Diagnostic, ParserError> {
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
        Ok(diagnostic)
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
            if self.at(SyntaxKind::NEWLINE) || self.generic_parameter_list_recovery_boundary() {
                break;
            }
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

        if self.at(SyntaxKind::NEWLINE) && self.nth(1) == SyntaxKind::GT {
            self.eat_nl();
        }
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
            if self.effect_set_item_boundary() {
                break;
            }
            if self.at(SyntaxKind::NEWLINE) {
                if self.effect_set_boundary_after_newline() {
                    break;
                }
                self.eat_nl();
                continue;
            }
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
            if self.effect_set_item_boundary() {
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
        if !self.effect_set_boundary_after_newline() {
            self.eat_nl();
        }
        self.expect(SyntaxKind::R_BRACE)?;
        set.complete(self, SyntaxKind::EFFECT_SET)?;
        Ok(())
    }

    fn effect_set_item_boundary(&self) -> bool {
        self.at_physical_line_start() && self.item_starts_at_after_attributes(self.position)
    }

    fn effect_set_boundary_after_newline(&self) -> bool {
        if !self.at(SyntaxKind::NEWLINE) {
            return false;
        }
        let mut position = self.position;
        while self
            .tokens
            .get(position)
            .is_some_and(|token| token.kind == SyntaxKind::NEWLINE)
        {
            position += 1;
        }
        self.tokens
            .get(position)
            .is_some_and(|token| token.flags.contains(crate::TokenFlags::AFTER_LINE_BREAK))
            && self.item_starts_at_after_attributes(position)
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
                let statement_starts_item = self.statement_starts_item();
                if self.recovered_match_tail && self.line_starts_top_level_item() {
                    self.expect_close_brace()?;
                    return Ok(false);
                }
                if self.at_any(&[SyntaxKind::DOC_OUTER, SyntaxKind::DOC_INNER]) {
                    self.misplaced_doc_comment()?;
                    continue;
                }
                let statement_ends_with_expression = self.statement()?;
                let recovered_item_stops_before_statement = statement_starts_item
                    && self.at_physical_line_start()
                    && self.statement_starts_at_after_attributes(self.position);
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
                if self.position != position
                    && (self.at_confirmed_block_boundary() || recovered_item_stops_before_statement)
                {
                    continue;
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
                self.expect_close_brace()?;
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

    fn expect_close_brace(&mut self) -> Result<bool, ParserError> {
        let already_reported_here = self.reported_expected_at == Some(self.position);
        let present = self.expect(SyntaxKind::R_BRACE)?;
        if !present && already_reported_here {
            self.emit_missing_error_node()?;
        }
        Ok(present)
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
        if self.statement_starts_item() {
            self.item_in_block()?;
            return Ok(false);
        }
        if self.at(SyntaxKind::AT) {
            let item_position = self.position;
            let attributes = self.start();
            let range = self
                .current_token()
                .map(|token| token.range)
                .ok_or(ParserError::InvalidDiagnosticSpan)?;
            if let Err(error) = self.attributes(true) {
                attributes.abandon(self)?;
                return Err(error);
            }
            if self.statement_starts_item() {
                self.emit_item_in_block_diagnostic(range, item_position)?;
                self.parse_item_in_block(attributes)?;
                return Ok(false);
            }
            self.emit_parser_diagnostic(
                sai_diagnostics::Code::InvalidAttributeTarget,
                range,
                "attribute is not allowed on this declaration",
            )?;
            attributes.complete(self, SyntaxKind::ERROR)?;
        }
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

    fn statement_starts_item(&self) -> bool {
        self.item_starts_at_after_attributes(self.position)
    }

    fn item_starts_at_after_attributes(&self, position: usize) -> bool {
        self.position_after_attributes(position)
            .is_some_and(|position| self.item_starts_at(position))
    }

    fn position_after_attributes(&self, mut position: usize) -> Option<usize> {
        loop {
            while self.tokens.get(position).is_some_and(|token| {
                matches!(token.kind, SyntaxKind::DOC_OUTER | SyntaxKind::DOC_INNER)
            }) {
                position += 1;
            }
            if self
                .tokens
                .get(position)
                .is_some_and(|token| token.kind == SyntaxKind::AT)
            {
                position = self.skip_attribute_at(position)?;
                while self.tokens.get(position).is_some_and(|token| {
                    matches!(
                        token.kind,
                        SyntaxKind::NEWLINE | SyntaxKind::DOC_OUTER | SyntaxKind::DOC_INNER
                    )
                }) {
                    position += 1;
                }
                continue;
            }
            break;
        }
        Some(position)
    }

    fn statement_starts_at_after_attributes(&self, position: usize) -> bool {
        self.position_after_attributes(position)
            .is_some_and(|position| {
                let kind = self.tokens.get(position).map(|token| token.kind);
                matches!(
                    kind,
                    Some(SyntaxKind::LET_KW | SyntaxKind::VAR_KW | SyntaxKind::ASSERT_KW)
                ) || self.item_starts_at(position)
                    || kind.is_some_and(|kind| EXPRESSION_START_KINDS.contains(&kind))
            })
    }

    fn generic_parameter_starts_at_after_attributes(&self, mut position: usize) -> bool {
        loop {
            while self.tokens.get(position).is_some_and(|token| {
                matches!(token.kind, SyntaxKind::DOC_OUTER | SyntaxKind::DOC_INNER)
            }) {
                position += 1;
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
                while self.tokens.get(position).is_some_and(|token| {
                    matches!(
                        token.kind,
                        SyntaxKind::NEWLINE | SyntaxKind::DOC_OUTER | SyntaxKind::DOC_INNER
                    )
                }) {
                    position += 1;
                }
                continue;
            }
            break;
        }
        match self.tokens.get(position).map(|token| token.kind) {
            Some(SyntaxKind::UIDENT) => self.tokens.get(position + 1).is_some_and(|token| {
                matches!(
                    token.kind,
                    SyntaxKind::COLON | SyntaxKind::COMMA | SyntaxKind::GT
                ) || (token.kind == SyntaxKind::NEWLINE
                    && self
                        .tokens
                        .get(position + 2)
                        .is_some_and(|next| next.kind == SyntaxKind::GT))
            }),
            Some(SyntaxKind::CONST_KW) => {
                self.tokens
                    .get(position + 1)
                    .is_some_and(|token| token.kind == SyntaxKind::UIDENT)
                    && self
                        .tokens
                        .get(position + 2)
                        .is_some_and(|token| token.kind == SyntaxKind::COLON)
            }
            _ => false,
        }
    }

    fn generic_parameter_list_recovery_boundary(&self) -> bool {
        if !self.at_physical_line_start()
            || !self.statement_starts_at_after_attributes(self.position)
        {
            return false;
        }
        if !self.generic_parameter_starts_at_after_attributes(self.position) {
            return true;
        }

        let Some(position) = self.position_after_attributes(self.position) else {
            return false;
        };
        if self
            .tokens
            .get(position)
            .is_none_or(|token| token.kind != SyntaxKind::CONST_KW)
            || self
                .tokens
                .get(position + 1)
                .is_none_or(|token| token.kind != SyntaxKind::UIDENT)
            || self
                .tokens
                .get(position + 2)
                .is_none_or(|token| token.kind != SyntaxKind::COLON)
        {
            return false;
        }

        let mut position = position + 3;
        let mut paren_depth = 0usize;
        let mut bracket_depth = 0usize;
        let mut brace_depth = 0usize;
        let mut angle_depth = 0usize;
        while let Some(token) = self.tokens.get(position) {
            match token.kind {
                SyntaxKind::EQ
                    if paren_depth == 0
                        && bracket_depth == 0
                        && brace_depth == 0
                        && angle_depth == 0 =>
                {
                    return true;
                }
                SyntaxKind::COMMA | SyntaxKind::GT | SyntaxKind::NEWLINE | SyntaxKind::EOF
                    if paren_depth == 0
                        && bracket_depth == 0
                        && brace_depth == 0
                        && angle_depth == 0 =>
                {
                    return false;
                }
                SyntaxKind::L_PAREN => paren_depth += 1,
                SyntaxKind::R_PAREN if paren_depth > 0 => paren_depth -= 1,
                SyntaxKind::L_BRACKET => bracket_depth += 1,
                SyntaxKind::R_BRACKET if bracket_depth > 0 => bracket_depth -= 1,
                SyntaxKind::L_BRACE => brace_depth += 1,
                SyntaxKind::R_BRACE if brace_depth > 0 => brace_depth -= 1,
                SyntaxKind::LT => angle_depth += 1,
                SyntaxKind::GT if angle_depth > 0 => angle_depth -= 1,
                _ => {}
            }
            position += 1;
        }
        false
    }

    fn unsupported_item_line_starts_statement(&self) -> bool {
        let next_position = self.position + 1;
        let next_kind = self.tokens.get(next_position).map(|token| token.kind);
        matches!(
            next_kind,
            Some(
                SyntaxKind::LET_KW
                    | SyntaxKind::VAR_KW
                    | SyntaxKind::ASSERT_KW
                    | SyntaxKind::R_BRACE
                    | SyntaxKind::IF_KW
                    | SyntaxKind::MATCH_KW
                    | SyntaxKind::LOOP_KW
                    | SyntaxKind::WHILE_KW
                    | SyntaxKind::FOR_KW
                    | SyntaxKind::RETURN_KW
                    | SyntaxKind::BREAK_KW
                    | SyntaxKind::CONTINUE_KW
            )
        ) || self.item_starts_at_after_attributes(next_position)
            || (next_kind == Some(SyntaxKind::AT)
                && self.statement_starts_at_after_attributes(next_position))
            || (next_kind == Some(SyntaxKind::LIDENT)
                && self
                    .tokens
                    .get(next_position + 1)
                    .is_some_and(|token| token.kind == SyntaxKind::L_PAREN))
    }

    fn item_starts_at(&self, position: usize) -> bool {
        let kind_at = |offset: usize| {
            position
                .checked_add(offset)
                .and_then(|index| self.tokens.get(index))
                .map(|token| token.kind)
        };
        let is_function_name = |offset: usize| kind_at(offset) == Some(SyntaxKind::LIDENT);

        match kind_at(0) {
            Some(
                SyntaxKind::USE_KW
                | SyntaxKind::PUB_KW
                | SyntaxKind::EXPORT_KW
                | SyntaxKind::TYPE_KW
                | SyntaxKind::ENUM_KW
                | SyntaxKind::TRAIT_KW
                | SyntaxKind::IMPL_KW
                | SyntaxKind::EXTERN_KW,
            ) => true,
            Some(SyntaxKind::CONST_KW) => kind_at(1) == Some(SyntaxKind::UIDENT),
            Some(SyntaxKind::FN_KW) => is_function_name(1),
            Some(SyntaxKind::ASYNC_KW) => {
                kind_at(1) == Some(SyntaxKind::FN_KW) && is_function_name(2)
            }
            Some(SyntaxKind::UNSAFE_KW) => {
                matches!(
                    kind_at(1),
                    Some(SyntaxKind::TRAIT_KW | SyntaxKind::IMPL_KW | SyntaxKind::EXTERN_KW)
                ) || (kind_at(1) == Some(SyntaxKind::FN_KW) && is_function_name(2))
                    || (kind_at(1) == Some(SyntaxKind::ASYNC_KW)
                        && kind_at(2) == Some(SyntaxKind::FN_KW)
                        && is_function_name(3))
            }
            Some(SyntaxKind::LIDENT)
                if token_is_contextual(self.source, self.tokens, position, "distinct")
                    || token_is_contextual(self.source, self.tokens, position, "resource")
                    || token_is_contextual(self.source, self.tokens, position, "invariant") =>
            {
                kind_at(1) == Some(SyntaxKind::UIDENT)
            }
            Some(SyntaxKind::LIDENT)
                if token_is_contextual(self.source, self.tokens, position, "effect")
                    || token_is_contextual(self.source, self.tokens, position, "interface")
                    || token_is_contextual(self.source, self.tokens, position, "world") =>
            {
                kind_at(1) == Some(SyntaxKind::LIDENT)
            }
            Some(SyntaxKind::LIDENT)
                if token_is_contextual(self.source, self.tokens, position, "test")
                    || token_is_contextual(self.source, self.tokens, position, "property") =>
            {
                kind_at(1) == Some(SyntaxKind::STRING)
            }
            _ => false,
        }
    }

    fn item_in_block(&mut self) -> Result<(), ParserError> {
        let item = self.start();
        let item_position = self.position;
        let range = self
            .current_token()
            .map(|token| token.range)
            .ok_or(ParserError::InvalidDiagnosticSpan)?;
        self.emit_item_in_block_diagnostic(range, item_position)?;

        self.parse_item_in_block(item)
    }

    fn emit_item_in_block_diagnostic(
        &mut self,
        range: sai_vfs::TextRange,
        item_position: usize,
    ) -> Result<(), ParserError> {
        const MESSAGE: &str = "item declarations are not allowed inside blocks";
        let mut diagnostic =
            self.parser_diagnostic(sai_diagnostics::Code::ItemInBlock, range, MESSAGE)?;
        if let Some((name, is_generic, is_unsafe, is_async, is_public, is_exported)) =
            self.block_function_name(item_position)
        {
            let message = if is_generic && !is_unsafe && !is_async && !is_public && !is_exported {
                format!(
                    "generic function `{name}` cannot be expressed as a closure; move it outside this block"
                )
            } else if is_generic || is_unsafe || is_async || is_public || is_exported {
                let mut constraints = Vec::new();
                if is_generic {
                    constraints.push("generic parameters");
                }
                if is_unsafe {
                    constraints.push("unsafe semantics");
                }
                if is_async {
                    constraints.push("async semantics");
                }
                if is_public {
                    constraints.push("public visibility");
                }
                if is_exported {
                    constraints.push("exported status");
                }
                format!(
                    "a local closure cannot preserve the {} of function `{name}`; move it outside this block",
                    constraints.join(", ")
                )
            } else {
                format!(
                    "replace this item with a closure expression, for example `let {name} = fn(..) {{ .. }}`"
                )
            };
            diagnostic
                .push_child(sai_diagnostics::Child {
                    kind: sai_diagnostics::ChildKind::Help,
                    message,
                    location: None,
                })
                .map_err(|_| ParserError::InvalidDiagnosticSpan)?;
        }
        self.emit_error(diagnostic);
        Ok(())
    }

    fn block_function_name(
        &self,
        mut position: usize,
    ) -> Option<(String, bool, bool, bool, bool, bool)> {
        loop {
            while self.tokens.get(position).is_some_and(|token| {
                matches!(token.kind, SyntaxKind::DOC_OUTER | SyntaxKind::DOC_INNER)
            }) {
                position += 1;
            }
            if self
                .tokens
                .get(position)
                .is_some_and(|token| token.kind == SyntaxKind::AT)
            {
                position = self.skip_attribute_at(position)?;
                while self.tokens.get(position).is_some_and(|token| {
                    matches!(
                        token.kind,
                        SyntaxKind::NEWLINE | SyntaxKind::DOC_OUTER | SyntaxKind::DOC_INNER
                    )
                }) {
                    position += 1;
                }
                continue;
            }
            break;
        }
        let mut is_public = false;
        let mut is_exported = false;
        while let Some(token) = self
            .tokens
            .get(position)
            .filter(|token| matches!(token.kind, SyntaxKind::PUB_KW | SyntaxKind::EXPORT_KW))
        {
            is_public |= token.kind == SyntaxKind::PUB_KW;
            is_exported |= token.kind == SyntaxKind::EXPORT_KW;
            position += 1;
        }
        let is_unsafe = self
            .tokens
            .get(position)
            .is_some_and(|token| token.kind == SyntaxKind::UNSAFE_KW);
        if is_unsafe {
            position += 1;
        }
        let is_async = self
            .tokens
            .get(position)
            .is_some_and(|token| token.kind == SyntaxKind::ASYNC_KW);
        if is_async {
            position += 1;
        }
        if self.tokens.get(position)?.kind != SyntaxKind::FN_KW {
            return None;
        }
        let name = self.tokens.get(position + 1)?;
        if name.kind != SyntaxKind::LIDENT {
            return None;
        }
        let name = self
            .source
            .get(name.range.start().to_usize()..name.range.end().to_usize())
            .map(str::to_owned)?;
        let is_generic = self
            .tokens
            .get(position + 2)
            .is_some_and(|token| token.kind == SyntaxKind::LT);
        Some((
            name,
            is_generic,
            is_unsafe,
            is_async,
            is_public,
            is_exported,
        ))
    }

    fn parse_item_in_block(&mut self, item: Marker) -> Result<(), ParserError> {
        match self.item() {
            Ok(_) => {}
            Err(ParserError::UnsupportedItem { .. }) => self.recover_unsupported_block_item()?,
            Err(error) => {
                item.abandon(self)?;
                return Err(error);
            }
        }
        item.complete(self, SyntaxKind::ERROR)?;
        Ok(())
    }

    fn recover_unsupported_block_item(&mut self) -> Result<(), ParserError> {
        let item_start = self.position;
        let item_kind = if self.at(SyntaxKind::UNSAFE_KW) {
            self.nth(1)
        } else {
            self.current()
        };
        let item_position = item_start + usize::from(self.at(SyntaxKind::UNSAFE_KW));
        let contextual =
            |spelling| token_is_contextual(self.source, self.tokens, item_position, spelling);
        let mut body_can_follow_newline = matches!(
            item_kind,
            SyntaxKind::TYPE_KW
                | SyntaxKind::ENUM_KW
                | SyntaxKind::TRAIT_KW
                | SyntaxKind::IMPL_KW
                | SyntaxKind::EXTERN_KW
        ) || (item_kind == SyntaxKind::LIDENT
            && (contextual("invariant")
                || contextual("resource")
                || contextual("interface")
                || contextual("world")
                || contextual("test")
                || contextual("property")));
        let where_clause_can_follow = matches!(
            item_kind,
            SyntaxKind::TYPE_KW | SyntaxKind::ENUM_KW | SyntaxKind::TRAIT_KW | SyntaxKind::IMPL_KW
        ) || (item_kind == SyntaxKind::LIDENT
            && contextual("distinct"));
        let generic_params_can_follow = matches!(
            item_kind,
            SyntaxKind::TYPE_KW | SyntaxKind::ENUM_KW | SyntaxKind::TRAIT_KW | SyntaxKind::IMPL_KW
        ) || (item_kind == SyntaxKind::LIDENT
            && contextual("distinct"));
        let effect_covers_can_follow = item_kind == SyntaxKind::LIDENT && contextual("effect");
        let mut paren_depth = 0usize;
        let mut bracket_depth = 0usize;
        let mut brace_depth = 0usize;
        let mut generic_depth = 0usize;
        let mut generic_lookahead = true;
        let mut generic_params_open = false;
        let mut generic_params_seen = false;
        let mut generic_param_expected = false;

        while !self.at(SyntaxKind::EOF) {
            let kind = self.current();
            if kind == SyntaxKind::NEWLINE
                && paren_depth == 0
                && bracket_depth == 0
                && brace_depth == 0
            {
                let next_starts_generic_parameter = generic_params_open
                    && generic_param_expected
                    && self.generic_parameter_starts_at_after_attributes(self.position + 1);
                let starts_later_item_or_statement =
                    self.unsupported_item_line_starts_statement() && !next_starts_generic_parameter;
                let next_expression_start = self.nth(1);
                let starts_expression_after_unclosed_parameters = generic_params_open
                    && generic_depth > 0
                    && !next_starts_generic_parameter
                    && EXPRESSION_START_KINDS.contains(&next_expression_start)
                    && next_expression_start != SyntaxKind::L_BRACE;
                if starts_later_item_or_statement || starts_expression_after_unclosed_parameters {
                    break;
                }
            }
            if kind == SyntaxKind::NEWLINE
                && paren_depth == 0
                && bracket_depth == 0
                && brace_depth == 0
                && generic_depth == 0
            {
                let next_position = self.position + 1;
                let next_is_where = self
                    .tokens
                    .get(next_position)
                    .is_some_and(|token| token.kind == SyntaxKind::WHERE_KW);
                let next_is_covers =
                    token_is_contextual(self.source, self.tokens, next_position, "covers");
                if (body_can_follow_newline && self.nth(1) == SyntaxKind::L_BRACE)
                    || (where_clause_can_follow && next_is_where)
                    || (effect_covers_can_follow && next_is_covers)
                {
                    self.bump();
                    continue;
                }
                break;
            }
            if kind == SyntaxKind::R_BRACE && brace_depth == 0 {
                break;
            }

            match kind {
                SyntaxKind::L_PAREN => paren_depth += 1,
                SyntaxKind::R_PAREN if paren_depth > 0 => paren_depth -= 1,
                SyntaxKind::L_BRACKET => bracket_depth += 1,
                SyntaxKind::R_BRACKET if bracket_depth > 0 => bracket_depth -= 1,
                SyntaxKind::L_BRACE => {
                    if brace_depth == 0 && paren_depth == 0 && bracket_depth == 0 {
                        let opens_effect_set = generic_depth > 0
                            && self.position > 0
                            && token_is_contextual(
                                self.source,
                                self.tokens,
                                self.position - 1,
                                "effects",
                            );
                        if !opens_effect_set {
                            generic_lookahead = false;
                            generic_depth = 0;
                            generic_params_open = false;
                        }
                    }
                    brace_depth += 1;
                }
                SyntaxKind::R_BRACE if brace_depth > 0 => brace_depth -= 1,
                SyntaxKind::EQ
                    if paren_depth == 0
                        && bracket_depth == 0
                        && brace_depth == 0
                        && generic_depth == 0 =>
                {
                    generic_lookahead = false;
                    generic_params_open = false;
                    if item_kind == SyntaxKind::TYPE_KW {
                        body_can_follow_newline = false;
                    }
                }
                SyntaxKind::LT if generic_lookahead => {
                    if generic_depth == 0 && generic_params_can_follow && !generic_params_seen {
                        generic_params_open = true;
                        generic_params_seen = true;
                        generic_param_expected = true;
                    }
                    generic_depth += 1;
                }
                SyntaxKind::GT if generic_depth > 0 => {
                    generic_depth -= 1;
                    if generic_depth == 0 {
                        generic_params_open = false;
                    }
                }
                SyntaxKind::COMMA
                    if generic_params_open
                        && generic_depth == 1
                        && paren_depth == 0
                        && bracket_depth == 0
                        && brace_depth == 0 =>
                {
                    generic_param_expected = true;
                }
                SyntaxKind::UIDENT | SyntaxKind::CONST_KW
                    if generic_params_open
                        && generic_param_expected
                        && generic_depth == 1
                        && paren_depth == 0
                        && bracket_depth == 0
                        && brace_depth == 0 =>
                {
                    generic_param_expected = false;
                }
                _ => {}
            }
            self.bump();
        }
        Ok(())
    }

    fn let_stmt(&mut self) -> Result<bool, ParserError> {
        let statement = self.start();
        let result = (|| {
            self.bump();
            self.pattern()?;
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

    fn pattern(&mut self) -> Result<CompletedMarker, ParserError> {
        if self.nesting_depth >= MAX_NESTING_DEPTH {
            return self.recover_overdeep_pattern();
        }
        self.nesting_depth += 1;
        let result = self.pattern_inner();
        self.nesting_depth -= 1;
        result
    }

    fn at_pattern_start(&self) -> bool {
        if self.at(SyntaxKind::DOT) {
            return self.nth(1) == SyntaxKind::UIDENT;
        }
        PATTERN_START_KINDS.contains(&self.current())
    }

    fn pattern_inner(&mut self) -> Result<CompletedMarker, ParserError> {
        if self.at(SyntaxKind::PIPE) {
            let alternatives = self.start();
            self.bump();
            self.alt_pattern()?;
            while self.eat(SyntaxKind::PIPE) {
                self.alt_pattern()?;
            }
            return alternatives.complete(self, SyntaxKind::OR_PAT);
        }

        let first = self.alt_pattern()?;
        if !self.at(SyntaxKind::PIPE) {
            return Ok(first);
        }
        let alternatives = first.precede(self);
        while self.eat(SyntaxKind::PIPE) {
            self.alt_pattern()?;
        }
        alternatives.complete(self, SyntaxKind::OR_PAT)
    }

    fn alt_pattern(&mut self) -> Result<CompletedMarker, ParserError> {
        if !self.binding_pattern_is_at_pattern() {
            return self.primary_pattern();
        }
        let at_pattern = self.start();
        self.binding_pattern()?;
        self.expect(SyntaxKind::AT)?;
        self.primary_pattern()?;
        at_pattern.complete(self, SyntaxKind::AT_PAT)
    }

    fn binding_pattern_is_at_pattern(&self) -> bool {
        match self.current() {
            SyntaxKind::LIDENT => self.nth(1) == SyntaxKind::AT,
            SyntaxKind::VAR_KW => {
                self.nth(1) == SyntaxKind::LIDENT && self.nth(2) == SyntaxKind::AT
            }
            SyntaxKind::BORROW_KW if self.nth(1) == SyntaxKind::MUT_KW => {
                self.nth(2) == SyntaxKind::LIDENT && self.nth(3) == SyntaxKind::AT
            }
            SyntaxKind::BORROW_KW => {
                self.nth(1) == SyntaxKind::LIDENT && self.nth(2) == SyntaxKind::AT
            }
            _ => false,
        }
    }

    fn binding_pattern(&mut self) -> Result<CompletedMarker, ParserError> {
        let pattern = self.start();
        if self.at_any(&[SyntaxKind::BORROW_KW, SyntaxKind::VAR_KW]) {
            let mode = self.start();
            if self.eat(SyntaxKind::BORROW_KW) {
                self.eat(SyntaxKind::MUT_KW);
            } else {
                self.bump();
            }
            mode.complete(self, SyntaxKind::BINDING_MODE)?;
        }
        self.name_node(&[SyntaxKind::LIDENT], SyntaxKind::NAME)?;
        pattern.complete(self, SyntaxKind::BINDING_PAT)
    }

    fn primary_pattern(&mut self) -> Result<CompletedMarker, ParserError> {
        match self.current() {
            SyntaxKind::UNDERSCORE => {
                let wildcard = self.start();
                self.bump();
                wildcard.complete(self, SyntaxKind::WILDCARD_PAT)
            }
            SyntaxKind::BORROW_KW | SyntaxKind::VAR_KW => self.binding_pattern(),
            SyntaxKind::LIDENT if self.nth(1) != SyntaxKind::DOT => self.binding_pattern(),
            SyntaxKind::LIDENT | SyntaxKind::UIDENT => self.const_or_ctor_pattern(),
            SyntaxKind::MINUS | SyntaxKind::INT | SyntaxKind::CHAR | SyntaxKind::BYTE => {
                let literal = self.literal_pattern(false)?;
                self.range_pattern_suffix(literal)
            }
            SyntaxKind::FLOAT => {
                let literal = self.literal_pattern(false)?;
                self.range_pattern_suffix(literal)
            }
            SyntaxKind::STRING
            | SyntaxKind::BYTE_STRING
            | SyntaxKind::TRUE_KW
            | SyntaxKind::FALSE_KW => self.literal_pattern(false),
            SyntaxKind::DOT2EQ => self.prefix_range_pattern(),
            SyntaxKind::DOT if self.nth(1) == SyntaxKind::UIDENT => self.implicit_member_pattern(),
            SyntaxKind::L_PAREN => self.paren_pattern(),
            SyntaxKind::L_BRACKET => self.slice_pattern(),
            _ => self.missing_pattern(),
        }
    }

    fn missing_pattern(&mut self) -> Result<CompletedMarker, ParserError> {
        self.expected_error_node(PATTERN_START_KINDS)
    }

    fn expected_error_node(
        &mut self,
        expected_kinds: &[SyntaxKind],
    ) -> Result<CompletedMarker, ParserError> {
        for kind in expected_kinds {
            self.expected.record(self.position, *kind);
        }
        self.emit_expected_token()?;
        self.start().complete(self, SyntaxKind::ERROR)
    }

    fn const_or_ctor_pattern(&mut self) -> Result<CompletedMarker, ParserError> {
        let path = self.const_path()?;
        if self.at_any(&[SyntaxKind::DOT2, SyntaxKind::DOT2EQ]) {
            return self.range_pattern_suffix(path);
        }
        let constructor = path.precede(self);
        if self.at(SyntaxKind::L_PAREN) {
            self.ctor_pat_args()?;
        }
        constructor.complete(self, SyntaxKind::CTOR_PAT)
    }

    fn const_path(&mut self) -> Result<CompletedMarker, ParserError> {
        let path = self.start();
        while self.at(SyntaxKind::LIDENT) {
            self.name_node(&[SyntaxKind::LIDENT], SyntaxKind::NAME_REF)?;
            self.expect(SyntaxKind::DOT)?;
        }
        self.name_node(&[SyntaxKind::UIDENT], SyntaxKind::NAME_REF)?;
        while self.eat(SyntaxKind::DOT) {
            self.name_node(&[SyntaxKind::UIDENT], SyntaxKind::NAME_REF)?;
        }
        path.complete(self, SyntaxKind::CONST_PATH)
    }

    fn implicit_member_pattern(&mut self) -> Result<CompletedMarker, ParserError> {
        let pattern = self.start();
        self.bump();
        self.name_node(&[SyntaxKind::UIDENT], SyntaxKind::NAME_REF)?;
        if self.at(SyntaxKind::L_PAREN) {
            self.ctor_pat_args()?;
        }
        pattern.complete(self, SyntaxKind::IMPLICIT_MEMBER_PAT)
    }

    fn literal_pattern(&mut self, range_end: bool) -> Result<CompletedMarker, ParserError> {
        let literal = self.start();
        let negative = self.eat(SyntaxKind::MINUS);
        match self.current() {
            SyntaxKind::INT => {
                self.bump();
            }
            SyntaxKind::CHAR | SyntaxKind::BYTE if !negative => {
                self.bump();
            }
            SyntaxKind::FLOAT => {
                let range = self
                    .current_token()
                    .map(|token| token.range)
                    .ok_or(ParserError::InvalidDiagnosticSpan)?;
                self.bump();
                self.emit_parser_diagnostic(
                    sai_diagnostics::Code::FloatPattern,
                    range,
                    "floating-point literals cannot be used in patterns",
                )?;
                return literal.complete(
                    self,
                    if range_end {
                        SyntaxKind::ERROR
                    } else {
                        SyntaxKind::LITERAL_PAT
                    },
                );
            }
            SyntaxKind::STRING
            | SyntaxKind::BYTE_STRING
            | SyntaxKind::TRUE_KW
            | SyntaxKind::FALSE_KW
                if !range_end && !negative =>
            {
                self.bump();
            }
            _ => {
                self.expect_any(if negative {
                    &[SyntaxKind::INT]
                } else if range_end {
                    RANGE_END_START_KINDS
                } else {
                    &[
                        SyntaxKind::INT,
                        SyntaxKind::FLOAT,
                        SyntaxKind::STRING,
                        SyntaxKind::BYTE_STRING,
                        SyntaxKind::CHAR,
                        SyntaxKind::BYTE,
                        SyntaxKind::TRUE_KW,
                        SyntaxKind::FALSE_KW,
                    ]
                })?;
            }
        }
        literal.complete(self, SyntaxKind::LITERAL_PAT)
    }

    fn range_end(&mut self) -> Result<CompletedMarker, ParserError> {
        match self.current() {
            SyntaxKind::MINUS
            | SyntaxKind::INT
            | SyntaxKind::FLOAT
            | SyntaxKind::CHAR
            | SyntaxKind::BYTE => self.literal_pattern(true),
            SyntaxKind::UIDENT => self.const_path(),
            SyntaxKind::LIDENT if self.nth(1) == SyntaxKind::DOT => self.const_path(),
            _ => self.expected_error_node(RANGE_END_START_KINDS),
        }
    }

    fn at_range_end_start(&self) -> bool {
        if self.at(SyntaxKind::LIDENT) {
            return self.nth(1) == SyntaxKind::DOT;
        }
        RANGE_END_START_KINDS.contains(&self.current())
    }

    fn range_pattern_suffix(
        &mut self,
        start: CompletedMarker,
    ) -> Result<CompletedMarker, ParserError> {
        if !self.at_any(&[SyntaxKind::DOT2, SyntaxKind::DOT2EQ]) {
            return Ok(start);
        }
        let range = start.precede(self);
        let inclusive = self.eat(SyntaxKind::DOT2EQ);
        if !inclusive {
            self.expect(SyntaxKind::DOT2)?;
        }
        if self.at_range_end_start() {
            self.range_end()?;
        } else if self.at(SyntaxKind::FLOAT) {
            self.invalid_float_range_end()?;
        } else if inclusive {
            self.expected_error_node(RANGE_END_START_KINDS)?;
        }
        range.complete(self, SyntaxKind::RANGE_PAT)
    }

    fn prefix_range_pattern(&mut self) -> Result<CompletedMarker, ParserError> {
        let range = self.start();
        self.bump();
        if self.at_range_end_start() {
            self.range_end()?;
        } else if self.at(SyntaxKind::FLOAT) {
            self.invalid_float_range_end()?;
        } else {
            self.expected_error_node(RANGE_END_START_KINDS)?;
        }
        range.complete(self, SyntaxKind::RANGE_PAT)
    }

    fn invalid_float_range_end(&mut self) -> Result<CompletedMarker, ParserError> {
        let invalid_end = self.start();
        let range = self
            .current_token()
            .map(|token| token.range)
            .ok_or(ParserError::InvalidDiagnosticSpan)?;
        self.bump();
        self.emit_parser_diagnostic(
            sai_diagnostics::Code::FloatPattern,
            range,
            "floating-point literals cannot be used in patterns",
        )?;
        invalid_end.complete(self, SyntaxKind::ERROR)
    }

    fn paren_pattern(&mut self) -> Result<CompletedMarker, ParserError> {
        let pattern = self.start();
        self.bump();
        if self.eat(SyntaxKind::R_PAREN) {
            return pattern.complete(self, SyntaxKind::TUPLE_PAT);
        }
        self.pattern()?;
        if !self.eat(SyntaxKind::COMMA) {
            self.expect(SyntaxKind::R_PAREN)?;
            return pattern.complete(self, SyntaxKind::PAREN_PAT);
        }
        while !self.at_any(&[
            SyntaxKind::R_PAREN,
            SyntaxKind::R_BRACKET,
            SyntaxKind::R_BRACE,
            SyntaxKind::NEWLINE,
            SyntaxKind::EOF,
        ]) {
            let item_position = self.position;
            self.pattern()?;
            if self.eat(SyntaxKind::COMMA) {
                continue;
            }
            if self.at_any(&[
                SyntaxKind::R_PAREN,
                SyntaxKind::R_BRACKET,
                SyntaxKind::R_BRACE,
                SyntaxKind::NEWLINE,
                SyntaxKind::EOF,
            ]) {
                break;
            }
            self.expect(SyntaxKind::COMMA)?;
            if self.position == item_position {
                let recovery_start = self.position;
                self.recover_to_list_separator()?;
                if self.position == recovery_start {
                    break;
                }
                self.eat(SyntaxKind::COMMA);
            }
        }
        self.expect(SyntaxKind::R_PAREN)?;
        pattern.complete(self, SyntaxKind::TUPLE_PAT)
    }

    fn slice_pattern(&mut self) -> Result<CompletedMarker, ParserError> {
        let pattern = self.start();
        self.bump();
        let result = (|| {
            let mut rest_count = 0usize;
            while !self.at_any(&[
                SyntaxKind::R_BRACKET,
                SyntaxKind::R_PAREN,
                SyntaxKind::R_BRACE,
                SyntaxKind::NEWLINE,
                SyntaxKind::EOF,
            ]) {
                let position = self.position;
                if self.at(SyntaxKind::DOT2) {
                    let range = self
                        .current_token()
                        .map(|token| token.range)
                        .ok_or(ParserError::InvalidDiagnosticSpan)?;
                    self.rest_pattern(true)?;
                    rest_count += 1;
                    if rest_count > 1 {
                        self.emit_parser_diagnostic(
                            sai_diagnostics::Code::MultipleRestPatterns,
                            range,
                            "a slice pattern may contain at most one rest pattern",
                        )?;
                    }
                } else {
                    self.pattern()?;
                }
                if self.eat(SyntaxKind::COMMA) {
                    continue;
                }
                if self.at_any(&[
                    SyntaxKind::R_BRACKET,
                    SyntaxKind::R_PAREN,
                    SyntaxKind::R_BRACE,
                    SyntaxKind::NEWLINE,
                    SyntaxKind::EOF,
                ]) {
                    break;
                }
                self.expect(SyntaxKind::COMMA)?;
                if self.position == position && self.recover_to_list_separator()? {
                    break;
                }
            }
            self.expect(SyntaxKind::R_BRACKET)?;
            Ok(())
        })();
        match result {
            Ok(()) => pattern.complete(self, SyntaxKind::SLICE_PAT),
            Err(error) => {
                pattern.abandon(self)?;
                Err(error)
            }
        }
    }

    fn rest_pattern(&mut self, allow_binding: bool) -> Result<CompletedMarker, ParserError> {
        let rest = self.start();
        self.expect(SyntaxKind::DOT2)?;
        if allow_binding
            && self.at_any(&[
                SyntaxKind::BORROW_KW,
                SyntaxKind::VAR_KW,
                SyntaxKind::LIDENT,
            ])
        {
            self.binding_pattern()?;
        }
        rest.complete(self, SyntaxKind::REST_PAT)
    }

    fn ctor_pat_args(&mut self) -> Result<(), ParserError> {
        let args = self.start();
        self.expect(SyntaxKind::L_PAREN)?;
        let result = (|| {
            let mut saw_rest = false;
            let mut reported_nonfinal_rest = false;
            while !self.at_any(&[
                SyntaxKind::R_PAREN,
                SyntaxKind::R_BRACE,
                SyntaxKind::R_BRACKET,
                SyntaxKind::NEWLINE,
                SyntaxKind::EOF,
            ]) {
                let position = self.position;
                let is_rest_arg = self.at(SyntaxKind::DOT2);
                if self.at(SyntaxKind::DOT2) {
                    self.rest_pattern(false)?;
                    saw_rest = true;
                } else if self.at(SyntaxKind::LIDENT) && self.nth(1) == SyntaxKind::COLON {
                    let named = self.start();
                    self.name_node(&[SyntaxKind::LIDENT], SyntaxKind::NAME_REF)?;
                    self.expect(SyntaxKind::COLON)?;
                    self.pattern()?;
                    named.complete(self, SyntaxKind::NAMED_PAT_ARG)?;
                } else {
                    self.pattern()?;
                }
                if is_rest_arg && self.at(SyntaxKind::COMMA) && self.nth(1) == SyntaxKind::R_PAREN {
                    self.expect(SyntaxKind::R_PAREN)?;
                    self.bump();
                    continue;
                }
                if self.eat(SyntaxKind::COMMA) {
                    if saw_rest && !self.at(SyntaxKind::R_PAREN) && !reported_nonfinal_rest {
                        self.expect(SyntaxKind::R_PAREN)?;
                        reported_nonfinal_rest = true;
                    }
                    continue;
                }
                if self.at_any(&[
                    SyntaxKind::R_PAREN,
                    SyntaxKind::R_BRACE,
                    SyntaxKind::R_BRACKET,
                    SyntaxKind::NEWLINE,
                    SyntaxKind::EOF,
                ]) {
                    break;
                }
                if saw_rest && !reported_nonfinal_rest {
                    self.expect(SyntaxKind::R_PAREN)?;
                    reported_nonfinal_rest = true;
                } else {
                    self.expect(SyntaxKind::COMMA)?;
                }
                if self.position == position && self.recover_to_list_separator()? {
                    break;
                }
            }
            self.expect(SyntaxKind::R_PAREN)?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                args.complete(self, SyntaxKind::CTOR_PAT_ARGS)?;
                Ok(())
            }
            Err(error) => {
                args.abandon(self)?;
                Err(error)
            }
        }
    }

    fn recover_overdeep_pattern(&mut self) -> Result<CompletedMarker, ParserError> {
        let error = self.start();
        let range = self.current_token().map(|token| token.range);
        let mut delimiters = Vec::new();
        let mut consumed = false;
        while !self.at(SyntaxKind::EOF) {
            let kind = self.current();
            if delimiters.is_empty()
                && matches!(
                    kind,
                    SyntaxKind::COMMA
                        | SyntaxKind::PIPE
                        | SyntaxKind::R_PAREN
                        | SyntaxKind::R_BRACKET
                        | SyntaxKind::R_BRACE
                        | SyntaxKind::NEWLINE
                )
            {
                break;
            }
            if self.at_confirmed_block_boundary() {
                break;
            }
            match kind {
                SyntaxKind::L_PAREN | SyntaxKind::L_BRACKET | SyntaxKind::L_BRACE => {
                    delimiters.push(kind);
                }
                SyntaxKind::R_PAREN | SyntaxKind::R_BRACKET | SyntaxKind::R_BRACE => {
                    let expected_open = match kind {
                        SyntaxKind::R_PAREN => SyntaxKind::L_PAREN,
                        SyntaxKind::R_BRACKET => SyntaxKind::L_BRACKET,
                        SyntaxKind::R_BRACE => SyntaxKind::L_BRACE,
                        _ => unreachable!("matched a closing delimiter"),
                    };
                    if delimiters.last() != Some(&expected_open) {
                        break;
                    }
                    delimiters.pop();
                }
                _ => {}
            }
            self.bump();
            consumed = true;
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
        if self.stop_expression_at_line_start && self.at_physical_line_start() {
            return self.missing_expression();
        }
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
            if self.at_any(&[
                SyntaxKind::DOT,
                SyntaxKind::L_PAREN,
                SyntaxKind::L_BRACKET,
                SyntaxKind::QUESTION,
            ]) {
                const POSTFIX_BINDING_POWER: u8 = 25;
                if POSTFIX_BINDING_POWER <= min_bp {
                    break;
                }
                lhs = self.postfix_expr(lhs)?;
                continue;
            }
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
                    let optional_block_endpoint = self.no_block_operand
                        && self.at(SyntaxKind::L_BRACE)
                        && operator.kind == SyntaxKind::DOT2;
                    if self.at_expression_start() && !optional_block_endpoint {
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
        let no_block_operand = self.no_block_operand;
        let stop_expression_at_line_start = self.stop_expression_at_line_start;
        let recovered_match_tail = self.recovered_match_tail;

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
        self.no_block_operand = no_block_operand;
        self.stop_expression_at_line_start = stop_expression_at_line_start;
        self.recovered_match_tail = recovered_match_tail;
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
            SyntaxKind::LIDENT
                if self.at_contextual("region")
                    && self.nth(1) == SyntaxKind::LIDENT
                    && self.nth(2) == SyntaxKind::L_BRACE =>
            {
                self.region_expr()
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
            SyntaxKind::L_PAREN => {
                self.with_no_block_operand(false, |parser| parser.paren_or_tuple_expr())
            }
            SyntaxKind::L_BRACE => {
                if self.no_block_operand {
                    self.emit_block_in_condition()?;
                }
                let expression = self.start();
                if let Err(error) = self.with_no_block_operand(false, |parser| parser.block()) {
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
                if let Err(error) = self.with_no_block_operand(false, |parser| parser.block()) {
                    expression.abandon(self)?;
                    return Err(error);
                }
                expression.complete(self, SyntaxKind::UNSAFE_BLOCK_EXPR)
            }
            SyntaxKind::DOT2 | SyntaxKind::DOT2EQ => self.prefix_range_expr(),
            SyntaxKind::DOT if self.nth(1) == SyntaxKind::UIDENT => self.implicit_member_expr(),
            SyntaxKind::DOT if self.nth(1) == SyntaxKind::AWAIT_KW => {
                Err(ParserError::UnsupportedSyntax {
                    position: self.position,
                    kind: SyntaxKind::DOT,
                })
            }
            SyntaxKind::L_BRACKET => {
                self.with_no_block_operand(false, |parser| parser.array_expr())
            }
            SyntaxKind::RETURN_KW => self.return_expr(),
            SyntaxKind::IF_KW => self.if_expr(),
            SyntaxKind::MATCH_KW => self.match_expr(),
            SyntaxKind::LABEL if self.nth(1) == SyntaxKind::COLON => self.loop_expr(),
            SyntaxKind::LOOP_KW | SyntaxKind::WHILE_KW | SyntaxKind::FOR_KW => self.loop_expr(),
            SyntaxKind::BREAK_KW => self.break_expr(),
            SyntaxKind::CONTINUE_KW => self.continue_expr(),
            SyntaxKind::MOVE_KW if self.nth(1) == SyntaxKind::FN_KW => self.closure_expr(),
            SyntaxKind::FN_KW => self.closure_expr(),
            SyntaxKind::ASYNC_KW => self.async_block_expr(),
            SyntaxKind::FSTRING_START => self.fstring_expr(),
            SyntaxKind::MOVE_KW => Err(ParserError::UnsupportedSyntax {
                position: self.position,
                kind: self.current(),
            }),
            _ => self.missing_expression(),
        }
    }

    fn fstring_expr(&mut self) -> Result<CompletedMarker, ParserError> {
        let expression = self.start();
        let result = (|| {
            self.expect(SyntaxKind::FSTRING_START)?;
            while !self.at_any(&[
                SyntaxKind::FSTRING_END,
                SyntaxKind::NEWLINE,
                SyntaxKind::EOF,
            ]) {
                if self.at_physical_line_start() {
                    break;
                }
                match self.current() {
                    SyntaxKind::FSTRING_TEXT => {
                        self.bump();
                    }
                    SyntaxKind::FSTRING_INTERP_OPEN => {
                        self.fstring_interpolation()?;
                    }
                    SyntaxKind::ERROR => {
                        self.bump();
                    }
                    _ => {
                        let unexpected = self.start();
                        self.bump();
                        unexpected.complete(self, SyntaxKind::ERROR)?;
                    }
                }
            }
            self.expect(SyntaxKind::FSTRING_END)?;
            Ok(())
        })();
        match result {
            Ok(()) => expression.complete(self, SyntaxKind::FSTRING_EXPR),
            Err(error) => {
                expression.abandon(self)?;
                Err(error)
            }
        }
    }

    fn fstring_interpolation(&mut self) -> Result<(), ParserError> {
        let interpolation = self.start();
        let result = (|| {
            self.expect(SyntaxKind::FSTRING_INTERP_OPEN)?;
            if self.recover_fstring_interpolation_prefix()? {
                self.with_expression_line_boundary(true, |parser| {
                    parser.with_no_block_operand(false, |parser| parser.expr_bp(0))
                })?;
            }
            let mut reported_unexpected_token = false;
            self.recover_fstring_interpolation_junk(&mut reported_unexpected_token)?;
            self.eat(SyntaxKind::FSTRING_SPEC);
            self.recover_fstring_interpolation_junk(&mut reported_unexpected_token)?;
            self.expect(SyntaxKind::FSTRING_INTERP_CLOSE)?;
            Ok(())
        })();
        match result {
            Ok(()) => interpolation
                .complete(self, SyntaxKind::FSTRING_INTERP)
                .map(|_| ()),
            Err(error) => {
                interpolation.abandon(self)?;
                Err(error)
            }
        }
    }

    fn recover_fstring_interpolation_prefix(&mut self) -> Result<bool, ParserError> {
        let mut reported_missing_expression = false;
        if !self.at_expression_start()
            && !self.at_any(&[
                SyntaxKind::FSTRING_SPEC,
                SyntaxKind::FSTRING_INTERP_CLOSE,
                SyntaxKind::FSTRING_END,
                SyntaxKind::NEWLINE,
                SyntaxKind::EOF,
            ])
            && !self.at(SyntaxKind::ERROR)
            && !self.at_physical_line_start()
        {
            self.missing_expression()?;
            reported_missing_expression = true;
        }
        while !self.at_expression_start()
            && !self.at_any(&[
                SyntaxKind::FSTRING_SPEC,
                SyntaxKind::FSTRING_INTERP_CLOSE,
                SyntaxKind::FSTRING_END,
                SyntaxKind::NEWLINE,
                SyntaxKind::EOF,
            ])
            && !self.at_physical_line_start()
        {
            if self.at(SyntaxKind::ERROR) {
                self.bump();
            } else {
                if !reported_missing_expression {
                    self.missing_expression()?;
                    reported_missing_expression = true;
                }
                let unexpected = self.start();
                self.bump();
                unexpected.complete(self, SyntaxKind::ERROR)?;
            }
        }
        if !self.at_expression_start() && !reported_missing_expression {
            self.missing_expression()?;
        }
        Ok(self.at_expression_start())
    }

    fn recover_fstring_interpolation_junk(
        &mut self,
        reported_unexpected_token: &mut bool,
    ) -> Result<(), ParserError> {
        while !self.at_any(&[
            SyntaxKind::FSTRING_SPEC,
            SyntaxKind::FSTRING_INTERP_CLOSE,
            SyntaxKind::FSTRING_END,
            SyntaxKind::NEWLINE,
            SyntaxKind::EOF,
        ]) && !self.at_physical_line_start()
        {
            if self.at(SyntaxKind::ERROR) {
                self.bump();
            } else {
                if !*reported_unexpected_token {
                    let range = self
                        .current_token()
                        .map(|token| token.range)
                        .ok_or(ParserError::InvalidDiagnosticSpan)?;
                    self.emit_parser_diagnostic(
                        sai_diagnostics::Code::ExpectedToken,
                        range,
                        "expected `}` to close format string interpolation",
                    )?;
                    *reported_unexpected_token = true;
                }
                let unexpected = self.start();
                self.bump();
                unexpected.complete(self, SyntaxKind::ERROR)?;
            }
        }
        Ok(())
    }

    fn implicit_member_expr(&mut self) -> Result<CompletedMarker, ParserError> {
        let expression = self.start();
        self.bump();
        self.name_node(&[SyntaxKind::UIDENT], SyntaxKind::NAME_REF)?;
        expression.complete(self, SyntaxKind::IMPLICIT_MEMBER_EXPR)
    }

    fn if_expr(&mut self) -> Result<CompletedMarker, ParserError> {
        if self.nesting_depth >= MAX_NESTING_DEPTH {
            return self.recover_overdeep_expression();
        }
        self.nesting_depth += 1;
        let result = self.if_expr_inner();
        self.nesting_depth -= 1;
        result
    }

    fn if_expr_inner(&mut self) -> Result<CompletedMarker, ParserError> {
        let expression = self.start();
        let result = (|| {
            self.bump();
            self.condition()?;
            self.with_no_block_operand(false, |parser| parser.block())?;

            if self.at(SyntaxKind::NEWLINE) && self.nth(1) == SyntaxKind::ELSE_KW {
                self.bump();
            }
            if self.at(SyntaxKind::ELSE_KW) {
                let else_branch = self.start();
                let branch_result = (|| {
                    self.bump();
                    if self.at(SyntaxKind::IF_KW) {
                        self.if_expr()?;
                    } else {
                        self.with_no_block_operand(false, |parser| parser.block())?;
                    }
                    Ok(())
                })();
                match branch_result {
                    Ok(()) => {
                        else_branch.complete(self, SyntaxKind::ELSE_BRANCH)?;
                    }
                    Err(error) => {
                        else_branch.abandon(self)?;
                        return Err(error);
                    }
                }
            }
            Ok(())
        })();
        match result {
            Ok(()) => expression.complete(self, SyntaxKind::IF_EXPR),
            Err(error) => {
                expression.abandon(self)?;
                Err(error)
            }
        }
    }

    fn loop_expr(&mut self) -> Result<CompletedMarker, ParserError> {
        if self.nesting_depth >= MAX_NESTING_DEPTH {
            return self.recover_overdeep_expression();
        }
        self.nesting_depth += 1;
        let result = self.loop_expr_inner();
        self.nesting_depth -= 1;
        result
    }

    fn loop_expr_inner(&mut self) -> Result<CompletedMarker, ParserError> {
        let expression = self.start();
        let result = (|| {
            self.loop_label()?;
            let kind = match self.current() {
                SyntaxKind::LOOP_KW => {
                    self.bump();
                    self.with_no_block_operand(false, |parser| parser.block())?;
                    SyntaxKind::LOOP_EXPR
                }
                SyntaxKind::WHILE_KW => {
                    self.bump();
                    self.condition()?;
                    self.with_no_block_operand(false, |parser| parser.block())?;
                    SyntaxKind::WHILE_EXPR
                }
                SyntaxKind::FOR_KW => {
                    self.bump();
                    self.eat(SyntaxKind::AWAIT_KW);
                    self.pattern()?;
                    self.expect(SyntaxKind::IN_KW)?;
                    self.with_no_block_operand(true, |parser| parser.expr_bp(0))?;
                    self.with_no_block_operand(false, |parser| parser.block())?;
                    SyntaxKind::FOR_EXPR
                }
                _ => {
                    self.expect_any(&[
                        SyntaxKind::LOOP_KW,
                        SyntaxKind::WHILE_KW,
                        SyntaxKind::FOR_KW,
                    ])?;
                    SyntaxKind::ERROR
                }
            };
            Ok(kind)
        })();
        match result {
            Ok(kind) => expression.complete(self, kind),
            Err(error) => {
                expression.abandon(self)?;
                Err(error)
            }
        }
    }

    fn loop_label(&mut self) -> Result<bool, ParserError> {
        if !self.at(SyntaxKind::LABEL) || self.nth(1) != SyntaxKind::COLON {
            return Ok(false);
        }
        let label = self.start();
        self.bump();
        self.expect(SyntaxKind::COLON)?;
        while self.eat_nl() {}
        label.complete(self, SyntaxKind::LOOP_LABEL)?;
        Ok(true)
    }

    fn region_expr(&mut self) -> Result<CompletedMarker, ParserError> {
        let expression = self.start();
        let result = (|| {
            self.bump();
            self.name_node(&[SyntaxKind::LIDENT], SyntaxKind::NAME)?;
            self.with_no_block_operand(false, |parser| parser.block())?;
            Ok(())
        })();
        match result {
            Ok(()) => expression.complete(self, SyntaxKind::REGION_EXPR),
            Err(error) => {
                expression.abandon(self)?;
                Err(error)
            }
        }
    }

    fn closure_expr(&mut self) -> Result<CompletedMarker, ParserError> {
        let expression = self.start();
        let result = (|| {
            self.eat(SyntaxKind::MOVE_KW);
            self.expect(SyntaxKind::FN_KW)?;
            self.closure_param_list()?;
            if self.at(SyntaxKind::THIN_ARROW) {
                self.ret_type()?;
            }
            self.with_no_block_operand(false, |parser| parser.block())?;
            Ok(())
        })();
        match result {
            Ok(()) => expression.complete(self, SyntaxKind::CLOSURE_EXPR),
            Err(error) => {
                expression.abandon(self)?;
                Err(error)
            }
        }
    }

    fn closure_param_list(&mut self) -> Result<(), ParserError> {
        let params = self.start();
        if !self.expect(SyntaxKind::L_PAREN)? {
            params.abandon(self)?;
            return Ok(());
        }
        while !self.at_any(&[SyntaxKind::R_PAREN, SyntaxKind::L_BRACE, SyntaxKind::EOF]) {
            if self.at_any(&[SyntaxKind::DOC_OUTER, SyntaxKind::DOC_INNER]) {
                self.misplaced_doc_comment()?;
                continue;
            }
            let position = self.position;
            if !self.closure_param()? {
                if self.at(SyntaxKind::COMMA) {
                    let error = self.start();
                    self.bump();
                    error.complete(self, SyntaxKind::ERROR)?;
                    continue;
                }
                self.recover_one()?;
                if self.position == position {
                    break;
                }
                continue;
            }
            if self.eat(SyntaxKind::COMMA) {
                continue;
            }
            if self.at_any(&[SyntaxKind::R_PAREN, SyntaxKind::L_BRACE, SyntaxKind::EOF]) {
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
        params.complete(self, SyntaxKind::CLOSURE_PARAM_LIST)?;
        Ok(())
    }

    fn closure_param(&mut self) -> Result<bool, ParserError> {
        if !self.at_any(&[
            SyntaxKind::VAR_KW,
            SyntaxKind::LIDENT,
            SyntaxKind::UNDERSCORE,
        ]) {
            self.expect_any(&[
                SyntaxKind::VAR_KW,
                SyntaxKind::LIDENT,
                SyntaxKind::UNDERSCORE,
            ])?;
            return Ok(false);
        }
        let param = self.start();
        self.eat(SyntaxKind::VAR_KW);
        self.name_node(
            &[SyntaxKind::LIDENT, SyntaxKind::UNDERSCORE],
            SyntaxKind::NAME,
        )?;
        if self.eat(SyntaxKind::COLON) {
            self.type_(true)?;
        }
        param.complete(self, SyntaxKind::CLOSURE_PARAM)?;
        Ok(true)
    }

    fn async_block_expr(&mut self) -> Result<CompletedMarker, ParserError> {
        let expression = self.start();
        let result = (|| {
            let async_range = self
                .current_token()
                .map(|token| token.range)
                .ok_or(ParserError::InvalidDiagnosticSpan)?;
            self.bump();
            if self.at(SyntaxKind::FN_KW)
                || (self.at(SyntaxKind::MOVE_KW) && self.nth(1) == SyntaxKind::FN_KW)
            {
                self.emit_parser_diagnostic(
                    sai_diagnostics::Code::NoAsyncClosure,
                    async_range,
                    "async closures are not supported; return an async block from a closure",
                )?;
                self.closure_expr()?;
                return Ok(SyntaxKind::ERROR);
            }
            self.eat(SyntaxKind::MOVE_KW);
            self.with_no_block_operand(false, |parser| parser.block())?;
            Ok(SyntaxKind::ASYNC_BLOCK_EXPR)
        })();
        match result {
            Ok(kind) => expression.complete(self, kind),
            Err(error) => {
                expression.abandon(self)?;
                Err(error)
            }
        }
    }

    fn match_expr(&mut self) -> Result<CompletedMarker, ParserError> {
        let expression = self.start();
        let result = (|| {
            self.bump();
            self.with_no_block_operand(true, |parser| parser.expr_bp(0))?;
            while self.eat_nl() {}
            self.with_no_block_operand(false, |parser| parser.match_arm_list())?;
            Ok(())
        })();
        match result {
            Ok(()) => expression.complete(self, SyntaxKind::MATCH_EXPR),
            Err(error) => {
                expression.abandon(self)?;
                Err(error)
            }
        }
    }

    fn match_arm_list(&mut self) -> Result<CompletedMarker, ParserError> {
        let arms = self.start();
        let result = (|| {
            if !self.eat(SyntaxKind::L_BRACE) {
                self.expect(SyntaxKind::L_BRACE)?;
                return Ok(false);
            }
            while self.eat_nl() {}
            while !self.at_any(&[SyntaxKind::R_BRACE, SyntaxKind::EOF]) {
                let parsed = self.match_arm()?;
                if !parsed {
                    if self.at(SyntaxKind::COMMA) {
                        let error = self.start();
                        self.bump();
                        error.complete(self, SyntaxKind::ERROR)?;
                        continue;
                    }
                    if self.at_confirmed_block_boundary() {
                        self.recover_match_arm_tail()?;
                        break;
                    }
                    if !self.recover_match_arm_separator()? {
                        break;
                    }
                    continue;
                }
                if self.eat(SyntaxKind::COMMA) {
                    while self.eat_nl() {}
                    continue;
                }
                if self.eat_nl() {
                    while self.eat_nl() {}
                    continue;
                }
                if self.at_any(&[SyntaxKind::R_BRACE, SyntaxKind::EOF]) {
                    break;
                }
                self.expect_any(&[SyntaxKind::COMMA, SyntaxKind::NEWLINE])?;
                if self.at(SyntaxKind::PIPE) || self.at_pattern_start() {
                    continue;
                }
                if !self.recover_match_arm_separator()? {
                    if self.at_confirmed_block_boundary() {
                        self.recover_match_arm_tail()?;
                    }
                    break;
                }
            }
            while self.eat_nl() {}
            if self.eat(SyntaxKind::R_BRACE) {
                Ok(true)
            } else {
                self.expect_close_brace()?;
                Ok(false)
            }
        })();
        match result {
            Ok(true) => arms.complete(self, SyntaxKind::MATCH_ARM_LIST),
            Ok(false) => arms.complete(self, SyntaxKind::ERROR),
            Err(error) => {
                arms.abandon(self)?;
                Err(error)
            }
        }
    }

    fn match_arm(&mut self) -> Result<bool, ParserError> {
        let arm = self.start();
        if !self.at(SyntaxKind::PIPE) && !self.at_pattern_start() {
            let result = self.missing_pattern();
            arm.abandon(self)?;
            return result.map(|_| false);
        }

        let result = (|| {
            self.pattern()?;
            if self.at(SyntaxKind::IF_KW) {
                let guard = self.start();
                let guard_result = (|| {
                    self.bump();
                    self.with_no_block_operand(false, |parser| parser.expr_bp(0))?;
                    Ok(())
                })();
                match guard_result {
                    Ok(()) => {
                        guard.complete(self, SyntaxKind::MATCH_GUARD)?;
                    }
                    Err(error) => {
                        guard.abandon(self)?;
                        return Err(error);
                    }
                }
            }
            self.expect(SyntaxKind::FAT_ARROW)?;
            while self.eat_nl() {}
            self.with_no_block_operand(false, |parser| parser.expr_bp(0))?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                arm.complete(self, SyntaxKind::MATCH_ARM)?;
                Ok(true)
            }
            Err(error) => {
                arm.abandon(self)?;
                Err(error)
            }
        }
    }

    fn recover_match_arm_separator(&mut self) -> Result<bool, ParserError> {
        let recovery_start = self.position;
        self.recover_to_list_separator()?;
        if self.position == recovery_start
            && self.at_any(&[SyntaxKind::R_PAREN, SyntaxKind::R_BRACKET])
            && self.nth(1) == SyntaxKind::FAT_ARROW
        {
            self.recover_one()?;
            self.recover_to_list_separator()?;
        }
        let progressed = self.position != recovery_start;
        while self.eat(SyntaxKind::COMMA) || self.eat_nl() {}
        Ok(progressed)
    }

    fn recover_match_arm_tail(&mut self) -> Result<(), ParserError> {
        let error = self.start();
        let mut brace_depth = 0usize;
        let mut consumed = false;
        self.recovered_match_tail = true;
        while !self.at(SyntaxKind::EOF) {
            if self.line_starts_top_level_item() {
                break;
            }
            match self.current() {
                SyntaxKind::L_BRACE => brace_depth += 1,
                SyntaxKind::R_BRACE if brace_depth == 0 => break,
                SyntaxKind::R_BRACE => brace_depth -= 1,
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

    fn condition(&mut self) -> Result<CompletedMarker, ParserError> {
        let condition = self.start();
        let result = if self.at(SyntaxKind::LET_KW) {
            let let_condition = self.start();
            let let_result = (|| {
                self.bump();
                self.pattern()?;
                self.expect(SyntaxKind::EQ)?;
                self.with_no_block_operand(true, |parser| parser.expr_bp(0))?;
                Ok(())
            })();
            match let_result {
                Ok(()) => {
                    let_condition.complete(self, SyntaxKind::LET_CONDITION)?;
                    Ok(())
                }
                Err(error) => {
                    let_condition.abandon(self)?;
                    Err(error)
                }
            }
        } else {
            self.with_no_block_operand(true, |parser| parser.expr_bp(0))
                .map(|_| ())
        };
        match result {
            Ok(()) => condition.complete(self, SyntaxKind::CONDITION),
            Err(error) => {
                condition.abandon(self)?;
                Err(error)
            }
        }
    }

    fn with_no_block_operand<T>(
        &mut self,
        no_block_operand: bool,
        parse: impl FnOnce(&mut Self) -> Result<T, ParserError>,
    ) -> Result<T, ParserError> {
        let previous = self.no_block_operand;
        self.no_block_operand = no_block_operand;
        let result = parse(self);
        self.no_block_operand = previous;
        result
    }

    fn with_expression_line_boundary<T>(
        &mut self,
        stop_at_line_start: bool,
        parse: impl FnOnce(&mut Self) -> Result<T, ParserError>,
    ) -> Result<T, ParserError> {
        let previous = self.stop_expression_at_line_start;
        self.stop_expression_at_line_start = stop_at_line_start;
        let result = parse(self);
        self.stop_expression_at_line_start = previous;
        result
    }

    fn emit_block_in_condition(&mut self) -> Result<(), ParserError> {
        let range = self
            .current_token()
            .map(|token| token.range)
            .ok_or(ParserError::InvalidDiagnosticSpan)?;
        self.emit_parser_diagnostic(
            sai_diagnostics::Code::BlockInCondition,
            range,
            "parenthesize a block expression used as a condition",
        )?;
        Ok(())
    }

    fn postfix_expr(&mut self, lhs: CompletedMarker) -> Result<CompletedMarker, ParserError> {
        let expression = lhs.precede(self);
        let result = (|| {
            let kind = match self.current() {
                SyntaxKind::DOT => {
                    self.bump();
                    if self.at_any(IDENT_KINDS) {
                        self.name_node(IDENT_KINDS, SyntaxKind::NAME_REF)?;
                        if self.at(SyntaxKind::LT) {
                            self.maybe_generic_args_expr()?;
                        }
                        SyntaxKind::FIELD_EXPR
                    } else if self.at(SyntaxKind::INT) {
                        let range = self
                            .current_token()
                            .map(|token| token.range)
                            .ok_or(ParserError::InvalidDiagnosticSpan)?;
                        let text = self.current_text();
                        let valid_decimal_index = !text.is_empty()
                            && text.bytes().all(|byte| byte.is_ascii_digit())
                            && (text == "0" || !text.starts_with('0'));
                        self.bump();
                        if !valid_decimal_index {
                            self.emit_parser_diagnostic(
                                sai_diagnostics::Code::InvalidTupleIndex,
                                range,
                                "tuple index must be plain decimal digits without leading zeroes",
                            )?;
                        }
                        SyntaxKind::TUPLE_INDEX_EXPR
                    } else if self.eat(SyntaxKind::AWAIT_KW) {
                        SyntaxKind::AWAIT_EXPR
                    } else {
                        self.expect_any(&[
                            SyntaxKind::LIDENT,
                            SyntaxKind::UIDENT,
                            SyntaxKind::INT,
                            SyntaxKind::AWAIT_KW,
                        ])?;
                        SyntaxKind::FIELD_EXPR
                    }
                }
                SyntaxKind::L_PAREN => {
                    self.with_no_block_operand(false, |parser| parser.arg_list())?;
                    SyntaxKind::CALL_EXPR
                }
                SyntaxKind::L_BRACKET => {
                    self.bump();
                    self.with_no_block_operand(false, |parser| parser.expr_bp(0))?;
                    self.expect(SyntaxKind::R_BRACKET)?;
                    SyntaxKind::INDEX_EXPR
                }
                SyntaxKind::QUESTION => {
                    self.bump();
                    SyntaxKind::TRY_EXPR
                }
                _ => {
                    return Err(ParserError::InvalidTokenStream {
                        position: self.position,
                    });
                }
            };
            Ok(kind)
        })();
        match result {
            Ok(kind) => expression.complete(self, kind),
            Err(error) => {
                expression.abandon(self)?;
                Err(error)
            }
        }
    }

    fn array_expr(&mut self) -> Result<CompletedMarker, ParserError> {
        let array = self.start();
        self.bump();
        let result = (|| {
            while !self.at_any(&[
                SyntaxKind::R_BRACKET,
                SyntaxKind::R_BRACE,
                SyntaxKind::R_PAREN,
                SyntaxKind::NEWLINE,
                SyntaxKind::EOF,
            ]) {
                let position = self.position;
                self.invalid_attributes_before_expression()?;
                let expression = self.expr_bp(0)?;
                if expression.kind() == SyntaxKind::ERROR
                    && self.position == position
                    && self.recover_to_list_separator()?
                {
                    break;
                }
                if self.eat(SyntaxKind::COMMA) {
                    continue;
                }
                if self.at_any(&[
                    SyntaxKind::R_BRACKET,
                    SyntaxKind::R_BRACE,
                    SyntaxKind::R_PAREN,
                    SyntaxKind::NEWLINE,
                    SyntaxKind::EOF,
                ]) {
                    break;
                }
                self.expect(SyntaxKind::COMMA)?;
                if self.position == position && self.recover_to_list_separator()? {
                    break;
                }
            }
            self.expect(SyntaxKind::R_BRACKET)?;
            Ok(())
        })();
        match result {
            Ok(()) => array.complete(self, SyntaxKind::ARRAY_EXPR),
            Err(error) => {
                array.abandon(self)?;
                Err(error)
            }
        }
    }

    fn arg_list(&mut self) -> Result<(), ParserError> {
        let args = self.start();
        self.expect(SyntaxKind::L_PAREN)?;
        let result = (|| {
            let mut base_arg_ranges = Vec::new();
            while !self.at_any(&[
                SyntaxKind::R_PAREN,
                SyntaxKind::R_BRACE,
                SyntaxKind::R_BRACKET,
                SyntaxKind::NEWLINE,
                SyntaxKind::EOF,
            ]) {
                if self.argument_starts_after_commas() {
                    self.report_nonfinal_base_args(&mut base_arg_ranges)?;
                }
                let position = self.position;
                if !self.arg(&mut base_arg_ranges)? && self.recover_to_list_separator()? {
                    break;
                }
                if self.eat(SyntaxKind::COMMA) {
                    continue;
                }
                if self.at_any(&[
                    SyntaxKind::R_PAREN,
                    SyntaxKind::R_BRACE,
                    SyntaxKind::R_BRACKET,
                    SyntaxKind::NEWLINE,
                    SyntaxKind::EOF,
                ]) {
                    break;
                }
                if self.argument_starts_after_commas() {
                    self.report_nonfinal_base_args(&mut base_arg_ranges)?;
                }
                self.expect(SyntaxKind::COMMA)?;
                if self.position == position && self.recover_to_list_separator()? {
                    break;
                }
            }
            self.expect(SyntaxKind::R_PAREN)?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                args.complete(self, SyntaxKind::ARG_LIST)?;
                Ok(())
            }
            Err(error) => {
                args.abandon(self)?;
                Err(error)
            }
        }
    }

    fn argument_starts_after_commas(&self) -> bool {
        let mut position = self.position;
        while self
            .tokens
            .get(position)
            .is_some_and(|token| token.kind == SyntaxKind::COMMA)
        {
            position += 1;
        }
        self.position_after_attributes(position)
            .and_then(|position| self.tokens.get(position))
            .is_some_and(|token| EXPRESSION_START_KINDS.contains(&token.kind))
    }

    fn report_nonfinal_base_args(
        &mut self,
        base_arg_ranges: &mut [(sai_vfs::TextRange, bool)],
    ) -> Result<(), ParserError> {
        for (range, is_nonfinal) in base_arg_ranges {
            if *is_nonfinal {
                continue;
            }
            self.emit_parser_diagnostic(
                sai_diagnostics::Code::BaseArgNotLast,
                *range,
                "record update argument must be last",
            )?;
            *is_nonfinal = true;
        }
        Ok(())
    }

    fn arg(
        &mut self,
        base_arg_ranges: &mut Vec<(sai_vfs::TextRange, bool)>,
    ) -> Result<bool, ParserError> {
        self.invalid_attributes_before_expression()?;
        if self.at(SyntaxKind::DOT2) {
            let range = self
                .current_token()
                .map(|token| token.range)
                .ok_or(ParserError::InvalidDiagnosticSpan)?;
            let base_arg = self.start();
            self.bump();
            self.invalid_attributes_before_expression()?;
            if let Err(error) = self.expr_bp(0) {
                base_arg.abandon(self)?;
                return Err(error);
            }
            base_arg.complete(self, SyntaxKind::BASE_ARG)?;
            base_arg_ranges.push((range, false));
            return Ok(true);
        }
        if self.at(SyntaxKind::LIDENT) && self.nth(1) == SyntaxKind::COLON {
            let named_arg = self.start();
            self.name_node(&[SyntaxKind::LIDENT], SyntaxKind::NAME_REF)?;
            self.expect(SyntaxKind::COLON)?;
            self.invalid_attributes_before_expression()?;
            if let Err(error) = self.expr_bp(0) {
                named_arg.abandon(self)?;
                return Err(error);
            }
            named_arg.complete(self, SyntaxKind::NAMED_ARG)?;
            return Ok(true);
        }

        let arg = self.start();
        let start_position = self.position;
        let expression = match self.expr_bp(0) {
            Ok(expression) => expression,
            Err(error) => {
                arg.abandon(self)?;
                return Err(error);
            }
        };
        if expression.kind() == SyntaxKind::ERROR && self.position == start_position {
            arg.abandon(self)?;
            return Ok(false);
        }
        arg.complete(self, SyntaxKind::ARG)?;
        Ok(true)
    }

    fn invalid_attributes_before_expression(&mut self) -> Result<(), ParserError> {
        if !self.at(SyntaxKind::AT) || self.skip_attribute_at(self.position).is_none() {
            return Ok(());
        }
        let attributes = self.start();
        self.attributes(false)?;
        attributes.complete(self, SyntaxKind::ERROR)?;
        Ok(())
    }

    fn at_physical_line_start(&self) -> bool {
        self.current_token()
            .is_some_and(|token| token.flags.contains(crate::TokenFlags::AFTER_LINE_BREAK))
    }

    fn at_confirmed_block_boundary(&self) -> bool {
        self.at_physical_line_start()
            && (self.at_any(&[
                SyntaxKind::LET_KW,
                SyntaxKind::VAR_KW,
                SyntaxKind::ASSERT_KW,
            ]) || self.line_starts_top_level_item())
    }

    fn recover_to_list_separator(&mut self) -> Result<bool, ParserError> {
        let error = self.start();
        let mut delimiters = Vec::new();
        let mut consumed = false;
        let mut reached_line_break = false;
        while !self.at(SyntaxKind::EOF) {
            let kind = self.current();
            if delimiters.is_empty() && kind == SyntaxKind::COMMA {
                break;
            }
            let matching_nested_close = match kind {
                SyntaxKind::R_PAREN | SyntaxKind::R_BRACKET | SyntaxKind::R_BRACE => {
                    let expected_open = match kind {
                        SyntaxKind::R_PAREN => SyntaxKind::L_PAREN,
                        SyntaxKind::R_BRACKET => SyntaxKind::L_BRACKET,
                        SyntaxKind::R_BRACE => SyntaxKind::L_BRACE,
                        _ => unreachable!("matched a closing delimiter"),
                    };
                    if delimiters.last() != Some(&expected_open) {
                        break;
                    }
                    delimiters.pop();
                    true
                }
                _ => false,
            };
            if kind == SyntaxKind::NEWLINE || self.at_confirmed_block_boundary() {
                reached_line_break = true;
                break;
            }
            if !matching_nested_close {
                match kind {
                    SyntaxKind::L_PAREN | SyntaxKind::L_BRACKET | SyntaxKind::L_BRACE => {
                        delimiters.push(kind);
                    }
                    _ => {}
                }
            }
            self.bump();
            consumed = true;
        }
        if consumed {
            error.complete(self, SyntaxKind::ERROR)?;
        } else {
            error.abandon(self)?;
        }
        Ok(reached_line_break)
    }

    fn paren_or_tuple_expr(&mut self) -> Result<CompletedMarker, ParserError> {
        let expression = self.start();
        self.bump();
        if self.eat(SyntaxKind::R_PAREN) {
            return expression.complete(self, SyntaxKind::UNIT_EXPR);
        }
        let result = (|| {
            self.invalid_attributes_before_expression()?;
            self.expr_bp(0)?;
            let is_tuple = self.eat(SyntaxKind::COMMA);
            if is_tuple {
                while !self.at_any(&[SyntaxKind::R_PAREN, SyntaxKind::EOF]) {
                    self.invalid_attributes_before_expression()?;
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
        let optional_block_endpoint =
            self.no_block_operand && self.at(SyntaxKind::L_BRACE) && kind == SyntaxKind::DOT2;
        if self.at_expression_start() && !optional_block_endpoint {
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
        if self.current() != SyntaxKind::NEWLINE
            && self.at_expression_start()
            && !(self.no_block_operand && self.at(SyntaxKind::L_BRACE))
        {
            if let Err(error) = self.expr_bp(0) {
                expression.abandon(self)?;
                return Err(error);
            }
        }
        expression.complete(self, SyntaxKind::RETURN_EXPR)
    }

    fn break_expr(&mut self) -> Result<CompletedMarker, ParserError> {
        let expression = self.start();
        self.bump();
        if self.at(SyntaxKind::LABEL) && self.nth(1) != SyntaxKind::COLON {
            self.bump();
        }
        if self.current() != SyntaxKind::NEWLINE
            && self.at_expression_start()
            && !(self.no_block_operand && self.at(SyntaxKind::L_BRACE))
        {
            if let Err(error) = self.expr_bp(0) {
                expression.abandon(self)?;
                return Err(error);
            }
        }
        expression.complete(self, SyntaxKind::BREAK_EXPR)
    }

    fn continue_expr(&mut self) -> Result<CompletedMarker, ParserError> {
        let expression = self.start();
        self.bump();
        if self.at(SyntaxKind::LABEL) {
            self.bump();
        }
        expression.complete(self, SyntaxKind::CONTINUE_EXPR)
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
                self.type_operand(true)?;
                ty.complete(self, SyntaxKind::BORROW_TYPE)?;
            }
            SyntaxKind::OWNED_KW => {
                let ty = self.start();
                self.bump();
                self.type_operand(false)?;
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

    fn type_operand(&mut self, allow_fx: bool) -> Result<(), ParserError> {
        if self.at_physical_line_start() && self.item_starts_at_after_attributes(self.position) {
            let range = self
                .current_token()
                .map(|token| token.range)
                .ok_or(ParserError::InvalidDiagnosticSpan)?;
            self.emit_parser_diagnostic(
                sai_diagnostics::Code::ExpectedToken,
                range,
                "expected a type before the next item",
            )?;
            return Ok(());
        }
        self.type_(allow_fx)
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

    fn variant_type_list(&mut self, close: SyntaxKind) -> Result<(), ParserError> {
        self.type_list_inner(close, true)
    }

    fn type_list(&mut self, close: SyntaxKind) -> Result<(), ParserError> {
        self.type_list_inner(close, false)
    }

    fn type_list_inner(
        &mut self,
        close: SyntaxKind,
        recover_at_statement_boundary: bool,
    ) -> Result<(), ParserError> {
        while !self.at_any(&[close, SyntaxKind::EOF])
            && !(recover_at_statement_boundary && self.variant_type_list_recovery_boundary())
        {
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
            if self.at(SyntaxKind::NEWLINE) {
                break;
            }
            if self.generic_argument_list_recovery_boundary() {
                break;
            }
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
        if self.at(SyntaxKind::NEWLINE) && self.nth(1) == SyntaxKind::GT {
            self.eat_nl();
        }
        self.expect(SyntaxKind::GT)?;
        args.complete(self, SyntaxKind::GENERIC_ARG_LIST)?;
        Ok(())
    }

    fn generic_argument_list_recovery_boundary(&self) -> bool {
        if !self.at_physical_line_start()
            || !self.statement_starts_at_after_attributes(self.position)
        {
            return false;
        }
        if self.item_starts_at_after_attributes(self.position) {
            return true;
        }

        let Some(position) = self.position_after_attributes(self.position) else {
            return false;
        };
        let Some(token) = self.tokens.get(position) else {
            return false;
        };
        if !GENERIC_ARG_START_KINDS.contains(&token.kind) {
            return true;
        }
        match token.kind {
            SyntaxKind::LIDENT | SyntaxKind::UIDENT => {
                !self.generic_argument_path_type_starts_at(position)
            }
            SyntaxKind::MINUS => !self
                .tokens
                .get(position + 1)
                .is_some_and(|next| next.kind == SyntaxKind::INT),
            SyntaxKind::FN_KW => self.closure_body_follows_fn_type(position),
            SyntaxKind::UNSAFE_KW => {
                !self
                    .tokens
                    .get(position + 1)
                    .is_some_and(|next| next.kind == SyntaxKind::FN_KW)
                    || !self
                        .tokens
                        .get(position + 2)
                        .is_some_and(|next| next.kind == SyntaxKind::L_PAREN)
            }
            _ => false,
        }
    }

    fn generic_argument_path_type_starts_at(&self, position: usize) -> bool {
        self.generic_argument_path_type_end_at(position).is_some()
    }

    fn generic_argument_path_type_end_at(&self, mut position: usize) -> Option<usize> {
        while self
            .tokens
            .get(position)
            .is_some_and(|token| token.kind == SyntaxKind::LIDENT)
        {
            if !self
                .tokens
                .get(position + 1)
                .is_some_and(|token| token.kind == SyntaxKind::DOT)
            {
                return None;
            }
            position += 2;
        }
        if !self
            .tokens
            .get(position)
            .is_some_and(|token| token.kind == SyntaxKind::UIDENT)
        {
            return None;
        }

        loop {
            position += 1;
            if self
                .tokens
                .get(position)
                .is_some_and(|token| token.kind == SyntaxKind::LT)
            {
                position = self.skip_generic_argument_list_at(position)?;
            }
            if self
                .tokens
                .get(position)
                .is_some_and(|token| token.kind == SyntaxKind::DOT)
            {
                if !self
                    .tokens
                    .get(position + 1)
                    .is_some_and(|token| token.kind == SyntaxKind::UIDENT)
                {
                    return None;
                }
                position += 1;
                continue;
            }
            return (!self
                .tokens
                .get(position)
                .is_some_and(|token| token.kind == SyntaxKind::L_PAREN))
            .then_some(position);
        }
    }

    fn skip_generic_argument_list_at(&self, position: usize) -> Option<usize> {
        if self.tokens.get(position)?.kind != SyntaxKind::LT {
            return None;
        }
        let mut depth = 0usize;
        for (index, token) in self.tokens.iter().enumerate().skip(position) {
            match token.kind {
                SyntaxKind::LT => depth += 1,
                SyntaxKind::GT => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return Some(index + 1);
                    }
                }
                SyntaxKind::EOF => return None,
                _ => {}
            }
        }
        None
    }

    fn closure_body_follows_fn_type(&self, position: usize) -> bool {
        if self
            .tokens
            .get(position + 1)
            .is_none_or(|token| token.kind != SyntaxKind::L_PAREN)
        {
            return false;
        }

        let mut depth = 0usize;
        let mut after_parameters = None;
        for (index, token) in self.tokens.iter().enumerate().skip(position + 1) {
            match token.kind {
                SyntaxKind::L_PAREN => depth += 1,
                SyntaxKind::R_PAREN if depth > 0 => {
                    depth -= 1;
                    if depth == 0 {
                        after_parameters = Some(index + 1);
                        break;
                    }
                }
                SyntaxKind::EOF => return false,
                _ => {}
            }
        }
        let Some(mut position) = after_parameters else {
            return false;
        };

        if self
            .tokens
            .get(position)
            .is_some_and(|token| token.kind == SyntaxKind::L_BRACE)
        {
            return true;
        }
        if !self
            .tokens
            .get(position)
            .is_some_and(|token| token.kind == SyntaxKind::THIN_ARROW)
        {
            return false;
        }
        position += 1;
        if self
            .tokens
            .get(position)
            .is_some_and(|token| token.kind == SyntaxKind::L_BRACE)
        {
            return true;
        }
        if !self
            .tokens
            .get(position)
            .is_some_and(|token| TYPE_START_KINDS.contains(&token.kind))
        {
            return false;
        }

        let mut paren_depth = 0usize;
        let mut bracket_depth = 0usize;
        let mut brace_depth = 0usize;
        let mut angle_depth = 0usize;
        while let Some(token) = self.tokens.get(position) {
            if paren_depth == 0 && bracket_depth == 0 && brace_depth == 0 && angle_depth == 0 {
                if token.kind == SyntaxKind::L_BRACE {
                    return true;
                }
                if token.kind == SyntaxKind::LIDENT
                    && token_is_contextual(self.source, self.tokens, position, "effects")
                    && self
                        .tokens
                        .get(position + 1)
                        .is_some_and(|next| next.kind == SyntaxKind::L_BRACE)
                {
                    return false;
                }
                if matches!(
                    token.kind,
                    SyntaxKind::NEWLINE
                        | SyntaxKind::COMMA
                        | SyntaxKind::GT
                        | SyntaxKind::R_PAREN
                        | SyntaxKind::R_BRACKET
                        | SyntaxKind::R_BRACE
                        | SyntaxKind::EOF
                ) {
                    return false;
                }
            }
            match token.kind {
                SyntaxKind::L_PAREN => paren_depth += 1,
                SyntaxKind::R_PAREN if paren_depth > 0 => paren_depth -= 1,
                SyntaxKind::L_BRACKET => bracket_depth += 1,
                SyntaxKind::R_BRACKET if bracket_depth > 0 => bracket_depth -= 1,
                SyntaxKind::L_BRACE => brace_depth += 1,
                SyntaxKind::R_BRACE if brace_depth > 0 => brace_depth -= 1,
                SyntaxKind::LT => angle_depth += 1,
                SyntaxKind::GT if angle_depth > 0 => angle_depth -= 1,
                _ => {}
            }
            position += 1;
        }
        false
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
            if brace_depth == 0 && self.at_confirmed_block_boundary() {
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
