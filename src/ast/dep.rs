use std::collections::HashMap;

use deno_ast::swc::ast;
use deno_ast::swc::ast::Callee;
use deno_ast::swc::ast::Expr;
use deno_ast::swc::atoms::Atom;
use deno_ast::swc::common::comments::CommentKind;
use deno_ast::swc::ecma_visit::Visit;
use deno_ast::swc::ecma_visit::VisitWith;
use deno_ast::MultiThreadedComments;
use deno_ast::ProgramRef;
use deno_ast::SourcePos;
use deno_ast::SourceRange;
use deno_ast::SourceRangedForSpanned;

use crate::analysis::DynamicDependencyKind;
use crate::analysis::ImportAttribute;
use crate::analysis::ImportAttributes;
use crate::analysis::StaticDependencyKind;

pub fn analyze_program_dependencies(
  program: ProgramRef,
  comments: &MultiThreadedComments,
) -> Vec<DependencyDescriptor> {
  let mut v = DependencyCollector {
    comments,
    items: vec![],
  };
  match program {
    ProgramRef::Module(n) => n.visit_with(&mut v),
    ProgramRef::Script(n) => n.visit_with(&mut v),
  }
  v.items
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependencyComment {
  pub kind: CommentKind,
  pub range: SourceRange,
  pub text: Atom,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DependencyDescriptor {
  Static(StaticDependencyDescriptor),
  Dynamic(DynamicDependencyDescriptor),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StaticDependencyDescriptor {
  pub kind: StaticDependencyKind,
  /// Any leading comments associated with the dependency. This is used for
  /// further processing of supported pragma that impact the dependency.
  pub leading_comments: Vec<DependencyComment>,
  /// The range of the import/export statement.
  pub range: SourceRange,
  /// The text specifier associated with the import/export statement.
  pub specifier: Atom,
  /// The range of the specifier.
  pub specifier_range: SourceRange,
  /// Import attributes for this dependency.
  pub import_attributes: ImportAttributes,
  /// If this is an import for side effects only (ex. `import './load.js';`)
  pub is_side_effect: bool,
}

impl From<StaticDependencyDescriptor> for DependencyDescriptor {
  fn from(descriptor: StaticDependencyDescriptor) -> Self {
    DependencyDescriptor::Static(descriptor)
  }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DynamicDependencyDescriptor {
  /// Kind of dynamic dependency.
  pub kind: DynamicDependencyKind,
  /// Any leading comments associated with the dependency. This is used for
  /// further processing of supported pragma that impact the dependency.
  pub leading_comments: Vec<DependencyComment>,
  /// The range of the import/export statement.
  pub range: SourceRange,
  /// The argument associated with the dynamic import
  pub argument: DynamicArgument,
  /// The range of the specifier.
  pub argument_range: SourceRange,
  /// Import attributes for this dependency.
  pub import_attributes: ImportAttributes,
}

impl From<DynamicDependencyDescriptor> for DependencyDescriptor {
  fn from(descriptor: DynamicDependencyDescriptor) -> Self {
    DependencyDescriptor::Dynamic(descriptor)
  }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DynamicArgument {
  String(Atom),
  Template(Vec<DynamicTemplatePart>),
  /// An expression that could not be analyzed.
  Expr,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DynamicTemplatePart {
  String(Atom),
  /// An expression that could not be analyzed.
  Expr,
}

struct DependencyCollector<'a> {
  comments: &'a MultiThreadedComments,
  pub items: Vec<DependencyDescriptor>,
}

impl DependencyCollector<'_> {
  fn get_leading_comments(&self, start: SourcePos) -> Vec<DependencyComment> {
    match self.comments.get_leading(start) {
      Some(leading) => leading
        .iter()
        .map(|c| DependencyComment {
          kind: c.kind,
          range: c.range(),
          text: c.text.clone(),
        })
        .collect(),
      None => Vec::new(),
    }
  }

  fn is_require(&self, callee: &Callee) -> bool {
    match callee {
      Callee::Expr(expr) => match &**expr {
        // assume any ident named `require` is a require call
        // even if it's not using the global or the result of
        // calling `createRequire`.
        Expr::Ident(ident) => ident.sym == "require",
        _ => false,
      },
      _ => false,
    }
  }
}

impl Visit for DependencyCollector<'_> {
  fn visit_import_decl(&mut self, node: &ast::ImportDecl) {
    let specifier = node.src.value.clone();
    let leading_comments = self.get_leading_comments(node.start());
    let kind = if node.type_only {
      StaticDependencyKind::ImportType
    } else {
      StaticDependencyKind::Import
    };
    self.items.push(
      StaticDependencyDescriptor {
        kind,
        leading_comments,
        range: node.range(),
        specifier,
        specifier_range: node.src.range(),
        import_attributes: parse_import_attributes(node.with.as_deref()),
        is_side_effect: node.specifiers.is_empty(),
      }
      .into(),
    );
  }

  fn visit_named_export(&mut self, node: &ast::NamedExport) {
    if let Some(src) = &node.src {
      let specifier = src.value.clone();
      let leading_comments = self.get_leading_comments(node.start());
      let kind = if node.type_only {
        StaticDependencyKind::ExportType
      } else {
        StaticDependencyKind::Export
      };
      self.items.push(
        StaticDependencyDescriptor {
          kind,
          leading_comments,
          range: node.range(),
          specifier,
          specifier_range: src.range(),
          import_attributes: parse_import_attributes(node.with.as_deref()),
          is_side_effect: false,
        }
        .into(),
      );
    }
  }

  fn visit_export_all(&mut self, node: &ast::ExportAll) {
    let specifier = node.src.value.clone();
    let leading_comments = self.get_leading_comments(node.start());
    let kind = if node.type_only {
      StaticDependencyKind::ExportType
    } else {
      StaticDependencyKind::Export
    };
    self.items.push(
      StaticDependencyDescriptor {
        kind,
        leading_comments,
        range: node.range(),
        specifier,
        specifier_range: node.src.range(),
        import_attributes: parse_import_attributes(node.with.as_deref()),
        is_side_effect: false,
      }
      .into(),
    );
  }

  fn visit_ts_import_type(&mut self, node: &ast::TsImportType) {
    let specifier = node.arg.value.clone();
    let leading_comments = self.get_leading_comments(node.start());
    self.items.push(
      StaticDependencyDescriptor {
        kind: StaticDependencyKind::ImportType,
        leading_comments,
        range: node.range(),
        specifier,
        specifier_range: node.arg.range(),
        import_attributes: node
          .attributes
          .as_ref()
          .map(|a| parse_import_attributes_from_object_lit(&a.with))
          .unwrap_or_default(),
        is_side_effect: false,
      }
      .into(),
    );
    node.visit_children_with(self);
  }

  fn visit_module_items(&mut self, items: &[ast::ModuleItem]) {
    items.visit_children_with(self);
  }

  fn visit_stmts(&mut self, items: &[ast::Stmt]) {
    items.visit_children_with(self)
  }

  fn visit_call_expr(&mut self, node: &ast::CallExpr) {
    node.visit_children_with(self);

    let is_esm = matches!(&node.callee, Callee::Import(_));
    if !is_esm && !self.is_require(&node.callee) {
      return;
    }
    let Some(arg) = node.args.first() else {
      return;
    };

    let argument = match &*arg.expr {
      Expr::Lit(ast::Lit::Str(specifier)) => {
        DynamicArgument::String(specifier.value.clone())
      }
      Expr::Tpl(tpl) => {
        if tpl.quasis.len() == 1 && tpl.exprs.is_empty() {
          DynamicArgument::String(
            tpl.quasis[0].cooked.as_ref().unwrap().clone(),
          )
        } else {
          let mut parts =
            Vec::with_capacity(tpl.quasis.len() + tpl.exprs.len());
          for i in 0..tpl.quasis.len() {
            let cooked = tpl.quasis[i].cooked.as_ref().unwrap();
            if !cooked.is_empty() {
              parts.push(DynamicTemplatePart::String(cooked.clone()));
            }
            if tpl.exprs.get(i).is_some() {
              parts.push(DynamicTemplatePart::Expr);
            }
          }
          DynamicArgument::Template(parts)
        }
      }
      Expr::Bin(bin) => {
        let mut parts = Vec::with_capacity(2);

        fn visit_bin(
          parts: &mut Vec<DynamicTemplatePart>,
          bin: &ast::BinExpr,
        ) -> Result<(), ()> {
          if bin.op != ast::BinaryOp::Add {
            return Err(());
          }

          match &*bin.left {
            Expr::Bin(left) => {
              visit_bin(parts, left)?;
            }
            Expr::Lit(ast::Lit::Str(str)) => {
              parts.push(DynamicTemplatePart::String(str.value.clone()));
            }
            _ => {
              if parts.is_empty() {
                return Err(());
              }
              parts.push(DynamicTemplatePart::Expr);
            }
          };

          if let Expr::Lit(ast::Lit::Str(str)) = &*bin.right {
            parts.push(DynamicTemplatePart::String(str.value.clone()));
          } else {
            parts.push(DynamicTemplatePart::Expr);
          }

          Ok(())
        }

        if visit_bin(&mut parts, bin).is_ok() {
          DynamicArgument::Template(parts)
        } else {
          DynamicArgument::Expr
        }
      }
      _ => DynamicArgument::Expr,
    };
    let dynamic_import_attributes =
      parse_dynamic_import_attributes(node.args.get(1));
    let leading_comments = self.get_leading_comments(node.start());
    self.items.push(
      DynamicDependencyDescriptor {
        kind: if is_esm {
          DynamicDependencyKind::Import
        } else {
          DynamicDependencyKind::Require
        },
        leading_comments,
        range: node.range(),
        argument,
        argument_range: arg.range(),
        import_attributes: dynamic_import_attributes,
      }
      .into(),
    );
  }

  fn visit_ts_import_equals_decl(&mut self, node: &ast::TsImportEqualsDecl) {
    use ast::TsModuleRef;

    if let TsModuleRef::TsExternalModuleRef(module) = &node.module_ref {
      let leading_comments = self.get_leading_comments(node.start());
      let expr = &module.expr;
      let specifier = expr.value.clone();

      let kind = if node.is_type_only {
        StaticDependencyKind::ImportType
      } else if node.is_export {
        StaticDependencyKind::ExportEquals
      } else {
        StaticDependencyKind::ImportEquals
      };

      self.items.push(
        StaticDependencyDescriptor {
          kind,
          leading_comments,
          range: node.range(),
          specifier,
          specifier_range: expr.range(),
          import_attributes: Default::default(),
          is_side_effect: false,
        }
        .into(),
      );
    }
  }
}

/// Parses import attributes into a hashmap. According to proposal the values
/// can only be strings (https://github.com/tc39/proposal-import-attributes#should-more-than-just-strings-be-supported-as-attribute-values)
/// and thus non-string values are skipped.
fn parse_import_attributes(
  maybe_attrs: Option<&ast::ObjectLit>,
) -> ImportAttributes {
  let Some(attrs) = maybe_attrs else {
    return ImportAttributes::None;
  };
  let mut import_attributes = HashMap::new();
  for prop in attrs.props.iter() {
    if let ast::PropOrSpread::Prop(prop) = prop {
      if let ast::Prop::KeyValue(key_value) = &**prop {
        let maybe_key = match &key_value.key {
          ast::PropName::Str(key) => Some(key.value.to_string()),
          ast::PropName::Ident(ident) => Some(ident.sym.to_string()),
          _ => None,
        };

        if let Some(key) = maybe_key {
          if let ast::Expr::Lit(ast::Lit::Str(str_)) = &*key_value.value {
            import_attributes
              .insert(key, ImportAttribute::Known(str_.value.to_string()));
          }
        }
      }
    }
  }
  ImportAttributes::Known(import_attributes)
}

/// Parses import attributes from the second arg of a dynamic import.
fn parse_dynamic_import_attributes(
  arg: Option<&ast::ExprOrSpread>,
) -> ImportAttributes {
  let arg = match arg {
    Some(arg) => arg,
    None => return ImportAttributes::None,
  };

  if arg.spread.is_some() {
    return ImportAttributes::Unknown;
  }

  let object_lit = match &*arg.expr {
    ast::Expr::Object(object_lit) => object_lit,
    _ => return ImportAttributes::Unknown,
  };
  let mut attributes_map = HashMap::new();
  let mut had_attributes_key = false;
  let mut had_with_key = false;

  for prop in object_lit.props.iter() {
    let prop = match prop {
      ast::PropOrSpread::Prop(prop) => prop,
      _ => return ImportAttributes::Unknown,
    };
    let key_value = match &**prop {
      ast::Prop::KeyValue(key_value) => key_value,
      _ => return ImportAttributes::Unknown,
    };
    let key = match &key_value.key {
      ast::PropName::Str(key) => key.value.to_string(),
      ast::PropName::Ident(ident) => ident.sym.to_string(),
      _ => return ImportAttributes::Unknown,
    };
    if key == "with" || key == "assert" && !had_with_key {
      had_attributes_key = true;
      had_with_key = key == "with";
      let attributes_lit = match &*key_value.value {
        ast::Expr::Object(lit) => lit,
        _ => return ImportAttributes::Unknown,
      };
      match parse_import_attributes_from_object_lit(attributes_lit) {
        ImportAttributes::Known(hash_map) => {
          attributes_map = hash_map;
        }
        value => return value,
      }
    }
  }

  if had_attributes_key {
    ImportAttributes::Known(attributes_map)
  } else {
    ImportAttributes::None
  }
}

fn parse_import_attributes_from_object_lit(
  attributes_lit: &ast::ObjectLit,
) -> ImportAttributes {
  let mut attributes_map = HashMap::with_capacity(attributes_lit.props.len());

  for prop in attributes_lit.props.iter() {
    let prop = match prop {
      ast::PropOrSpread::Prop(prop) => prop,
      _ => return ImportAttributes::Unknown,
    };
    let key_value = match &**prop {
      ast::Prop::KeyValue(key_value) => key_value,
      _ => return ImportAttributes::Unknown,
    };
    let key = match &key_value.key {
      ast::PropName::Str(key) => key.value.to_string(),
      ast::PropName::Ident(ident) => ident.sym.to_string(),
      _ => return ImportAttributes::Unknown,
    };
    if let ast::Expr::Lit(value_lit) = &*key_value.value {
      attributes_map.insert(
        key,
        if let ast::Lit::Str(str_) = value_lit {
          ImportAttribute::Known(str_.value.to_string())
        } else {
          ImportAttribute::Unknown
        },
      );
    } else {
      attributes_map.insert(key, ImportAttribute::Unknown);
    }
  }
  ImportAttributes::Known(attributes_map)
}

#[cfg(test)]
mod tests {
  use crate::ModuleSpecifier;
  use deno_ast::swc::atoms::Atom;
  use deno_ast::swc::common::comments::CommentKind;
  use deno_ast::SourcePos;
  use deno_ast::SourceRange;
  use deno_ast::SourceRangedForSpanned;

  use pretty_assertions::assert_eq;

  use super::*;

  fn helper(
    specifier: &str,
    source: &str,
  ) -> (SourcePos, Vec<DependencyDescriptor>) {
    let source = deno_ast::parse_module(deno_ast::ParseParams {
      specifier: ModuleSpecifier::parse(specifier).unwrap(),
      text: source.into(),
      media_type: crate::MediaType::Tsx,
      capture_tokens: false,
      scope_analysis: false,
      maybe_syntax: None,
    })
    .unwrap();
    (
      source.program_ref().start(),
      analyze_program_dependencies(source.program_ref(), source.comments()),
    )
  }

  #[test]
  fn test_parsed_module_get_dependencies() {
    let source = r#"import * as bar from "./test.ts";
/** JSDoc */
import type { Foo } from "./foo.d.ts";
/// <reference foo="bar" />
export * as Buzz from "./buzz.ts";
// @some-pragma
/**
 * Foo
 */
export type { Fizz } from "./fizz.d.ts";
const { join } = require("path");
// dynamic
await import("./foo1.ts");
try {
    const foo = await import("./foo.ts");
} catch (e) {
    // pass
}
try {
    const foo = require("some_package");
} catch (e) {
    // pass
}
import foo2 = require("some_package_foo");
import type FooType = require('some_package_foo_type');
export import bar2 = require("some_package_bar");
const foo3 = require.resolve("some_package_resolve");
try {
    const foo4 = require.resolve("some_package_resolve_foo");
} catch (e) {
    // pass
}
      "#;
    let (start_pos, dependencies) = helper("file:///test.ts", source);
    assert_eq!(
      dependencies,
      vec![
        StaticDependencyDescriptor {
          kind: StaticDependencyKind::Import,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos, start_pos + 33),
          specifier: Atom::from("./test.ts"),
          specifier_range: SourceRange::new(start_pos + 21, start_pos + 32),
          import_attributes: Default::default(),
          is_side_effect: false,
        }
        .into(),
        StaticDependencyDescriptor {
          kind: StaticDependencyKind::ImportType,
          leading_comments: vec![DependencyComment {
            kind: CommentKind::Block,
            text: r#"* JSDoc "#.into(),
            range: SourceRange::new(start_pos + 34, start_pos + 46),
          }],
          range: SourceRange::new(start_pos + 47, start_pos + 85),
          specifier: Atom::from("./foo.d.ts"),
          specifier_range: SourceRange::new(start_pos + 72, start_pos + 84),
          import_attributes: Default::default(),
          is_side_effect: false,
        }
        .into(),
        StaticDependencyDescriptor {
          kind: StaticDependencyKind::Export,
          leading_comments: vec![DependencyComment {
            kind: CommentKind::Line,
            text: r#"/ <reference foo="bar" />"#.into(),
            range: SourceRange::new(start_pos + 86, start_pos + 113),
          }],
          range: SourceRange::new(start_pos + 114, start_pos + 148),
          specifier: Atom::from("./buzz.ts"),
          specifier_range: SourceRange::new(start_pos + 136, start_pos + 147),
          import_attributes: Default::default(),
          is_side_effect: false,
        }
        .into(),
        StaticDependencyDescriptor {
          kind: StaticDependencyKind::ExportType,
          leading_comments: vec![
            DependencyComment {
              kind: CommentKind::Line,
              text: r#" @some-pragma"#.into(),
              range: SourceRange::new(start_pos + 149, start_pos + 164),
            },
            DependencyComment {
              kind: CommentKind::Block,
              text: "*\n * Foo\n ".into(),
              range: SourceRange::new(start_pos + 165, start_pos + 179),
            }
          ],
          range: SourceRange::new(start_pos + 180, start_pos + 220),
          specifier: Atom::from("./fizz.d.ts"),
          specifier_range: SourceRange::new(start_pos + 206, start_pos + 219),
          import_attributes: Default::default(),
          is_side_effect: false,
        }
        .into(),
        DynamicDependencyDescriptor {
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 238, start_pos + 253),
          argument: DynamicArgument::String(Atom::from("path")),
          argument_range: SourceRange::new(start_pos + 246, start_pos + 252),
          import_attributes: Default::default(),
          kind: DynamicDependencyKind::Require,
        }
        .into(),
        DynamicDependencyDescriptor {
          kind: DynamicDependencyKind::Import,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 272, start_pos + 291),
          argument: DynamicArgument::String(Atom::from("./foo1.ts")),
          argument_range: SourceRange::new(start_pos + 279, start_pos + 290),
          import_attributes: Default::default(),
        }
        .into(),
        DynamicDependencyDescriptor {
          kind: DynamicDependencyKind::Import,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 321, start_pos + 339),
          argument: DynamicArgument::String(Atom::from("./foo.ts")),
          argument_range: SourceRange::new(start_pos + 328, start_pos + 338),
          import_attributes: Default::default(),
        }
        .into(),
        DynamicDependencyDescriptor {
          kind: DynamicDependencyKind::Require,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 391, start_pos + 414),
          argument: DynamicArgument::String(Atom::from("some_package")),
          argument_range: SourceRange::new(start_pos + 399, start_pos + 413),
          import_attributes: Default::default(),
        }
        .into(),
        StaticDependencyDescriptor {
          kind: StaticDependencyKind::ImportEquals,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 444, start_pos + 486),
          specifier: Atom::from("some_package_foo"),
          specifier_range: SourceRange::new(start_pos + 466, start_pos + 484),
          import_attributes: Default::default(),
          is_side_effect: false,
        }
        .into(),
        StaticDependencyDescriptor {
          kind: StaticDependencyKind::ImportType,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 487, start_pos + 542),
          specifier: Atom::from("some_package_foo_type"),
          specifier_range: SourceRange::new(start_pos + 517, start_pos + 540),
          import_attributes: Default::default(),
          is_side_effect: false,
        }
        .into(),
        StaticDependencyDescriptor {
          kind: StaticDependencyKind::ExportEquals,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 543, start_pos + 592),
          specifier: Atom::from("some_package_bar"),
          specifier_range: SourceRange::new(start_pos + 572, start_pos + 590),
          import_attributes: Default::default(),
          is_side_effect: false,
        }
        .into(),
      ]
    );
  }

  #[test]
  fn test_import_attributes() {
    let source = r#"import * as bar from "./test.ts" with { "type": "typescript" };
export * from "./test.ts" with { "type": "typescript" };
export { bar } from "./test.json" with { "type": "json" };
import foo from "./foo.json" with { type: "json" };
const fizz = await import("./fizz.json", { "with": { type: "json" } });
const buzz = await import("./buzz.json", { with: { "type": "json" } });
const d1 = await import("./d1.json");
const d2 = await import("./d2.json", {});
const d3 = await import("./d3.json", bar);
const d4 = await import("./d4.json", { with: {} });
const d5 = await import("./d5.json", { with: bar });
const d6 = await import("./d6.json", { with: {}, ...bar });
const d7 = await import("./d7.json", { with: {}, ["assert"]: "bad" });
const d8 = await import("./d8.json", { with: { type: bar } });
const d9 = await import("./d9.json", { with: { type: "json", ...bar } });
const d10 = await import("./d10.json", { with: { type: "json", ["type"]: "bad" } });
      "#;
    let (start_pos, dependencies) = helper("file:///test.ts", source);
    let expected_attributes1 = ImportAttributes::Known({
      let mut map = HashMap::new();
      map.insert(
        "type".to_string(),
        ImportAttribute::Known("typescript".to_string()),
      );
      map
    });
    let expected_attributes2 = ImportAttributes::Known({
      let mut map = HashMap::new();
      map.insert(
        "type".to_string(),
        ImportAttribute::Known("json".to_string()),
      );
      map
    });
    let dynamic_expected_attributes2 = ImportAttributes::Known({
      let mut map = HashMap::new();
      map.insert(
        "type".to_string(),
        ImportAttribute::Known("json".to_string()),
      );
      map
    });
    assert_eq!(
      dependencies,
      vec![
        StaticDependencyDescriptor {
          kind: StaticDependencyKind::Import,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos, start_pos + 63),
          specifier: Atom::from("./test.ts"),
          specifier_range: SourceRange::new(start_pos + 21, start_pos + 32),
          import_attributes: expected_attributes1.clone(),
          is_side_effect: false,
        }
        .into(),
        StaticDependencyDescriptor {
          kind: StaticDependencyKind::Export,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 64, start_pos + 120),
          specifier: Atom::from("./test.ts"),
          specifier_range: SourceRange::new(start_pos + 78, start_pos + 89),
          import_attributes: expected_attributes1,
          is_side_effect: false,
        }
        .into(),
        StaticDependencyDescriptor {
          kind: StaticDependencyKind::Export,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 121, start_pos + 179),
          specifier: Atom::from("./test.json"),
          specifier_range: SourceRange::new(start_pos + 141, start_pos + 154),
          import_attributes: expected_attributes2.clone(),
          is_side_effect: false,
        }
        .into(),
        StaticDependencyDescriptor {
          kind: StaticDependencyKind::Import,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 180, start_pos + 231),
          specifier: Atom::from("./foo.json"),
          specifier_range: SourceRange::new(start_pos + 196, start_pos + 208),
          import_attributes: expected_attributes2,
          is_side_effect: false,
        }
        .into(),
        DynamicDependencyDescriptor {
          kind: DynamicDependencyKind::Import,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 251, start_pos + 302),
          argument: DynamicArgument::String(Atom::from("./fizz.json")),
          argument_range: SourceRange::new(start_pos + 258, start_pos + 271),
          import_attributes: dynamic_expected_attributes2.clone(),
        }
        .into(),
        DynamicDependencyDescriptor {
          kind: DynamicDependencyKind::Import,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 323, start_pos + 374),
          argument: DynamicArgument::String(Atom::from("./buzz.json")),
          argument_range: SourceRange::new(start_pos + 330, start_pos + 343),
          import_attributes: dynamic_expected_attributes2,
        }
        .into(),
        DynamicDependencyDescriptor {
          kind: DynamicDependencyKind::Import,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 393, start_pos + 412),
          argument: DynamicArgument::String(Atom::from("./d1.json")),
          argument_range: SourceRange::new(start_pos + 400, start_pos + 411),
          import_attributes: Default::default(),
        }
        .into(),
        DynamicDependencyDescriptor {
          kind: DynamicDependencyKind::Import,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 431, start_pos + 454),
          argument: DynamicArgument::String(Atom::from("./d2.json")),
          argument_range: SourceRange::new(start_pos + 438, start_pos + 449),
          import_attributes: Default::default(),
        }
        .into(),
        DynamicDependencyDescriptor {
          kind: DynamicDependencyKind::Import,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 473, start_pos + 497),
          argument: DynamicArgument::String(Atom::from("./d3.json")),
          argument_range: SourceRange::new(start_pos + 480, start_pos + 491),
          import_attributes: ImportAttributes::Unknown,
        }
        .into(),
        DynamicDependencyDescriptor {
          kind: DynamicDependencyKind::Import,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 516, start_pos + 549),
          argument: DynamicArgument::String(Atom::from("./d4.json")),
          argument_range: SourceRange::new(start_pos + 523, start_pos + 534),
          import_attributes: ImportAttributes::Known(HashMap::new()),
        }
        .into(),
        DynamicDependencyDescriptor {
          kind: DynamicDependencyKind::Import,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 568, start_pos + 602),
          argument: DynamicArgument::String(Atom::from("./d5.json")),
          argument_range: SourceRange::new(start_pos + 575, start_pos + 586),
          import_attributes: ImportAttributes::Unknown,
        }
        .into(),
        DynamicDependencyDescriptor {
          kind: DynamicDependencyKind::Import,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 621, start_pos + 662),
          argument: DynamicArgument::String(Atom::from("./d6.json")),
          argument_range: SourceRange::new(start_pos + 628, start_pos + 639),
          import_attributes: ImportAttributes::Unknown,
        }
        .into(),
        DynamicDependencyDescriptor {
          kind: DynamicDependencyKind::Import,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 681, start_pos + 733),
          argument: DynamicArgument::String(Atom::from("./d7.json")),
          argument_range: SourceRange::new(start_pos + 688, start_pos + 699),
          import_attributes: ImportAttributes::Unknown,
        }
        .into(),
        DynamicDependencyDescriptor {
          kind: DynamicDependencyKind::Import,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 752, start_pos + 796),
          argument: DynamicArgument::String(Atom::from("./d8.json")),
          argument_range: SourceRange::new(start_pos + 759, start_pos + 770),
          import_attributes: ImportAttributes::Known({
            let mut map = HashMap::new();
            map.insert("type".to_string(), ImportAttribute::Unknown);
            map
          }),
        }
        .into(),
        DynamicDependencyDescriptor {
          kind: DynamicDependencyKind::Import,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 815, start_pos + 870),
          argument: DynamicArgument::String(Atom::from("./d9.json")),
          argument_range: SourceRange::new(start_pos + 822, start_pos + 833),
          import_attributes: ImportAttributes::Unknown,
        }
        .into(),
        DynamicDependencyDescriptor {
          kind: DynamicDependencyKind::Import,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 890, start_pos + 955),
          argument: DynamicArgument::String(Atom::from("./d10.json")),
          argument_range: SourceRange::new(start_pos + 897, start_pos + 909),
          import_attributes: ImportAttributes::Unknown,
        }
        .into(),
      ]
    );
  }

  #[test]
  fn test_dynamic_imports() {
    let source = r#"const d1 = await import(`./d1.json`);
const d2 = await import(`${value}`);
const d3 = await import(`./test/${value}`);
const d4 = await import(`${value}/test`);
const d5 = await import(`${value}${value2}`);
const d6 = await import(`${value}/test/${value2}`);
const d7 = await import(`./${value}/test/${value2}/`);
const d8 = await import("./foo/" + value);
const d9 = await import("./foo/" + value + ".ts");
const d10 = await import(value + ".ts");
const d11 = await import("./foo/" - value);
const d12 = await import(expr);
"#;
    let (start_pos, dependencies) = helper("file:///test.ts", source);
    assert_eq!(
      dependencies,
      vec![
        DynamicDependencyDescriptor {
          kind: DynamicDependencyKind::Import,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 17, start_pos + 36),
          argument: DynamicArgument::String(Atom::from("./d1.json")),
          argument_range: SourceRange::new(start_pos + 24, start_pos + 35),
          import_attributes: ImportAttributes::None,
        }
        .into(),
        DynamicDependencyDescriptor {
          kind: DynamicDependencyKind::Import,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 55, start_pos + 73),
          argument: DynamicArgument::Template(vec![DynamicTemplatePart::Expr]),
          argument_range: SourceRange::new(start_pos + 62, start_pos + 72),
          import_attributes: ImportAttributes::None,
        }
        .into(),
        DynamicDependencyDescriptor {
          kind: DynamicDependencyKind::Import,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 92, start_pos + 117),
          argument: DynamicArgument::Template(vec![
            DynamicTemplatePart::String(Atom::from("./test/")),
            DynamicTemplatePart::Expr,
          ]),
          argument_range: SourceRange::new(start_pos + 99, start_pos + 116),
          import_attributes: ImportAttributes::None,
        }
        .into(),
        DynamicDependencyDescriptor {
          kind: DynamicDependencyKind::Import,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 136, start_pos + 159),
          argument: DynamicArgument::Template(vec![
            DynamicTemplatePart::Expr,
            DynamicTemplatePart::String(Atom::from("/test")),
          ]),
          argument_range: SourceRange::new(start_pos + 143, start_pos + 158),
          import_attributes: ImportAttributes::None,
        }
        .into(),
        DynamicDependencyDescriptor {
          kind: DynamicDependencyKind::Import,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 178, start_pos + 205),
          argument: DynamicArgument::Template(vec![
            DynamicTemplatePart::Expr,
            DynamicTemplatePart::Expr,
          ]),
          argument_range: SourceRange::new(start_pos + 185, start_pos + 204),
          import_attributes: ImportAttributes::None,
        }
        .into(),
        DynamicDependencyDescriptor {
          kind: DynamicDependencyKind::Import,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 224, start_pos + 257),
          argument: DynamicArgument::Template(vec![
            DynamicTemplatePart::Expr,
            DynamicTemplatePart::String(Atom::from("/test/")),
            DynamicTemplatePart::Expr,
          ]),
          argument_range: SourceRange::new(start_pos + 231, start_pos + 256),
          import_attributes: ImportAttributes::None,
        }
        .into(),
        DynamicDependencyDescriptor {
          kind: DynamicDependencyKind::Import,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 276, start_pos + 312),
          argument: DynamicArgument::Template(vec![
            DynamicTemplatePart::String(Atom::from("./")),
            DynamicTemplatePart::Expr,
            DynamicTemplatePart::String(Atom::from("/test/")),
            DynamicTemplatePart::Expr,
            DynamicTemplatePart::String(Atom::from("/")),
          ]),
          argument_range: SourceRange::new(start_pos + 283, start_pos + 311),
          import_attributes: ImportAttributes::None,
        }
        .into(),
        DynamicDependencyDescriptor {
          kind: DynamicDependencyKind::Import,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 331, start_pos + 355),
          argument: DynamicArgument::Template(vec![
            DynamicTemplatePart::String(Atom::from("./foo/")),
            DynamicTemplatePart::Expr,
          ]),
          argument_range: SourceRange::new(start_pos + 338, start_pos + 354),
          import_attributes: ImportAttributes::None,
        }
        .into(),
        DynamicDependencyDescriptor {
          kind: DynamicDependencyKind::Import,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 374, start_pos + 406),
          argument: DynamicArgument::Template(vec![
            DynamicTemplatePart::String(Atom::from("./foo/")),
            DynamicTemplatePart::Expr,
            DynamicTemplatePart::String(Atom::from(".ts")),
          ]),
          argument_range: SourceRange::new(start_pos + 381, start_pos + 405),
          import_attributes: ImportAttributes::None,
        }
        .into(),
        DynamicDependencyDescriptor {
          kind: DynamicDependencyKind::Import,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 426, start_pos + 447),
          argument: DynamicArgument::Expr,
          argument_range: SourceRange::new(start_pos + 433, start_pos + 446),
          import_attributes: ImportAttributes::None,
        }
        .into(),
        DynamicDependencyDescriptor {
          kind: DynamicDependencyKind::Import,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 467, start_pos + 491),
          argument: DynamicArgument::Expr,
          argument_range: SourceRange::new(start_pos + 474, start_pos + 490),
          import_attributes: ImportAttributes::None,
        }
        .into(),
        DynamicDependencyDescriptor {
          kind: DynamicDependencyKind::Import,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 511, start_pos + 523),
          argument: DynamicArgument::Expr,
          argument_range: SourceRange::new(start_pos + 518, start_pos + 522),
          import_attributes: ImportAttributes::None,
        }
        .into(),
      ]
    );
  }

  #[test]
  fn ts_import_object_lit_property() {
    let source = r#"
export declare const SomeValue: typeof Core & import("./a.d.ts").Constructor<{
    paginate: import("./b.d.ts").PaginateInterface;
} & import("./c.d.ts", { with: { "resolution-mode": "import" } }).RestEndpointMethods>;
"#;
    let (start_pos, dependencies) = helper("file:///test.ts", source);
    assert_eq!(
      dependencies,
      vec![
        StaticDependencyDescriptor {
          kind: StaticDependencyKind::ImportType,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 46, start_pos + 217),
          specifier: Atom::from("./a.d.ts"),
          specifier_range: SourceRange::new(start_pos + 53, start_pos + 63),
          import_attributes: ImportAttributes::None,
          is_side_effect: false,
        }
        .into(),
        StaticDependencyDescriptor {
          kind: StaticDependencyKind::ImportType,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 93, start_pos + 129),
          specifier: Atom::from("./b.d.ts"),
          specifier_range: SourceRange::new(start_pos + 100, start_pos + 110),
          import_attributes: ImportAttributes::None,
          is_side_effect: false,
        }
        .into(),
        StaticDependencyDescriptor {
          kind: StaticDependencyKind::ImportType,
          leading_comments: Vec::new(),
          range: SourceRange::new(start_pos + 135, start_pos + 216),
          specifier: Atom::from("./c.d.ts"),
          specifier_range: SourceRange::new(start_pos + 142, start_pos + 152),
          import_attributes: ImportAttributes::Known(HashMap::from([(
            "resolution-mode".to_string(),
            ImportAttribute::Known("import".to_string())
          )])),
          is_side_effect: false,
        }
        .into()
      ]
    );
  }
}
