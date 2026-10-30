use super::*;
use crate::domain::package::DeclarationKind;
use crate::domain::resolution::{InstallCheck, PackageRequest, check_install};

const DOCUMENT: NixPackageDocument = NixPackageDocument;

fn name(package: &str) -> PackageName {
    PackageName::parse(package).unwrap()
}

/// What installing `package` would do to `source`.
#[derive(Debug, PartialEq, Eq)]
enum Addition {
    AlreadyPresent,
    Edited(String),
}

fn plan_addition(source: &str, package: &PackageName) -> Result<Addition, EditError> {
    let declared = DOCUMENT.analyze(source)?;
    let request = PackageRequest::resolve(package, &declared.aliases);
    match check_install(&declared.declarations, &request.identity) {
        InstallCheck::AlreadyDeclared(_) => Ok(Addition::AlreadyPresent),
        _ => DOCUMENT
            .add(source, &request.identity)
            .map(Addition::Edited),
    }
}

fn edited(source: &str, package: &str) -> String {
    match plan_addition(source, &name(package)).unwrap() {
        Addition::Edited(content) => content,
        Addition::AlreadyPresent => panic!("{package} was already present"),
    }
}

fn is_present(source: &str, package: &str) -> bool {
    plan_addition(source, &name(package)).unwrap() == Addition::AlreadyPresent
}

fn refusal(source: &str) -> String {
    match plan_addition(source, &name("foo")) {
        Err(EditError::UnsupportedStructure(reason)) => reason,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn adds_to_home_packages_module_with_let_and_comments() {
    let source = "\
{ pkgs, inputs, ... }:

let
  system = pkgs.stdenv.hostPlatform.system;
  unstable = import inputs.nixpkgs-unstable {
    inherit system;
    config.allowUnfree = true;
  };
in

{
  home.packages = with pkgs; [
    # Development
    git
    unstable.vscode

    # Comes from a flake input
    inputs.spotifast.packages.${pkgs.stdenv.hostPlatform.system}.default
    yt-dlp
  ];
}
";
    let expected = source.replace("    yt-dlp\n", "    yt-dlp\n    foo\n");
    assert_eq!(edited(source, "foo"), expected);
}

#[test]
fn adds_to_environment_system_packages_with_prefix() {
    let source = "{ pkgs, ... }:\n{\n  environment.systemPackages = [\n    pkgs.git\n  ];\n}\n";
    let expected = source.replace("    pkgs.git\n", "    pkgs.git\n    pkgs.foo\n");
    assert_eq!(edited(source, "foo"), expected);
}

#[test]
fn adds_to_bare_lists() {
    assert_eq!(
        edited("{ pkgs }: with pkgs; [\n  git\n]\n", "foo"),
        "{ pkgs }: with pkgs; [\n  git\n  foo\n]\n"
    );
    assert_eq!(
        edited("pkgs: [\n  pkgs.git\n]\n", "foo"),
        "pkgs: [\n  pkgs.git\n  pkgs.foo\n]\n"
    );
    assert_eq!(
        edited("{ pkgs }: with pkgs; [\n  git\n]\n", "foo"),
        "{ pkgs }: with pkgs; [\n  git\n  foo\n]\n"
    );
    assert_eq!(
        edited("{ pkgs }:\nlet x = 1; in\nwith pkgs; [\n  git\n]\n", "foo"),
        "{ pkgs }:\nlet x = 1; in\nwith pkgs; [\n  git\n  foo\n]\n"
    );
}

#[test]
fn adds_to_single_line_lists() {
    assert_eq!(
        edited(
            "{ pkgs, ... }: { home.packages = with pkgs; [ git vim ]; }",
            "foo"
        ),
        "{ pkgs, ... }: { home.packages = with pkgs; [ git vim foo ]; }"
    );
    assert_eq!(
        edited("{ pkgs, ... }: { home.packages = with pkgs; []; }", "foo"),
        "{ pkgs, ... }: { home.packages = with pkgs; [ foo ]; }"
    );
    assert_eq!(
        edited("{ pkgs, ... }: { home.packages = with pkgs; [ ]; }", "foo"),
        "{ pkgs, ... }: { home.packages = with pkgs; [ foo ]; }"
    );
}

#[test]
fn adds_when_closing_bracket_shares_the_last_entry_line() {
    assert_eq!(
        edited("{ pkgs }: with pkgs; [\n  a\n  b ]\n", "foo"),
        "{ pkgs }: with pkgs; [\n  a\n  b foo ]\n"
    );
}

#[test]
fn indents_empty_multiline_lists_from_the_closing_bracket() {
    assert_eq!(
        edited(
            "{ pkgs, ... }: {\n  home.packages = with pkgs; [\n  ];\n}\n",
            "foo"
        ),
        "{ pkgs, ... }: {\n  home.packages = with pkgs; [\n    foo\n  ];\n}\n"
    );
    assert_eq!(
        edited("{ pkgs }: with pkgs; [\n  # nothing yet\n]\n", "foo"),
        "{ pkgs }: with pkgs; [\n  # nothing yet\n  foo\n]\n"
    );
}

#[test]
fn preserves_tabs_crlf_and_missing_final_newline() {
    assert_eq!(
        edited("{ pkgs }: with pkgs; [\n\tgit\n]", "foo"),
        "{ pkgs }: with pkgs; [\n\tgit\n\tfoo\n]"
    );
    assert_eq!(
        edited("{ pkgs }: with pkgs; [\r\n  git\r\n]\r\n", "foo"),
        "{ pkgs }: with pkgs; [\r\n  git\r\n  foo\r\n]\r\n"
    );
}

#[test]
fn keeps_a_trailing_comment_on_the_last_entry() {
    assert_eq!(
        edited(
            "{ pkgs }: with pkgs; [\n  git # version control\n]\n",
            "foo"
        ),
        "{ pkgs }: with pkgs; [\n  git # version control\n  foo\n]\n"
    );
}

#[test]
fn uses_the_line_indent_after_a_multiline_entry() {
    let source = "{ pkgs }: with pkgs; [\n  (foo.override {\n    x = 1;\n  })\n]\n";
    assert_eq!(
        edited(source, "bar"),
        "{ pkgs }: with pkgs; [\n  (foo.override {\n    x = 1;\n  })\n  bar\n]\n"
    );
}

#[test]
fn inserting_twice_is_idempotent() {
    let once = edited("{ pkgs }: with pkgs; [\n  git\n]\n", "foo");
    assert!(is_present(&once, "foo"));
}

#[test]
fn detects_duplicates_in_all_supported_spellings() {
    assert!(is_present("{ pkgs }: with pkgs; [ git ]", "git"));
    assert!(is_present("{ pkgs }: with pkgs; [ pkgs.git ]", "git"));
    assert!(is_present("{ pkgs }: [ pkgs.git ]", "git"));
    assert!(is_present("{ pkgs }: [ pkgs . git ]", "git"));
    assert!(is_present("{ pkgs }: with pkgs; [ git ]", "pkgs.git"));
    assert!(is_present(
        "{ pkgs }: with pkgs; [ kdePackages.kate ]",
        "kdePackages.kate"
    ));
    assert!(is_present("{ pkgs }: with pkgs; [ (f x) git ]", "git"));
}

#[test]
fn does_not_confuse_other_text_with_a_duplicate() {
    for source in [
        "{ pkgs }: with pkgs; [\n  # git\n]\n",
        "{ pkgs }: with pkgs; [\n  /* git */\n]\n",
        "{ pkgs }: with pkgs; [ \"git\" ]",
        "{ pkgs }: with pkgs; [ git-lfs ]",
        "{ pkgs }: with pkgs; [ unstable.git ]",
        "{ pkgs }: with pkgs; [ git.foo ]",
        "{ pkgs }: with pkgs; [ (f git) ]",
        "{ pkgs }: with pkgs; [ (python3.withPackages (ps: [ ps.git ])) ]",
        "{ pkgs }: with pkgs; [ inputs.x.packages.${system}.git ]",
    ] {
        assert!(!is_present(source, "git"), "{source}");
    }
}

#[test]
fn ignores_target_names_in_comments_and_strings() {
    let source = "{ pkgs, ... }: {\n  # home.packages = [ old ];\n  x = \"home.packages = [ s ];\";\n  home.packages = with pkgs; [\n    git\n  ];\n}\n";
    assert!(edited(source, "foo").contains("    git\n    foo\n"));
}

#[test]
fn skips_function_defaults_and_let_bindings_before_the_module() {
    let source = "{ pkgs ? import <nixpkgs> { }, ... }:\nlet a = { b = [ 1 ]; }; in {\n  home.packages = [\n    pkgs.git\n  ];\n}\n";
    assert!(edited(source, "foo").contains("    pkgs.git\n    pkgs.foo\n"));
}

#[test]
fn refuses_when_no_list_is_recognized() {
    for source in [
        "",
        "{ }",
        "{ pkgs, ... }: { }",
        "{ pkgs, ... }: { fonts.packages = [ ]; }",
        "{ pkgs, ... }: { x.home.packages = [ ]; }",
    ] {
        assert!(
            refusal(source).contains("no package list"),
            "{source}: {}",
            refusal(source)
        );
    }
}

#[test]
fn refuses_expressions_it_cannot_edit_safely() {
    for source in [
        "{ pkgs }: with pkgs; [ a ] ++ [ b ]",
        "[ pkgs.a ] ++ extra",
        "{ pkgs, ... }: { home.packages = with pkgs; [ a ] ++ [ b ]; }",
        "{ pkgs, ... }: { home.packages = (with pkgs; [ a ]); }",
        "{ pkgs, ... }: { home.packages = lib.mkForce [ a ]; }",
        "{ pkgs, ... }: { home.packages = with lib; [ a ]; }",
        "{ pkgs, ... }: { home.packages = with pkgs; with lib; [ a ]; }",
        "{ pkgs, ... }: { home.packages = extra; }",
        "{ pkgs, ... }: { home.packages = if x then [ a ] else [ b ]; }",
    ] {
        refusal(source);
    }
}

#[test]
fn refuses_nested_or_conditional_attributes() {
    for source in [
        "{ pkgs, lib, ... }: lib.mkIf true { home.packages = [ ]; }",
        "{ pkgs, ... }: let home.packages = [ ]; in { }",
        "{ pkgs, ... }: { x = let home.packages = [ ]; in 1; }",
    ] {
        assert!(refusal(source).contains("top level"), "{source}");
    }
}

#[test]
fn refuses_more_than_one_list() {
    let source =
        "{ pkgs, ... }: {\n  home.packages = [ ];\n  environment.systemPackages = [ ];\n}\n";
    assert!(refusal(source).contains("2 package lists"));
    let nested = "{ pkgs, lib, ... }: {\n  home.packages = [ ];\n  x = lib.mkIf true { home.packages = [ ]; };\n}\n";
    assert!(refusal(nested).contains("2 package lists"));
}

#[test]
fn refuses_unbalanced_or_unterminated_files() {
    for source in [
        "{ pkgs, ... }: { home.packages = [ a; }",
        "{ pkgs, ... }: { home.packages = [ a ]; ",
        "{ pkgs, ... }: { home.packages = [ \"a ]; }",
        "{ pkgs }: with pkgs; [ a /* ]",
        "let a = 1; { home.packages = [ ]; }",
    ] {
        refusal(source);
    }
}

#[test]
fn refuses_prefixed_entry_without_pkgs_in_scope() {
    assert!(refusal("{ git }: [ git ]").contains("pkgs"));
    assert!(refusal("[ ]").contains("pkgs"));
}

#[test]
fn accepts_common_module_shapes() {
    for (source, expected) in [
        (
            "{ pkgs, ... }: with pkgs; { environment.systemPackages = [ git ]; }",
            "[ git foo ]",
        ),
        (
            "{ lib, pkgs, ... }: with lib; { environment.systemPackages = with pkgs; [ git ]; }",
            "[ git foo ]",
        ),
        (
            "{ lib, pkgs, ... }: with pkgs; with lib; { home.packages = [ git ]; }",
            "[ git pkgs.foo ]",
        ),
        ("args@{ pkgs, ... }: with pkgs; [ git ]", "[ git foo ]"),
        ("{ pkgs, ... }@args: [ pkgs.git ]", "[ pkgs.git pkgs.foo ]"),
        (
            "args@{ pkgs, ... }: { home.packages = [ pkgs.git ]; }",
            "[ pkgs.git pkgs.foo ]",
        ),
        (
            "{ pkgs, ... }: { home = { packages = [ pkgs.git ]; }; }",
            "[ pkgs.git pkgs.foo ]",
        ),
        (
            "{ pkgs, ... }: { environment = { systemPackages = [ pkgs.git ]; }; }",
            "[ pkgs.git pkgs.foo ]",
        ),
        (
            "{ pkgs, ... }: { users.users.me.packages = with pkgs; [ git ]; }",
            "[ git foo ]",
        ),
        (
            "{ pkgs, ... }: { users = { users.me = { packages = [ pkgs.git ]; }; }; }",
            "[ pkgs.git pkgs.foo ]",
        ),
        (
            "{ pkgs, ... }: { config.environment.systemPackages = [ pkgs.git ]; }",
            "[ pkgs.git pkgs.foo ]",
        ),
        (
            "{ pkgs, ... }: { config = { home.packages = [ pkgs.git ]; }; }",
            "[ pkgs.git pkgs.foo ]",
        ),
    ] {
        let result = edited(source, "foo");
        assert!(result.contains(expected), "{source} -> {result}");
    }
}

#[test]
fn refuses_lists_of_several_users() {
    let source = "{ pkgs, ... }: { users.users.a.packages = [ ]; users.users.b.packages = [ ]; }";
    assert!(refusal(source).contains("2 package lists"));
}

#[test]
fn refuses_attribute_of_another_set() {
    assert!(
        refusal("{ pkgs, ... }: { x = { home.packages = [ ]; }; }").contains("no package list")
    );
}

#[test]
fn refuses_with_pkgs_without_pkgs_bound_by_the_file() {
    for source in [
        "with pkgs; [ git ]",
        "with pkgs; { home.packages = [ git ]; }",
        "{ home.packages = with pkgs; [ git ]; }",
    ] {
        assert!(refusal(source).contains("not in scope"), "{source}");
    }
}

#[test]
fn name_bound_by_the_file_is_written_with_the_prefix() {
    assert_eq!(
        edited("{ pkgs, foo }: with pkgs; [\n  git\n]\n", "foo"),
        "{ pkgs, foo }: with pkgs; [\n  git\n  pkgs.foo\n]\n"
    );
    assert_eq!(
        edited(
            "{ pkgs }: let foo = 1; in with pkgs; [\n  git\n]\n",
            "foo.bar"
        ),
        "{ pkgs }: let foo = 1; in with pkgs; [\n  git\n  pkgs.foo.bar\n]\n"
    );
}

const WITH_CHANNEL: &str = "{ pkgs, inputs, ... }:\nlet\n  unstable = import inputs.nixpkgs-unstable { };\nin {\n  home.packages = with pkgs; [\n    git\n    unstable.vscode\n  ];\n}\n";

#[test]
fn channel_entries_are_written_without_the_pkgs_prefix() {
    let source = "{ pkgs, inputs, ... }:\nlet\n  unstable = import inputs.nixpkgs-unstable { };\nin {\n  home.packages = [\n    pkgs.git\n  ];\n}\n";
    assert!(edited(source, "unstable.foo").contains("    pkgs.git\n    unstable.foo\n"));
    assert!(edited(WITH_CHANNEL, "unstable.foo").contains("    unstable.foo\n  ];"));
}

#[test]
fn overridden_package_is_a_duplicate() {
    assert!(is_present(
        "{ pkgs }: with pkgs; [ (git.override { }) ]",
        "git"
    ));
    assert!(is_present(
        "{ pkgs }: [ (pkgs.git.overrideAttrs (o: { })) ]",
        "git"
    ));
}

#[test]
fn same_package_from_another_channel_is_not_a_silent_duplicate() {
    let declared = DOCUMENT.analyze(WITH_CHANNEL).unwrap();
    for package in ["vscode", "pkgs.vscode"] {
        let request = PackageRequest::resolve(&name(package), &declared.aliases);
        assert!(matches!(
            check_install(&declared.declarations, &request.identity),
            InstallCheck::DeclaredElsewhere(_)
        ));
    }
    let same = PackageRequest::resolve(&name("unstable.vscode"), &declared.aliases);
    assert!(matches!(
        check_install(&declared.declarations, &same.identity),
        InstallCheck::AlreadyDeclared(_)
    ));
}

fn declarations(source: &str) -> Vec<(String, Option<String>, DeclarationKind, usize)> {
    DOCUMENT
        .analyze(source)
        .unwrap()
        .declarations
        .into_iter()
        .map(|entry| {
            (
                entry.text,
                entry
                    .identity
                    .map(|identity| format!("{}:{}", identity.source.label(), identity.attr)),
                entry.kind,
                entry.line,
            )
        })
        .collect()
}

#[test]
fn analyze_recognizes_every_entry_form() {
    let source = "{ pkgs, inputs, ... }:\nlet\n  unstable = import inputs.nixpkgs-unstable { };\nin {\n  home.packages = with pkgs; [\n    # a comment\n    git\n    pkgs.vim\n    unstable.vscode\n    kdePackages.kate\n    inputs.spotifast.packages.${pkgs.system}.default\n    (hello.override { x = 1; })\n    (python3.withPackages (ps: [ ps.foo ]))\n    \"text\"\n    a.b or c\n  ];\n}\n";
    let found = declarations(source);
    let expected = [
        ("git", Some("nixpkgs:git"), DeclarationKind::Plain, 7),
        ("pkgs.vim", Some("nixpkgs:vim"), DeclarationKind::Plain, 8),
        (
            "unstable.vscode",
            Some("unstable:vscode"),
            DeclarationKind::Plain,
            9,
        ),
        (
            "kdePackages.kate",
            Some("nixpkgs:kdePackages.kate"),
            DeclarationKind::Plain,
            10,
        ),
        (
            "inputs.spotifast.packages.${pkgs.system}.default",
            Some("spotifast:spotifast"),
            DeclarationKind::Plain,
            11,
        ),
        (
            "(hello.override { x = 1; })",
            Some("nixpkgs:hello"),
            DeclarationKind::Override,
            12,
        ),
        (
            "(python3.withPackages (ps: [ ps.foo ]))",
            Some("nixpkgs:python3.withPackages"),
            DeclarationKind::Expression,
            13,
        ),
        ("\"text\"", None, DeclarationKind::Opaque, 14),
        (
            "a.b or c",
            Some("nixpkgs:a.b"),
            DeclarationKind::Expression,
            15,
        ),
    ];
    assert_eq!(found.len(), expected.len());
    for (actual, wanted) in found.iter().zip(expected) {
        assert_eq!(actual.0, wanted.0);
        assert_eq!(actual.1.as_deref(), wanted.1);
        assert_eq!(actual.2, wanted.2);
        assert_eq!(actual.3, wanted.3);
    }
}

#[test]
fn analyze_marks_untraceable_names_without_with_pkgs() {
    let found = declarations("{ pkgs, mine }: [ mine pkgs.git ]");
    assert_eq!(found[0].1.as_deref(), Some("unknown:mine"));
    assert_eq!(found[1].1.as_deref(), Some("nixpkgs:git"));
}

fn removed(source: &str, text: &str) -> String {
    let declared = DOCUMENT.analyze(source).unwrap();
    let declaration = declared
        .declarations
        .iter()
        .find(|entry| entry.text == text)
        .unwrap_or_else(|| panic!("{text} not found"));
    DOCUMENT.remove(source, declaration).unwrap()
}

#[test]
fn removal_takes_the_whole_line_of_an_entry_alone_on_it() {
    assert_eq!(
        removed("{ pkgs }: with pkgs; [\n  git\n  vim\n]\n", "git"),
        "{ pkgs }: with pkgs; [\n  vim\n]\n"
    );
    assert_eq!(
        removed("{ pkgs }: with pkgs; [\r\n  git\r\n  vim\r\n]\r\n", "git"),
        "{ pkgs }: with pkgs; [\r\n  vim\r\n]\r\n"
    );
}

#[test]
fn removal_keeps_comments_on_other_lines_and_takes_its_own() {
    assert_eq!(
        removed(
            "{ pkgs }: with pkgs; [\n  # editors\n  vim # main\n  # git # disabled\n  git\n]\n",
            "vim"
        ),
        "{ pkgs }: with pkgs; [\n  # editors\n  # git # disabled\n  git\n]\n"
    );
}

#[test]
fn removal_inside_a_line_keeps_its_neighbors() {
    let source = "{ pkgs }: with pkgs; [ a b c ]";
    assert_eq!(removed(source, "a"), "{ pkgs }: with pkgs; [ b c ]");
    assert_eq!(removed(source, "b"), "{ pkgs }: with pkgs; [ a c ]");
    assert_eq!(removed(source, "c"), "{ pkgs }: with pkgs; [ a b ]");
    assert_eq!(
        removed("{ pkgs }: with pkgs; [ a ]", "a"),
        "{ pkgs }: with pkgs; [ ]"
    );
    assert_eq!(
        removed("{ pkgs }: with pkgs; [ a /* note */ b ]", "b"),
        "{ pkgs }: with pkgs; [ a /* note */ ]"
    );
}

#[test]
fn removal_of_an_entry_sharing_the_closing_line() {
    assert_eq!(
        removed("{ pkgs }: with pkgs; [\n  a\n  b ]\n", "b"),
        "{ pkgs }: with pkgs; [\n  a\n  ]\n"
    );
}

#[test]
fn removal_takes_a_whole_multiline_entry() {
    let source = "{ pkgs }: with pkgs; [\n  (foo.override {\n    x = 1;\n  })\n  bar\n]\n";
    assert_eq!(
        removed(source, "(foo.override {\n    x = 1;\n  })"),
        "{ pkgs }: with pkgs; [\n  bar\n]\n"
    );
}

#[test]
fn removal_takes_a_dynamic_path_as_one_entry() {
    let source =
        "{ pkgs, inputs }: with pkgs; [\n  inputs.x.packages.${pkgs.system}.default\n  git\n]\n";
    assert_eq!(
        removed(source, "inputs.x.packages.${pkgs.system}.default"),
        "{ pkgs, inputs }: with pkgs; [\n  git\n]\n"
    );
}

#[test]
fn removal_leaves_the_rest_of_a_real_file_identical() {
    let source = "{ pkgs, inputs, ... }:\n\nlet\n  system = pkgs.stdenv.hostPlatform.system;\n  unstable = import inputs.nixpkgs-unstable {\n    inherit system;\n  };\nin\n\n{\n  home.packages = with pkgs; [\n    # Tools\n    git\n    unstable.vscode\n\n    # Audio\n    mpv\n  ];\n}\n";
    assert_eq!(
        removed(source, "unstable.vscode"),
        source.replace("    unstable.vscode\n", "")
    );
}

#[test]
fn removal_refuses_a_stale_declaration() {
    let source = "{ pkgs }: with pkgs; [ a b ]";
    let mut declaration = DOCUMENT.analyze(source).unwrap().declarations.remove(0);
    declaration.start = 0;
    assert!(DOCUMENT.remove(source, &declaration).is_err());
}

#[test]
fn removal_does_not_touch_similar_names_or_strings() {
    let source = "{ pkgs }: with pkgs; [\n  git\n  git-lfs\n  \"git\"\n  # git\n]\n";
    assert_eq!(
        removed(source, "git"),
        "{ pkgs }: with pkgs; [\n  git-lfs\n  \"git\"\n  # git\n]\n"
    );
}

#[test]
fn add_refuses_what_is_not_an_attribute() {
    let source = "{ pkgs }: with pkgs; [ git ]";
    let identity = PackageIdentity::nixpkgs("evil ] ++ [ pkgs.foo");
    assert!(matches!(
        DOCUMENT.add(source, &identity),
        Err(EditError::InvalidAttribute(_))
    ));
}

#[test]
fn accepts_rec_sets() {
    assert!(
        edited(
            "{ pkgs, ... }: rec { home.packages = [ pkgs.git ]; }",
            "foo"
        )
        .contains("[ pkgs.git pkgs.foo ]")
    );
    assert!(
        edited(
            "{ pkgs, ... }: { home = rec { packages = [ pkgs.git ]; }; }",
            "foo"
        )
        .contains("[ pkgs.git pkgs.foo ]")
    );
    assert!(
        refusal("{ pkgs, lib, ... }: lib.mkIf true rec { home.packages = [ ]; }")
            .contains("top level")
    );
}

#[test]
fn unusual_entries_are_one_declaration_each() {
    let found = declarations("{ pkgs, x }: with pkgs; [ <nixpkgs> ./a${x}/b rec { y = 1; } git ]");
    let texts: Vec<&str> = found.iter().map(|entry| entry.0.as_str()).collect();
    assert_eq!(texts, ["<nixpkgs>", "./a${x}/b", "rec { y = 1; }", "git"]);
    assert_eq!(found[2].1, None);
}

#[test]
fn long_or_chains_do_not_exhaust_the_stack() {
    let chain = "a or ".repeat(200_000);
    let source = format!("{{ pkgs }}: with pkgs; [ {chain}b ]");
    assert_eq!(declarations(&source).len(), 1);
}
