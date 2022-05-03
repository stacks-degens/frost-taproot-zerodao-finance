use std::collections::*;

use digest::{Digest, FixedOutput};
use elliptic_curve as ec;
use ff::{Field, PrimeField};
use rand::SeedableRng;
use rand::{rngs::OsRng, CryptoRng, Rng, RngCore};
use thiserror::Error;
use vsss_rs::{Feldman, FeldmanVerifier};
use vsss_rs::{Shamir, Share as ShamirShare};

use ec::group::{Curve, Group, GroupEncoding, ScalarMul};
use ec::ScalarArithmetic;

trait GroupElem<F: PrimeField>: Curve + GroupEncoding + Default + ScalarMul<F> {}

impl<F: PrimeField, T> GroupElem<F> for T where T: Curve + GroupEncoding + Default + ScalarMul<F> {}

#[derive(Debug, Error)]
pub enum Error {
    #[error("no other participants specified")]
    NoOtherParticipants,

    #[error("unimplemented")]
    Unimplemented,
}

struct ParticipantState<F: PrimeField, G: GroupElem<F>> {
    round: u32,

    // Setup variables
    id: u32,
    feldman: Feldman,
    other_participant_shares: HashMap<u32, ParticipantData<F, G>>,
    ctx: u8, // not sure what this is, copied from the Go code

    // Round 1 variables.
    verifier: Option<FeldmanVerifier<G::Scalar, G>>,
    secret_shares: Option<Vec<ShamirShare>>,

    // Other variables (for unimplemented rounds)
    sk_share: Option<G::Scalar>,
    vk: Option<G>,
    vk_share: Option<G>,
}

struct ParticipantData<F: PrimeField, G: GroupElem<F>> {
    id: u32,
    share: Option<ShamirShare>,
    verifiers: Option<FeldmanVerifier<G::Scalar, G>>,
    _pd: ::std::marker::PhantomData<F>,
}

impl<F: PrimeField, G: GroupElem<F>> ParticipantState<F, G> {
    fn new(
        id: u32,
        thresh: u32,
        ctx: u8,
        other_participants: Vec<u32>,
    ) -> Result<ParticipantState<F, G>, Error> {
        if other_participants.is_empty() {
            return Err(Error::NoOtherParticipants);
        }

        let limit = other_participants.len() + 1;

        let rng = OsRng::default();

        let feldman = vsss_rs::Feldman {
            n: limit,
            t: thresh as usize,
        };

        let mut other_participant_shares = HashMap::new();
        for opid in other_participants {
            let pd: ParticipantData<F, G> = ParticipantData {
                id: opid,
                share: None,
                verifiers: None,
                _pd: ::std::marker::PhantomData,
            };

            other_participant_shares.insert(opid, pd);
        }

        Ok(ParticipantState {
            round: 1,
            id,
            feldman,
            other_participant_shares,
            ctx,
            sk_share: None,
            vk: None,
            vk_share: None,
            verifier: None,
            secret_shares: None,
        })
    }
}

struct Round1Bcast<F: PrimeField, G: GroupElem<F>> {
    verifiers: FeldmanVerifier<G::Scalar, G>,
    wi: G::Scalar,
    ci: G::Scalar,
    _pd: ::std::marker::PhantomData<F>,
}

struct Round1Result<F: PrimeField, G: GroupElem<F>> {
    broadcast: Round1Bcast<G::Scalar, G>,
    p2p: ShamirShare,
    _pd: ::std::marker::PhantomData<F>,
}

type Round1Send = HashMap<u32, ShamirShare>;

#[derive(Debug, Error)]
enum Round1Error {
    #[error("wrong round {0}")]
    WrongRound(u32),

    #[error("feldman: {0:?}")]
    Feldman(vsss_rs::Error),

    #[error("unimplemented")]
    Unimplemented,
}

fn round_1<F: PrimeField, G: GroupElem<F>, R: RngCore + CryptoRng>(
    participant: &mut ParticipantState<F, G>,
    secret: G::Scalar,
    rng: &mut R,
) -> Result<(Round1Bcast<F, G>, Round1Send), Round1Error> {
    if participant.round != 1 {
        return Err(Round1Error::WrongRound(participant.round));
    }

    // TODO should we check the number of participants?

    // There was some stuff here but due to Rust we don't need it.
    let s = secret;

    // Step 1 - (Aj0,...Ajt), (xi1,...,xin) <- FeldmanShare(s)
    let (shares, verifier) = participant
        .feldman
        .split_secret::<G::Scalar, G, _>(s, None, rng)
        .map_err(Round1Error::Feldman)?;

    // Step 2 - Sample ki <- Z_q
    let ki = G::Scalar::random(rng);

    // Step 3 - Compute Ri = ki*G
    let ri = G::generator() * ki;

    // Step 4 - Compute Ci = H(i, CTX, g^{a_(i,0)}, R_i), where CTX is fixed context string
    let mut buf = Vec::new();
    buf.extend(u32::to_be_bytes(participant.id));
    buf.push(participant.ctx);
    buf.extend(verifier.commitments[0].to_bytes().as_ref()); // TODO `.to_affine()`?
    buf.extend(ri.to_bytes().as_ref());

    // Figure out the hash-to-field thing.
    let ci = hash_to_field::<G::Scalar>(&buf);

    // Step 5 - Compute Wi = ki+a_{i,0}*c_i mod q. Note that a_{i,0} is the secret.
    //
    // The original code is `wi := s.MulAdd(ci, ki)`.
    //
    // The original MulAdd function doc is:
    // ```
    // // MulAdd returns element * y + z mod p
    // MulAdd(y, z Scalar) Scalar
    // ```
    // So we just do this in two steps.
    //
    // There's also this note on the original line:
    //
    // > Note: We have to compute scalar in the following way when using ed25519 curve, rather than scalar := dp.Scalar.Mul(s, Ci)
    // > there is an invalid encoding error when we compute scalar as above.
    let wi = (s * ci) + ki;

    // Step 6 - Broadcast (Ci, Wi, Ci) to other participants
    let r1bc = Round1Bcast {
        verifiers: verifier.clone(),
        wi,
        ci,
        _pd: ::std::marker::PhantomData,
    };

    // Step 7 - P2PSend f_i(j) to each participant Pj and keep (i, f_j(i)) for himself
    let mut p2p_send = HashMap::new();
    for i in 0..participant.other_participant_shares.len() {
        p2p_send.insert(i as u32, shares[i - 1].clone());
    }

    // Save the things generated in step 1.
    participant.verifier = Some(verifier);
    participant.secret_shares = Some(shares);

    // Update round counter.
    participant.round = 2;

    Ok((r1bc, p2p_send))
}

// TODO Verify this is a valid way to do it.
fn hash_to_curve<G: Group>(buf: &[u8]) -> G {
    let mut rng = hash_to_chacha20(buf);
    G::random(&mut rng)
}

// TODO Verify this is a valid way to do it.
fn hash_to_field<F: PrimeField>(buf: &[u8]) -> F {
    let mut rng = hash_to_chacha20(buf);
    F::random(&mut rng)
}

// TODO Verify this is a valid way to do it.
fn hash_to_chacha20(buf: &[u8]) -> rand_chacha::ChaCha20Rng {
    let mut comm_digest = sha2::Sha256::default();
    comm_digest.update(&buf);
    let mut comm_hash: [u8; 32] = [0u8; 32];
    let comm_hash_out = comm_digest.finalize();

    for i in 0..32 {
        // FIXME Why do I have to do this byte-by-byte?  Where is copy_from_slice?
        comm_hash[i] = comm_hash_out[i];
    }

    rand_chacha::ChaCha20Rng::from_seed(comm_hash)
}

struct Round2Bcast<F: PrimeField, G: GroupElem<F>> {
    vk: G,
    vk_share: G,
    _pd: ::std::marker::PhantomData<F>,
}

#[derive(Debug, Error)]
enum Round2Error {
    #[error("commitment is zero")]
    CommitmentZero,

    #[error("commitment not on curve")]
    CommitmentNotOnCurve,

    #[error("hash check failed with participant {0}")]
    HashCheckFailed(u32),

    #[error("p2psend table missing participent {0}")]
    PeerSendMissing(u32),

    #[error("feldman verify failed for participant {0}")]
    PeerFeldmanVerifyFailed(u32),
}

fn round_2<F: PrimeField, G: GroupElem<F>>(
    participant: &mut ParticipantState<F, G>,
    bcast: &HashMap<u32, Round1Bcast<F, G>>,
    p2psend: HashMap<u32, ShamirShare>,
) -> Result<Round2Bcast<F, G>, Round2Error> {
    // We should validate Wi and Ci values in Round1Bcats
    for (id, bc) in bcast.iter() {
        if bc.ci.is_zero() {
            return Err(Round2Error::CommitmentZero);
        }

        // The Go code also verifies that the point is on the curve, but Rust is
        // better than Go, so it's not possible for our libs to construct an
        // instance of a group element that isn't a valid group element, because
        // obviously lol.
    }

    // Step 2 - for j in 1,...,n
    for (id, bc) in bcast.iter() {
        // Step 3 - if j == i, continue
        if *id == participant.id {
            continue;
        }

        // Step 4 - Check equation c_j = H(j, CTX, A_{j,0}, g^{w_j}*A_{j,0}^{-c_j}
        // Get Aj0
        let aj0 = &bc.verifiers.commitments[0];

        // Compute g^{w_j}
        let prod1 = G::generator() * bc.wi;

        // Compute A_{j,0}^{-c_j}, and sum.
        let prod2 = *aj0 * bc.ci.invert().unwrap(); // checked nonzero
        let prod = prod1 + prod2;

        // Now build commitment.
        let mut buf = Vec::new();
        buf.extend(u32::to_be_bytes(participant.id));
        buf.push(participant.ctx);
        buf.extend(aj0.to_bytes().as_ref()); // TODO `.to_affine()`?
        buf.extend(prod.to_bytes().as_ref());

        // Figure out the hash-to-field thing.
        let cj = hash_to_field::<G::Scalar>(&buf);

        // Check equation.
        if cj != bc.ci {
            return Err(Round2Error::HashCheckFailed(*id));
        }

        // Step 5 - FeldmanVerify
        let fji = p2psend
            .get(id)
            .ok_or_else(|| Round2Error::PeerSendMissing(*id))?;

        if !bc.verifiers.verify(fji) {
            return Err(Round2Error::PeerFeldmanVerifyFailed(*id));
        }
    }

    // FIXME convert to soft error?
    // FIXME BROKEN
    let sk_bytes = &participant.secret_shares.unwrap()[participant.id as usize].0;
    let mut sk = G::Scalar::from_repr(sk_bytes).expect("shamir share parse as scalar failed");

    let mut vk: G = participant.verifier.unwrap().commitments[0];

    // Step 6 - Compute signing key share ski = \sum_{j=1}^n xji
    for (id, bc) in bcast.iter() {
        if *id == participant.id {
            continue;
        }

        // FIXME convert to soft error?
        // FIXME BROKEN
        let t2 = G::Scalar::from_repr(&p2psend[id].0).expect("p2psend parse failed");
        sk += t2;
    }

    // Step 8 - Compute verification key vk = sum(A_{j,0}), j = 1,...,n
    for (id, bc) in bcast.iter() {
        if *id == participant.id {
            continue;
        }

        vk += bc.verifiers.commitments[0];
    }

    // Store signing key share.
    participant.sk_share = Some(sk);

    // Step 7 - Compute verification key share vki = ski*G and store.
    let vk_share = G::generator() * sk;
    participant.vk_share = Some(vk_share);

    // Store verification key.
    participant.vk = Some(vk);

    // Update round number.
    participant.round = 3;

    Ok(Round2Bcast {
        vk,
        vk_share,
        _pd: ::std::marker::PhantomData,
    })
}