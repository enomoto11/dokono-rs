//! Classify whether a `(file, position)` lies inside `#[cfg(test)]`-gated code.
//!
//! Detection is shallow: matches `#[cfg(test)]` and `#[cfg(any(test, ...))]` on any
//! inline `mod`, `fn`, `impl`, or other top-level item. Cross-file mods
//! (`#[cfg(test)] mod foo;` with `foo.rs`) are not followed. This is sufficient for the
//! `#[cfg(test)] mod tests { ... }` idiom that dominates idiomatic Rust.

use anyhow::{Context, Result};
use dokono_core::types::Position;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use syn::spanned::Spanned;

#[derive(Default)]
pub struct CfgClassifier {
    cache: HashMap<PathBuf, Vec<LineRange>>,
}

#[derive(Debug, Clone, Copy)]
struct LineRange {
    /// 0-based inclusive.
    start: u32,
    /// 0-based inclusive.
    end: u32,
}

impl CfgClassifier {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_test_gated(&mut self, file: &Path, pos: Position) -> Result<bool> {
        let ranges = self.parsed(file)?;
        Ok(ranges
            .iter()
            .any(|r| pos.line >= r.start && pos.line <= r.end))
    }

    fn parsed(&mut self, file: &Path) -> Result<&[LineRange]> {
        if !self.cache.contains_key(file) {
            let ranges = parse_file_ranges(file).unwrap_or_else(|e| {
                tracing::debug!("cfg_test: parse failed for {}: {e}", file.display());
                Vec::new()
            });
            self.cache.insert(file.to_path_buf(), ranges);
        }
        Ok(self.cache.get(file).unwrap())
    }
}

fn parse_file_ranges(file: &Path) -> Result<Vec<LineRange>> {
    let source =
        std::fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
    parse_source_ranges(&source).with_context(|| format!("parse {}", file.display()))
}

fn parse_source_ranges(source: &str) -> Result<Vec<LineRange>> {
    let syntax = syn::parse_file(source)?;
    let mut out = Vec::new();
    collect(&syntax.items, &mut out);
    Ok(out)
}

fn collect(items: &[syn::Item], out: &mut Vec<LineRange>) {
    for item in items {
        let attrs = item_attrs(item);
        let is_test = attrs.is_some_and(has_cfg_test);
        if is_test {
            out.push(item_range(item));
        } else if let syn::Item::Mod(m) = item
            && let Some((_, inner)) = &m.content
        {
            collect(inner, out);
        }
    }
}

fn item_attrs(item: &syn::Item) -> Option<&[syn::Attribute]> {
    Some(match item {
        syn::Item::Fn(x) => &x.attrs,
        syn::Item::Mod(x) => &x.attrs,
        syn::Item::Impl(x) => &x.attrs,
        syn::Item::Struct(x) => &x.attrs,
        syn::Item::Enum(x) => &x.attrs,
        syn::Item::Trait(x) => &x.attrs,
        syn::Item::Const(x) => &x.attrs,
        syn::Item::Static(x) => &x.attrs,
        syn::Item::Use(x) => &x.attrs,
        _ => return None,
    })
}

fn item_range(item: &syn::Item) -> LineRange {
    let span = item.span();
    let start = span.start();
    let end = span.end();
    LineRange {
        start: start.line.saturating_sub(1) as u32,
        end: end.line.saturating_sub(1) as u32,
    }
}

fn has_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(attr_matches_cfg_test)
}

fn attr_matches_cfg_test(attr: &syn::Attribute) -> bool {
    if !attr.path().is_ident("cfg") {
        return false;
    }
    let mut found = false;
    let _ = attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("test") {
            found = true;
        } else if meta.path.is_ident("any") {
            meta.parse_nested_meta(|inner| {
                if inner.path.is_ident("test") {
                    found = true;
                }
                let _ = inner.input.parse::<proc_macro2::TokenStream>();
                Ok(())
            })?;
        }
        Ok(())
    });
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gated_at(source: &str, line: u32) -> bool {
        let ranges = parse_source_ranges(source).unwrap();
        ranges.iter().any(|r| line >= r.start && line <= r.end)
    }

    #[test]
    fn cfg_test_mod_is_gated() {
        let src = "fn outer() {}\n\
                   #[cfg(test)]\n\
                   mod tests {\n    \
                       fn inner() {}\n\
                   }\n";
        assert!(gated_at(src, 3));
        assert!(!gated_at(src, 0));
    }

    #[test]
    fn cfg_any_test_is_gated() {
        let src = "#[cfg(any(test, feature = \"x\"))]\n\
                   mod tests {\n    \
                       fn inner() {}\n\
                   }\n";
        assert!(gated_at(src, 2));
    }

    #[test]
    fn non_cfg_attr_is_not_gated() {
        let src = "#[derive(Debug)]\nstruct S;\n";
        assert!(!gated_at(src, 1));
    }

    #[test]
    fn cfg_feature_only_is_not_gated() {
        let src = "#[cfg(feature = \"x\")]\nfn foo() {}\n";
        assert!(!gated_at(src, 1));
    }

    #[test]
    fn cfg_test_fn_is_gated() {
        let src = "#[cfg(test)]\nfn helper() {}\nfn prod() {}\n";
        assert!(gated_at(src, 1));
        assert!(!gated_at(src, 2));
    }

    #[test]
    fn nested_cfg_test_mod_is_gated() {
        let src = "mod outer {\n    \
                       #[cfg(test)]\n    \
                       mod inner {\n        \
                           fn deep() {}\n    \
                       }\n\
                   }\n";
        assert!(gated_at(src, 3));
    }
}
