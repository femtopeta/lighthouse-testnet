use super::get_attesting_indices;
use crate::per_block_processing::errors::{AttestationInvalid as Invalid, BlockOperationError};
use types::*;

type Result<T> = std::result::Result<T, BlockOperationError<Invalid>>;

/// Convert `attestation` to (almost) indexed-verifiable form.
///
/// Spec v0.12.1
pub fn get_indexed_attestation<E: EthSpec>(
    committee: &[usize],
    attestation: &Attestation<E>,
) -> Result<IndexedAttestation<E>> {
    let attesting_indices = get_attesting_indices::<E>(committee, &attestation.aggregation_bits)?;

    Ok(IndexedAttestation {
        attesting_indices: VariableList::new(attesting_indices)?,
        data: attestation.data.clone(),
        signature: attestation.signature.clone(),
    })
}
