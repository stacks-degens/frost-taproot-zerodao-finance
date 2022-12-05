use digest::Digest;
use elliptic_curve as ec;
pub use ff::{Field, PrimeField};
use rand::SeedableRng;

use ec::group::{Curve, Group, GroupEncoding, ScalarMul};
use sha2::Sha256;

use super::hash::*;
use super::math::{Math, Secp256k1Math};

/// In the Go code this is just a hash-to-field of the serialized inputs, so we
/// could make this totally general and implement it for all `Math`s, it seems.
pub trait ChallengeDeriver<M: Math>: Clone {
    fn derive_challenge(&self, msg: &[u8; 32], pk: M::G, r: M::G) -> <M::G as Group>::Scalar;
}

#[derive(Clone)]
pub struct UniversalChderiv;

impl<M: Math> ChallengeDeriver<M> for UniversalChderiv {
    fn derive_challenge(&self, msg: &[u8; 32], pk: M::G, r: M::G) -> <M::G as Group>::Scalar {
        let mut buf = msg.to_vec();
        buf.extend(M::group_repr_to_bytes(pk.to_bytes()));
        buf.extend(M::group_repr_to_bytes(r.to_bytes()));
        hash_to_field(&buf)
    }
}

/// Taken from k256 code.
const BIP340_CHALLENGE_TAG: &[u8] = b"BIP0340/challenge";

/// Adapted from k256 code.  This is intended to slot into the FROST signing
/// rounds and generate challenges that are specifically compliant with the
/// BIP340 spec (tagged hash, x-only pubkey, etc.), and so it's specialized for
/// secp256k1 instead of being able to work on any curve.
#[derive(Clone)]
pub struct Bip340Chderiv;

impl ChallengeDeriver<Secp256k1Math> for Bip340Chderiv {
    fn derive_challenge(
        &self,
        msg_digest: &[u8; 32],
        pk: <Secp256k1Math as Math>::G,
        r: <Secp256k1Math as Math>::G,
    ) -> <<Secp256k1Math as Math>::G as Group>::Scalar {
        let res = tagged_hash(BIP340_CHALLENGE_TAG)
            .chain_update(r.to_bytes())
            .chain_update(&pk.to_affine().to_bytes()[1..33])
            .chain_update(msg_digest)
            .finalize();
        let fb =
            k256::FieldBytes::try_from(res).expect("chderiv: bip340 cast to field scalar repr");
        let parsed = k256::Scalar::from_repr(fb);

        if parsed.is_some().into() {
            parsed.unwrap()
        } else {
            panic!("chderiv: bip340 unrepr to field scalar")
        }
    }
}

/// Taken from k256 code.
fn tagged_hash(tag: &[u8]) -> Sha256 {
    let tag_hash = Sha256::digest(tag);
    let mut digest = Sha256::new();
    digest.update(&tag_hash);
    digest.update(&tag_hash);
    digest
}