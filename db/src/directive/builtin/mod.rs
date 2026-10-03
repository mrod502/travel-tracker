//! The directives the library ships with, and the one call that registers them.

pub mod skip_tx;
pub mod up_down;

use crate::directive::registry::DirectiveRegistry;
use crate::error::RegistrationError;

use skip_tx::SkipTx;
use up_down::{DownBegin, DownEnd, UpBegin, UpEnd};

/// Register every built-in directive on `registry`.
///
/// Fails on the first duplicate key rather than overwriting — see
/// [`DirectiveRegistry::register`]. Prefer
/// [`DirectiveRegistry::with_builtins`](DirectiveRegistry::with_builtins) over
/// calling this directly unless the registry already holds something.
pub fn register_all(registry: &mut DirectiveRegistry) -> Result<(), RegistrationError> {
    registry.register::<UpBegin>()?;
    registry.register::<UpEnd>()?;
    registry.register::<DownBegin>()?;
    registry.register::<DownEnd>()?;
    registry.register::<SkipTx>()?;
    Ok(())
}
