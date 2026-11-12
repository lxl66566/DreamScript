//! The platform declaration's parse surface (H5): `platform "std"
//! provides <console, fsys, net, time>`, the provides clause optional
//! (an absent list is the embed target), the name one plain string
//! literal — escapes and interpolation holes are parse errors.

use ds_diag::{ErrorCode, Files};
use ds_parse::{ItemKind, PlatformName, parse};

/// Parses one source as a library file, or renders the failure.
fn ast(src: &str) -> ds_parse::Ast {
    let mut files = Files::new();
    let file = files.add_source("t.ds", src);
    parse(&files, file).unwrap_or_else(|diags| panic!("should parse: {diags:?}"))
}

/// The first parse diagnostic's code.
fn first_code(src: &str) -> ErrorCode {
    let mut files = Files::new();
    let file = files.add_source("t.ds", src);
    let diags = parse(&files, file).expect_err("should not parse");
    diags[0].code
}

/// The single platform declaration of a parsed source.
fn platform(src: &str) -> (String, Vec<String>) {
    let parsed = ast(src);
    let Some(item) = parsed
        .blocks
        .iter()
        .flat_map(|block| block.items.iter())
        .find_map(|item| match &item.kind {
            ItemKind::Platform(p) => Some(p),
            _ => None,
        })
    else {
        panic!("the source declares a platform");
    };
    (
        item.name.name.clone(),
        item.provides.iter().map(|c| c.name.clone()).collect(),
    )
}

#[test]
fn the_full_declaration_parses_with_every_capability() {
    assert_eq!(
        platform("platform \"std\" provides <console, fsys, net, time>"),
        ("std".to_owned(), vec![
            "console".to_owned(),
            "fsys".to_owned(),
            "net".to_owned(),
            "time".to_owned()
        ]),
    );
}

#[test]
fn an_absent_provides_list_is_the_embed_target() {
    assert_eq!(
        platform("platform \"embed\""),
        ("embed".to_owned(), Vec::new())
    );
}

#[test]
fn a_trailing_comma_parses_like_imports_select() {
    assert_eq!(
        platform("platform \"web\" provides <console, net,>"),
        ("web".to_owned(), vec![
            "console".to_owned(),
            "net".to_owned()
        ])
    );
}

#[test]
fn an_interpolating_name_is_not_a_plain_literal() {
    assert_eq!(
        first_code("platform \"std$env\" provides <console>"),
        ErrorCode::InvalidLiteral
    );
}

#[test]
fn an_escaped_name_is_not_a_plain_literal() {
    assert_eq!(
        first_code("platform \"std\\n\" provides <console>"),
        ErrorCode::InvalidLiteral
    );
}

#[test]
fn an_empty_name_is_not_a_platform() {
    assert_eq!(
        first_code("platform \"\" provides <console>"),
        ErrorCode::InvalidLiteral
    );
}

#[test]
fn a_declaration_sits_among_other_items() {
    let src = concat!(
        "platform \"std\" provides <console>\n",
        "tag io_err\n",
        "extern fn println (msg: string) -> ()\n",
    );
    let parsed = ast(src);
    let kinds: Vec<bool> = parsed
        .blocks
        .iter()
        .flat_map(|block| block.items.iter())
        .map(|item| matches!(item.kind, ItemKind::Platform(_)))
        .collect();
    assert_eq!(kinds, vec![true, false, false]);
}

/// The parsed name carries the literal's span, for the checker's
/// diagnostics to address.
#[test]
fn the_name_span_addresses_the_literal() {
    let parsed = ast("platform \"std\" provides <console>");
    let item = parsed
        .blocks
        .iter()
        .flat_map(|block| block.items.iter())
        .find_map(|item| match &item.kind {
            ItemKind::Platform(p) => Some(p),
            _ => None,
        })
        .expect("the source declares a platform");
    let PlatformName { span, .. } = &item.name;
    let mut files = Files::new();
    files.add_source("t.ds", "platform \"std\" provides <console>");
    let source = files.source(span.file);
    assert_eq!(&source[span.start as usize..span.end as usize], "\"std\"");
}
