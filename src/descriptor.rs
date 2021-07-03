use bdk::keys::{KeyError, IntoDescriptorKey};
use bdk::miniscript::Segwitv0;
use bdk::template::{DescriptorTemplate, DescriptorTemplateOut};
use bdk::descriptor::DescriptorError;

pub struct MoveOrRedeemWithTimeLock<K: IntoDescriptorKey<Segwitv0>> {
    move_key: K,
    redeem_key: K,
}

impl<K: IntoDescriptorKey<Segwitv0>> MoveOrRedeemWithTimeLock<K> {
    pub fn new(move_key: K, redeem_key: K) -> Self {
        Self {
            move_key,
            redeem_key,
        }
    }
}

impl<K: IntoDescriptorKey<Segwitv0>> DescriptorTemplate for MoveOrRedeemWithTimeLock<K> {
    fn build(self) -> Result<DescriptorTemplateOut, DescriptorError> {
        let move_key = self.move_key;
        let redeem_key = self.redeem_key.into_descriptor_key()?;

        // This descriptor is equivalent to the following miniscript:
        // andor(pk(redeem_key),older(1000),pk(move_key))
        // TODO: try an other script that takes a low likelihood of a redemption into account:
        // or_d(pk(redeem_key),and_v(v:pkh(move_key),older(1000)))
        let desc = bdk::descriptor!(
            wsh (
                and_or (
                    pk (redeem_key),
                    older (1000),
                    pk (move_key),
                )
            )
        )?;

        Ok(desc)
    }
}
