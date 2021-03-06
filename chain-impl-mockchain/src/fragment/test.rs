#[warn(unused_imports)]
use super::*;
use crate::config::ConfigParam;
use quickcheck::{Arbitrary, Gen, TestResult};
use quickcheck_macros::quickcheck;

impl Arbitrary for Fragment {
    fn arbitrary<G: Gen>(g: &mut G) -> Self {
        match g.next_u32() % 10 {
            0 => Fragment::Initial(Arbitrary::arbitrary(g)),
            1 => Fragment::OldUtxoDeclaration(Arbitrary::arbitrary(g)),
            2 => Fragment::Transaction(Arbitrary::arbitrary(g)),
            3 => Fragment::OwnerStakeDelegation(Arbitrary::arbitrary(g)),
            4 => Fragment::StakeDelegation(Arbitrary::arbitrary(g)),
            5 => Fragment::PoolRegistration(Arbitrary::arbitrary(g)),
            6 => Fragment::PoolRetirement(Arbitrary::arbitrary(g)),
            //7 => Fragment::PoolUpdate(Arbitrary::arbitrary(g)),
            8 => Fragment::UpdateProposal(Arbitrary::arbitrary(g)),
            _ => Fragment::UpdateVote(Arbitrary::arbitrary(g)),
        }
    }
}

#[quickcheck]
fn fragment_serialization_bijection(b: Fragment) -> TestResult {
    let b_got = Fragment::from_raw(&b.to_raw()).unwrap();
    TestResult::from_bool(b == b_got)
}

quickcheck! {
    fn initial_ents_serialization_bijection(b: ConfigParams) -> TestResult {
        property::testing::serialization_bijection_r(b)
    }
}

impl Arbitrary for ConfigParams {
    fn arbitrary<G: Gen>(g: &mut G) -> Self {
        let size = u8::arbitrary(g) as usize;
        ConfigParams(
            std::iter::repeat_with(|| ConfigParam::arbitrary(g))
                .take(size)
                .collect(),
        )
    }
}
