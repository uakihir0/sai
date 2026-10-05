# 03. Formal Grammar (EBNF)

**Status:** Normative
**Stage:** 0 (per-feature stages are noted on individual rules)
**Related:** [02-lexical-structure.md](02-lexical-structure.md), [decisions.md](../design/decisions.md) D-SYN-01..10

This chapter defines the complete syntax of SAI source files in EBNF. It defines syntactic well-formedness only. Static semantics such as name resolution, types, and ownership are defined in later chapters.

---

## 3.1 Notation

### 3.1.1 EBNF notation

| Notation | Meaning |
|---|---|
| `Rule ::= ...` | rule definition |
| `A B` | sequence |
| `A \| B` | alternative |
| `A?` | zero or one |
| `A*` | zero or more |
| `A+` | one or more |
| `( ... )` | grouping |
| `'fn'` | strict keyword or punctuator (terminal) |
| `"effects"` | contextual keyword: a `LIDENT` with exactly this spelling (02 §2.5.2) |
| `LIDENT` `UIDENT` `INT` ... | lexical token kinds (02 §2.2) |
| `NL` | the `NEWLINE` token |
| `nl` | shorthand for `NL?`; the parser skips a NEWLINE at this position |
| `List<X, S>` | `X (S X)* S?`: one or more X separated by S, with an optional trailing separator. A possibly empty list is written `List<X, S>?` |
| `Ident` | `LIDENT \| UIDENT` |

### 3.1.2 Separators

```ebnf
Sep       ::= ',' nl | NL          (* separator inside braces: a comma or a line break *)
Comma     ::= ','                  (* inside ( ) [ ] < > line breaks are ignored lexically *)
```

- Inside `( )` and `[ ]` the lexer does not generate NEWLINE (02 §2.10), so `NL` never appears in the grammar inside them.
- Lists inside `{ }` are separated by `Sep` (D-SYN-07).
- The `< >` of type arguments and type parameters are not pushed on the bracket stack. `<` and `,` are not in E, so no NEWLINE is generated directly after them. A NEWLINE can, however, be generated at the end of the line before a closing `>`. That is why an `nl` is placed before every closing `>`.

### 3.1.3 NEWLINE inside blocks

An `nl` is allowed directly after `'{'` and directly before `'}'`. The rules below write this explicitly as `'{' nl` / `nl '}'`.

---

## 3.2 Files

```ebnf
File          ::= Shebang? ModuleHeader Items EOF
ModuleHeader  ::= DOC_INNER* ( 'unsafe' "module" NL )?
Items         ::= ( Item ( NL Item )* NL? )?
```

- `unsafe module` makes the whole file an unsafe context (D-SYN-08, [12-unsafe-and-ffi.md](12-unsafe-and-ffi.md)).
- There are no attributes on files, modules, or packages ([15-attributes-and-lints.md](15-attributes-and-lints.md) §15.2.1). Package-wide settings are written in `sai.package`. An attribute before the first item attaches to that item.

---

## 3.3 Items

```ebnf
Item          ::= OuterDocs Attributes ItemBody
OuterDocs     ::= ( DOC_OUTER )*
Attributes    ::= ( Attribute nl )*
Attribute     ::= '@' LIDENT AttrArgs?
AttrArgs      ::= '(' List<AttrArg, Comma>? ')'
AttrArg       ::= Ident ( '(' List<AttrArg, Comma>? ')' )?
                | STRING
                | INT
                | Ident '=' ( STRING | INT | Ident )

ItemBody      ::= UseDecl
                | Visibility? FnDecl
                | Visibility? TypeDecl
                | Visibility? EnumDecl
                | Visibility? DistinctDecl
                | Visibility? ResourceDecl
                | Visibility? TraitDecl
                | ImplDecl
                | Visibility? ConstDecl
                | InvariantDecl
                | Visibility? EffectDecl
                | Visibility? InterfaceDecl
                | Visibility? WorldDecl
                | ExternBlock
                | TestDecl                                  (* Stage 0 syntax / Stage 3 execution *)
                | PropertyDecl

Visibility    ::= 'pub' | 'export'
```

- The syntax of `AttrArgs` is generic. The arguments each attribute accepts are defined in [15-attributes-and-lints.md](15-attributes-and-lints.md). Attribute names form a closed set; an unknown attribute name is the error `UNKNOWN_ATTRIBUTE`. Values that are not identifiers, such as target names containing `-`, are written as `STRING` (`@target("wasm32-wasi")`).
- `Attributes` appear in the grammar before an `Item`, a `FieldDecl`, a `VariantDecl`, a `TraitItem`, an `ImplItem`, an `InterfaceItem`, an `IfaceResourceItem`, and an `ExternItem`. A parser also accepts an attribute sequence before a statement, a parameter, a generic parameter, a record-variant field, or a `WorldItem`, so that it can report `INVALID_ATTRIBUTE_TARGET` (15 §15.2.1) and continue as if the attributes were absent.
- Only a `FnDecl` may be marked `export` (a core Wasm export, D-MOD-02). `export` on any other item is the error `INVALID_EXPORT_TARGET`. Component exports through a world are declared with `WorldDecl`.
- The sequence `pub export` does not exist syntactically (there is only one Visibility). Writing `pub export` produces the error `REDUNDANT_PUB_EXPORT`.
- Re-exports with `pub use` are covered in §3.4.

---

## 3.4 Use declarations

```ebnf
UseDecl       ::= 'pub'? 'use' PKG_PATH UseAlias? UseItems?
UseAlias      ::= 'as' LIDENT
UseItems      ::= '.' '{' nl List<UseItem, Sep> nl '}'
UseItem       ::= Ident ( 'as' Ident )?
```

- A `PKG_PATH` denotes a package identity followed by a module path (or an interface name) (02 §2.9). `use acme:server/http/client` binds the module `http/client` as `client`.
- `UseAlias` and `UseItems` may both be given (`use sai:http/client as http.{Request}`). In that case both the module alias and the items are bound.
- `pub use` re-exports items. A `pub use` MUST have `UseItems` (`PUB_USE_WITHOUT_ITEMS`, chapter 04), and only the items listed there are re-exported (a module itself cannot be re-exported). A re-exported item keeps the symbol identity of its original definition (D-MOD-04).
- The case of an alias must match the case of the original name (`UIDENT` for `UIDENT`, `LIDENT` for `LIDENT`).
- The semantics are defined in [04-modules-and-names.md](04-modules-and-names.md).

---

## 3.5 Functions

```ebnf
FnDecl        ::= FnHead FnBody?
FnHead        ::= 'unsafe'? 'async'? 'fn' LIDENT GenericParams?
                  '(' List<Param, Comma>? ')'
                  ( nl '->' ReturnType )?
                  FnClauses
FnClauses     ::= ( nl WhereClause )?
                  ( nl BorrowsClause )?
                  ( nl EffectsClause )?
                  ( nl RequiresClause )*
                  ( nl EnsuresClause )*
FnBody        ::= nl Block

Param         ::= SelfParam | NamedParam
SelfParam     ::= 'var'? 'self' ':' Type
NamedParam    ::= 'var'? ( LIDENT | '_' ) ':' Type

ReturnType    ::= TypeNoFx
WhereClause   ::= 'where' WherePred ( ',' nl WherePred )*
WherePred     ::= Type ':' WhereBounds
WhereBounds   ::= WhereBound ( '+' WhereBound )*
WhereBound    ::= BoundNoFx | '(' Bound ')'

BorrowsClause ::= "borrows" '{' nl List<Provenance, Sep>? nl '}'
Provenance    ::= ( LIDENT | 'self' ) ( '.' LIDENT )*

EffectsClause ::= "effects" EffectSet
EffectSet     ::= '{' nl List<EffectRef, Sep>? nl '}'
EffectRef     ::= LIDENT ( '.' LIDENT )*

RequiresClause ::= "requires" Expr
EnsuresClause  ::= "ensures" Expr
```

- **The clause order is fixed** (D-SYN-06). Clauses in any other order are the error `CLAUSE_ORDER`, with a fix that reorders them. `where`, `borrows`, and `effects` may each appear at most once (`DUPLICATE_CLAUSE`); `requires` and `ensures` may repeat.
- `FnBody` may be omitted only for trait method declarations, interface function declarations, and functions in extern blocks. Omitting the body anywhere else is the error `MISSING_FN_BODY`. A trait method may have a body, which is its default. A body on an interface function is `INTERFACE_FN_BODY` (chapter 13), and a body on a function of an extern block is `UNEXPECTED_FN_BODY`.
- A `SelfParam` may appear only as the first parameter of a function inside an `impl`, a `trait`, or an interface resource. Its type must be `Self`, `borrow Self`, or `borrow mut Self` (`INVALID_SELF_TYPE`).
- A parameter marked `var` can be reassigned and borrowed with `borrow mut` inside the function body.
- Parameters cannot be destructured with patterns (D-SYN-06); use `let` in the body.
- **Binding of return types and effects clauses**: in the return type of a function declaration (`ReturnType`) and in the bounds of a where clause, a top-level function pointer type or `Fn` sugar bound cannot carry its own effects clause (`TypeNoFx` / `BoundNoFx`). An `effects` that follows such a position always belongs to the function declaration. To put effects on a returned function pointer type, parenthesize the type: `-> (fn(I32) -> I32 effects { alloc })`.
- The expressions of `requires` and `ensures` are ordinary `Expr`s. A `{` after the expression starts the body block. SAI has no expression form with a postfix `{`, so this is never ambiguous.

### 3.5.1 Generic parameters

```ebnf
GenericParams ::= '<' List<GenericParam, Comma> nl '>'
GenericParam  ::= UIDENT ( ':' Bounds )?
                | 'const' UIDENT ':' Type
Bounds        ::= Bound ( '+' Bound )*
Bound         ::= BoundNoFx BoundEffects?
BoundNoFx     ::= FnSugar | TraitRef
BoundEffects  ::= "effects" EffectSet
TraitRef      ::= TypePath                         (* its GenericArgs may contain associated type bindings Item = T *)
FnSugar       ::= ( 'Fn' | 'FnMut' | 'FnOnce' ) '(' List<Type, Comma>? ')' ( '->' TypeNoFx )?
```

- Syntactically, `Fn` / `FnMut` / `FnOnce` are `UIDENT`s (prelude trait names) under the condition that a `(` follows.
- `BoundEffects` caps the effects reached through that bound (D-EFF-02).
- The type of a `const` parameter must be an integer type, `Bool`, or `Char` (v0.1).

---

## 3.6 Type declarations

```ebnf
TypeDecl      ::= 'type' UIDENT GenericParams? ( StructDef | AliasDef )
StructDef     ::= ( nl WhereClause )? nl '{' nl List<FieldDecl, Sep>? nl '}'
AliasDef      ::= '=' nl Type ( nl RefinementClause )?
RefinementClause ::= 'where' Expr

FieldDecl     ::= OuterDocs Attributes 'pub'? LIDENT ':' Type

EnumDecl      ::= 'enum' UIDENT GenericParams? ( nl WhereClause )?
                  nl '{' nl List<VariantDecl, Sep>? nl '}'
VariantDecl   ::= OuterDocs Attributes UIDENT VariantShape? ( '=' '-'? INT )?
VariantShape  ::= '(' List<Type, Comma> ')'                     (* tuple variant  *)
                | '(' List<VariantField, Comma> ')'              (* record variant *)
VariantField  ::= LIDENT ':' Type

DistinctDecl  ::= "distinct" UIDENT GenericParams? '=' 'pub'? Type ( nl RefinementClause )?

ResourceDecl  ::= "resource" UIDENT ( nl '{' nl List<FieldDecl, Sep>? nl '}' )?
```

- `type X = T` is a transparent type alias. `type X = T where p` is a refined type, which is nominal (D-TYPE-03). Aliases may be generic (`type Pair<T> = (T, T)`). Refined types cannot be generic in v0.1 (`GENERIC_REFINED_TYPE`, chapter 05).
- Inside the expression of a `RefinementClause`, `self` denotes the value being refined.
- The two alternatives of `VariantShape` are distinguished by whether the tokens right after `(` are `LIDENT ':'`. A single variant cannot mix tuple and record fields.
- An explicit discriminant (`= INT`) is allowed only in an enum whose variants are all unit variants (`EXPLICIT_DISCRIMINANT_ON_DATA_ENUM`).
- The `'pub'` in a `distinct` declaration makes the constructor from the base type and the accessor `.value` public (D-MOD-02).
- A `ResourceDecl` without a body is a resource with no fields. Resources inside an interface use a different syntax (§3.9).

---

## 3.7 Traits and impls

```ebnf
TraitDecl     ::= 'unsafe'? 'trait' UIDENT GenericParams? ( ':' Bounds )?
                  ( nl WhereClause )? nl '{' nl List<TraitItem, NL>? nl '}'
TraitItem     ::= OuterDocs Attributes ( AssocTypeDecl | FnDecl )
AssocTypeDecl ::= 'type' UIDENT ( ':' Bounds )?

ImplDecl      ::= 'unsafe'? 'impl' GenericParams? ImplTarget ( nl WhereClause )?
                  nl '{' nl List<ImplItem, NL>? nl '}'
ImplTarget    ::= TraitRef 'for' Type                (* trait impl *)
                | Type                               (* inherent impl: the last path segment is a UIDENT *)
                | ModPath                            (* interface impl: the last path segment is a LIDENT *)
ImplItem      ::= OuterDocs Attributes ( AssocTypeDef | 'pub'? FnDecl )
AssocTypeDef  ::= 'type' UIDENT '=' Type
ModPath       ::= LIDENT ( '.' LIDENT )*
```

- `ImplTarget` is chosen as follows. If `for` appears, it is a trait impl. Otherwise, if the last path segment is a `UIDENT` it is an inherent impl, and if it is a `LIDENT` it is an interface impl (D-WIT-01).
- Functions inside a trait impl or an interface impl cannot be marked `pub`; their visibility follows the trait or interface (`VISIBILITY_ON_TRAIT_IMPL_ITEM`).
- An `AssocTypeDef` inside an interface impl binds an abstract resource declared by the interface to a concrete resource type.

---

## 3.8 Constants, invariants, and effects

```ebnf
ConstDecl     ::= 'const' UIDENT ':' Type '=' nl Expr
InvariantDecl ::= "invariant" UIDENT nl '{' nl List<Expr, Sep> nl '}'
EffectDecl    ::= "effect" LIDENT ( nl "covers" EffectSet )?
```

- Each expression of an `InvariantDecl` is a predicate of type `Bool`; the invariant is their conjunction. Inside each expression, `self` denotes (a `borrow` of) a value of the type. An `InvariantDecl` must be placed in the same module as its type (`INVARIANT_OUTSIDE_DEFINING_MODULE`).

---

## 3.9 Interfaces and worlds (Stage 4)

```ebnf
InterfaceDecl ::= "interface" LIDENT nl '{' nl List<InterfaceItem, NL>? nl '}'
InterfaceItem ::= OuterDocs Attributes
                  ( TypeDecl | EnumDecl | IfaceResource | FnHead )
IfaceResource ::= "resource" UIDENT ( nl '{' nl List<IfaceResourceItem, NL>? nl '}' )?
IfaceResourceItem ::= OuterDocs Attributes FnHead

WorldDecl     ::= "world" LIDENT nl '{' nl List<WorldItem, NL>? nl '}'
WorldItem     ::= OuterDocs ( "import" WorldRef
                            | 'export' WorldRef
                            | "import" FnHead
                            | 'export' FnHead
                            | "include" WorldRef )
WorldRef      ::= PKG_PATH | ModPath
```

- Functions inside an interface have no body (`FnHead` only). A `TypeDecl` inside an interface may only be a record (StructDef) or an alias; refined types are not allowed.
- Among the functions of an `IfaceResource`: a function with a `self` parameter is a method. A function named `new` without `self` whose return type is `Self` (or that resource type) is the WIT constructor. Every other function is a static function ([13-components-and-wit.md](13-components-and-wit.md)).
- A `WorldRef` that is a `PKG_PATH` refers to an external interface or world, and a `ModPath` refers to an interface or world of the current package, or to an external interface that a `use` has bound in the module ([04-modules-and-names.md](04-modules-and-names.md) §4.3.2). The `import` and `export` of a world must be **per interface**. Naming a whole package (`wasi:http`) is the error `WORLD_IMPORT_NEEDS_INTERFACE` (D-WIT-03).
- `import FnHead` / `export FnHead` declare functions that belong to the world directly (WIT `import f: func(..)` / `export f: func(..)`). The implementation of an exported function is the function of the same name in the root module of the same package.

---

## 3.10 Extern blocks

```ebnf
ExternBlock   ::= 'unsafe'? 'extern' STRING ( "module" STRING )?
                  nl '{' nl List<ExternItem, NL>? nl '}'
ExternItem    ::= OuterDocs Attributes FnHead
```

- The only ABI strings allowed are `"wasm"` (Stage 1) and `"C"` (Stage 7) (`UNKNOWN_ABI`).
- `extern "wasm"` requires `module "name"` (the module name of the Wasm import). The function name becomes the field name of the import; it can be changed with `@import_name("..")`.
- `extern "C"` requires `'unsafe'`.

---

## 3.11 Tests (Stage 3)

```ebnf
TestDecl      ::= "test" STRING nl Block
PropertyDecl  ::= "property" STRING '(' List<NamedParam, Comma> ')' nl Block
```

- `test` and `property` are keywords only in item position when followed by a `STRING`.
- The semantics are defined in [14-testing.md](14-testing.md).

---

## 3.12 Statements and blocks

```ebnf
Block         ::= '{' nl ( Stmt ( NL Stmt )* )? nl '}'
Stmt          ::= LetStmt
                | VarStmt
                | AssignStmt
                | AssertStmt
                | ExprStmt

LetStmt       ::= 'let' Pattern ( ':' Type )? '=' nl Expr ( nl 'else' nl Block )?
VarStmt       ::= 'var' LIDENT ( ':' Type )? '=' nl Expr
AssignStmt    ::= Expr AssignOp nl Expr
AssignOp      ::= '=' | '+=' | '-=' | '*=' | '/=' | '%=' | '+%=' | '-%=' | '*%='
                | '&=' | '|=' | '^=' | '<<=' | '>>='
AssertStmt    ::= 'assert' Expr ( ',' Expr )?
ExprStmt      ::= Expr
```

- An initializer is required; `let x: I32` alone cannot be written (`MISSING_INITIALIZER`, D-SYN-10).
- A `LetStmt` with an `else` clause requires a refutable pattern (`LET_ELSE_IRREFUTABLE`), and the else block must diverge, that is, have type `Never` (`LET_ELSE_NOT_DIVERGING`). Both are checked by the type checker (chapter 06).
- An `AssignStmt` is distinguished from an `ExprStmt` by whether an `AssignOp` follows the leading `Expr`. The left-hand side must be a place expression: a local, a field, an index, or a deref (`INVALID_ASSIGN_TARGET`).
- Items (functions, types, and so on) cannot be declared inside blocks in v0.1 (`ITEM_IN_BLOCK`, chapter 06).
- An assignment statement has no value, and assignments cannot appear inside expressions.

---

## 3.13 Expressions

### 3.13.1 Precedence hierarchy

Rules higher in the list bind more loosely (D-SYN-05).

```ebnf
Expr          ::= OrExpr
OrExpr        ::= AndExpr ( 'or' AndExpr )*
AndExpr       ::= NotExpr ( 'and' NotExpr )*
NotExpr       ::= 'not' NotExpr | CmpExpr
CmpExpr       ::= RangeExpr ( CmpOp RangeExpr )?                  (* non-associative *)
CmpOp         ::= '==' | '!=' | '<' | '<=' | '>' | '>='
RangeExpr     ::= BitOrExpr ( RangeOp BitOrExpr? )?               (* non-associative *)
                | RangeOp BitOrExpr?
RangeOp       ::= '..' | '..='
BitOrExpr     ::= BitXorExpr ( '|' BitXorExpr )*
BitXorExpr    ::= BitAndExpr ( '^' BitAndExpr )*
BitAndExpr    ::= ShiftExpr ( '&' ShiftExpr )*
ShiftExpr     ::= AddExpr ( ( '<<' | '>>' ) AddExpr )*
AddExpr       ::= MulExpr ( ( '+' | '-' | '+%' | '-%' ) MulExpr )*
MulExpr       ::= PrefixExpr ( ( '*' | '/' | '%' | '*%' ) PrefixExpr )*
PrefixExpr    ::= ( '-' | '~' | '*' | 'borrow' 'mut'? ) PrefixExpr
                | PostfixExpr
```

- `a < b < c` and `a == b == c` are the syntax error `CHAINED_COMPARISON`. Likewise, `a..b..c` is `CHAINED_RANGE`.
- The right-hand side of `..=` cannot be omitted (`a..=` is not allowed).
- `>`, `>=`, `>>`, and `>>=` are composed from `GT` tokens using their `joint` flags (02 §2.8).
- The operand of `borrow` / `borrow mut` must be a place expression (D-OWN-02). The grammar also accepts a borrow of a temporary; the lifetime rules for that case are in chapter 07.

### 3.13.2 Postfix expressions

```ebnf
PostfixExpr   ::= PrimaryExpr PostfixOp*
PostfixOp     ::= '.' Ident GenericArgsExpr?               (* field / method / module item / associated item *)
                | '.' INT                                  (* tuple index *)
                | '.' 'await'                              (* Stage 5 *)
                | CallArgs
                | '[' Expr ']'
                | '?'
CallArgs      ::= '(' List<Arg, Comma>? ')'
Arg           ::= ( LIDENT ':' )? Expr
                | '..' Expr                                (* record update: User(name: n, ..base) *)
```

- Syntactically, `a.b` does not distinguish field access, module qualification, and associated item access. Name resolution (chapter 04) tells them apart.
- An `Arg` may carry a name (`LIDENT ':'`) only when constructing a record (a struct or a record variant). A named argument in an ordinary function call is the error `NAMED_ARGUMENT_ON_FUNCTION`. `'..' Expr` may appear only as the last argument of a record construction (`BASE_ARG_NOT_LAST`).
- The `INT` of `'.' INT` must be decimal and must not contain a prefix or `_` (`INVALID_TUPLE_INDEX`, chapter 06).

### 3.13.3 Generic arguments in expressions

```ebnf
GenericArgsExpr ::= '<' List<GenericArg, Comma> nl '>'
```

**Disambiguation rule** (D-SYN-04): when the parser sees `<` directly after an `Ident` in an expression (in the path of a `PrimaryExpr`, or right after `'.' Ident`), it decides the interpretation as follows.

1. It parses, **speculatively** and with the type syntax, the `GenericArg` list starting at `<` up to the closing `>`.
2. If this succeeds and the token right after the closing `>` belongs to the set F below, the list is taken as generic arguments.
   `F = { '(' , '.' , ')' , ']' , '}' , ',' , '?' , NL , '=>' , '{' , EOF }`
3. Otherwise the speculative parse is discarded and `<` is parsed as a comparison operator.

Comparisons are non-associative, so this rule never changes the meaning of a valid program (D-SYN-04).

### 3.13.4 Primary expressions

```ebnf
PrimaryExpr   ::= Literal
                | Ident GenericArgsExpr?                   (* name: module, type, value, or function *)
                | 'self'
                | 'Self'
                | ImplicitMember
                | ParenOrTuple
                | ArrayExpr
                | Block
                | 'unsafe' Block
                | 'async' 'move'? Block                    (* Stage 5 *)
                | IfExpr
                | MatchExpr
                | LoopExpr
                | WhileExpr
                | ForExpr
                | ClosureExpr
                | RegionExpr
                | 'return' Expr?
                | 'break' LABEL? Expr?
                | 'continue' LABEL?

ImplicitMember ::= '.' UIDENT                              (* enum variant of the expected type *)

ParenOrTuple  ::= '(' ')'                                  (* unit *)
                | '(' Expr ')'                             (* parenthesized *)
                | '(' Expr ',' ')'                         (* one-element tuple *)
                | '(' Expr ( ',' Expr )+ ','? ')'          (* tuple *)

ArrayExpr     ::= '[' List<Expr, Comma>? ']'

Literal       ::= INT | FLOAT | STRING | FSTRING | BYTE_STRING | CHAR | BYTE
                | 'true' | 'false'
```

- `return`, `break`, and `continue` belong to E (02 §2.10), so at the end of a line they end the expression. The expression after `return` must start on the same line.
- An `ImplicitMember` may be followed by `CallArgs` (the payload of a tuple or record variant). This is parsed as a postfix operation (§3.13.2).
- An `ImplicitMember` without an expected type to resolve against is the error `IMPLICIT_MEMBER_NEEDS_TYPE` (chapter 06).

### 3.13.5 Control expressions

```ebnf
IfExpr        ::= 'if' Condition Block ( nl 'else' ( IfExpr | Block ) )?
Condition     ::= 'let' Pattern '=' Expr
                | Expr

MatchExpr     ::= 'match' Expr nl '{' nl List<MatchArm, Sep>? nl '}'
MatchArm      ::= Pattern ( 'if' Expr )? '=>' nl Expr

LoopExpr      ::= LoopLabel? 'loop' Block
WhileExpr     ::= LoopLabel? 'while' Condition Block
ForExpr       ::= LoopLabel? 'for' 'await'? Pattern 'in' Expr Block      (* for await: Stage 5 *)
LoopLabel     ::= LABEL ':' nl

ClosureExpr   ::= 'move'? 'fn' '(' List<ClosureParam, Comma>? ')' ( '->' TypeNoFx )? Block
ClosureParam  ::= 'var'? ( LIDENT | '_' ) ( ':' Type )?

RegionExpr    ::= "region" LIDENT Block
```

- An expression in condition position (`Condition`, the scrutinee of `match`, and the expression after `in` in `for`) must not start with a `Block` (`BLOCK_IN_CONDITION`). It can be written by parenthesizing it. This restriction exists for readability.
- In an `IfExpr`, `else` may be on a different line from the preceding `}`. Because `else` is in the continuation set C, no NEWLINE is generated (02 §2.10).
- Match arms are separated by NEWLINE or a comma. When an arm body is a `Block`, a line break after its `}` separates it from the next arm.
- The `region` of a `RegionExpr` is a keyword only at the start of an expression when followed by `LIDENT` `'{'`.
- Only `loop`, `while`, and `for` may carry a `LABEL`.

---

## 3.14 Patterns

```ebnf
Pattern       ::= '|'? AltPattern ( '|' AltPattern )*
AltPattern    ::= BindingPattern '@' PrimaryPattern            (* name @ pat *)
                | PrimaryPattern

PrimaryPattern ::= '_'
                | BindingPattern
                | LiteralPattern
                | RangePattern
                | TuplePattern
                | SlicePattern
                | CtorPattern
                | ImplicitMemberPattern
                | '(' Pattern ')'

BindingPattern ::= BindingMode? LIDENT
BindingMode   ::= 'borrow' 'mut'? | 'var'

LiteralPattern ::= '-'? INT | STRING | BYTE_STRING | CHAR | BYTE | 'true' | 'false'
RangePattern  ::= RangeEnd '..=' RangeEnd
                | RangeEnd '..' RangeEnd?
                | '..=' RangeEnd
RangeEnd      ::= '-'? INT | CHAR | BYTE | ConstPath
ConstPath     ::= ( LIDENT '.' )* UIDENT ( '.' UIDENT )*

TuplePattern  ::= '(' ')'
                | '(' Pattern ',' ')'
                | '(' Pattern ( ',' Pattern )+ ','? ')'

SlicePattern  ::= '[' List<SliceElem, Comma>? ']'
SliceElem     ::= Pattern
                | '..' ( BindingMode? LIDENT )?                (* ignore the rest, or bind the rest as a subslice *)

CtorPattern   ::= ConstPath ( '(' CtorPatArgs? ')' )?
ImplicitMemberPattern ::= '.' UIDENT ( '(' CtorPatArgs? ')' )?
CtorPatArgs   ::= List<CtorPatArg, Comma> ( ',' '..' )?
                | '..'
CtorPatArg    ::= LIDENT ':' Pattern                         (* named: record *)
                | Pattern                                    (* positional: tuple variant / distinct / punning *)
```

- **Case rule** (D-LEX-02): a bare `LIDENT` in a pattern always introduces a new binding. A bare `UIDENT` (or a `ConstPath`) refers to a unit variant, a const, or a constructor without arguments.
- A `ConstPath` denotes a type path (`http.Status`) or a member of one (`http.Status.Ok`). Name resolution must resolve it to a variant, a distinct constructor, a struct constructor, or a const.
- In a record pattern, a `CtorPatArg` that consists only of an unnamed `BindingPattern` is punning: `User(id, borrow name)` means `User(id: id, name: borrow name)`.
- A pattern cannot bind the same name twice. The alternatives of an or-pattern, however, must each bind the same set of names with the same types and binding modes (`OR_PATTERN_BINDING_MISMATCH`).
- A `SlicePattern` may contain at most one `..` element (`MULTIPLE_REST_PATTERNS`).
- Floating-point literal patterns cannot be written (`FLOAT_PATTERN`). A parser accepts a `FLOAT` in pattern position only to report this error.

---

## 3.15 Types

```ebnf
Type          ::= TypeNoFx | FnPtrTypeFx
TypeNoFx      ::= TypePath
                | 'Self' ( '.' UIDENT )*
                | 'borrow' 'mut'? Type
                | 'owned' TypeNoFx
                | TupleType
                | '(' Type ')'                                (* grouping *)
                | '_'                                         (* inferred (only in annotations inside expressions) *)
                | FnPtrType

TypePath      ::= ( LIDENT '.' )* UIDENT GenericArgs? ( '.' UIDENT GenericArgs? )*
GenericArgs   ::= '<' List<GenericArg, Comma> nl '>'
GenericArg    ::= Type
                | INT | '-' INT | CHAR | 'true' | 'false'      (* const argument *)
                | UIDENT '=' Type                             (* associated type binding (only in bounds) *)

TupleType     ::= '(' ')'
                | '(' Type ',' ')'
                | '(' Type ( ',' Type )+ ','? ')'

FnPtrType     ::= 'unsafe'? 'fn' '(' List<Type, Comma>? ')' ( '->' TypeNoFx )?
FnPtrTypeFx   ::= FnPtrType EffectsClause
```

- A `TypePath` denotes `module.Type`, `Type<Args>`, or `T.Item` (an associated type). The sequence of `LIDENT`s before the first `UIDENT` is a module path.
- **`TypeNoFx` vs `Type`**: `Type` includes function pointer types with an effects clause. The return type of a function declaration, the return type of a closure, the return type of a function pointer type, and the bounds of a where clause use `TypeNoFx`. An `effects` that follows such a position belongs to the enclosing construct. To write a function pointer type with effects as the return type of another function pointer type, parenthesize it.

  ```sai
  fn add_one(x: I32) -> I32 {
      x + 1
  }

  // `alloc` is an effect of `make_adder`, and the returned function pointer is pure.
  // `fn make_adder() -> fn(I32) -> I32 effects { alloc }` on one line means the same.
  pub fn make_adder() -> fn(I32) -> I32
      effects { alloc }
  {
      add_one
  }

  // `alloc` is an effect of the returned function pointer.
  pub fn make_alloc_adder() -> (fn(I32) -> I32 effects { alloc }) {
      add_one
  }

  pub fn apply_twice(x: I32) -> I32
      effects { alloc }
  {
      // A let annotation is a `Type`, so the effect belongs to the function pointer.
      let f: fn(I32) -> I32 effects { alloc } = add_one
      f(f(x))
  }
  ```
- A function pointer type without an effects clause is pure (has no effects) (D-EFF-02).
- `'_'` may be used only in type annotations inside expressions and in generic arguments inside expressions. It cannot be used in item signatures (`INFER_IN_SIGNATURE`).
- `owned T` is accepted as a synonym of `T` (D-OWN-01). The formatter preserves `owned`.

---

## 3.16 Syntactic diagnostics

Diagnostics of the syntax of this chapter. All are errors and none is a lint. Most are reported by the parser or by the syntactic validation that follows it ([impl/02](../impl/02-frontend-ast-hir.md) §2.5.12). `INVARIANT_OUTSIDE_DEFINING_MODULE` and the cases of `OR_PATTERN_BINDING_MISMATCH`, `NAMED_ARGUMENT_ON_FUNCTION` and `INVALID_ASSIGN_TARGET` that depend on what a name denotes or on types are decided after name resolution and type checking ([impl/03](../impl/03-name-resolution-and-type-checking.md)).

| Code | Severity | Description |
|---|---|---|
| `EXPECTED_TOKEN` | error | an expected token is missing (the data includes the set of expected tokens) |
| `EXPECTED_EXPRESSION` | error | an expression is required but missing |
| `UNEXPECTED_NEWLINE` | error | defined in [02](02-lexical-structure.md) §2.12; a NEWLINE in a position where the grammar does not allow it ([02](02-lexical-structure.md) §2.10) |
| `CHAINED_COMPARISON` | error | chained comparison operators (§3.13.1) |
| `CHAINED_RANGE` | error | chained range operators, such as `a..b..c` (§3.13.1) |
| `CLAUSE_ORDER` | error | function signature clauses out of order (§3.5) |
| `DUPLICATE_CLAUSE` | error | a second `where`, `borrows`, or `effects` clause on one function (§3.5) |
| `MISSING_FN_BODY` | error | a function that needs a body has none (§3.5) |
| `UNEXPECTED_FN_BODY` | error | a body on a function of an extern block (§3.5, §3.10) |
| `INVALID_SELF_TYPE` | error | invalid type for a self parameter (§3.5) |
| `NAMED_ARGUMENT_ON_FUNCTION` | error | named argument in an ordinary function call (§3.13.2) |
| `BASE_ARG_NOT_LAST` | error | a `..base` argument that is not the last argument (§3.13.2) |
| `INVALID_ASSIGN_TARGET` | error | the left-hand side of an assignment is not a place expression (§3.12) |
| `MISSING_INITIALIZER` | error | `let` or `var` without `=` (§3.12) |
| `BLOCK_IN_CONDITION` | error | an expression in condition position starts with `{` (§3.13.5) |
| `REDUNDANT_PUB_EXPORT` | error | `pub export` (§3.3) |
| `INVALID_EXPORT_TARGET` | error | `export` on something other than a function (§3.3) |
| `EXPLICIT_DISCRIMINANT_ON_DATA_ENUM` | error | explicit discriminant on an enum with payloads (§3.6) |
| `UNKNOWN_ATTRIBUTE` | error | an attribute name outside the closed set of 15 §15.2.3 (§3.3) |
| `UNKNOWN_ABI` | error | unknown extern ABI (§3.10) |
| `WORLD_IMPORT_NEEDS_INTERFACE` | error | a world import or export that is not per interface (§3.9) |
| `INFER_IN_SIGNATURE` | error | `_` type in a signature (§3.15) |
| `VISIBILITY_ON_TRAIT_IMPL_ITEM` | error | `pub` on an item of a trait or interface impl (§3.7) |
| `OR_PATTERN_BINDING_MISMATCH` | error | or-pattern alternatives bind different names, or one name with different types or binding modes (§3.14) |
| `MULTIPLE_REST_PATTERNS` | error | more than one `..` in a slice pattern (§3.14) |
| `FLOAT_PATTERN` | error | a floating-point literal in a pattern (§3.14) |
| `INVARIANT_OUTSIDE_DEFINING_MODULE` | error | invariant placed outside the module that defines the type (§3.8) |

Codes named in this chapter but defined elsewhere: `IMPLICIT_MEMBER_NEEDS_TYPE`, `LET_ELSE_IRREFUTABLE`, `LET_ELSE_NOT_DIVERGING`, `ITEM_IN_BLOCK`, and `INVALID_TUPLE_INDEX` ([06](06-expressions-and-statements.md)); `PUB_USE_WITHOUT_ITEMS` ([04](04-modules-and-names.md)); `GENERIC_REFINED_TYPE` ([05](05-types.md)); `INVALID_ATTRIBUTE_TARGET` ([15](15-attributes-and-lints.md)); `INTERFACE_FN_BODY` ([13](13-components-and-wit.md)). The complete catalog is [appendix A](appendix-a-diagnostic-codes.md).

---

## 3.17 Example: parsing the draft's main examples

The draft's main examples, corrected as in [design/draft-reconciliation.md](../design/draft-reconciliation.md) §1:

```sai noncanonical
//! User lookup

use acme:image/model.{Image}
use sai:http/client.{HttpClient}

pub distinct UserId = U64

pub type User {
    id: UserId
    name: String
}

pub enum UserError {
    NotFound
    Network
    InvalidResponse
}

type Percentage = I32 where self >= 0 and self <= 100

@derive(Copy, Clone, PartialEq)
enum Status {
    Pending
    Active
    Disabled(reason: Str)
}

@derive(Copy, Clone, PartialEq, PartialOrd, Sub)
distinct Money = I64

pub fn width(image: borrow Image) -> U32 {
    image.width
}

pub async fn fetch_user(
    http: borrow HttpClient,
    id: UserId,
) -> Result<User, UserError>
    effects { alloc, net.http }
{
    let path = "/users/" + id.value.to_string().as_str()
    let response = http.get(path.as_str()).await.map_err(fn(_) { UserError.Network })?
    response.json<User>().map_err(fn(_) { UserError.InvalidResponse })
}

fn withdraw(balance: Money, amount: Money) -> Money
    requires amount <= balance
    ensures result >= Money(0)
{
    balance - amount
}
```

- In `pub async fn fetch_user(`, line breaks inside `(` are ignored. The `->` after `)` is on the same line. A NEWLINE is generated after `UserError>`, and the `nl` of `FnClauses` skips it. The NEWLINE after `effects { alloc, net.http }` is skipped by the `nl` of `FnBody`. (`sai fmt` would print the head of `fetch_user` on one line, because it fits. It is broken here to show the NEWLINE rules, so the block is marked `noncanonical`, [impl/15](../impl/15-conformance-and-testing.md) §15.8.3 item 4.)
- No NEWLINE is generated after `=` in a type alias, because `=` is not in E. `type Percentage =` followed by the type on the next line parses the same way.
- `UserId` is a distinct type and inherits no methods of `U64` (D-TYPE-03), so the conversion goes through `id.value`. The concatenation `"/users/" + id.value.to_string().as_str()` is `Str + Str`, which produces a `String` (chapter 06). Its `Add` impl lives in the alloc library and has the `alloc` effect (D-EXPR-13), which is why `fetch_user` declares `alloc`.
- `?` requires the error type of its operand to be the error type of the function (D-EXPR-05), so each library error is mapped to a `UserError` first. The closures `fn(_) { .. }` ignore their argument.
- `response.json<User>()` uses the disambiguation rule of §3.13.3: `<User>` is followed by `(`, so it is a generic argument list. It also requires `User` to implement the decoding trait of the JSON library (O-01 of the reconciliation document).
- `ensures result >= Money(0)` compares two `Money` values: a literal never has a distinct type (D-TYPE-03). The derived `PartialOrd` and `Sub` make `<=`, `>=` and `-` available on `Money` (chapter 05 §5.12).
