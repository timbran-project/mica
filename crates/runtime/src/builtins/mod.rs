mod assembly;
mod numeric;
mod scalar;

use crate::BuiltinRegistry;

pub(crate) fn install_scalar_builtins(registry: BuiltinRegistry) -> BuiltinRegistry {
    assembly::install(numeric::install(scalar::install(registry)))
}
