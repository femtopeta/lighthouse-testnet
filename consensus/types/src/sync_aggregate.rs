use crate::consts::altair::SYNC_COMMITTEE_SUBNET_COUNT;
use crate::test_utils::TestRandom;
use crate::{AggregateSignature, BitVector, EthSpec, SyncCommitteeContribution};
use derivative::Derivative;
use safe_arith::{ArithError, SafeArith};
use serde::{Deserialize, Serialize};
use ssz_derive::{Decode, Encode};
use test_random_derive::TestRandom;
use tree_hash_derive::TreeHash;

#[derive(Debug, PartialEq)]
pub enum Error {
    SszTypesError(ssz_types::Error),
    ArithError(ArithError),
}

impl From<ArithError> for Error {
    fn from(e: ArithError) -> Error {
        Error::ArithError(e)
    }
}

#[derive(
    Debug,
    Clone,
    Serialize,
    Deserialize,
    Encode,
    Decode,
    TreeHash,
    TestRandom,
    Derivative,
    arbitrary::Arbitrary,
)]
#[derivative(PartialEq, Hash(bound = "E: EthSpec"))]
#[serde(bound = "E: EthSpec")]
#[arbitrary(bound = "E: EthSpec")]
pub struct SyncAggregate<E: EthSpec> {
    pub sync_committee_bits: BitVector<E::SyncCommitteeSize>,
    pub sync_committee_signature: AggregateSignature,
}

impl<E: EthSpec> SyncAggregate<E> {
    /// New aggregate to be used as the seed for aggregating other signatures.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            sync_committee_bits: BitVector::default(),
            sync_committee_signature: AggregateSignature::infinity(),
        }
    }

    /// Create a `SyncAggregate` from a slice of `SyncCommitteeContribution`s.
    ///
    /// Equivalent to `process_sync_committee_contributions` from the spec.
    pub fn from_contributions(
        contributions: &[SyncCommitteeContribution<E>],
    ) -> Result<SyncAggregate<E>, Error> {
        let mut sync_aggregate = Self::new();
        let sync_subcommittee_size =
            E::sync_committee_size().safe_div(SYNC_COMMITTEE_SUBNET_COUNT as usize)?;
        for contribution in contributions {
            for (index, participated) in contribution.aggregation_bits.iter().enumerate() {
                if participated {
                    let participant_index = sync_subcommittee_size
                        .safe_mul(contribution.subcommittee_index as usize)?
                        .safe_add(index)?;
                    sync_aggregate
                        .sync_committee_bits
                        .set(participant_index, true)
                        .map_err(Error::SszTypesError)?;
                }
            }
            sync_aggregate
                .sync_committee_signature
                .add_assign_aggregate(&contribution.signature);
        }
        Ok(sync_aggregate)
    }

    /// Empty aggregate to be used at genesis.
    ///
    /// Contains an empty signature and should *not* be used as the starting point for aggregation,
    /// use `new` instead.
    pub fn empty() -> Self {
        Self {
            sync_committee_bits: BitVector::default(),
            sync_committee_signature: AggregateSignature::empty(),
        }
    }

    /// Returns how many bits are `true` in `self.sync_committee_bits`.
    pub fn num_set_bits(&self) -> usize {
        self.sync_committee_bits.num_set_bits()
    }
}
