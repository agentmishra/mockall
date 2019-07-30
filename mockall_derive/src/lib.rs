// vim: tw=80
//! Proc Macros for use with Mockall
//!
//! You probably don't want to use this crate directly.  Instead, you use use
//! its reexports via the [`mockall`](../mockall/index.html) crate.

#![recursion_limit="512"]

#![cfg_attr(feature = "nightly", feature(proc_macro_diagnostic))]
extern crate proc_macro;

use cfg_if::cfg_if;
use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::{
    *,
    punctuated::Pair,
    punctuated::Punctuated,
    spanned::Spanned
};

mod automock;
mod expectation;
mod mock;
use crate::automock::do_automock;
use crate::mock::{Mock, do_mock};

struct MethodTypes {
    is_static: bool,
    /// Type of Expectation returned by the expect method
    expectation: Type,
    /// Type of Expectations container used by the expect method, without its
    /// generics fields
    expectations: Type,
    /// Type of Expectations object stored in the mock structure, with generics
    /// fields
    expect_obj: Type,
    /// Method to call when invoking the expectation
    call: Ident,
    /// Version of the arguments used for the Predicates
    altargs: Punctuated<FnArg, Token![,]>,
    /// Expressions producing references from the arguments
    matchexprs: Punctuated<Expr, Token![,]>,
}

cfg_if! {
    // proc-macro2's Span::unstable method requires the nightly feature, and it
    // doesn't work in test mode.
    // https://github.com/alexcrichton/proc-macro2/issues/159
    if #[cfg(all(feature = "nightly", not(test)))] {
        fn compile_error(span: Span, msg: &'static str) {
            span.unstable()
                .error(msg)
                .emit();
        }
    } else {
        fn compile_error(_span: Span, msg: &str) {
            panic!("{}.  More information may be available when mockall is built with the \"nightly\" feature.", msg);
        }
    }
}

/// Replace any "impl trait" types with "Box<dyn trait>" equivalents
fn deimplify(rt: &mut ReturnType) {
    if let ReturnType::Type(_, ty) = rt {
        if let Type::ImplTrait(ref tit) = &**ty {
            let bounds = &tit.bounds;
            *ty = parse2(quote!(Box<dyn #bounds>)).unwrap();
        }
    }
}

/// Remove any mutability qualifiers from a method's argument list
fn demutify(args: &Punctuated<FnArg, token::Comma>)
    -> Punctuated<FnArg, token::Comma>
{
    let mut output = args.clone();
    for arg in output.iter_mut() {
        match arg {
            FnArg::SelfValue(arg_self) => arg_self.mutability = None,
            FnArg::Captured(arg_captured) => demutify_arg(arg_captured),
            _ => (),    // Nothing to do
        }
    }
    output
}

/// Remove any "mut" from a method argument's binding.
fn demutify_arg(arg: &mut ArgCaptured) {
    match arg.pat {
        Pat::Wild(_) => {
            compile_error(arg.span(),
                "Mocked methods must have named arguments");
        },
        Pat::Ident(ref mut pat_ident) => {
            if let Some(r) = &pat_ident.by_ref {
                compile_error(r.span(),
                    "Mockall does not support by-reference argument bindings");
            }
            if let Some((_at, subpat)) = &pat_ident.subpat {
                compile_error(subpat.span(),
                    "Mockall does not support subpattern bindings");
            }
            pat_ident.mutability = None;
        },
        _ => {
            compile_error(arg.span(), "Unsupported argument type");
        }
    };
}

/// Replace any references to `Self` in `literal_type` with `actual`.  Useful
/// for constructor methods
fn deselfify(literal_type: &mut Type, actual: &Ident) {
    match literal_type {
        Type::Slice(s) => {
            deselfify(s.elem.as_mut(), actual);
        },
        Type::Array(a) => {
            deselfify(a.elem.as_mut(), actual);
        },
        Type::Ptr(p) => {
            deselfify(p.elem.as_mut(), actual);
        },
        Type::Reference(r) => {
            deselfify(r.elem.as_mut(), actual);
        },
        Type::BareFn(_bfn) => {
            unimplemented!()
        },
        Type::Tuple(tuple) => {
            for elem in tuple.elems.iter_mut() {
                deselfify(elem, actual);
            }
        }
        Type::Path(type_path) => {
            if let Some(ref _qself) = type_path.qself {
                compile_error(type_path.span(), "QSelf is TODO");
            }
            let p = &mut type_path.path;
            for seg in p.segments.iter_mut() {
                if seg.ident == "Self" {
                    seg.ident = actual.clone()
                }
                if let PathArguments::AngleBracketed(abga) = &mut seg.arguments
                {
                    for arg in abga.args.iter_mut() {
                        match arg {
                            GenericArgument::Type(ty) =>
                                deselfify(ty, actual),
                            GenericArgument::Binding(b) =>
                                deselfify(&mut b.ty, actual),
                            _ => /* Nothing to do */(),
                        }
                    }
                }
            }
        },
        Type::Paren(p) => {
            deselfify(p.elem.as_mut(), actual);
        },
        Type::Group(g) => {
            deselfify(g.elem.as_mut(), actual);
        },
        Type::Macro(_) | Type::Verbatim(_) => {
            compile_error(literal_type.span(),
                "mockall_derive does not support this type as a return argument");
        },
        Type::ImplTrait(_) => {
            panic!("deimplify should've already been run on this output type");
        },
        Type::TraitObject(tto) => {
            // Change types like `dyn Self` into `MockXXX`.  For now,
            // don't worry about multiple trait bounds, because they aren't very
            // useful in combination with Self.
            if tto.bounds.len() == 1 {
                if let TypeParamBound::Trait(t)
                    = tto.bounds.first().unwrap().value()
                {
                    let path = &t.path;
                    let mut new_type: Type = parse2(quote!(#path)).unwrap();
                    deselfify(&mut new_type, actual);
                    *literal_type = new_type;
                }
            }
        },
        Type::Infer(_) | Type::Never(_) =>
        {
            /* Nothing to do */
        }
    }
}

fn supersuperfy_path(path: &mut Path) {
        if let Some(Pair::Punctuated(t, _)) = path.segments.first() {
            if t.ident == "super" {
                let ps = PathSegment {
                    ident: Ident::new("super", path.segments.span()),
                    arguments: PathArguments::None
                };
                path.segments.insert(0, ps.clone());
                path.segments.insert(0, ps);
            }
        }
}

/// Replace any references to `super::X` in `original` with `super::super::X`.
fn supersuperfy(original: &Type) -> Type {
    let mut output = original.clone();
    fn recurse(t: &mut Type) {
        match t {
            Type::Slice(s) => {
                supersuperfy(s.elem.as_mut());
            },
            Type::Array(a) => {
                supersuperfy(a.elem.as_mut());
            },
            Type::Ptr(p) => {
                supersuperfy(p.elem.as_mut());
            },
            Type::Reference(r) => {
                supersuperfy(r.elem.as_mut());
            },
            Type::BareFn(_bfn) => {
                unimplemented!()
            },
            Type::Tuple(tuple) => {
                for elem in tuple.elems.iter_mut() {
                    supersuperfy(elem);
                }
            }
            Type::Path(type_path) => {
                if let Some(ref _qself) = type_path.qself {
                    compile_error(type_path.span(), "QSelf is TODO");
                }
                supersuperfy_path(&mut type_path.path)
            },
            Type::Paren(p) => {
                supersuperfy(p.elem.as_mut());
            },
            Type::Group(g) => {
                supersuperfy(g.elem.as_mut());
            },
            Type::Macro(_) | Type::Verbatim(_) => {
                compile_error(t.span(),
                    "mockall_derive does not support this type in this position");
            },
            Type::ImplTrait(_) => {
                panic!("deimplify should've already been run on this output type");
            },
            Type::TraitObject(tto) => {
                for bound in tto.bounds.iter_mut() {
                    if let TypeParamBound::Trait(tb) = bound {
                        supersuperfy_path(&mut tb.path)
                    }
                }
            },
            Type::Infer(_) | Type::Never(_) =>
            {
                /* Nothing to do */
            }
        }
    }
    recurse(&mut output);
    output
}

/// Generate a mock identifier from the regular one: eg "Foo" => "MockFoo"
fn gen_mock_ident(ident: &Ident) -> Ident {
    Ident::new(&format!("Mock{}", ident), ident.span())
}

/// Generate an identifier for the mock struct's private module: eg "Foo" =>
/// "__mock_Foo"
fn gen_mod_ident(struct_: &Ident, trait_: Option<&Ident>)
    -> Ident
{
    if let Some(t) = trait_ {
        Ident::new(&format!("__mock_{}_{}", struct_, t), struct_.span())
    } else {
        Ident::new(&format!("__mock_{}", struct_), struct_.span())
    }
}

/// Combine two Generics structs, producing a new one that has the union of
/// their parameters.  The output's parameter bounds are not accurate!
fn merge_generics(x: &Generics, y: &Generics) -> Generics {
    /// Compare only the identifiers of two GenericParams
    fn cmp_gp_idents(x: &GenericParam, y: &GenericParam) -> bool {
        use GenericParam::*;

        match (x, y) {
            (Type(xtp), Type(ytp)) => xtp.ident == ytp.ident,
            (Lifetime(xld), Lifetime(yld)) => xld.lifetime == yld.lifetime,
            (Const(xc), Const(yc)) => xc.ident == yc.ident,
            _ => false
        }
    }

    /// Compare only the identifiers of two WherePredicates
    fn cmp_wp_idents(x: &WherePredicate, y: &WherePredicate) -> bool {
        use WherePredicate::*;

        match (x, y) {
            (Type(xpt), Type(ypt)) => xpt.bounded_ty == ypt.bounded_ty,
            (Lifetime(xpl), Lifetime(ypl)) => xpl.lifetime == ypl.lifetime,
            (Eq(xeq), Eq(yeq)) => xeq.lhs_ty == yeq.lhs_ty,
            _ => false
        }
    }

    if x.lt_token.is_none() {
        y.clone()
    } else if y.lt_token.is_none() {
        x.clone()
    } else {
        let mut out = x.clone();
        // First merge the params
        for param in y.params.iter() {
            if ! x.params.iter().any(|t| cmp_gp_idents(t, param)) {
                out.params.push(param.clone());
            }
        }
        // Then merge the where clauses
        if x.where_clause.is_none() {
            out.where_clause = y.where_clause.clone();
        } else if let Some(wc) = &y.where_clause {
            if x.where_clause.is_none() {
                out.where_clause = Some(wc.clone());
            }
            for predicate in wc.predicates.iter() {
                let x_p = &x.where_clause.as_ref().unwrap().predicates;
                let out_p = &mut out.where_clause.as_mut().unwrap().predicates;
                if ! x_p.iter().any(|t| cmp_wp_idents(t, predicate)) {
                    out_p.push(predicate.clone());
                }
            }
        }
        out
    }
}

/// Return the visibility that should be used for expectation!, given the
/// original method's visibility.
///
/// # Arguments
/// - `vis`:    Original visibility of the item
/// - `levels`: How many modules will the mock item be nested in?
fn expectation_visibility(vis: &Visibility, levels: u32)
    -> Visibility
{
    debug_assert!(levels > 0);
    let in_token = Token![in](vis.span());
    let super_token = Token![super](vis.span());
    match vis {
        Visibility::Inherited => {
            // Private items need pub(in super::[...]) for each level
            let mut path = Path::from(super_token);
            for _ in 1..levels {
                path.segments.push(super_token.into());
            }
            let supersuper = Visibility::Restricted(VisRestricted{
                pub_token: Token![pub](vis.span()),
                paren_token: token::Paren::default(),
                in_token: Some(in_token),
                path: Box::new(path)
            });
            supersuper.into()
        },
        Visibility::Restricted(vr) => {
            // crate => don't change
            // in crate::* => don't change
            // super => in super::super::super
            // self => in super::super
            // in anything_else => super::super::anything_else
            if vr.path.segments.first().unwrap().value().ident == "crate" {
                vr.clone().into()
            } else {
                let mut out = vr.clone();
                out.in_token = Some(in_token);
                for _ in 0..levels {
                    out.path.segments.insert(0, super_token.into());
                }
                out.into()
            }
        },
        _ => vis.clone()
    }
}

/// Extract useful data about a method
///
/// # Arguments
///
/// * `sig`:            Signature of the original method
/// * `generics`:       Generics of the method's parent trait or structure,
///                     _not_ the method itself.
fn method_types(sig: &MethodSig, generics: Option<&Generics>)
    -> MethodTypes
{
    let mut is_static = true;
    let mut altargs = Punctuated::new();
    let mut matchexprs = Punctuated::new();
    let mut args = Punctuated::<Type, Token![,]>::new();
    let ident = &sig.ident;
    let is_generic = !sig.decl.generics.params.is_empty();
    let merged_g = if let Some(g) = generics {
        merge_generics(&g, &sig.decl.generics)
    } else {
        sig.decl.generics.clone()
    };
    let (_ig, tg, _wc) = merged_g.split_for_impl();
    for (i, fn_arg) in sig.decl.inputs.iter().enumerate() {
        match fn_arg {
            FnArg::Captured(arg) => {
                args.push(arg.ty.clone());
                let mep = if let Pat::Ident(arg_ident) = &arg.pat {
                    Expr::Path(ExprPath{
                        attrs: Vec::new(),
                        qself: None,
                        path: Path::from(arg_ident.ident.clone())
                    })
                } else {
                    compile_error(arg.span(),
                        "Unexpected argument pattern in function declaration");
                    break;
                };
                let alt_ty = if let Type::Reference(tr) = &arg.ty {
                    // /* Remove any "mut" */
                    matchexprs.push(mep);
                    (*tr.elem).clone()
                } else {
                    let matchexpr = Expr::Reference(ExprReference {
                        attrs: Vec::new(),
                        and_token: Token![&](arg.span()),
                        mutability: None,
                        expr: Box::new(mep)
                    });
                    matchexprs.push(matchexpr);
                    arg.ty.clone()
                };
                let altident = Ident::new(&format!("p{}", i), arg.span());
                let altpat = Pat::Ident(PatIdent {
                    by_ref: None,
                    mutability: None,
                    ident: altident,
                    subpat: None
                });
                let altarg = FnArg::Captured(ArgCaptured {
                    pat: altpat,
                    colon_token: Token![:](arg.span()),
                    ty: alt_ty
                });
                altargs.push(altarg);
            },
            FnArg::SelfRef(_) => {
                is_static = false;
            },
            FnArg::SelfValue(_) => {
                is_static = false;
            },
            _ => compile_error(fn_arg.span(),
                "Should be unreachable for normal Rust code")
        }
    }

    let span = Span::call_site();
    let call = match &sig.decl.output {
        ReturnType::Default => {
            Ident::new("call", span)
        },
        ReturnType::Type(_, ty) => {
            match ty.as_ref() {
                Type::Reference(r) => {
                    if let Some(ref lt) = r.lifetime {
                        if lt.ident != "static" {
                            compile_error(r.span(), "Non-'static non-'self lifetimes are not yet supported");
                        }
                    }
                    if r.mutability.is_some() {
                        Ident::new("call_mut", span)
                    } else {
                        Ident::new("call", span)
                    }
                },
                _ => Ident::new("call", span)
            }
        }
    };

    let expectation_ident = Ident::new("Expectation", span);
    let expectations_ident = if is_generic {
        Ident::new("GenericExpectations", span)
    } else {
        Ident::new("Expectations", span)
    };
    let expectations = parse2(
        quote!(#ident::#expectations_ident)
    ).unwrap();
    let expect_obj = if is_generic {
        parse2(quote!(#expectations)).unwrap()
    } else {
        parse2(quote!(#expectations #tg)).unwrap()
    };
    let expect_ts = quote!(#ident::#expectation_ident #tg);
    let expectation: Type = parse2(expect_ts).unwrap();

    MethodTypes{is_static, expectation, expectations,
                call, expect_obj, altargs, matchexprs}
}

/// Manually mock a structure.
///
/// Sometimes `automock` can't be used.  In those cases you can use `mock!`,
/// which basically involves repeating the struct's or trait's definitions.
///
/// The format is:
///
/// * Optional visibility specifier
/// * Real structure name and generics fields
/// * 0 or more methods of the structure, written without bodies, enclosed in
///   a {} block
/// * 0 or more traits to implement for the structure, written like normal
///   traits
///
/// # Examples
///
/// Mock a trait.  This is the simplest use case.
/// ```
/// # use mockall_derive::mock;
/// trait Foo {
///     fn foo(&self, x: u32);
/// }
/// mock!{
///     pub MyStruct<T: Clone + 'static> {
///         fn bar(&self) -> u8;
///     }
///     trait Foo {
///         fn foo(&self, x: u32);
///     }
/// }
/// # fn main() {}
/// ```
///
/// When mocking a generic struct's implementation of a generic trait, use the
/// same namespace for their generic parameters.  For example, if you wanted to
/// mock `Rc`, do
/// ```
/// # use mockall_derive::mock;
/// mock!{
///     pub Rc<T: 'static> {}
///     trait AsRef<T> {
///         fn as_ref(&self) -> &T;
///     }
/// }
/// # fn main() {}
/// ```
/// *not*
/// ```compile_fail
/// # use mockall_derive::mock;
/// mock!{
///     pub Rc<Q: 'static> {}
///     trait AsRef<T: 'static> {
///         fn as_ref(&self) -> &T;
///     }
/// }
/// # fn main() {}
/// ```
/// Associated types can easily be mocked by specifying a concrete type in the
/// `mock!{}` invocation.  But be careful not to reference the associated type
/// in the signatures of any of the trait's methods; repeat the concrete type
/// instead.  For example, do:
/// ```
/// # use mockall_derive::mock;
/// mock!{
///     MyIter {}
///     trait Iterator {
///         type Item=u32;
///
///         fn next(&mut self) -> Option<u32>;
///     }
/// }
/// # fn main() {}
/// ```
/// *not*
/// ```compile_fail
/// # use mockall_derive::mock;
/// mock!{
///     MyIter {}
///     trait Iterator {
///         type Item=u32;
///
///         fn next(&mut self) -> Option<<Self as Iterator>::Item>;
///     }
/// }
/// # fn main() {}
/// ```
#[proc_macro]
pub fn mock(item: proc_macro::TokenStream) -> proc_macro::TokenStream {
    do_mock(item.into()).into()
}

/// Automatically generate mock types for structs and traits.
///
/// This is by far the easiest way to use Mockall.  It works on almost all
/// traits, and almost all structs that have a single `impl` block.  In either
/// case, it will generate a mock struct whose name is the name of the mocked
/// struct/trait prepended with "Mock".  For each method of the original, the
/// mock struct will have a method named `expect_whatever` that allows you to
/// set expectations.  There will also be one `checkpoint` method that calls
/// [`checkpoint`] for every single method.
///
/// # Examples
///
/// The simplest use case is mocking a no-frills trait
/// ```
/// # use mockall_derive::*;
/// #[automock]
/// pub trait Foo {
///     fn foo(&self, key: i16);
/// }
///
/// let mock = MockFoo::new();
/// ```
///
/// Mocking a structure:
/// ```
/// # use mockall_derive::*;
/// struct Foo {}
/// #[automock]
/// impl Foo {
///     fn foo(&self) -> u32 {
///         // ...
///         # unimplemented!()
///     }
/// }
/// ```
///
/// Mocking a trait with associated types requires adding a metaitem to the
/// attribute:
/// ```
/// # use mockall_derive::*;
/// #[automock(type Item=u32;)]
/// trait Foo {
///     type Item;
///     fn foo(&self) -> Self::Item;
/// }
/// ```
///
/// Finally, `#[automock]` can also mock foreign functions.  This requires
/// another metaitem to specify the mock module name.
///
/// ```
/// # use mockall_derive::*;
/// #[automock(mod mock_ffi;)]
/// extern "C" {
///     pub fn foo() -> u32;
/// }
/// ```
///
/// [`checkpoint`]: ../mockall/struct.Expectations.html#method.checkpoint
#[proc_macro_attribute]
pub fn automock(attrs: proc_macro::TokenStream, input: proc_macro::TokenStream)
    -> proc_macro::TokenStream
{
    let input: proc_macro2::TokenStream = input.into();
    let mut output = input.clone();
    output.extend(do_automock(attrs.into(), input));
    output.into()
}
