//! The completed pre-admission failure boundary; admitted peers and publication errors stay separate.

use super::*;

/// Preserves the original failed setup result before any peer enters the occupied set.
pub(in crate::native_account::hosted_world) async fn recover(
    _world: &mut NativeHostedWorld,
    _process: &NativeHostedProcess,
    error: io::Error,
) -> io::Result<()> {
    Err(error)
}
