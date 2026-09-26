mod numeric;
mod scalar;

use crate::BuiltinRegistry;

pub(crate) fn install_scalar_builtins(registry: BuiltinRegistry) -> BuiltinRegistry {
    numeric::install(scalar::install(registry))
}
