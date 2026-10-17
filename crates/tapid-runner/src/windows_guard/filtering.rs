//! Persistent, transactionally published package-SID denial. Engine closure never lifts denial.
use super::{SERVICE, wide};
use sha2::{Digest, Sha256};
use std::{
    mem::zeroed,
    ptr::{null, null_mut},
};
use windows_sys::{
    Win32::{Foundation::*, NetworkManagement::WindowsFilteringPlatform::*},
    core::GUID,
};
const PROVIDER: GUID = GUID::from_u128(0x525298b8_f101_4507_bdbc_e127221f6101);
const SUBLAYER: GUID = GUID::from_u128(0x525298b8_f101_4507_bdbc_e127221f6102);
const ALREADY_EXISTS: u32 = 0x80320009;
pub(super) const MAX_FILTERS: u32 = 8192;
fn check(error: u32) -> Result<(), u32> {
    if error == 0 { Ok(()) } else { Err(error) }
}
pub(super) struct Engine(isize);
impl Drop for Engine {
    fn drop(&mut self) {
        unsafe {
            FwpmEngineClose0(self.0);
        }
    }
}
impl Engine {
    pub fn connect() -> Result<Self, u32> {
        let mut handle = 0;
        check(unsafe { FwpmEngineOpen0(null(), 10, null(), null(), &mut handle) })?;
        Ok(Self(handle))
    }
    pub fn open() -> Result<Self, u32> {
        let engine = Self::connect()?;
        let handle = engine.0;
        let mut name = wide("Tapid network denial v1");
        let mut service = wide(SERVICE);
        let mut provider: FWPM_PROVIDER0 = unsafe { zeroed() };
        provider.providerKey = PROVIDER;
        provider.displayData.name = name.as_mut_ptr();
        provider.flags = FWPM_PROVIDER_FLAG_PERSISTENT;
        provider.serviceName = service.as_mut_ptr();
        let error = unsafe { FwpmProviderAdd0(handle, &provider, null_mut()) };
        if error != ALREADY_EXISTS {
            check(error)?;
        }
        let mut key = PROVIDER;
        let mut sub: FWPM_SUBLAYER0 = unsafe { zeroed() };
        sub.subLayerKey = SUBLAYER;
        sub.providerKey = &mut key;
        sub.flags = FWPM_SUBLAYER_FLAG_PERSISTENT;
        sub.displayData.name = name.as_mut_ptr();
        sub.weight = 0xffff;
        let error = unsafe { FwpmSubLayerAdd0(handle, &sub, null_mut()) };
        if error != ALREADY_EXISTS {
            check(error)?;
        }
        Ok(engine)
    }
    pub fn count(&self) -> Result<u32, u32> {
        let mut template: FWPM_FILTER_ENUM_TEMPLATE0 = unsafe { zeroed() };
        let mut provider = PROVIDER;
        template.providerKey = &mut provider;
        template.enumType = FWP_FILTER_ENUM_FULLY_CONTAINED;
        template.actionMask = u32::MAX;
        let mut enumerator = 0;
        check(unsafe { FwpmFilterCreateEnumHandle0(self.0, &template, &mut enumerator) })?;
        let result = (|| {
            let mut total = 0u32;
            loop {
                let mut entries = null_mut();
                let mut count = 0;
                check(unsafe {
                    FwpmFilterEnum0(self.0, enumerator, 128, &mut entries, &mut count)
                })?;
                if !entries.is_null() {
                    unsafe {
                        FwpmFreeMemory0((&mut entries as *mut *mut *mut FWPM_FILTER0).cast());
                    }
                }
                total = total.checked_add(count).ok_or(ERROR_NOT_ENOUGH_QUOTA)?;
                if count == 0 {
                    return Ok(total);
                }
                if total > MAX_FILTERS {
                    return Err(ERROR_NOT_ENOUGH_QUOTA);
                }
            }
        })();
        unsafe {
            FwpmFilterDestroyEnumHandle0(self.0, enumerator);
        }
        result
    }
    pub fn deny(&self, sid: &mut [u32]) -> Result<[GUID; 2], u32> {
        if self.count()? > MAX_FILTERS - 2 {
            return Err(ERROR_NOT_ENOUGH_QUOTA);
        }
        let keys = keys(sid);
        self.transaction(|| {
            for (index, layer) in [
                FWPM_LAYER_ALE_RESOURCE_ASSIGNMENT_V4,
                FWPM_LAYER_ALE_RESOURCE_ASSIGNMENT_V6,
            ]
            .into_iter()
            .enumerate()
            {
                let mut condition: FWPM_FILTER_CONDITION0 = unsafe { zeroed() };
                condition.fieldKey = FWPM_CONDITION_ALE_PACKAGE_ID;
                condition.matchType = FWP_MATCH_EQUAL;
                condition.conditionValue.r#type = FWP_SID;
                condition.conditionValue.Anonymous.sid = sid.as_mut_ptr().cast();
                let mut filter: FWPM_FILTER0 = unsafe { zeroed() };
                let mut provider = PROVIDER;
                let mut name = wide("Tapid exact AppContainer bind denial");
                filter.filterKey = keys[index];
                filter.displayData.name = name.as_mut_ptr();
                filter.flags = FWPM_FILTER_FLAG_PERSISTENT;
                filter.providerKey = &mut provider;
                filter.layerKey = layer;
                filter.subLayerKey = SUBLAYER;
                filter.weight.r#type = FWP_UINT8;
                filter.weight.Anonymous.uint8 = 15;
                filter.numFilterConditions = 1;
                filter.filterCondition = &mut condition;
                filter.action.r#type = FWP_ACTION_BLOCK;
                // An existing key rejects the whole transaction; it never transfers an earlier lease.
                check(unsafe { FwpmFilterAdd0(self.0, &filter, null_mut(), null_mut()) })?;
            }
            Ok(())
        })?;
        Ok(keys)
    }
    pub fn remove_empty(&self, job: isize, keys: &[GUID; 2]) -> Result<(), u32> {
        if !super::identity::job_empty(job)? {
            return Err(ERROR_BUSY);
        }
        self.transaction(|| {
            for key in keys {
                check(unsafe { FwpmFilterDeleteByKey0(self.0, key) })?;
            }
            Ok(())
        })
    }
    fn transaction(&self, operation: impl FnOnce() -> Result<(), u32>) -> Result<(), u32> {
        check(unsafe { FwpmTransactionBegin0(self.0, 0) })?;
        let result = operation().and_then(|()| check(unsafe { FwpmTransactionCommit0(self.0) }));
        if result.is_err() {
            unsafe {
                FwpmTransactionAbort0(self.0);
            }
        }
        result
    }
}
fn keys(sid: &[u32]) -> [GUID; 2] {
    std::array::from_fn(|layer| {
        let mut hash = Sha256::new();
        hash.update(b"TapidNetworkGuardV1/package-bind-deny/");
        hash.update([layer as u8]);
        for word in sid {
            hash.update(word.to_le_bytes());
        }
        let bytes: [u8; 32] = hash.finalize().into();
        GUID::from_u128(u128::from_le_bytes(bytes[..16].try_into().unwrap()))
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guard_filter_keys_bind_both_layer_and_entire_sid() {
        let a = keys(&[1, 2, 3]);
        let b = keys(&[1, 2, 4]);
        let bytes = |g: GUID| format!("{}:{}:{}:{:?}", g.data1, g.data2, g.data3, g.data4);
        assert_ne!(bytes(a[0]), bytes(a[1]));
        assert_ne!(bytes(a[0]), bytes(b[0]));
        assert_eq!(bytes(a[0]), bytes(keys(&[1, 2, 3])[0]));
    }
}
