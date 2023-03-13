//! Compute the fixpoint of a recursive record.
use std::rc::Rc;

use super::*;
use crate::{
    label::ty_path::{Elem, Path},
    position::TermPos,
    types::{RecordRowF, TypeF, Types},
};

// Update the environment of a term by extending it with a recursive environment. In the general
// case, the term is expected to be a variable pointing to the element to be patched. Otherwise, it's
// considered to have no dependencies and is left untouched.
//
// This function achieve the same as `patch_field`, but is somehow lower-level, as it operates on a
// general `RichTerm` instead of a `Field`. In practice, the patched term is either the value of a
// field or one of its pending contract.
fn patch_term<C: Cache>(
    cache: &mut C,
    term: &RichTerm,
    rec_env: &[(Ident, CacheIndex)],
    env: &Environment,
) -> Result<(), EvalError> {
    if let Term::Var(var_id) = &*term.term {
        // TODO: Shouldn't be mutable, [`CBNCache`] abstraction is leaking.
        let mut idx = env
            .get(var_id)
            .cloned()
            .ok_or(EvalError::UnboundIdentifier(*var_id, term.pos))?;

        cache.build_cached(&mut idx, rec_env);
    };

    Ok(())
}

/// Build a recursive environment from record bindings. For each field, `rec_env` either extracts
/// the corresponding cache element from the environment in the general case, or create a closure
/// on the fly if the field is a constant. The resulting environment is to be passed to the
/// [`patch_field`] function.
///
/// # Pending contracts
///
/// Since the implementation of RFC005, contract are lazily applied at the level of record fields.
/// Consider the following example:
///
/// ```nickel
/// let Schema = {bar | default = "hello"} in
///
/// {
///   foo | Schema = {},
///   baz = foo.bar
/// }
/// ```
///
/// This program is valid and should output `{foo.bar = "hello", baz = "hello"}`. To make this
/// happen, we have to ensure the pending contract `Schema` is applied to the recursive occurrence
/// `foo` used in the definition `baz = foo.bar`. That is, we must apply pending contracts to their
/// corresponding field when building the recursive environment.
pub fn rec_env<'a, I: Iterator<Item = (&'a Ident, &'a Field)>, C: Cache>(
    cache: &mut C,
    bindings: I,
    env: &Environment,
    pos_record: TermPos,
) -> Result<Vec<(Ident, CacheIndex)>, EvalError> {
    bindings
        .map(|(id, field)| {
            if let Some(ref value) = field.value {
                let idx = match value.as_ref() {
                    Term::Var(ref var_id) => {
                        let idx = env
                            .get(var_id)
                            .cloned()
                            .ok_or(EvalError::UnboundIdentifier(*var_id, value.pos))?;
                        idx
                    }
                    _ => {
                        // If we are in this branch, `rt` must be a constant after the share normal form
                        // transformation, hence it should not need an environment, which is why it is
                        // dropped.
                        let closure = Closure {
                            body: value.clone(),
                            env: Environment::new(),
                        };

                        cache.add(closure, IdentKind::Record, BindingType::Normal)
                    }
                };

                // We now need to wrap the binding in a value with contracts applied.
                // Pending contracts might use identifiers from the current record's environment,
                // so we start from in the environment of the original record.
                let mut final_env = env.clone();
                let id_value = Ident::fresh();
                final_env.insert(id_value, idx);

                fn descend_path(t: &Types, path: &[Elem]) -> Types {
                    match (&t.types, path.first()) {
                        (
                            TypeF::Forall {
                                body,
                                var,
                                var_kind,
                            },
                            Some(_),
                        ) => Types {
                            types: TypeF::Forall {
                                body: Box::new(descend_path(body.as_ref(), path)),
                                var: *var,
                                var_kind: *var_kind,
                            },
                            ..*t
                        },
                        (TypeF::Arrow(dom, _), Some(Elem::Domain)) => {
                            descend_path(dom.as_ref(), &path[1..])
                        }
                        (TypeF::Arrow(_, codom), Some(Elem::Codomain)) => {
                            descend_path(codom.as_ref(), &path[1..])
                        }
                        (TypeF::Record(rows), Some(Elem::Field(ident))) => {
                            use crate::types::RecordRowsIteratorItem::*;
                            for row_item in rows.iter() {
                                match row_item {
                                    Row(RecordRowF { id, types: ty }) if id == *ident => {
                                        return descend_path(ty, &path[1..])
                                    }
                                    TailDyn | TailVar(_) => unreachable!(),
                                    _ => (),
                                }
                            }
                            unreachable!()
                        }
                        (TypeF::Dict(ty), Some(Elem::Dict))
                        | (TypeF::Array(ty), Some(Elem::Array)) => {
                            descend_path(ty.as_ref(), &path[1..])
                        }
                        (_, None) => t.clone(),
                        (_, Some(_)) => unreachable!(),
                    }
                }

                let with_ctr_applied = PendingContract::apply_all(
                    RichTerm::new(Term::Var(id_value), value.pos),
                    field
                        .pending_contracts
                        .iter()
                        .cloned()
                        .map(|ctr| PendingContract {
                            contract: ctr.contract,
                            dual_contract: //None,

                            ctr.dual_contract.or_else(|| {
                                eprintln!(
                                    "{}: starting {} {:?}",
                                    id, ctr.label.types, ctr.label.path
                                );
                                let types = descend_path(ctr.label.types.as_ref(), &ctr.label.path);
                                eprintln!("{}: operating with {}", id, types);

                                let dual_contract = types.dual_contract().unwrap();
                                let contract = types.contract().unwrap();

                                eprintln!("polarity: {}\ndual_contract: {dual_contract}\ncontract: {contract}\n", ctr.label.polarity);

                                if ctr.label.polarity {
                                    Some(contract)
                                } else {
                                    Some(dual_contract)
                                }
                            }),
                            label: ctr.label,
                        }),
                    value.pos,
                );

                let final_closure = Closure {
                    body: with_ctr_applied,
                    env: final_env,
                };

                Ok((
                    *id,
                    cache.add(final_closure, IdentKind::Record, BindingType::Normal),
                ))
            } else {
                let error = EvalError::MissingFieldDef {
                    id: *id,
                    metadata: field.metadata.clone(),
                    pos_record,
                    // The access is not yet known (there may not be any access, if this error is
                    // never raised).
                    //
                    // This field is filled by the evaluation function when a `MissingFieldDef` is
                    // extracted from the environment.
                    pos_access: TermPos::None,
                };

                let closure = Closure {
                    body: RichTerm::from(Term::RuntimeError(error)),
                    env: Environment::new(),
                };

                Ok((
                    *id,
                    cache.add(closure, IdentKind::Record, BindingType::Normal),
                ))
            }
        })
        .collect()
}

/// Update the environment of the content of a recursive record field by extending it with a
/// recursive environment. When seeing revertible elements as a memoizing device for functions, this
/// step correspond to function application (see documentation of [crate::eval::lazy::ThunkData]).
///
/// For each field, retrieve the set set of dependencies from the corresponding element in the
/// environment, and only add those dependencies to the environment. This avoids retaining
/// reference-counted pointers to unused data. If no dependencies are available, conservatively add
/// all the recursive environment. See [`crate::transform::free_vars`].
pub fn patch_field<C: Cache>(
    cache: &mut C,
    field: &Field,
    rec_env: &[(Ident, CacheIndex)],
    env: &Environment,
) -> Result<(), EvalError> {
    if let Some(ref value) = field.value {
        patch_term(cache, value, rec_env, env)?;
    }

    // We must patch the contracts contained in the fields' pending contracts as well, since they
    // can depend recursively on other fields, as in:
    //
    // ```
    // let Variant = match {
    //   `num => Number,
    //   `str => String,
    //   `any => Dyn,
    // } in
    //
    // {
    //   tag | default = `num,
    //   value | Variant tag,
    // }
    // ```
    //
    // Here, `Variant` depends on `tag` recursively.
    for pending_contract in field.pending_contracts.iter() {
        patch_term(cache, &pending_contract.contract, rec_env, env)?;
    }

    Ok(())
}
