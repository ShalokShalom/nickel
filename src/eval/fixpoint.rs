//! Compute the fixpoint of a recursive record.
use super::*;

/// Build a recursive environment from record bindings. For each field, `rec_env` either extracts
/// the corresponding thunk from the environment in the general case, or create a closure on the
/// fly if the field is a constant. The resulting environment is to be passed to the
/// [`patch_field`] function.
pub fn rec_env<'a, I: Iterator<Item = (&'a Ident, &'a RichTerm)>>(
    bindings: I,
    env: &Environment,
) -> Result<Vec<(Ident, Thunk)>, EvalError> {
    bindings
        .map(|(id, rt)| match rt.as_ref() {
            Term::Var(ref var_id) => {
                let thunk = env
                    .get(var_id)
                    .ok_or_else(|| EvalError::UnboundIdentifier(var_id.clone(), rt.pos))?;
                Ok((id.clone(), thunk))
            }
            _ => {
                // If we are in this branch, `rt` must be a constant after the share normal form
                // transformation, hence it should not need an environment, which is why it is
                // dropped.
                let closure = Closure {
                    body: rt.clone(),
                    env: Environment::new(),
                };
                Ok((id.clone(), Thunk::new(closure, IdentKind::Let)))
            }
        })
        .collect()
}

/// Update the environment of the content of a recursive record field by extending it with a
/// recursive environment.
pub fn patch_field<F: Fn(&(Ident, Thunk)) -> bool>(
    rt: &RichTerm,
    rec_env: &[(Ident, Thunk)],
    env: &Environment,
    filter_env: F,
) -> Result<(), EvalError> {
    if let Term::Var(var_id) = &*rt.term {
        let mut thunk = env
            .get(var_id)
            .ok_or_else(|| EvalError::UnboundIdentifier(var_id.clone(), rt.pos))?;
        thunk
            .borrow_mut()
            .env
            .extend(rec_env.iter().cloned().filter(filter_env));
    }
    // Thanks to the share normal form transformation, the content is either a constant or a
    // variable. In the constant case, the environment is irrelevant and we don't have to do
    // anything in the `else` case.
    Ok(())
}
